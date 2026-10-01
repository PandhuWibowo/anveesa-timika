//! Username/password accounts, stored in the vault itself (barrier-encrypted)
//! at `auth/userpass/users/<name>`, passwords hashed with Argon2id.
//!
//! All read-modify-writes (failed-attempt counters, lockout, password change)
//! use guarded commits, so they're correct with N instances behind a balancer.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::policy::{breach_count, LoginPolicy};
use super::mfa;
use sha2::Digest;
use crate::core::{Core, MAX_RETRIES};
use crate::error::{AppError, AppResult};
use crate::token::{self, TokenEntry};

const PREFIX: &str = "auth/userpass/users/";
/// `admin` — everything · `read-only` — read secrets and status ·
/// `ssh` — only the servers they are granted (no access to vault data).
pub const ROLES: &[&str] = &["admin", "read-only", "ssh"];
/// A fixed, valid Argon2id hash: verifying against it when the user doesn't
/// exist keeps "no such user" as slow as "wrong password".
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$NxuVYbW5x0lYVWzJrc3Lx8x9IGbB9f1wLl9Z0bb0G2Y";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LastLogin {
    pub time: DateTime<Utc>,
    pub ip: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct User {
    pub username: String,
    pub password_hash: String,
    pub policies: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub password_changed_at: DateTime<Utc>,
    #[serde(default)]
    pub must_change_password: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub failed_attempts: u32,
    #[serde(default)]
    pub locked_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_login: Option<LastLogin>,
    #[serde(default)]
    pub last_failed_login: Option<LastLogin>,
    /// Previous password hashes (newest first), for the history rule.
    #[serde(default)]
    pub history: Vec<String>,
    /// Two-factor authentication, once enabled.
    #[serde(default)]
    pub mfa: Option<mfa::Mfa>,
    /// A TOTP secret being set up (until a code from it is confirmed).
    #[serde(default)]
    pub mfa_pending: Option<mfa::Pending>,
}

impl User {
    /// Safe to return over the API (no hashes).
    pub fn summary(&self, policy: &LoginPolicy) -> Value {
        json!({
            "username": self.username,
            "policies": self.policies,
            "created_at": self.created_at,
            "password_changed_at": self.password_changed_at,
            "password_expires_at": password_expires_at(self, policy),
            "must_change_password": self.must_change_password,
            "disabled": self.disabled,
            "locked_until": self.locked_until.filter(|t| *t > Utc::now()),
            "failed_attempts": self.failed_attempts,
            "last_login": self.last_login,
            "last_failed_login": self.last_failed_login,
            "mfa_enabled": self.mfa.is_some(),
            "mfa_required": policy.mfa_required_for(&self.policies),
        })
    }
}

fn path(name: &str) -> String {
    format!("{PREFIX}{name}")
}

pub fn clean_username(raw: &str) -> AppResult<String> {
    let u = raw.trim().to_lowercase();
    let ok = !u.is_empty()
        && u.len() <= 64
        && u.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'));
    if ok { Ok(u) } else { Err(AppError::BadRequest("username: 1–64 of a-z 0-9 . _ - @".into())) }
}

fn password_expires_at(u: &User, p: &LoginPolicy) -> Option<DateTime<Utc>> {
    (p.max_age_days > 0).then(|| u.password_changed_at + Duration::days(p.max_age_days as i64))
}

async fn hash(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|h| h.to_string())
            .map_err(|e| AppError::Internal(format!("argon2: {e}")))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

async fn verify(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
    })
    .await
    .unwrap_or(false)
}

/// Policy + breach check for a new password.
async fn check_new_password(p: &LoginPolicy, http: &reqwest::Client, user: &str, password: &str) -> AppResult<()> {
    let v = p.violations(user, password);
    if !v.is_empty() {
        return Err(AppError::BadRequest(format!("password must be: {}", v.join("; "))));
    }
    if p.breach_check {
        match breach_count(http, password).await {
            Ok(n) if n > 0 => {
                return Err(AppError::BadRequest(format!(
                    "this password appeared in {n} known data breaches — choose another"
                )))
            }
            Ok(_) => {}
            // Don't block account management because a third-party API is down.
            Err(e) => tracing::warn!("breached-password check unavailable: {e}"),
        }
    }
    Ok(())
}

pub async fn is_active(core: &Core, username: &str) -> AppResult<bool> {
    Ok(core.get_json::<User>(&path(username)).await?.is_some_and(|u| !u.disabled))
}

// ─── login ───────────────────────────────────────────────────────────────────

pub struct LoginOutcome {
    /// None while the second factor is still owed (no session yet).
    pub entry: Option<TokenEntry>,
    pub body: Value,
}

/// Password OK, waiting for the second factor: `auth/mfa-pending/<sha256(token)>`.
#[derive(Serialize, Deserialize)]
struct MfaChallenge {
    username: String,
    created: DateTime<Utc>,
    #[serde(default)]
    attempts: u32,
}

const MFA_CHALLENGE_MINUTES: i64 = 5;
const MFA_CHALLENGE_ATTEMPTS: u32 = 5;

fn challenge_path(token: &str) -> String {
    format!("auth/mfa-pending/{}", hex::encode(sha2::Sha256::digest(token.as_bytes())))
}

pub async fn login(core: &Core, p: &LoginPolicy, username: &str, password: &str, ip: Option<String>) -> AppResult<LoginOutcome> {
    let generic = || AppError::Auth("invalid username or password".into());
    let Ok(username) = clean_username(username) else {
        verify(password.to_string(), DUMMY_HASH.into()).await;
        return Err(generic());
    };

    for _ in 0..MAX_RETRIES {
        let now = Utc::now();
        let (user, guard) = core.get_json_guarded::<User>(&path(&username)).await?;
        let Some(mut user) = user else {
            verify(password.to_string(), DUMMY_HASH.into()).await;
            return Err(generic());
        };
        if user.disabled {
            verify(password.to_string(), DUMMY_HASH.into()).await;
            return Err(generic());
        }
        if let Some(until) = user.locked_until.filter(|t| *t > now) {
            let mins = (until - now).num_minutes() + 1;
            return Err(AppError::Forbidden(format!(
                "account locked after repeated failed sign-ins — try again in {mins} min or ask an administrator to unlock it"
            )));
        }

        if !verify(password.to_string(), user.password_hash.clone()).await {
            user.failed_attempts += 1;
            user.last_failed_login = Some(LastLogin { time: now, ip: ip.clone() });
            let mut locked = false;
            if p.lockout_threshold > 0 && user.failed_attempts >= p.lockout_threshold {
                user.locked_until = Some(now + Duration::minutes(p.lockout_minutes as i64));
                user.failed_attempts = 0;
                locked = true;
            }
            match core.commit(vec![(path(&username), serde_json::to_vec(&user)?)], vec![], vec![guard]).await {
                Err(AppError::WriteConflict) => continue,
                Err(e) => return Err(e),
                Ok(()) => {}
            }
            if locked {
                tracing::warn!("account `{username}` locked after {} failed sign-ins", p.lockout_threshold);
                return Err(AppError::Forbidden(format!(
                    "too many failed sign-ins — account locked for {} min",
                    p.lockout_minutes
                )));
            }
            return Err(generic());
        }

        // Password OK. With 2FA on, the session waits for the second factor.
        if user.mfa.is_some() {
            let mfa_token = token::random_token();
            let ch = MfaChallenge { username: username.clone(), created: now, attempts: 0 };
            core.commit(vec![(challenge_path(&mfa_token), serde_json::to_vec(&ch)?)], vec![], vec![]).await?;
            return Ok(LoginOutcome {
                entry: None,
                body: json!({ "mfa_required": true, "mfa_token": mfa_token, "username": username }),
            });
        }
        match open_session(core, p, &username, user, guard, ip.clone(), vec![], vec![], json!({})).await? {
            Some(o) => return Ok(o),
            None => continue,
        }
    }
    Err(AppError::WriteConflict)
}

/// Sign-in complete: record it and issue the session (password-change-only or
/// 2FA-setup-only when that's owed). `extra_*` join the same atomic commit.
/// None = lost a write race (caller retries).
#[allow(clippy::too_many_arguments)]
async fn open_session(
    core: &Core,
    p: &LoginPolicy,
    username: &str,
    mut user: User,
    guard: crate::storage::Guard,
    ip: Option<String>,
    extra_deletes: Vec<String>,
    extra_guards: Vec<crate::storage::Guard>,
    extra_body: Value,
) -> AppResult<Option<LoginOutcome>> {
    let now = Utc::now();
    let previous = user.last_login.clone();
    let previous_failed = user.last_failed_login.clone();
    user.failed_attempts = 0;
    user.locked_until = None;
    user.last_login = Some(LastLogin { time: now, ip: ip.clone() });
    let expired = password_expires_at(&user, p).is_some_and(|t| t <= now);
    let must_change = user.must_change_password || expired;
    let must_setup_mfa = !must_change && user.mfa.is_none() && p.mfa_required_for(&user.policies);
    let (policies, max) = if must_change {
        (vec!["change-password".to_string()], Duration::minutes(15))
    } else if must_setup_mfa {
        (vec!["mfa-setup".to_string()], Duration::minutes(30))
    } else {
        (user.policies.clone(), Duration::hours(p.session_max_hours as i64))
    };
    let (token, entry) = token::new_session(username, policies, Duration::minutes(p.session_idle_minutes as i64), max);
    let puts = vec![(path(username), serde_json::to_vec(&user)?), (token::storage_path(&token), serde_json::to_vec(&entry)?)];
    let mut guards = vec![guard];
    guards.extend(extra_guards);
    match core.commit(puts, extra_deletes, guards).await {
        Err(AppError::WriteConflict) => return Ok(None),
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    let mut body = json!({
        "token": token,
        "username": username,
        "policies": entry.policies,
        "expires_at": entry.expires_at,
        "max_expires_at": entry.max_expires_at,
        "idle_timeout_secs": entry.idle_secs,
        "must_change_password": must_change,
        "must_setup_mfa": must_setup_mfa,
        "password_expired": expired,
        "password_expires_at": password_expires_at(&user, p),
        // Shown after sign-in so users notice sessions that weren't theirs.
        "last_login": previous,
        "last_failed_login": previous_failed,
    });
    if let (Value::Object(b), Value::Object(x)) = (&mut body, extra_body) {
        b.extend(x);
    }
    Ok(Some(LoginOutcome { entry: Some(entry), body }))
}

/// Check a TOTP or recovery code against an enabled second factor (marks the
/// TOTP step used / consumes the recovery code). Returns whether it matched
/// and if it was a recovery code.
fn check_second_factor(m: &mut mfa::Mfa, code: &str) -> Option<bool> {
    if let Some(step) = mfa::check(&m.secret, code, Utc::now(), m.last_step) {
        m.last_step = step;
        return Some(false);
    }
    mfa::use_recovery(m, code).then_some(true)
}

/// The second step of signing in: the code from the authenticator app (or a
/// recovery code) for the `mfa_token` the password step returned.
pub async fn verify_mfa(core: &Core, p: &LoginPolicy, mfa_token: &str, code: &str, ip: Option<String>) -> AppResult<LoginOutcome> {
    let expired = || AppError::Auth("this sign-in has expired — enter your password again".into());
    let cpath = challenge_path(mfa_token.trim());
    for _ in 0..MAX_RETRIES {
        let now = Utc::now();
        let (ch, cguard) = core.get_json_guarded::<MfaChallenge>(&cpath).await?;
        let Some(mut ch) = ch else { return Err(expired()) };
        if ch.created + Duration::minutes(MFA_CHALLENGE_MINUTES) < now {
            let _ = core.commit(vec![], vec![cpath.clone()], vec![]).await;
            return Err(expired());
        }
        let (user, guard) = core.get_json_guarded::<User>(&path(&ch.username)).await?;
        let Some(mut user) = user.filter(|u| !u.disabled) else { return Err(expired()) };
        if let Some(until) = user.locked_until.filter(|t| *t > now) {
            let mins = (until - now).num_minutes() + 1;
            return Err(AppError::Forbidden(format!("account locked after repeated failed sign-ins — try again in {mins} min")));
        }
        let Some(m) = user.mfa.as_mut() else { return Err(expired()) };
        match check_second_factor(m, code) {
            Some(was_recovery) => {
                let left = m.recovery.len();
                let extra = json!({ "used_recovery_code": was_recovery, "recovery_codes_left": left });
                match open_session(core, p, &ch.username.clone(), user, guard, ip.clone(), vec![cpath.clone()], vec![cguard], extra).await? {
                    Some(o) => return Ok(o),
                    None => continue,
                }
            }
            None => {
                // A wrong code counts like a wrong password (lockout), and the
                // challenge allows only a few tries.
                ch.attempts += 1;
                user.failed_attempts += 1;
                user.last_failed_login = Some(LastLogin { time: now, ip: ip.clone() });
                let mut locked = false;
                if p.lockout_threshold > 0 && user.failed_attempts >= p.lockout_threshold {
                    user.locked_until = Some(now + Duration::minutes(p.lockout_minutes as i64));
                    user.failed_attempts = 0;
                    locked = true;
                }
                let (puts, deletes) = if ch.attempts >= MFA_CHALLENGE_ATTEMPTS || locked {
                    (vec![(path(&ch.username), serde_json::to_vec(&user)?)], vec![cpath.clone()])
                } else {
                    (vec![(path(&ch.username), serde_json::to_vec(&user)?), (cpath.clone(), serde_json::to_vec(&ch)?)], vec![])
                };
                match core.commit(puts, deletes, vec![guard, cguard]).await {
                    Err(AppError::WriteConflict) => continue,
                    Err(e) => return Err(e),
                    Ok(()) => {}
                }
                if locked {
                    return Err(AppError::Forbidden(format!("too many failed sign-ins — account locked for {} min", p.lockout_minutes)));
                }
                if ch.attempts >= MFA_CHALLENGE_ATTEMPTS {
                    return Err(AppError::Auth("too many wrong codes — enter your password again".into()));
                }
                return Err(AppError::Auth("that code isn't right, or it was just used — wait for the next code in your authenticator app".into()));
            }
        }
    }
    Err(AppError::WriteConflict)
}

// ─── 2FA self-service ───────────────────────────────────────────────────────

/// Read-modify-write a user with retries.
async fn update_user<T>(core: &Core, username: &str, mut f: impl FnMut(&mut User) -> AppResult<T>) -> AppResult<T> {
    for _ in 0..MAX_RETRIES {
        let (user, guard) = core.get_json_guarded::<User>(&path(username)).await?;
        let mut user = user.ok_or_else(|| AppError::NotFound(format!("user `{username}`")))?;
        let out = f(&mut user)?;
        match core.commit(vec![(path(username), serde_json::to_vec(&user)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(out),
        }
    }
    Err(AppError::WriteConflict)
}

pub async fn mfa_status(core: &Core, p: &LoginPolicy, username: &str) -> AppResult<Value> {
    let u: User = core.get_json(&path(username)).await?.ok_or_else(|| AppError::NotFound(format!("user `{username}`")))?;
    Ok(json!({
        "enabled": u.mfa.is_some(),
        "enabled_at": u.mfa.as_ref().map(|m| m.enabled_at),
        "recovery_remaining": u.mfa.as_ref().map(|m| m.recovery.len()).unwrap_or(0),
        "required": p.mfa_required_for(&u.policies),
    }))
}

/// Start setting up 2FA: a new secret (inactive until confirmed).
pub async fn mfa_begin(core: &Core, username: &str) -> AppResult<String> {
    update_user(core, username, |u| {
        if u.mfa.is_some() {
            return Err(AppError::Conflict("two-factor authentication is already on — turn it off first to use a new device".into()));
        }
        let secret = mfa::new_secret();
        u.mfa_pending = Some(mfa::Pending { secret: secret.clone(), created: Utc::now() });
        Ok(secret)
    })
    .await
}

/// Finish setup with a code from the new secret. Returns the recovery codes (shown once).
pub async fn mfa_confirm(core: &Core, username: &str, code: &str) -> AppResult<Vec<String>> {
    update_user(core, username, |u| {
        let pending = u.mfa_pending.clone().filter(|pd| pd.created + Duration::minutes(30) > Utc::now()).ok_or_else(|| {
            AppError::BadRequest("setup has expired — start again".into())
        })?;
        let step = mfa::check(&pending.secret, code, Utc::now(), 0)
            .ok_or_else(|| AppError::BadRequest("that code isn't right — check the time on your phone and try the newest code".into()))?;
        let (codes, hashes) = mfa::new_recovery_codes();
        u.mfa = Some(mfa::Mfa { secret: pending.secret, enabled_at: Utc::now(), last_step: step, recovery: hashes });
        u.mfa_pending = None;
        Ok(codes)
    })
    .await
}

/// Turn 2FA off (needs a current code or a recovery code).
pub async fn mfa_disable(core: &Core, p: &LoginPolicy, username: &str, code: &str) -> AppResult<()> {
    update_user(core, username, |u| {
        if p.mfa_required_for(&u.policies) {
            return Err(AppError::Forbidden("two-factor authentication is required for your account and can't be turned off".into()));
        }
        let m = u.mfa.as_mut().ok_or_else(|| AppError::BadRequest("two-factor authentication isn't on".into()))?;
        check_second_factor(m, code).ok_or_else(|| AppError::BadRequest("that code isn't right".into()))?;
        u.mfa = None;
        Ok(())
    })
    .await
}

/// New recovery codes (the old ones stop working). Needs a current code.
pub async fn mfa_new_recovery(core: &Core, username: &str, code: &str) -> AppResult<Vec<String>> {
    update_user(core, username, |u| {
        let m = u.mfa.as_mut().ok_or_else(|| AppError::BadRequest("two-factor authentication isn't on".into()))?;
        check_second_factor(m, code).ok_or_else(|| AppError::BadRequest("that code isn't right".into()))?;
        let (codes, hashes) = mfa::new_recovery_codes();
        m.recovery = hashes;
        Ok(codes)
    })
    .await
}

/// Administrators: remove someone's 2FA (lost phone). They set it up again at next sign-in if required.
pub async fn mfa_reset(core: &Core, p: &LoginPolicy, username: &str) -> AppResult<Value> {
    let name = clean_username(username)?;
    update_user(core, &name, |u| {
        u.mfa = None;
        u.mfa_pending = None;
        Ok(u.summary(p))
    })
    .await
}

/// After setup in a 2FA-setup-only session: swap it for a full session.
pub async fn upgrade_session(core: &Core, p: &LoginPolicy, username: &str, old_token_path: &str, ip: Option<String>) -> AppResult<LoginOutcome> {
    for _ in 0..MAX_RETRIES {
        let (user, guard) = core.get_json_guarded::<User>(&path(username)).await?;
        let user = user.ok_or_else(|| AppError::NotFound(format!("user `{username}`")))?;
        if let Some(o) = open_session(core, p, username, user, guard, ip.clone(), vec![old_token_path.to_string()], vec![], json!({})).await? {
            return Ok(o);
        }
    }
    Err(AppError::WriteConflict)
}

/// Extend the caller's session by the idle timeout, capped at its maximum.
pub async fn renew(core: &Core, token_path: &str) -> AppResult<Value> {
    for _ in 0..MAX_RETRIES {
        let (entry, guard) = core.get_json_guarded::<TokenEntry>(token_path).await?;
        let mut entry = entry.ok_or_else(|| AppError::Auth("permission denied".into()))?;
        let (Some(idle), Some(max)) = (entry.idle_secs, entry.max_expires_at) else {
            return Ok(json!({ "expires_at": null, "renewable": false }));
        };
        entry.expires_at = Some((Utc::now() + Duration::seconds(idle)).min(max));
        match core.commit(vec![(token_path.to_string(), serde_json::to_vec(&entry)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(json!({ "expires_at": entry.expires_at, "max_expires_at": max, "renewable": true })),
        }
    }
    Err(AppError::WriteConflict)
}

/// Change your own password (also clears an expired/initial-password state).
pub async fn change_password(
    core: &Core,
    p: &LoginPolicy,
    http: &reqwest::Client,
    username: &str,
    old: &str,
    new: &str,
) -> AppResult<()> {
    check_new_password(p, http, username, new).await?;
    for _ in 0..MAX_RETRIES {
        let (user, guard) = core.get_json_guarded::<User>(&path(username)).await?;
        let mut user = user.ok_or_else(|| AppError::NotFound("user".into()))?;
        if !verify(old.to_string(), user.password_hash.clone()).await {
            return Err(AppError::BadRequest("current password is wrong".into()));
        }
        if verify(new.to_string(), user.password_hash.clone()).await {
            return Err(AppError::BadRequest("new password must differ from the current one".into()));
        }
        for h in user.history.iter().take(p.history) {
            if verify(new.to_string(), h.clone()).await {
                return Err(AppError::BadRequest(format!("must not reuse any of your last {} passwords", p.history)));
            }
        }
        let new_hash = hash(new.to_string()).await?;
        let old_hash = std::mem::replace(&mut user.password_hash, new_hash);
        user.history.insert(0, old_hash);
        user.history.truncate(p.history);
        user.password_changed_at = Utc::now();
        user.must_change_password = false;
        match core.commit(vec![(path(username), serde_json::to_vec(&user)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(()),
        }
    }
    Err(AppError::WriteConflict)
}

// ─── administration ─────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct UpsertReq {
    pub password: Option<String>,
    pub policies: Option<Vec<String>>,
    pub must_change_password: Option<bool>,
    pub disabled: Option<bool>,
}

pub async fn upsert(core: &Core, p: &LoginPolicy, http: &reqwest::Client, name: &str, req: UpsertReq) -> AppResult<Value> {
    let name = clean_username(name)?;
    if let Some(pol) = &req.policies {
        if pol.is_empty() || pol.iter().any(|r| !ROLES.contains(&r.as_str())) {
            return Err(AppError::BadRequest(format!("policies must be one of {ROLES:?}")));
        }
    }
    let new_hash = match &req.password {
        Some(pw) => {
            check_new_password(p, http, &name, pw).await?;
            Some(hash(pw.clone()).await?)
        }
        None => None,
    };
    for _ in 0..MAX_RETRIES {
        let now = Utc::now();
        let (existing, guard) = core.get_json_guarded::<User>(&path(&name)).await?;
        let user = match existing {
            None => User {
                username: name.clone(),
                password_hash: new_hash.clone().ok_or_else(|| AppError::BadRequest("password is required for a new user".into()))?,
                policies: req.policies.clone().unwrap_or_else(|| vec!["read-only".into()]),
                created_at: now,
                password_changed_at: now,
                // An administrator-chosen password must be replaced at first sign-in.
                must_change_password: req.must_change_password.unwrap_or(true),
                disabled: req.disabled.unwrap_or(false),
                failed_attempts: 0,
                locked_until: None,
                last_login: None,
                last_failed_login: None,
                history: vec![],
                mfa: None,
                mfa_pending: None,
            },
            Some(mut u) => {
                if let Some(h) = &new_hash {
                    let old = std::mem::replace(&mut u.password_hash, h.clone());
                    u.history.insert(0, old);
                    u.history.truncate(p.history);
                    u.password_changed_at = now;
                    u.must_change_password = req.must_change_password.unwrap_or(true);
                } else if let Some(m) = req.must_change_password {
                    u.must_change_password = m;
                }
                if let Some(pol) = &req.policies {
                    u.policies = pol.clone();
                }
                if let Some(d) = req.disabled {
                    u.disabled = d;
                }
                u
            }
        };
        match core.commit(vec![(path(&name), serde_json::to_vec(&user)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(user.summary(p)),
        }
    }
    Err(AppError::WriteConflict)
}

pub async fn unlock(core: &Core, p: &LoginPolicy, name: &str) -> AppResult<Value> {
    let name = clean_username(name)?;
    for _ in 0..MAX_RETRIES {
        let (user, guard) = core.get_json_guarded::<User>(&path(&name)).await?;
        let mut user = user.ok_or_else(|| AppError::NotFound(format!("user `{name}`")))?;
        user.failed_attempts = 0;
        user.locked_until = None;
        match core.commit(vec![(path(&name), serde_json::to_vec(&user)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            Err(e) => return Err(e),
            Ok(()) => return Ok(user.summary(p)),
        }
    }
    Err(AppError::WriteConflict)
}

pub async fn delete(core: &Core, name: &str) -> AppResult<()> {
    let name = clean_username(name)?;
    let (user, guard) = core.get_json_guarded::<User>(&path(&name)).await?;
    user.ok_or_else(|| AppError::NotFound(format!("user `{name}`")))?;
    core.commit(vec![], vec![path(&name)], vec![guard]).await
}

pub async fn list(core: &Core, p: &LoginPolicy) -> AppResult<Vec<Value>> {
    let mut out = Vec::new();
    for key in core.list(PREFIX).await? {
        if let Some(u) = core.get_json::<User>(&key).await? {
            out.push(u.summary(p));
        }
    }
    Ok(out)
}

pub async fn get(core: &Core, p: &LoginPolicy, name: &str) -> AppResult<Value> {
    let name = clean_username(name)?;
    core.get_json::<User>(&path(&name))
        .await?
        .map(|u| u.summary(p))
        .ok_or_else(|| AppError::NotFound(format!("user `{name}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dummy_hash_is_valid_argon2() {
        // If this ever stopped parsing, missing users would answer faster than
        // wrong passwords (a username-enumeration timing leak).
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
        assert!(!verify("x".into(), DUMMY_HASH.into()).await);
        let h = hash("correct horse battery staple".into()).await.unwrap();
        assert!(verify("correct horse battery staple".into(), h).await);
    }
}
