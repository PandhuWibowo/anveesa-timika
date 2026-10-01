//! Login policy profiles.
//!
//! `LOGIN_POLICY=us-nist` — NIST SP 800-63B rev. 4 (password as the only factor)
//!   plus NIST SP 800-53 AC-7 (failed logins) and AC-8 (system-use notification).
//! `LOGIN_POLICY=cn-mlps` — GB/T 22239-2019 (等级保护 2.0), level 3:
//!   identity authentication (complexity, periodic change), login-failure
//!   handling (lock after repeated failures), session timeout, last-login display.
//!
//! Every value can be overridden individually (see `from_env`).

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct LoginPolicy {
    pub profile: String,
    pub min_length: usize,
    pub max_length: usize,
    /// How many of {lower, upper, digit, symbol} a password must contain (0 = no rule).
    pub require_classes: usize,
    /// Reject passwords on the built-in common-password list.
    pub blocklist: bool,
    /// Also check Have I Been Pwned (k-anonymity range API).
    pub breach_check: bool,
    /// Force a change after this many days (0 = never).
    pub max_age_days: u32,
    /// Refuse reusing the last N passwords (0 = off).
    pub history: usize,
    /// Consecutive failed logins that lock the account (0 = never).
    pub lockout_threshold: u32,
    pub lockout_minutes: u32,
    /// Session ends after this much inactivity…
    pub session_idle_minutes: u32,
    /// …and at the latest after this long, activity or not.
    pub session_max_hours: u32,
    /// Shown on the login page before sign-in.
    pub banner: Option<String>,
    /// Chinese version of the banner, shown when the page is in Chinese.
    pub banner_zh: Option<String>,
    /// User must tick "I acknowledge" to sign in (AC-8).
    pub banner_ack_required: bool,
    pub privacy_notice_url: Option<String>,
    /// Who must use two-factor authentication: off · admins · all.
    pub mfa_required: String,
}

const US_BANNER: &str = "You are accessing a restricted information system. Usage may be monitored, \
recorded, and subject to audit. Unauthorized use is prohibited and may result in disciplinary action \
and criminal and civil penalties. By using this system you consent to such monitoring and recording.";

impl LoginPolicy {
    pub fn us_nist() -> Self {
        Self {
            profile: "us-nist".into(),
            min_length: 15,
            max_length: 128,
            require_classes: 0,
            blocklist: true,
            breach_check: false,
            max_age_days: 0,
            history: 0,
            lockout_threshold: 10,
            lockout_minutes: 15,
            session_idle_minutes: 30,
            session_max_hours: 12,
            banner: Some(US_BANNER.into()),
            banner_zh: None,
            banner_ack_required: true,
            privacy_notice_url: None,
            mfa_required: "off".into(),
        }
    }

    pub fn cn_mlps() -> Self {
        Self {
            profile: "cn-mlps".into(),
            min_length: 8,
            max_length: 128,
            require_classes: 3,
            blocklist: true,
            breach_check: false,
            max_age_days: 90,
            history: 5,
            lockout_threshold: 5,
            lockout_minutes: 30,
            session_idle_minutes: 30,
            session_max_hours: 8,
            banner: None,
            banner_zh: None,
            banner_ack_required: false,
            privacy_notice_url: None,
            mfa_required: "off".into(),
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let e = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let mut p = match e("LOGIN_POLICY").as_deref().unwrap_or("us-nist") {
            "us-nist" | "us" => Self::us_nist(),
            "cn-mlps" | "cn" => Self::cn_mlps(),
            other => anyhow::bail!("LOGIN_POLICY must be us-nist or cn-mlps, got `{other}`"),
        };
        macro_rules! num {
            ($field:ident, $key:literal) => {
                if let Some(v) = e($key) {
                    p.$field = v.parse().map_err(|_| anyhow::anyhow!(concat!($key, " must be a number")))?;
                }
            };
        }
        num!(min_length, "PASSWORD_MIN_LENGTH");
        num!(max_length, "PASSWORD_MAX_LENGTH");
        num!(require_classes, "PASSWORD_REQUIRE_CLASSES");
        num!(max_age_days, "PASSWORD_MAX_AGE_DAYS");
        num!(history, "PASSWORD_HISTORY");
        num!(lockout_threshold, "LOCKOUT_THRESHOLD");
        num!(lockout_minutes, "LOCKOUT_MINUTES");
        num!(session_idle_minutes, "SESSION_IDLE_MINUTES");
        num!(session_max_hours, "SESSION_MAX_HOURS");
        if let Some(v) = e("PASSWORD_BREACH_CHECK") {
            p.breach_check = v == "hibp";
        }
        if let Some(v) = e("LOGIN_BANNER") {
            p.banner = Some(v);
        }
        p.banner_zh = e("LOGIN_BANNER_ZH");
        if let Some(v) = e("LOGIN_BANNER_REQUIRE_ACK") {
            p.banner_ack_required = v == "true";
        }
        p.privacy_notice_url = e("PRIVACY_NOTICE_URL");
        if let Some(v) = e("MFA_REQUIRED") {
            if !matches!(v.as_str(), "off" | "admins" | "all") {
                anyhow::bail!("MFA_REQUIRED must be off, admins or all, got `{v}`");
            }
            p.mfa_required = v;
        }
        p.require_classes = p.require_classes.min(4);
        p.session_idle_minutes = p.session_idle_minutes.max(1);
        p.session_max_hours = p.session_max_hours.max(1);
        Ok(p)
    }

    /// Must someone with these roles use two-factor authentication?
    pub fn mfa_required_for(&self, policies: &[String]) -> bool {
        match self.mfa_required.as_str() {
            "all" => true,
            "admins" => policies.iter().any(|p| p == "admin" || p == "root"),
            _ => false,
        }
    }

    /// Everything wrong with `password` for `username` (empty = acceptable).
    /// History and breach checks run separately (they need I/O).
    pub fn violations(&self, username: &str, password: &str) -> Vec<String> {
        let mut v = Vec::new();
        let len = password.chars().count();
        if len < self.min_length {
            v.push(format!("at least {} characters", self.min_length));
        }
        if len > self.max_length {
            v.push(format!("at most {} characters", self.max_length));
        }
        if self.require_classes > 0 {
            let classes = [
                password.chars().any(|c| c.is_lowercase()),
                password.chars().any(|c| c.is_uppercase()),
                password.chars().any(|c| c.is_ascii_digit()),
                password.chars().any(|c| !c.is_alphanumeric()),
            ]
            .iter()
            .filter(|b| **b)
            .count();
            if classes < self.require_classes {
                v.push(format!(
                    "{} of: lowercase, uppercase, digit, symbol",
                    self.require_classes
                ));
            }
        }
        let lower = password.to_lowercase();
        if !username.is_empty() && lower.contains(&username.to_lowercase()) {
            v.push("must not contain the username".into());
        }
        if self.blocklist && is_common(&lower) {
            v.push("too common — it appears on lists of frequently used passwords".into());
        }
        v
    }
}

/// Frequently used and breached passwords (lower-cased), plus trivial patterns.
fn is_common(lower: &str) -> bool {
    const COMMON: &[&str] = &[
        "password", "password1", "password12", "password123", "password1234", "passw0rd", "p@ssw0rd",
        "p@ssword", "123456", "1234567", "12345678", "123456789", "1234567890", "12345678910",
        "qwerty", "qwerty123", "qwertyuiop", "1q2w3e4r", "1q2w3e4r5t", "1qaz2wsx", "zaq12wsx",
        "abc123", "abcd1234", "111111", "000000", "123123", "654321", "666666", "888888", "987654321",
        "iloveyou", "admin", "admin123", "admin@123", "administrator", "root", "toor", "letmein",
        "welcome", "welcome1", "welcome123", "monkey", "dragon", "football", "baseball", "sunshine",
        "princess", "master", "shadow", "superman", "trustno1", "changeme", "changeit", "secret",
        "default", "guest", "test", "test123", "testing", "login", "access", "hello", "hello123",
        "starwars", "whatever", "michael", "jennifer", "charlie", "freedom", "computer", "internet",
        "asdfghjkl", "asdf1234", "zxcvbnm", "zxcvbnm123", "aa123456", "a123456", "a12345678",
        "woaini", "woaini1314", "woaini520", "5201314", "1314520", "wodemima", "mima123",
        "qq123456", "qq123456789", "abc123456", "aa123123", "123qwe", "qwe123", "1qazxsw2",
        "timika", "timika123", "vault", "vault123", "secrets", "correcthorsebatterystaple",
    ];
    if COMMON.contains(&lower) {
        return true;
    }
    // A single repeated character, or a common word with only digits appended.
    let chars: Vec<char> = lower.chars().collect();
    if !chars.is_empty() && chars.iter().all(|c| *c == chars[0]) {
        return true;
    }
    let stem = lower.trim_end_matches(|c: char| c.is_ascii_digit() || c == '!' || c == '@' || c == '#');
    stem.len() >= 4 && stem.len() < lower.len() && COMMON.contains(&stem)
}

/// Have I Been Pwned range API (k-anonymity: only 5 hex chars of the SHA-1
/// leave the server). Returns how often the password appears in breaches.
pub async fn breach_count(http: &reqwest::Client, password: &str) -> anyhow::Result<u64> {
    use sha1::{Digest, Sha1};
    let hash = hex::encode_upper(Sha1::digest(password.as_bytes()));
    let (prefix, suffix) = hash.split_at(5);
    let body = http
        .get(format!("https://api.pwnedpasswords.com/range/{prefix}"))
        .header("Add-Padding", "true")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(body
        .lines()
        .find_map(|l| l.split_once(':').filter(|(s, _)| s.eq_ignore_ascii_case(suffix)))
        .and_then(|(_, n)| n.trim().parse().ok())
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_nist_is_length_based_without_composition_rules() {
        let p = LoginPolicy::us_nist();
        assert!(p.violations("alice", "correct horse battery").is_empty());
        assert!(!p.violations("alice", "Sh0rt!pass").is_empty()); // < 15
        assert!(!p.violations("alice", "password12345678").is_empty()); // common stem
        assert!(!p.violations("alice", "alice-is-the-best-admin").is_empty()); // contains username
    }

    #[test]
    fn cn_mlps_requires_three_classes() {
        let p = LoginPolicy::cn_mlps();
        assert!(!p.violations("wang", "abcdefgh").is_empty()); // 1 class
        assert!(!p.violations("wang", "abcdefg1").is_empty()); // 2 classes
        assert!(p.violations("wang", "Abcdefg1").is_empty()); // 3 classes, 8 chars
        assert!(!p.violations("wang", "woaini1314").is_empty()); // common
    }
}
