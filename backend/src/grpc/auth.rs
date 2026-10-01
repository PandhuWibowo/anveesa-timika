use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use serde_json::Value;
use tonic::{Request, Response, Status};

use super::convert::str_opt;
use super::pb::auth_service_server::AuthService;
use super::{pb, reply, Ctx, Need, Reply};
use crate::auth::{captcha, userpass};
use crate::error::AppError;

fn last_login(v: &Value) -> Option<pb::LastLogin> {
    Some(pb::LastLogin { time: str_opt(&v["time"])?, ip: str_opt(&v["ip"]) })
}

/// `User::summary` JSON → message.
fn user_pb(v: &Value) -> pb::User {
    pb::User {
        username: str_opt(&v["username"]).unwrap_or_default(),
        policies: strings(&v["policies"]),
        created_at: str_opt(&v["created_at"]).unwrap_or_default(),
        password_changed_at: str_opt(&v["password_changed_at"]).unwrap_or_default(),
        password_expires_at: str_opt(&v["password_expires_at"]),
        must_change_password: v["must_change_password"] == true,
        disabled: v["disabled"] == true,
        locked_until: str_opt(&v["locked_until"]),
        failed_attempts: v["failed_attempts"].as_u64().unwrap_or(0) as u32,
        last_login: last_login(&v["last_login"]),
        last_failed_login: last_login(&v["last_failed_login"]),
        mfa_enabled: v["mfa_enabled"] == true,
        mfa_required: v["mfa_required"] == true,
    }
}

fn login_pb(b: &Value) -> pb::LoginResponse {
    pb::LoginResponse {
        token: str_opt(&b["token"]).unwrap_or_default(),
        username: str_opt(&b["username"]).unwrap_or_default(),
        policies: strings(&b["policies"]),
        expires_at: str_opt(&b["expires_at"]),
        max_expires_at: str_opt(&b["max_expires_at"]),
        idle_timeout_secs: b["idle_timeout_secs"].as_i64().unwrap_or(0),
        must_change_password: b["must_change_password"] == true,
        password_expired: b["password_expired"] == true,
        password_expires_at: str_opt(&b["password_expires_at"]),
        last_login: last_login(&b["last_login"]),
        last_failed_login: last_login(&b["last_failed_login"]),
        mfa_required: b["mfa_required"] == true,
        mfa_token: str_opt(&b["mfa_token"]),
        must_setup_mfa: b["must_setup_mfa"] == true,
        used_recovery_code: b["used_recovery_code"] == true,
        recovery_codes_left: b["recovery_codes_left"].as_u64().unwrap_or(0) as u32,
    }
}

/// A sign-in answer; the audit log records who signed in (when a session was issued).
fn login_reply(out: userpass::LoginOutcome) -> Response<pb::LoginResponse> {
    let msg = login_pb(&out.body);
    match &out.entry {
        Some(e) => reply(msg, e),
        None => Response::new(msg),
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
}

impl Ctx {
    async fn captcha_key(&self) -> Result<[u8; 32], Status> {
        Ok(self.st.core.derive_key(b"timika captcha pow v1").await.ok_or(AppError::Sealed)?)
    }
}

#[tonic::async_trait]
impl AuthService for Ctx {
    async fn get_login_config(&self, _: Request<()>) -> Reply<pb::LoginConfig> {
        let p = &self.st.auth.policy;
        let c = self.st.auth.captcha.public();
        Ok(Response::new(pb::LoginConfig {
            policy: Some(pb::Policy {
                profile: p.profile.clone(),
                min_length: p.min_length as u32,
                max_length: p.max_length as u32,
                require_classes: p.require_classes as u32,
                max_age_days: p.max_age_days,
                history: p.history as u32,
                lockout_threshold: p.lockout_threshold,
                lockout_minutes: p.lockout_minutes,
                session_idle_minutes: p.session_idle_minutes,
                session_max_hours: p.session_max_hours,
                breach_check: p.breach_check,
            }),
            banner: p.banner.clone(),
            banner_zh: p.banner_zh.clone(),
            banner_ack_required: p.banner_ack_required,
            privacy_notice_url: p.privacy_notice_url.clone(),
            mfa_required: p.mfa_required.clone(),
            captcha: Some(pb::CaptchaConfig {
                provider: str_opt(&c["provider"]).unwrap_or_default(),
                fallback: str_opt(&c["fallback"]),
                site_key: str_opt(&c["site_key"]),
                domain: str_opt(&c["domain"]),
            }),
        }))
    }

    async fn get_captcha_challenge(&self, _: Request<()>) -> Reply<pb::CaptchaChallenge> {
        let c = self.st.auth.captcha.pow_challenge(&self.captcha_key().await?);
        Ok(Response::new(pb::CaptchaChallenge {
            algorithm: str_opt(&c["algorithm"]).unwrap_or_default(),
            challenge: str_opt(&c["challenge"]).unwrap_or_default(),
            maxnumber: c["maxnumber"].as_u64().unwrap_or(0),
            salt: str_opt(&c["salt"]).unwrap_or_default(),
            signature: str_opt(&c["signature"]).unwrap_or_default(),
        }))
    }

    async fn login(&self, req: Request<pb::LoginRequest>) -> Reply<pb::LoginResponse> {
        self.public().await?;
        let auth = &self.st.auth;
        let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        let ip = auth.client_ip(&req.metadata().clone().into_headers(), peer);
        let r = req.into_inner();
        // The attempted username, so failed sign-ins show who was targeted.
        crate::audit::target(format!("user {}", r.username.trim().to_lowercase()));
        if let Err(wait) = auth.rate_limit(ip.as_deref().unwrap_or("unknown")).await {
            return Err(AppError::RateLimited(format!("too many sign-in attempts from your address — wait {wait}s")).into());
        }
        if auth.policy.banner_ack_required && !r.banner_ack {
            return Err(Status::invalid_argument("you must acknowledge the system-use notice to sign in"));
        }
        if auth.captcha.enabled() {
            let sub = r.captcha.map(|c| captcha::Submission { provider: c.provider, token: c.token }).unwrap_or_default();
            auth.captcha.verify(&self.captcha_key().await?, &sub, ip.as_deref()).await.map_err(Status::permission_denied)?;
        }
        let out = userpass::login(&self.st.core, &auth.policy, &r.username, &r.password, ip).await?;
        Ok(login_reply(out))
    }

    async fn verify_mfa(&self, req: Request<pb::VerifyMfaRequest>) -> Reply<pb::LoginResponse> {
        let auth = &self.st.auth;
        let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        let ip = auth.client_ip(&req.metadata().clone().into_headers(), peer);
        if let Err(wait) = auth.rate_limit(ip.as_deref().unwrap_or("unknown")).await {
            return Err(AppError::RateLimited(format!("too many sign-in attempts from your address — wait {wait}s")).into());
        }
        let r = req.into_inner();
        let out = userpass::verify_mfa(&self.st.core, &auth.policy, &r.mfa_token, &r.code, ip).await?;
        crate::audit::target(format!(
            "user {}{}",
            out.body["username"].as_str().unwrap_or("?"),
            if out.body["used_recovery_code"] == true { " (recovery code)" } else { "" }
        ));
        Ok(login_reply(out))
    }

    async fn get_mfa_status(&self, req: Request<()>) -> Reply<pb::MfaStatus> {
        let (me, _) = self.who(&req, Need::Session).await?;
        let name = me.username.clone().ok_or_else(|| Status::invalid_argument("the root token has no account — sign in as a user"))?;
        let v = userpass::mfa_status(&self.st.core, &self.st.auth.policy, &name).await?;
        Ok(reply(
            pb::MfaStatus {
                enabled: v["enabled"] == true,
                enabled_at: str_opt(&v["enabled_at"]),
                recovery_remaining: v["recovery_remaining"].as_u64().unwrap_or(0) as u32,
                required: v["required"] == true,
            },
            &me,
        ))
    }

    async fn begin_mfa_setup(&self, req: Request<()>) -> Reply<pb::MfaSetup> {
        let (me, _) = self.who(&req, Need::Session).await?;
        let name = me.username.clone().ok_or_else(|| Status::invalid_argument("the root token has no account — sign in as a user"))?;
        crate::audit::target(format!("user {name} (2FA setup started)"));
        let secret = userpass::mfa_begin(&self.st.core, &name).await?;
        let issuer = std::env::var("MFA_ISSUER").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "Timika".into());
        let uri = crate::auth::mfa::uri(&issuer, &name, &secret);
        Ok(reply(pb::MfaSetup { qr_svg: crate::auth::mfa::qr_svg(&uri), secret, uri }, &me))
    }

    async fn confirm_mfa_setup(&self, req: Request<pb::MfaCode>) -> Reply<pb::MfaConfirmed> {
        let (me, token_path) = self.who(&req, Need::Session).await?;
        let name = me.username.clone().ok_or_else(|| Status::invalid_argument("the root token has no account — sign in as a user"))?;
        crate::audit::target(format!("user {name} (2FA turned on)"));
        let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        let ip = self.st.auth.client_ip(&req.metadata().clone().into_headers(), peer);
        let codes = userpass::mfa_confirm(&self.st.core, &name, &req.get_ref().code).await?;
        // A 2FA-setup-only session becomes a full one.
        let session = if me.policies.iter().any(|p| p == "mfa-setup") {
            let out = userpass::upgrade_session(&self.st.core, &self.st.auth.policy, &name, &token_path, ip).await?;
            Some(login_pb(&out.body))
        } else {
            None
        };
        Ok(reply(pb::MfaConfirmed { recovery_codes: codes, session }, &me))
    }

    async fn disable_mfa(&self, req: Request<pb::MfaCode>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let name = me.username.clone().ok_or_else(|| Status::invalid_argument("the root token has no account"))?;
        crate::audit::target(format!("user {name} (2FA turned off)"));
        userpass::mfa_disable(&self.st.core, &self.st.auth.policy, &name, &req.get_ref().code).await?;
        Ok(reply((), &me))
    }

    async fn new_recovery_codes(&self, req: Request<pb::MfaCode>) -> Reply<pb::RecoveryCodes> {
        let (me, _) = self.who(&req, Need::Bastion).await?;
        let name = me.username.clone().ok_or_else(|| Status::invalid_argument("the root token has no account"))?;
        crate::audit::target(format!("user {name} (new recovery codes)"));
        let codes = userpass::mfa_new_recovery(&self.st.core, &name, &req.get_ref().code).await?;
        Ok(reply(pb::RecoveryCodes { codes }, &me))
    }

    async fn reset_user_mfa(&self, req: Request<pb::UserRef>) -> Reply<pb::User> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        crate::audit::target(format!("user {} (2FA reset)", req.get_ref().username));
        let v = userpass::mfa_reset(&self.st.core, &self.st.auth.policy, &req.get_ref().username).await?;
        Ok(reply(user_pb(&v), &me))
    }

    async fn lookup_self(&self, req: Request<()>) -> Reply<pb::TokenInfo> {
        let (me, _) = self.who(&req, Need::Session).await?;
        let v = serde_json::to_value(&me).map_err(AppError::from)?;
        let msg = pb::TokenInfo {
            display_name: me.display_name.clone(),
            policies: me.policies.clone(),
            created_at: str_opt(&v["created_at"]).unwrap_or_default(),
            username: me.username.clone(),
            expires_at: str_opt(&v["expires_at"]),
            max_expires_at: str_opt(&v["max_expires_at"]),
        };
        Ok(reply(msg, &me))
    }

    async fn renew_self(&self, req: Request<()>) -> Reply<pb::RenewResponse> {
        let (me, path) = self.who(&req, Need::Session).await?;
        let v = userpass::renew(&self.st.core, &path).await?;
        Ok(reply(
            pb::RenewResponse {
                expires_at: str_opt(&v["expires_at"]),
                max_expires_at: str_opt(&v["max_expires_at"]),
                renewable: v["renewable"] == true,
            },
            &me,
        ))
    }

    async fn revoke_self(&self, req: Request<()>) -> Reply<()> {
        let (me, path) = self.who(&req, Need::Session).await?;
        self.st.core.commit(vec![], vec![path], vec![]).await?;
        Ok(reply((), &me))
    }

    /// The session ends afterwards: sign in again with the new password.
    async fn change_password(&self, req: Request<pb::ChangePasswordRequest>) -> Reply<()> {
        let (me, path) = self.who(&req, Need::Session).await?;
        let username = me
            .username
            .clone()
            .ok_or_else(|| Status::invalid_argument("the root token has no password — sign in as a user"))?;
        let r = req.get_ref();
        userpass::change_password(&self.st.core, &self.st.auth.policy, &self.st.http, &username, &r.old_password, &r.new_password)
            .await?;
        self.st.core.commit(vec![], vec![path], vec![]).await?;
        Ok(reply((), &me))
    }

    async fn list_users(&self, req: Request<()>) -> Reply<pb::ListUsersResponse> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let users = userpass::list(&self.st.core, &self.st.auth.policy).await?;
        Ok(reply(
            pb::ListUsersResponse {
                users: users.iter().map(user_pb).collect(),
                roles: userpass::ROLES.iter().map(|s| s.to_string()).collect(),
            },
            &me,
        ))
    }

    async fn get_user(&self, req: Request<pb::UserRef>) -> Reply<pb::User> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        crate::audit::target(format!("user {}", req.get_ref().username));
        let v = userpass::get(&self.st.core, &self.st.auth.policy, &req.get_ref().username).await?;
        Ok(reply(user_pb(&v), &me))
    }

    async fn upsert_user(&self, req: Request<pb::UpsertUserRequest>) -> Reply<pb::User> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let r = req.into_inner();
        let mut what = vec![];
        if r.password.is_some() { what.push("password".to_string()); }
        if !r.policies.is_empty() { what.push(format!("role {}", r.policies.join(","))); }
        if let Some(d) = r.disabled { what.push(if d { "disabled".into() } else { "enabled".into() }); }
        crate::audit::target(format!("user {}{}", r.username.trim().to_lowercase(), if what.is_empty() { String::new() } else { format!(" ({})", what.join(", ")) }));
        let up = userpass::UpsertReq {
            password: r.password,
            policies: (!r.policies.is_empty()).then_some(r.policies),
            must_change_password: r.must_change_password,
            disabled: r.disabled,
        };
        let v = userpass::upsert(&self.st.core, &self.st.auth.policy, &self.st.http, &r.username, up).await?;
        Ok(reply(user_pb(&v), &me))
    }

    async fn delete_user(&self, req: Request<pb::UserRef>) -> Reply<()> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        let name = &req.get_ref().username;
        crate::audit::target(format!("user {name}"));
        if me.username.as_deref() == Some(name.trim().to_lowercase().as_str()) {
            return Err(Status::invalid_argument("you can't delete your own account"));
        }
        userpass::delete(&self.st.core, name).await?;
        Ok(reply((), &me))
    }

    async fn unlock_user(&self, req: Request<pb::UserRef>) -> Reply<pb::User> {
        let (me, _) = self.who(&req, Need::Admin).await?;
        crate::audit::target(format!("user {}", req.get_ref().username));
        let v = userpass::unlock(&self.st.core, &self.st.auth.policy, &req.get_ref().username).await?;
        Ok(reply(user_pb(&v), &me))
    }
}
