//! How a source is reached: a CLI on a monitored server over SSH, or the
//! provider's API with credentials from the vault. Either way the answer is
//! JSON, and for one provider the same JSON.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use russh::client::Handle;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::Source;
use crate::bastion::archive::q;
use crate::bastion::{ssh, ssh::Pinned};
use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::monitor::{self as mon, SystemCfg};
use crate::seal::awskms::{sign, Creds, SignInput};

const MAX: usize = 24 * 1024 * 1024;

pub enum Via {
    Cli(Handle<Pinned>),
    Api { http: reqwest::Client, secret: BTreeMap<String, String>, token: tokio::sync::Mutex<Option<String>> },
}

fn unavailable(what: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Unavailable(format!("{what}: {e}").chars().take(400).collect())
}

impl Via {
    pub async fn open(core: &Core, src: &Source, secret: BTreeMap<String, String>) -> AppResult<Self> {
        if src.access == "server" {
            let cfg: SystemCfg = core.get_json(&mon::system_path(&src.asset)).await?.ok_or_else(|| AppError::BadRequest("the server this source reads through is not monitored (any more)".into()))?;
            return Ok(Via::Cli(mon::engine::connect(core, &cfg).await.map_err(AppError::Unavailable)?));
        }
        let mut b = reqwest::Client::builder().use_rustls_tls().timeout(Duration::from_secs(30)).connect_timeout(Duration::from_secs(10));
        // A cluster's own certificate authority.
        if let Some(ca) = secret.get("ca").filter(|c| !c.is_empty()) {
            for cert in reqwest::Certificate::from_pem_bundle(ca.as_bytes()).map_err(|e| AppError::BadRequest(format!("the CA certificate is not PEM: {e}")))? {
                b = b.add_root_certificate(cert);
            }
        }
        Ok(Via::Api { http: b.build().map_err(|e| unavailable("http client", e))?, secret, token: Default::default() })
    }

    pub fn is_cli(&self) -> bool {
        matches!(self, Via::Cli(_))
    }

    pub async fn close(self) {
        if let Via::Cli(h) = self {
            ssh::disconnect(&h).await;
        }
    }

    /// Run `program args…` on the server and read its JSON. Every argument is
    /// one quoted word; what the program says on failure becomes the error.
    pub async fn cli(&self, program: &str, args: &[&str]) -> AppResult<Value> {
        let Via::Cli(h) = self else { return Err(AppError::BadRequest("not a server source".into())) };
        let words: String = args.iter().map(|a| format!(" {}", q(a))).collect();
        let script = format!(
            "PATH=\"$PATH:/usr/local/bin:/snap/bin:$HOME/bin:$HOME/.local/bin:$HOME/google-cloud-sdk/bin\"\ncommand -v {program} >/dev/null 2>&1 || {{ echo '#@missing'; exit 127; }}\nE=$(mktemp) || exit 3\n{program}{words} 2>\"$E\"; RC=$?\nif [ $RC != 0 ]; then echo '#@err'; tail -c 700 \"$E\"; fi\nrm -f \"$E\"; exit $RC"
        );
        let (code, out) = ssh::exec_out(h, &format!("cd -- '.' && sh -c {}", q(&script)), b"", Duration::from_secs(60), MAX).await.map_err(AppError::Unavailable)?;
        if code == 127 && out.contains("#@missing") {
            return Err(AppError::Unavailable(format!("`{program}` is not installed on that server")));
        }
        if code != 0 {
            let why = out.split("#@err").nth(1).unwrap_or(&out).lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("no output").to_string();
            return Err(unavailable(program, why));
        }
        serde_json::from_str(out.trim()).map_err(|_| unavailable(program, "the answer was not JSON"))
    }

    fn api(&self) -> AppResult<(&reqwest::Client, &BTreeMap<String, String>)> {
        match self {
            Via::Api { http, secret, .. } => Ok((http, secret)),
            Via::Cli(_) => Err(AppError::BadRequest("not a vault source".into())),
        }
    }
    fn field(&self, k: &str) -> AppResult<&str> {
        self.api()?.1.get(k).map(String::as_str).ok_or_else(|| AppError::BadRequest(format!("the source has no `{k}`")))
    }

    async fn send(&self, what: &str, req: reqwest::RequestBuilder) -> AppResult<String> {
        let res = req.send().await.map_err(|e| unavailable(what, e.without_url()))?;
        let status = res.status();
        let body = res.text().await.map_err(|e| unavailable(what, e.without_url()))?;
        if !status.is_success() {
            // The provider's own words, without echoing a whole page.
            let msg = serde_json::from_str::<Value>(&body).ok().and_then(|v| ["message", "Message", "error_description"].iter().find_map(|k| v[k].as_str().or(v["error"][k].as_str()).or(v["Error"][k].as_str()).map(String::from))).or_else(|| xml_text(&body, "Message")).unwrap_or_else(|| body.chars().take(200).collect());
            return Err(unavailable(what, format!("{} — {msg}", status.as_u16())));
        }
        Ok(body)
    }

    /// GET a JSON document with a bearer token (Kubernetes, Google, Azure).
    pub async fn get_json(&self, what: &str, url: &str, token: &str) -> AppResult<Value> {
        let (http, _) = self.api()?;
        let body = self.send(what, http.get(url).bearer_auth(token).header("accept", "application/json")).await?;
        serde_json::from_str(&body).map_err(|_| unavailable(what, "the answer was not JSON"))
    }

    /// AWS: a Query API action, signed (SigV4); the XML answer as JSON.
    pub async fn aws(&self, endpoint: &str, service: &str, region: &str, action: &str, version: &str, params: &[(&str, &str)]) -> AppResult<Value> {
        let (http, _) = self.api()?;
        let creds = Creds { access_key: self.field("access_key_id")?.into(), secret_key: self.field("secret_access_key")?.into(), token: self.api()?.1.get("session_token").cloned(), expires: None };
        let base = if endpoint.is_empty() { format!("https://{service}.{region}.amazonaws.com") } else { endpoint.to_string() };
        let host = base.split("://").nth(1).unwrap_or(&base).split('/').next().unwrap_or_default().to_string();
        let mut out = Value::Null;
        let mut next = String::new();
        for _ in 0..5 {
            let mut body = format!("Action={action}&Version={version}");
            for (k, v) in params {
                body.push_str(&format!("&{k}={}", urlencode(v)));
            }
            if !next.is_empty() {
                body.push_str(&format!("&{}={}", if service == "ec2" { "NextToken" } else { "Marker" }, urlencode(&next)));
            }
            let mut headers = BTreeMap::new();
            headers.insert("host".to_string(), host.clone());
            headers.insert("content-type".to_string(), "application/x-www-form-urlencoded; charset=utf-8".to_string());
            let signed = sign(&SignInput { method: "POST", path: "/", query: "", headers, body: body.as_bytes(), service, region, creds: &creds, now: Utc::now() });
            let mut req = http.post(format!("{base}/")).body(body);
            for (k, v) in signed.iter().filter(|(k, _)| *k != "host") {
                req = req.header(k, v);
            }
            let page = xml_to_json(&self.send(&format!("{service} {action}"), req).await?);
            // <XResponse><XResult>…</XResult></XResponse> (elb) or <XResponse>…</XResponse> (ec2)
            let inner = page.as_object().and_then(|o| o.values().next()).cloned().unwrap_or(Value::Null);
            let inner = match inner.get(format!("{action}Result")) {
                Some(r) => r.clone(),
                None => inner,
            };
            next = [super::s(&inner, "NextToken"), super::s(&inner, "NextMarker")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            merge(&mut out, inner);
            if next.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// Tencent Cloud API 3.0 (TC3-HMAC-SHA256); answers the `Response` object.
    pub async fn tencent(&self, endpoint: &str, service: &str, version: &str, region: &str, action: &str, payload: Value) -> AppResult<Value> {
        let (http, _) = self.api()?;
        let (id, key) = (self.field("secret_id")?, self.field("secret_key")?);
        let host = if endpoint.is_empty() { format!("{service}.tencentcloudapi.com") } else { endpoint.split("://").nth(1).unwrap_or(endpoint).split('/').next().unwrap_or_default().to_string() };
        let url = if endpoint.is_empty() { format!("https://{host}/") } else { format!("{endpoint}/") };
        let body = payload.to_string();
        let now = Utc::now();
        let auth = tc3(id, key, service, &host, action, &body, now.timestamp(), &now.format("%Y-%m-%d").to_string());
        let req = http.post(url).header("authorization", auth).header("content-type", "application/json; charset=utf-8").header("host", &host).header("x-tc-action", action).header("x-tc-version", version).header("x-tc-timestamp", now.timestamp().to_string()).header("x-tc-region", region).body(body);
        let what = format!("{service} {action}");
        let v: Value = serde_json::from_str(&self.send(&what, req).await?).map_err(|_| unavailable(&what, "the answer was not JSON"))?;
        let r = v.get("Response").cloned().unwrap_or(v);
        if let Some(m) = r["Error"]["Message"].as_str() {
            return Err(unavailable(&what, format!("{} — {m}", super::s(&r["Error"], "Code"))));
        }
        Ok(r)
    }

    /// Google: an access token from the service account key (a signed JWT), kept for this reading.
    pub async fn google_token(&self, endpoint: &str) -> AppResult<String> {
        let Via::Api { http, token, .. } = self else { return Err(AppError::BadRequest("not a vault source".into())) };
        let mut cached = token.lock().await;
        if let Some(t) = cached.as_ref() {
            return Ok(t.clone());
        }
        let sa: Value = serde_json::from_str(self.field("service_account")?).map_err(|_| AppError::BadRequest("the service account key is not the JSON file Google gives".into()))?;
        let (email, key) = (super::s(&sa, "client_email"), super::s(&sa, "private_key"));
        let aud = if endpoint.is_empty() { super::s(&sa, "token_uri") } else { format!("{endpoint}/token") };
        let aud = if aud.is_empty() { "https://oauth2.googleapis.com/token".to_string() } else { aud };
        let now = Utc::now().timestamp();
        let jwt = jwt_rs256(&json!({ "iss": email, "scope": "https://www.googleapis.com/auth/compute.readonly", "aud": aud, "iat": now, "exp": now + 600 }), &key)?;
        let body = self.send("google sign-in", http.post(&aud).form(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", jwt.as_str())])).await?;
        let t = serde_json::from_str::<Value>(&body).ok().map(|v| super::s(&v, "access_token")).filter(|t| !t.is_empty()).ok_or_else(|| unavailable("google sign-in", "no access token in the answer"))?;
        *cached = Some(t.clone());
        Ok(t)
    }

    /// Azure: an access token for the app registration (client credentials), kept for this reading.
    pub async fn azure_token(&self, endpoint: &str) -> AppResult<String> {
        let Via::Api { http, token, .. } = self else { return Err(AppError::BadRequest("not a vault source".into())) };
        let mut cached = token.lock().await;
        if let Some(t) = cached.as_ref() {
            return Ok(t.clone());
        }
        let login = if endpoint.is_empty() { "https://login.microsoftonline.com".to_string() } else { endpoint.to_string() };
        let url = format!("{login}/{}/oauth2/v2.0/token", self.field("tenant_id")?);
        let form = [("grant_type", "client_credentials"), ("client_id", self.field("client_id")?), ("client_secret", self.field("client_secret")?), ("scope", "https://management.azure.com/.default")];
        let body = self.send("azure sign-in", http.post(url).form(&form)).await?;
        let t = serde_json::from_str::<Value>(&body).ok().map(|v| super::s(&v, "access_token")).filter(|t| !t.is_empty()).ok_or_else(|| unavailable("azure sign-in", "no access token in the answer"))?;
        *cached = Some(t.clone());
        Ok(t)
    }
}

/// Lists found under the same key are joined (pages of one answer).
fn merge(into: &mut Value, page: Value) {
    match (into, page) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match (a.get_mut(&k), v) {
                    (Some(Value::Array(x)), Value::Array(y)) => x.extend(y),
                    (_, v) => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, page) => *slot = page,
    }
}

pub fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn hmac256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// Tencent Cloud's signature v3 for a JSON POST.
pub fn tc3(secret_id: &str, secret_key: &str, service: &str, host: &str, action: &str, body: &str, timestamp: i64, date: &str) -> String {
    let canonical = format!("POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:{host}\nx-tc-action:{}\n\ncontent-type;host;x-tc-action\n{}", action.to_lowercase(), hex::encode(Sha256::digest(body.as_bytes())));
    let scope = format!("{date}/{service}/tc3_request");
    let to_sign = format!("TC3-HMAC-SHA256\n{timestamp}\n{scope}\n{}", hex::encode(Sha256::digest(canonical.as_bytes())));
    let k_date = hmac256(format!("TC3{secret_key}").as_bytes(), date.as_bytes());
    let k_service = hmac256(&k_date, service.as_bytes());
    let k_signing = hmac256(&k_service, b"tc3_request");
    format!("TC3-HMAC-SHA256 Credential={secret_id}/{scope}, SignedHeaders=content-type;host;x-tc-action, Signature={}", hex::encode(hmac256(&k_signing, to_sign.as_bytes())))
}

/// A JWT signed with a service account's RSA key (PKCS#8 PEM).
pub fn jwt_rs256(claims: &Value, pem: &str) -> AppResult<String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bad = || AppError::BadRequest("the service account's private key could not be read".into());
    let der = base64::engine::general_purpose::STANDARD.decode(pem.lines().filter(|l| !l.starts_with("-----")).map(str::trim).collect::<String>()).map_err(|_| bad())?;
    let key = ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|_| bad())?;
    let head = format!("{}.{}", b64.encode(br#"{"alg":"RS256","typ":"JWT"}"#), b64.encode(claims.to_string()));
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), head.as_bytes(), &mut sig).map_err(|_| bad())?;
    Ok(format!("{head}.{}", b64.encode(sig)))
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(unescape(&xml[start..end]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// EC2 names in its XML what the CLI's JSON names differently.
fn rename(k: &str) -> String {
    let mut c = k.chars();
    let k: String = match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    };
    match k.as_str() {
        "ReservationSet" => "Reservations".into(),
        "InstancesSet" => "Instances".into(),
        "InstanceState" => "State".into(),
        "IpAddress" => "PublicIpAddress".into(),
        "TagSet" => "Tags".into(),
        "NatGatewaySet" => "NatGateways".into(),
        "NatGatewayAddressSet" => "NatGatewayAddresses".into(),
        "RouteTableSet" => "RouteTables".into(),
        "RouteSet" => "Routes".into(),
        "AssociationSet" => "Associations".into(),
        "NetworkInterfaceSet" => "NetworkInterfaces".into(),
        "PrivateIpAddressesSet" => "PrivateIpAddresses".into(),
        _ => k,
    }
}

/// AWS's Query-API XML as the JSON its CLI prints: elements become keys,
/// `<item>` / `<member>` children become lists, names are the CLI's.
pub fn xml_to_json(xml: &str) -> Value {
    fn element(src: &str, mut i: usize) -> (Value, usize) {
        // `i` is just after an opening tag; read until its closing tag.
        let mut fields: Vec<(String, Value)> = Vec::new();
        let mut text = String::new();
        loop {
            let Some(lt) = src[i..].find('<').map(|x| x + i) else {
                i = src.len();
                break;
            };
            text.push_str(&src[i..lt]);
            let Some(gt) = src[lt..].find('>').map(|x| x + lt) else { return (Value::Null, src.len()) };
            let tag = &src[lt + 1..gt];
            if tag.starts_with('/') {
                i = gt + 1;
                break;
            }
            if tag.starts_with('?') || tag.starts_with('!') {
                i = gt + 1;
                continue;
            }
            let name = tag.split([' ', '/']).next().unwrap_or_default().rsplit(':').next().unwrap_or_default().to_string();
            if tag.ends_with('/') {
                fields.push((name, Value::String(String::new())));
                i = gt + 1;
                continue;
            }
            let (v, after) = element(src, gt + 1);
            fields.push((name, v));
            i = after;
        }
        if fields.is_empty() {
            return (Value::String(unescape(text.trim())), i);
        }
        if fields.iter().all(|f| f.0 == "item" || f.0 == "member") {
            return (Value::Array(fields.into_iter().map(|f| f.1).collect()), i);
        }
        let mut o = Map::new();
        for (k, v) in fields {
            o.insert(rename(&k), v);
        }
        (Value::Object(o), i)
    }
    element(xml, 0).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ec2_xml_reads_like_the_cli() {
        let v = xml_to_json(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/">
  <requestId>abc</requestId>
  <reservationSet>
    <item><reservationId>r-1</reservationId><instancesSet><item>
      <instanceId>i-0abc</instanceId><instanceState><code>16</code><name>running</name></instanceState>
      <privateIpAddress>10.0.1.5</privateIpAddress><ipAddress>54.1.2.3</ipAddress>
      <tagSet><item><key>Name</key><value>web &amp; api</value></item></tagSet>
      <placement><availabilityZone>us-east-1a</availabilityZone></placement><productCodes/>
    </item></instancesSet></item>
  </reservationSet>
</DescribeInstancesResponse>"#,
        );
        let i = &v["DescribeInstancesResponse"]["Reservations"][0]["Instances"][0];
        assert_eq!((i["InstanceId"].as_str(), i["State"]["Name"].as_str(), i["PrivateIpAddress"].as_str(), i["PublicIpAddress"].as_str()), (Some("i-0abc"), Some("running"), Some("10.0.1.5"), Some("54.1.2.3")));
        assert_eq!((i["Tags"][0]["Key"].as_str(), i["Tags"][0]["Value"].as_str(), i["Placement"]["AvailabilityZone"].as_str(), i["ProductCodes"].as_str()), (Some("Name"), Some("web & api"), Some("us-east-1a"), Some("")));
        let elb = xml_to_json("<DescribeLoadBalancersResponse><DescribeLoadBalancersResult><LoadBalancers><member><LoadBalancerName>web</LoadBalancerName><Scheme>internet-facing</Scheme></member></LoadBalancers><NextMarker>m1</NextMarker></DescribeLoadBalancersResult></DescribeLoadBalancersResponse>");
        assert_eq!(elb["DescribeLoadBalancersResponse"]["DescribeLoadBalancersResult"]["LoadBalancers"][0]["LoadBalancerName"], "web");
        let mut all = serde_json::json!({ "LoadBalancers": [1], "NextMarker": "m1" });
        merge(&mut all, serde_json::json!({ "LoadBalancers": [2] }));
        assert_eq!(all["LoadBalancers"], serde_json::json!([1, 2]), "pages are joined");
    }

    /// The example in Tencent Cloud's signature v3 documentation.
    #[test]
    fn tencent_signature_v3() {
        // The payload holds the escapes as written: backslash, u, four digits.
        let body = "{\"Limit\": 1, \"Filters\": [{\"Values\": [\"\\u672a\\u547d\\u540d\"], \"Name\": \"instance-name\"}]}";
        assert_eq!(hex::encode(Sha256::digest(body.as_bytes())), "35e9c5b0e3ae67532d3c9f17ead6c90222632e5b1ff7f6e89887f1398934f064");
        let auth = tc3("AKIDz8krbsJ5yKBZQpn74WFkmLPx3*******", "Gu5t9xGARNpq86cd98joQYCN3*******", "cvm", "cvm.tencentcloudapi.com", "DescribeInstances", body, 1551113065, "2019-02-25");
        assert_eq!(auth, "TC3-HMAC-SHA256 Credential=AKIDz8krbsJ5yKBZQpn74WFkmLPx3*******/2019-02-25/cvm/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature=be4f67d323c78ab9acb7395e43c0dbcf822a9cfac32fea2449a7bc7726b770a3");
    }

    #[test]
    fn encodes_and_refuses_bad_keys() {
        assert_eq!(urlencode("arn:aws:elb/a b"), "arn%3Aaws%3Aelb%2Fa%20b");
        assert!(jwt_rs256(&json!({}), "-----BEGIN PRIVATE KEY-----\nnot a key\n-----END PRIVATE KEY-----").is_err());
    }
}
