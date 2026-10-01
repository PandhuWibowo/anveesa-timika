//! AWS KMS as the key service, via the KMS JSON API with SigV4 signing.
//!
//! SEAL_AWSKMS_KEY_ID      key id, ARN or alias (`alias/timika-unseal`)
//! SEAL_AWSKMS_REGION      default $AWS_REGION / $AWS_DEFAULT_REGION
//! SEAL_AWSKMS_ENDPOINT    override (VPC endpoint, LocalStack)
//!
//! Credentials, first match wins (the standard AWS chain, minus config files):
//!   1. AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY [/ AWS_SESSION_TOKEN]
//!   2. web identity — EKS IRSA: AWS_WEB_IDENTITY_TOKEN_FILE + AWS_ROLE_ARN
//!   3. ECS task role: AWS_CONTAINER_CREDENTIALS_RELATIVE_URI / _FULL_URI
//!   4. EC2 instance profile via IMDSv2
//!
//! The IAM policy needs only kms:Encrypt and kms:Decrypt on that key.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{env, require};

#[derive(Clone)]
pub struct Creds {
    pub access_key: String,
    pub secret_key: String,
    pub token: Option<String>,
    pub expires: Option<DateTime<Utc>>,
}

pub struct AwsKms {
    key_id: String,
    region: String,
    endpoint: String,
    http: reqwest::Client,
    cached: Mutex<Option<Creds>>,
}

impl AwsKms {
    pub fn from_env(http: reqwest::Client) -> anyhow::Result<Self> {
        let region = env("SEAL_AWSKMS_REGION")
            .or_else(|| env("AWS_REGION"))
            .or_else(|| env("AWS_DEFAULT_REGION"))
            .ok_or_else(|| anyhow::anyhow!("SEAL_TYPE=awskms needs SEAL_AWSKMS_REGION or AWS_REGION"))?;
        Ok(Self {
            key_id: require("SEAL_AWSKMS_KEY_ID")?,
            endpoint: env("SEAL_AWSKMS_ENDPOINT")
                .unwrap_or_else(|| format!("https://kms.{region}.amazonaws.com"))
                .trim_end_matches('/')
                .to_string(),
            region,
            http,
            cached: Mutex::new(None),
        })
    }

    pub fn describe(&self) -> String {
        format!("AWS KMS key `{}` in {}", self.key_id, self.region)
    }

    pub async fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<(String, String)> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let v = self.call("Encrypt", json!({ "KeyId": self.key_id, "Plaintext": b64.encode(plaintext) })).await?;
        let blob = v["CiphertextBlob"].as_str().ok_or_else(|| anyhow::anyhow!("kms: no CiphertextBlob"))?;
        let key = v["KeyId"].as_str().unwrap_or(&self.key_id);
        Ok((key.to_string(), blob.to_string()))
    }

    pub async fn decrypt(&self, ciphertext: &str) -> anyhow::Result<Vec<u8>> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let v = self.call("Decrypt", json!({ "KeyId": self.key_id, "CiphertextBlob": ciphertext })).await?;
        let pt = v["Plaintext"].as_str().ok_or_else(|| anyhow::anyhow!("kms: no Plaintext"))?;
        Ok(b64.decode(pt)?)
    }

    async fn call(&self, action: &str, body: Value) -> anyhow::Result<Value> {
        let creds = self.creds().await?;
        let body = serde_json::to_vec(&body)?;
        let url = reqwest::Url::parse(&format!("{}/", self.endpoint))?;
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/x-amz-json-1.1".to_string());
        headers.insert("host".to_string(), host);
        headers.insert("x-amz-target".to_string(), format!("TrentService.{action}"));
        let signed = sign(&SignInput {
            method: "POST",
            path: "/",
            query: "",
            headers,
            body: &body,
            service: "kms",
            region: &self.region,
            creds: &creds,
            now: Utc::now(),
        });
        let mut req = self.http.post(url).timeout(Duration::from_secs(10)).body(body);
        for (k, v) in signed {
            if k != "host" {
                req = req.header(k, v);
            }
        }
        let res = req.send().await?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            // Force a credentials refresh on auth errors (expired session etc).
            if status.as_u16() == 400 || status.as_u16() == 403 {
                *self.cached.lock().await = None;
            }
            anyhow::bail!("kms {action} failed ({status}): {} {}", v["__type"], v["message"].as_str().or(v["Message"].as_str()).unwrap_or(""));
        }
        Ok(v)
    }

    async fn creds(&self) -> anyhow::Result<Creds> {
        let mut cached = self.cached.lock().await;
        if let Some(c) = cached.as_ref() {
            let fresh = c.expires.map(|e| e - chrono::Duration::minutes(5) > Utc::now()).unwrap_or(true);
            if fresh {
                return Ok(c.clone());
            }
        }
        let c = resolve_creds(&self.http, &self.region).await?;
        *cached = Some(c.clone());
        Ok(c)
    }
}

// ─── credential chain ────────────────────────────────────────────────────────

async fn resolve_creds(http: &reqwest::Client, region: &str) -> anyhow::Result<Creds> {
    if let (Some(access_key), Some(secret_key)) = (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY")) {
        return Ok(Creds { access_key, secret_key, token: env("AWS_SESSION_TOKEN"), expires: None });
    }
    if let (Some(file), Some(role)) = (env("AWS_WEB_IDENTITY_TOKEN_FILE"), env("AWS_ROLE_ARN")) {
        let token = std::fs::read_to_string(&file)?;
        let session = env("AWS_ROLE_SESSION_NAME").unwrap_or_else(|| "timika".into());
        let sts = env("AWS_STS_ENDPOINT").unwrap_or_else(|| format!("https://sts.{region}.amazonaws.com"));
        let xml = http
            .post(format!("{}/", sts.trim_end_matches('/')))
            .form(&[
                ("Action", "AssumeRoleWithWebIdentity"),
                ("Version", "2011-06-15"),
                ("RoleArn", role.as_str()),
                ("RoleSessionName", session.as_str()),
                ("WebIdentityToken", token.trim()),
            ])
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        return Ok(Creds {
            access_key: xml_tag(&xml, "AccessKeyId")?,
            secret_key: xml_tag(&xml, "SecretAccessKey")?,
            token: Some(xml_tag(&xml, "SessionToken")?),
            expires: xml_tag(&xml, "Expiration").ok().and_then(|e| e.parse().ok()),
        });
    }
    if let Some(rel) = env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") {
        return container_creds(http, &format!("http://169.254.170.2{rel}"), None).await;
    }
    if let Some(full) = env("AWS_CONTAINER_CREDENTIALS_FULL_URI") {
        let auth = env("AWS_CONTAINER_AUTHORIZATION_TOKEN");
        return container_creds(http, &full, auth).await;
    }
    imds_creds(http).await
}

async fn container_creds(http: &reqwest::Client, url: &str, auth: Option<String>) -> anyhow::Result<Creds> {
    let mut req = http.get(url).timeout(Duration::from_secs(5));
    if let Some(a) = auth {
        req = req.header("Authorization", a);
    }
    parse_json_creds(&req.send().await?.error_for_status()?.json().await?)
}

async fn imds_creds(http: &reqwest::Client) -> anyhow::Result<Creds> {
    let base = "http://169.254.169.254/latest";
    let token = http
        .put(format!("{base}/api/token"))
        .header("X-aws-ec2-metadata-token-ttl-seconds", "21600")
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("no AWS credentials found (env, web identity, ECS or EC2 instance profile)"))?
        .text()
        .await?;
    let role = http
        .get(format!("{base}/meta-data/iam/security-credentials/"))
        .header("X-aws-ec2-metadata-token", &token)
        .timeout(Duration::from_secs(2))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let role = role.lines().next().unwrap_or_default().trim().to_string();
    let v: Value = http
        .get(format!("{base}/meta-data/iam/security-credentials/{role}"))
        .header("X-aws-ec2-metadata-token", &token)
        .timeout(Duration::from_secs(2))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    parse_json_creds(&v)
}

fn parse_json_creds(v: &Value) -> anyhow::Result<Creds> {
    let s = |k: &str| v[k].as_str().map(String::from).ok_or_else(|| anyhow::anyhow!("credentials response missing {k}"));
    Ok(Creds {
        access_key: s("AccessKeyId")?,
        secret_key: s("SecretAccessKey")?,
        token: s("Token").ok(),
        expires: s("Expiration").ok().and_then(|e| e.parse().ok()),
    })
}

fn xml_tag(xml: &str, tag: &str) -> anyhow::Result<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open).ok_or_else(|| anyhow::anyhow!("STS response missing {tag}"))? + open.len();
    let end = xml[start..].find(&close).ok_or_else(|| anyhow::anyhow!("STS response malformed"))? + start;
    Ok(xml[start..end].to_string())
}

// ─── SigV4 ───────────────────────────────────────────────────────────────────

pub struct SignInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    /// Lower-case header names → values; must include `host`.
    pub headers: BTreeMap<String, String>,
    pub body: &'a [u8],
    pub service: &'a str,
    pub region: &'a str,
    pub creds: &'a Creds,
    pub now: DateTime<Utc>,
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// AWS Signature Version 4. Returns the full header set to send (including
/// `x-amz-date`, the session token if any, and `authorization`).
pub fn sign(i: &SignInput) -> BTreeMap<String, String> {
    let amz_date = i.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = i.now.format("%Y%m%d").to_string();
    let mut headers = i.headers.clone();
    headers.insert("x-amz-date".into(), amz_date.clone());
    if let Some(t) = &i.creds.token {
        headers.insert("x-amz-security-token".into(), t.clone());
    }
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
    let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        i.method,
        i.path,
        i.query,
        canonical_headers,
        signed_headers,
        hex::encode(Sha256::digest(i.body))
    );
    let scope = format!("{date}/{}/{}/aws4_request", i.region, i.service);
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex::encode(Sha256::digest(canonical.as_bytes())));
    let k_date = hmac(format!("AWS4{}", i.creds.secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, i.region.as_bytes());
    let k_service = hmac(&k_region, i.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, to_sign.as_bytes()));
    headers.insert(
        "authorization".into(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            i.creds.access_key
        ),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS SigV4 test suite, "get-vanilla".
    #[test]
    fn sigv4_get_vanilla() {
        let creds = Creds {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            token: None,
            expires: None,
        };
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "example.amazonaws.com".to_string());
        let out = sign(&SignInput {
            method: "GET",
            path: "/",
            query: "",
            headers,
            body: b"",
            service: "service",
            region: "us-east-1",
            creds: &creds,
            now: "2015-08-30T12:36:00Z".parse().unwrap(),
        });
        assert_eq!(
            out["authorization"],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn sts_xml() {
        let xml = "<R><Credentials><AccessKeyId>AK</AccessKeyId><SecretAccessKey>SK</SecretAccessKey></Credentials></R>";
        assert_eq!(xml_tag(xml, "AccessKeyId").unwrap(), "AK");
        assert!(xml_tag(xml, "SessionToken").is_err());
    }
}
