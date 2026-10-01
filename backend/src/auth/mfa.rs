//! Two-factor authentication: TOTP (RFC 6238 — 30 s steps, 6 digits,
//! HMAC-SHA1, the authenticator-app standard) plus one-time recovery codes.
//!
//! - The shared secret lives in the user record, which is barrier-encrypted.
//! - A code is accepted for the current step ±1 (clock drift), and never twice:
//!   the last step used is remembered (replay protection).
//! - Recovery codes are shown once; only their SHA-256 is stored, and each
//!   works once.

use chrono::{DateTime, Utc};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub const STEP_SECS: u64 = 30;
pub const RECOVERY_CODES: usize = 10;

/// An enabled second factor.
#[derive(Serialize, Deserialize, Clone)]
pub struct Mfa {
    /// Base32 TOTP secret.
    pub secret: String,
    pub enabled_at: DateTime<Utc>,
    /// The last TOTP step accepted (a code can't be used twice).
    #[serde(default)]
    pub last_step: u64,
    /// SHA-256 (hex) of the unused recovery codes.
    #[serde(default)]
    pub recovery: Vec<String>,
}

/// A secret being set up (not active until a code from it is confirmed).
#[derive(Serialize, Deserialize, Clone)]
pub struct Pending {
    pub secret: String,
    pub created: DateTime<Utc>,
}

pub fn new_secret() -> String {
    let mut b = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut b);
    BASE32_NOPAD.encode(&b)
}

fn code_at(secret: &[u8], step: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("hmac key");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = ((h[off] as u32 & 0x7f) << 24) | ((h[off + 1] as u32) << 16) | ((h[off + 2] as u32) << 8) | h[off + 3] as u32;
    bin % 1_000_000
}

/// The step a 6-digit `code` matches (now ±1), if any and not already used.
pub fn check(secret_b32: &str, code: &str, now: DateTime<Utc>, last_step: u64) -> Option<u64> {
    let code = code.trim().replace(' ', "");
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let want: u32 = code.parse().ok()?;
    let secret = BASE32_NOPAD.decode(secret_b32.as_bytes()).ok()?;
    let step = now.timestamp().max(0) as u64 / STEP_SECS;
    [step.saturating_sub(1), step, step + 1]
        .into_iter()
        .filter(|s| *s > last_step)
        .find(|s| code_at(&secret, *s) == want)
}

pub fn hash_recovery(code: &str) -> String {
    let normalized: String = code.trim().to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    hex::encode(Sha256::digest(normalized.as_bytes()))
}

/// New recovery codes: (shown to the user, stored hashes).
pub fn new_recovery_codes() -> (Vec<String>, Vec<String>) {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    let codes: Vec<String> = (0..RECOVERY_CODES)
        .map(|_| {
            let s: String = (0..10).map(|_| ALPHABET[(rng.next_u32() as usize) % ALPHABET.len()] as char).collect();
            format!("{}-{}", &s[..5], &s[5..])
        })
        .collect();
    let hashes = codes.iter().map(|c| hash_recovery(c)).collect();
    (codes, hashes)
}

/// Is `code` one of the recovery codes? Removes it if so.
pub fn use_recovery(m: &mut Mfa, code: &str) -> bool {
    let h = hash_recovery(code);
    match m.recovery.iter().position(|x| *x == h) {
        Some(i) => {
            m.recovery.remove(i);
            true
        }
        None => false,
    }
}

/// `otpauth://` URI for authenticator apps.
pub fn uri(issuer: &str, username: &str, secret: &str) -> String {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period={STEP_SECS}",
        enc(issuer),
        enc(username),
        enc(issuer)
    )
}

/// The URI as an SVG QR code.
pub fn qr_svg(data: &str) -> String {
    match qrcode::QrCode::new(data.as_bytes()) {
        Ok(code) => code
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(180, 180)
            .quiet_zone(true)
            .dark_color(qrcode::render::svg::Color("#111111"))
            .light_color(qrcode::render::svg::Color("#ffffff"))
            .build(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vectors() {
        // RFC 6238 appendix B (SHA-1 secret "12345678901234567890"), 6-digit truncation.
        let secret = b"12345678901234567890";
        for (t, want) in [(59u64, 287082), (1111111109, 81804), (1111111111, 50471), (1234567890, 5924), (2000000000, 279037)] {
            assert_eq!(code_at(secret, t / STEP_SECS), want, "t={t}");
        }
    }

    #[test]
    fn accepts_drift_and_refuses_replay() {
        let s = new_secret();
        let raw = BASE32_NOPAD.decode(s.as_bytes()).unwrap();
        let now = Utc::now();
        let step = now.timestamp() as u64 / STEP_SECS;
        let code = format!("{:06}", code_at(&raw, step));
        assert_eq!(check(&s, &code, now, 0), Some(step));
        assert_eq!(check(&s, &code, now, step), None, "replay");
        let prev = format!("{:06}", code_at(&raw, step - 1));
        assert_eq!(check(&s, &prev, now, 0), Some(step - 1), "previous step");
        let old = format!("{:06}", code_at(&raw, step - 3));
        assert_eq!(check(&s, &old, now, 0), None, "too old");
        assert_eq!(check(&s, "12345", now, 0), None);
        assert_eq!(check(&s, "abcdef", now, 0), None);
    }

    #[test]
    fn recovery_codes_work_once() {
        let (codes, hashes) = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        let mut m = Mfa { secret: new_secret(), enabled_at: Utc::now(), last_step: 0, recovery: hashes };
        assert!(use_recovery(&mut m, &codes[3].to_uppercase()), "case and dash insensitive");
        assert!(!use_recovery(&mut m, &codes[3]), "only once");
        assert_eq!(m.recovery.len(), RECOVERY_CODES - 1);
    }

    #[test]
    fn otpauth_uri() {
        let u = uri("Timika Prod", "jane@corp", "ABC");
        assert_eq!(u, "otpauth://totp/Timika%20Prod:jane%40corp?secret=ABC&issuer=Timika%20Prod&algorithm=SHA1&digits=6&period=30");
        assert!(qr_svg(&u).starts_with("<?xml") || qr_svg(&u).contains("<svg"));
    }
}
