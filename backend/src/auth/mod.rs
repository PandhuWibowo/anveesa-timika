//! Sign-in: username/password accounts, login policy profiles (US NIST /
//! China MLPS), captcha, rate limiting and sessions. See docs/LOGIN.md.

pub mod captcha;
pub mod mfa;
pub mod policy;
pub mod userpass;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use tokio::sync::Mutex;

pub struct AuthService {
    pub policy: policy::LoginPolicy,
    pub captcha: captcha::Captcha,
    /// Use X-Forwarded-For for the client IP (only behind a trusted proxy).
    pub trust_xff: bool,
    limiter: Mutex<HashMap<String, (Instant, u32)>>,
    limit: u32,
    window: Duration,
}

impl AuthService {
    pub fn from_env(http: reqwest::Client) -> anyhow::Result<Self> {
        let e = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let policy = policy::LoginPolicy::from_env()?;
        let captcha = captcha::Captcha::from_env(http)?;
        tracing::info!(
            "sign-in: policy {} · captcha {}{}",
            policy.profile,
            captcha.public()["provider"].as_str().unwrap_or("?"),
            captcha.public()["fallback"].as_str().map(|f| format!(" (fallback {f})")).unwrap_or_default()
        );
        Ok(Self {
            policy,
            captcha,
            trust_xff: e("TRUST_X_FORWARDED_FOR").as_deref() == Some("true"),
            limiter: Mutex::new(HashMap::new()),
            limit: e("LOGIN_RATE_LIMIT").and_then(|v| v.parse().ok()).unwrap_or(30),
            window: Duration::from_secs(e("LOGIN_RATE_WINDOW_SECS").and_then(|v| v.parse().ok()).unwrap_or(300)),
        })
    }

    /// Per-IP sliding-ish window for sign-in attempts (per instance). Account
    /// lockout is the cross-instance defence; this blunts spraying many users.
    pub async fn rate_limit(&self, ip: &str) -> Result<(), u64> {
        let mut m = self.limiter.lock().await;
        let now = Instant::now();
        m.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        let e = m.entry(ip.to_string()).or_insert((now, 0));
        e.1 += 1;
        if e.1 > self.limit {
            return Err((self.window - now.duration_since(e.0)).as_secs().max(1));
        }
        Ok(())
    }

    pub fn client_ip(&self, headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
        if self.trust_xff {
            if let Some(ip) = headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(ip);
            }
        }
        peer.map(|p| p.ip().to_string())
    }
}
