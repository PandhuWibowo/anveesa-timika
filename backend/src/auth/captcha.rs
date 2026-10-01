//! Captcha for the login form.
//!
//! CAPTCHA_PROVIDER   pow (default) · turnstile · recaptcha · hcaptcha · geetest · tencent · none
//! CAPTCHA_FALLBACK   pow · none — used by the page when the provider's script
//!                    can't load (e.g. a US provider inside mainland China)
//!
//! | provider  | reachable from mainland China | third party sees the user |
//! |-----------|-------------------------------|---------------------------|
//! | pow       | yes (self-hosted)             | no                        |
//! | turnstile | unreliable                    | Cloudflare                |
//! | recaptcha | only via www.recaptcha.net    | Google                    |
//! | hcaptcha  | unreliable                    | hCaptcha                  |
//! | geetest   | yes                           | GeeTest                   |
//! | tencent   | yes                           | Tencent Cloud             |
//!
//! `pow` is an ALTCHA-compatible proof-of-work: the browser spends ~0.5s of CPU
//! finding a number, no puzzle to solve (accessible), no data leaves your servers.

use std::collections::HashMap;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const POW_TTL_SECS: i64 = 180;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn need(k: &str) -> anyhow::Result<String> {
    env(k).ok_or_else(|| anyhow::anyhow!("{k} is required for this CAPTCHA_PROVIDER"))
}

fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    m.update(msg);
    hex::encode(m.finalize().into_bytes())
}

#[derive(Clone)]
enum Provider {
    None,
    Pow,
    Turnstile { site: String, secret: String },
    Recaptcha { site: String, secret: String, domain: String },
    Hcaptcha { site: String, secret: String },
    Geetest { id: String, key: String },
    Tencent { app_id: u64, app_secret: String, secret_id: String, secret_key: String },
}

impl Provider {
    fn name(&self) -> &'static str {
        match self {
            Provider::None => "none",
            Provider::Pow => "pow",
            Provider::Turnstile { .. } => "turnstile",
            Provider::Recaptcha { .. } => "recaptcha",
            Provider::Hcaptcha { .. } => "hcaptcha",
            Provider::Geetest { .. } => "geetest",
            Provider::Tencent { .. } => "tencent",
        }
    }

    fn parse(name: &str) -> anyhow::Result<Self> {
        Ok(match name {
            "none" => Provider::None,
            "pow" => Provider::Pow,
            "turnstile" => Provider::Turnstile { site: need("CAPTCHA_SITE_KEY")?, secret: need("CAPTCHA_SECRET")? },
            "recaptcha" => Provider::Recaptcha {
                site: need("CAPTCHA_SITE_KEY")?,
                secret: need("CAPTCHA_SECRET")?,
                // www.recaptcha.net serves the same service and is reachable from mainland China.
                domain: env("CAPTCHA_RECAPTCHA_DOMAIN").unwrap_or_else(|| "www.google.com".into()),
            },
            "hcaptcha" => Provider::Hcaptcha { site: need("CAPTCHA_SITE_KEY")?, secret: need("CAPTCHA_SECRET")? },
            "geetest" => Provider::Geetest { id: need("CAPTCHA_GEETEST_ID")?, key: need("CAPTCHA_GEETEST_KEY")? },
            "tencent" => Provider::Tencent {
                app_id: need("CAPTCHA_TENCENT_APP_ID")?.parse().map_err(|_| anyhow::anyhow!("CAPTCHA_TENCENT_APP_ID must be a number"))?,
                app_secret: need("CAPTCHA_TENCENT_APP_SECRET")?,
                secret_id: need("TENCENTCLOUD_SECRET_ID")?,
                secret_key: need("TENCENTCLOUD_SECRET_KEY")?,
            },
            other => anyhow::bail!("unknown CAPTCHA_PROVIDER `{other}`"),
        })
    }
}

/// What the browser submits with the login form.
#[derive(Deserialize, Debug, Default)]
pub struct Submission {
    pub provider: String,
    /// Provider token; for `pow` the base64 ALTCHA payload; for `geetest` a JSON
    /// object string; for `tencent` `{"ticket","randstr"}` JSON.
    pub token: String,
}

pub struct Captcha {
    primary: Provider,
    fallback: Option<Provider>,
    pow_max: u64,
    http: reqwest::Client,
    /// Spent pow challenges (per instance) until they expire — no replays.
    used: Mutex<HashMap<String, i64>>,
}

impl Captcha {
    pub fn from_env(http: reqwest::Client) -> anyhow::Result<Self> {
        let primary = Provider::parse(&env("CAPTCHA_PROVIDER").unwrap_or_else(|| "pow".into()))?;
        let fallback = match env("CAPTCHA_FALLBACK").as_deref() {
            Some("pow") if !matches!(primary, Provider::Pow | Provider::None) => Some(Provider::Pow),
            Some("pow") | Some("none") | None => None,
            Some(other) => anyhow::bail!("CAPTCHA_FALLBACK must be pow or none, got `{other}`"),
        };
        Ok(Self {
            primary,
            fallback,
            pow_max: env("CAPTCHA_POW_MAX_NUMBER").and_then(|v| v.parse().ok()).unwrap_or(100_000),
            http,
            used: Mutex::new(HashMap::new()),
        })
    }

    pub fn enabled(&self) -> bool {
        !matches!(self.primary, Provider::None)
    }

    /// Public settings for the login page (site keys only — never secrets).
    pub fn public(&self) -> Value {
        let mut v = json!({ "provider": self.primary.name(), "fallback": self.fallback.as_ref().map(|f| f.name()) });
        match &self.primary {
            Provider::Turnstile { site, .. } | Provider::Hcaptcha { site, .. } => v["site_key"] = json!(site),
            Provider::Recaptcha { site, domain, .. } => {
                v["site_key"] = json!(site);
                v["domain"] = json!(domain);
            }
            Provider::Geetest { id, .. } => v["site_key"] = json!(id),
            Provider::Tencent { app_id, .. } => v["site_key"] = json!(app_id.to_string()),
            Provider::None | Provider::Pow => {}
        }
        v
    }

    /// A fresh proof-of-work challenge (ALTCHA format), signed with `key`.
    pub fn pow_challenge(&self, key: &[u8; 32]) -> Value {
        let mut rnd = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut rnd);
        let salt = format!("{}?expires={}", hex::encode(rnd), Utc::now().timestamp() + POW_TTL_SECS);
        let number = rand::random::<u64>() % self.pow_max.max(1);
        let challenge = hex::encode(Sha256::digest(format!("{salt}{number}").as_bytes()));
        json!({
            "algorithm": "SHA-256",
            "challenge": challenge,
            "maxnumber": self.pow_max,
            "salt": salt,
            "signature": hmac_hex(key, challenge.as_bytes()),
        })
    }

    pub async fn verify(&self, key: &[u8; 32], sub: &Submission, ip: Option<&str>) -> Result<(), String> {
        let provider = if sub.provider == self.primary.name() {
            &self.primary
        } else {
            match &self.fallback {
                Some(f) if sub.provider == f.name() => f,
                _ => return Err("captcha required".into()),
            }
        };
        if sub.token.trim().is_empty() && !matches!(provider, Provider::None) {
            return Err("captcha required".into());
        }
        match provider {
            Provider::None => Ok(()),
            Provider::Pow => self.verify_pow(key, &sub.token).await,
            Provider::Turnstile { secret, .. } => {
                self.siteverify("https://challenges.cloudflare.com/turnstile/v0/siteverify", secret, &sub.token, ip, None).await
            }
            Provider::Recaptcha { secret, domain, .. } => {
                self.siteverify(&format!("https://{domain}/recaptcha/api/siteverify"), secret, &sub.token, ip, None).await
            }
            Provider::Hcaptcha { secret, site } => {
                self.siteverify("https://api.hcaptcha.com/siteverify", secret, &sub.token, ip, Some(site)).await
            }
            Provider::Geetest { id, key } => self.verify_geetest(id, key, &sub.token).await,
            Provider::Tencent { app_id, app_secret, secret_id, secret_key } => {
                self.verify_tencent(*app_id, app_secret, secret_id, secret_key, &sub.token, ip).await
            }
        }
    }

    async fn verify_pow(&self, key: &[u8; 32], token: &str) -> Result<(), String> {
        let bad = || "captcha failed".to_string();
        let raw = B64.decode(token.trim()).map_err(|_| bad())?;
        let p: Value = serde_json::from_slice(&raw).map_err(|_| bad())?;
        let (Some(challenge), Some(salt), Some(sig)) = (p["challenge"].as_str(), p["salt"].as_str(), p["signature"].as_str()) else {
            return Err(bad());
        };
        let number = p["number"].as_u64().ok_or_else(bad)?;
        if hmac_hex(key, challenge.as_bytes()) != sig {
            return Err(bad());
        }
        let expires: i64 = salt.split("expires=").nth(1).and_then(|v| v.split('&').next()).and_then(|v| v.parse().ok()).ok_or_else(bad)?;
        let now = Utc::now().timestamp();
        if expires < now {
            return Err("captcha expired — please try again".into());
        }
        if hex::encode(Sha256::digest(format!("{salt}{number}").as_bytes())) != challenge {
            return Err(bad());
        }
        let mut used = self.used.lock().await;
        used.retain(|_, exp| *exp >= now);
        if used.insert(challenge.to_string(), expires).is_some() {
            return Err("captcha already used".into());
        }
        Ok(())
    }

    async fn siteverify(&self, url: &str, secret: &str, token: &str, ip: Option<&str>, sitekey: Option<&str>) -> Result<(), String> {
        let mut form = vec![("secret", secret.to_string()), ("response", token.to_string())];
        if let Some(ip) = ip {
            form.push(("remoteip", ip.to_string()));
        }
        if let Some(s) = sitekey {
            form.push(("sitekey", s.to_string()));
        }
        let v: Value = self
            .http
            .post(url)
            .form(&form)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| format!("captcha service unreachable: {e}"))?
            .json()
            .await
            .map_err(|e| format!("captcha service: {e}"))?;
        if v["success"].as_bool() == Some(true) {
            Ok(())
        } else {
            tracing::debug!("captcha rejected: {v}");
            Err("captcha failed".into())
        }
    }

    /// GeeTest v4 secondary validation. Token = JSON {lot_number, captcha_output, pass_token, gen_time}.
    async fn verify_geetest(&self, id: &str, key: &str, token: &str) -> Result<(), String> {
        let t: Value = serde_json::from_str(token).map_err(|_| "captcha failed".to_string())?;
        let s = |k: &str| t[k].as_str().unwrap_or_default().to_string();
        let lot = s("lot_number");
        let form = [
            ("lot_number", lot.clone()),
            ("captcha_output", s("captcha_output")),
            ("pass_token", s("pass_token")),
            ("gen_time", s("gen_time")),
            ("sign_token", hmac_hex(key.as_bytes(), lot.as_bytes())),
        ];
        let v: Value = self
            .http
            .post(format!("https://gcaptcha4.geetest.com/validate?captcha_id={id}"))
            .form(&form)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| format!("captcha service unreachable: {e}"))?
            .json()
            .await
            .map_err(|e| format!("captcha service: {e}"))?;
        if v["result"] == "success" { Ok(()) } else { Err("captcha failed".into()) }
    }

    /// Tencent Cloud Captcha: DescribeCaptchaResult, TC3-HMAC-SHA256 signed.
    async fn verify_tencent(
        &self,
        app_id: u64,
        app_secret: &str,
        secret_id: &str,
        secret_key: &str,
        token: &str,
        ip: Option<&str>,
    ) -> Result<(), String> {
        let t: Value = serde_json::from_str(token).map_err(|_| "captcha failed".to_string())?;
        let body = json!({
            "CaptchaType": 9,
            "Ticket": t["ticket"],
            "Randstr": t["randstr"],
            "UserIp": ip.unwrap_or("127.0.0.1"),
            "CaptchaAppId": app_id,
            "AppSecretKey": app_secret,
        })
        .to_string();
        let ts = Utc::now().timestamp();
        let auth = tc3_authorization(secret_id, secret_key, "captcha.tencentcloudapi.com", "captcha", "DescribeCaptchaResult", &body, ts);
        let v: Value = self
            .http
            .post("https://captcha.tencentcloudapi.com/")
            .header("Authorization", auth)
            .header("Content-Type", "application/json; charset=utf-8")
            .header("X-TC-Action", "DescribeCaptchaResult")
            .header("X-TC-Version", "2019-07-22")
            .header("X-TC-Timestamp", ts.to_string())
            .body(body)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| format!("captcha service unreachable: {e}"))?
            .json()
            .await
            .map_err(|e| format!("captcha service: {e}"))?;
        if v["Response"]["CaptchaCode"].as_i64() == Some(1) {
            Ok(())
        } else {
            tracing::debug!("tencent captcha rejected: {v}");
            Err("captcha failed".into())
        }
    }
}

/// Tencent Cloud API v3 (TC3-HMAC-SHA256) Authorization header.
pub fn tc3_authorization(secret_id: &str, secret_key: &str, host: &str, service: &str, action: &str, body: &str, ts: i64) -> String {
    let date = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default().format("%Y-%m-%d").to_string();
    let signed_headers = "content-type;host;x-tc-action";
    let canonical = format!(
        "POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:{host}\nx-tc-action:{}\n\n{signed_headers}\n{}",
        action.to_lowercase(),
        hex::encode(Sha256::digest(body.as_bytes()))
    );
    let scope = format!("{date}/{service}/tc3_request");
    let to_sign = format!("TC3-HMAC-SHA256\n{ts}\n{scope}\n{}", hex::encode(Sha256::digest(canonical.as_bytes())));
    let mac = |k: &[u8], m: &[u8]| {
        let mut h = Hmac::<Sha256>::new_from_slice(k).expect("any key length");
        h.update(m);
        h.finalize().into_bytes().to_vec()
    };
    let k_date = mac(format!("TC3{secret_key}").as_bytes(), date.as_bytes());
    let k_service = mac(&k_date, service.as_bytes());
    let k_signing = mac(&k_service, b"tc3_request");
    let signature = hex::encode(mac(&k_signing, to_sign.as_bytes()));
    format!("TC3-HMAC-SHA256 Credential={secret_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pow_only() -> Captcha {
        Captcha { primary: Provider::Pow, fallback: None, pow_max: 5_000, http: reqwest::Client::new(), used: Mutex::new(HashMap::new()) }
    }

    fn solve(c: &Value) -> String {
        let salt = c["salt"].as_str().unwrap();
        let target = c["challenge"].as_str().unwrap();
        let n = (0..=c["maxnumber"].as_u64().unwrap())
            .find(|n| hex::encode(Sha256::digest(format!("{salt}{n}").as_bytes())) == target)
            .unwrap();
        B64.encode(json!({ "algorithm": "SHA-256", "challenge": target, "number": n, "salt": salt, "signature": c["signature"] }).to_string())
    }

    #[tokio::test]
    async fn pow_roundtrip_replay_and_forgery() {
        let cap = pow_only();
        let key = [9u8; 32];
        let ch = cap.pow_challenge(&key);
        let token = solve(&ch);
        let sub = Submission { provider: "pow".into(), token: token.clone() };
        assert!(cap.verify(&key, &sub, None).await.is_ok());
        assert_eq!(cap.verify(&key, &sub, None).await.unwrap_err(), "captcha already used");
        // Signed with another key (another vault / forged): rejected.
        let other = cap.pow_challenge(&[1u8; 32]);
        let sub = Submission { provider: "pow".into(), token: solve(&other) };
        assert!(cap.verify(&key, &sub, None).await.is_err());
        // Wrong provider name: rejected.
        let sub = Submission { provider: "turnstile".into(), token };
        assert!(cap.verify(&key, &sub, None).await.is_err());
    }

    #[test]
    fn tc3_header_shape() {
        let h = tc3_authorization("AKIDEXAMPLE", "secret", "captcha.tencentcloudapi.com", "captcha", "DescribeCaptchaResult", "{}", 1_700_000_000);
        assert!(h.starts_with("TC3-HMAC-SHA256 Credential=AKIDEXAMPLE/2023-11-14/captcha/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature="));
        assert_eq!(h.rsplit('=').next().unwrap().len(), 64);
    }
}
