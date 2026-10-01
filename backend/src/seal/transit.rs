//! HashiCorp Vault / OpenBao Transit as the key service.
//!
//! SEAL_TRANSIT_ADDR       https://vault.example:8200
//! SEAL_TRANSIT_KEY        transit key name (e.g. `timika-unseal`)
//! SEAL_TRANSIT_MOUNT      mount path (default `transit`)
//! SEAL_TRANSIT_TOKEN      token with encrypt+decrypt on that key, or
//! SEAL_TRANSIT_TOKEN_FILE path re-read on every call (rotating tokens)
//! SEAL_TRANSIT_NAMESPACE  Vault Enterprise / OpenBao namespace (optional)
//! SEAL_TRANSIT_CA_FILE    CA for the Vault server (optional)

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};

use super::{env, require};

pub struct Transit {
    addr: String,
    mount: String,
    key: String,
    token: Option<String>,
    token_file: Option<String>,
    namespace: Option<String>,
    http: reqwest::Client,
}

impl Transit {
    pub fn from_env() -> anyhow::Result<Self> {
        let mut b = reqwest::Client::builder().use_rustls_tls().timeout(Duration::from_secs(10));
        if let Some(ca) = env("SEAL_TRANSIT_CA_FILE") {
            for cert in reqwest::Certificate::from_pem_bundle(&std::fs::read(ca)?)? {
                b = b.add_root_certificate(cert);
            }
        }
        let token = env("SEAL_TRANSIT_TOKEN");
        let token_file = env("SEAL_TRANSIT_TOKEN_FILE");
        if token.is_none() && token_file.is_none() {
            anyhow::bail!("SEAL_TYPE=transit needs SEAL_TRANSIT_TOKEN or SEAL_TRANSIT_TOKEN_FILE");
        }
        Ok(Self {
            addr: require("SEAL_TRANSIT_ADDR")?.trim_end_matches('/').to_string(),
            mount: env("SEAL_TRANSIT_MOUNT").unwrap_or_else(|| "transit".into()).trim_matches('/').to_string(),
            key: require("SEAL_TRANSIT_KEY")?,
            token,
            token_file,
            namespace: env("SEAL_TRANSIT_NAMESPACE"),
            http: b.build()?,
        })
    }

    pub fn describe(&self) -> String {
        format!("transit key `{}` at {}/v1/{}", self.key, self.addr, self.mount)
    }

    fn token(&self) -> anyhow::Result<String> {
        match &self.token_file {
            Some(f) => Ok(std::fs::read_to_string(f)?.trim().to_string()),
            None => Ok(self.token.clone().unwrap_or_default()),
        }
    }

    async fn call(&self, op: &str, body: Value) -> anyhow::Result<Value> {
        let mut req = self
            .http
            .post(format!("{}/v1/{}/{op}/{}", self.addr, self.mount, self.key))
            .header("X-Vault-Token", self.token()?)
            .json(&body);
        if let Some(ns) = &self.namespace {
            req = req.header("X-Vault-Namespace", ns);
        }
        let res = req.send().await?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            anyhow::bail!("transit {op} failed ({status}): {}", v["errors"]);
        }
        Ok(v)
    }

    pub async fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<(String, String)> {
        let v = self.call("encrypt", json!({ "plaintext": B64.encode(plaintext) })).await?;
        let ct = v["data"]["ciphertext"].as_str().ok_or_else(|| anyhow::anyhow!("transit: no ciphertext"))?;
        Ok((self.key.clone(), ct.to_string()))
    }

    pub async fn decrypt(&self, ciphertext: &str) -> anyhow::Result<Vec<u8>> {
        let v = self.call("decrypt", json!({ "ciphertext": ciphertext })).await?;
        let pt = v["data"]["plaintext"].as_str().ok_or_else(|| anyhow::anyhow!("transit: no plaintext"))?;
        Ok(B64.decode(pt)?)
    }
}
