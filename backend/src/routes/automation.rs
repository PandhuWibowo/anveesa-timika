//! `POST /v1/automation/hooks/<repo>` — push webhooks from GitHub, Gitea
//! (`X-Hub-Signature-256`: HMAC-SHA256 of the body with the repository's
//! webhook secret) and GitLab (`X-Gitlab-Token`: the secret itself).
//!
//! A push to the tracked branch pulls the repository and, with "plan on push",
//! starts the safe action (plan · check · preview) for every project the push
//! touched. Answers 202 right away; the work continues in the background.
//!
//! `POST /v1/automation/trigger/<repo>` — CI starts a run (see `trigger`).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::automation::{self as auto, HookDelivery};
use crate::state::AppState;

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// Constant-time check of the delivery's signature / token.
pub fn verify(headers: &HeaderMap, body: &[u8], secret: &str) -> bool {
    if let Some(sig) = header(headers, "x-hub-signature-256").and_then(|s| s.strip_prefix("sha256=")) {
        let Ok(sig) = hex::decode(sig) else { return false };
        let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
        m.update(body);
        return m.verify_slice(&sig).is_ok();
    }
    if let Some(token) = header(headers, "x-gitlab-token") {
        let (a, b) = (token.as_bytes(), secret.as_bytes());
        return a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
    }
    false
}

fn reply(code: StatusCode, v: Value) -> Response {
    (code, Json(v)).into_response()
}

/// Files a push changed (empty = unknown, e.g. a forced push or a big push).
fn changed_files(p: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for c in p["commits"].as_array().into_iter().flatten() {
        for k in ["added", "modified", "removed"] {
            for f in c[k].as_array().into_iter().flatten() {
                if let Some(f) = f.as_str() {
                    out.push(f.to_string());
                }
            }
        }
    }
    out
}

pub async fn hook(State(st): State<AppState>, Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let core = st.core.clone();
    if core.is_sealed().await {
        return reply(StatusCode::SERVICE_UNAVAILABLE, json!({ "error": "vault is sealed" }));
    }
    let Ok(repo) = auto::get_repo(&core, &id).await else {
        return reply(StatusCode::NOT_FOUND, json!({ "error": "unknown repository" }));
    };
    crate::audit::target(format!("repository {} (webhook)", repo.name));
    let Ok(secrets) = auto::get_secrets(&core, &id).await else {
        return reply(StatusCode::SERVICE_UNAVAILABLE, json!({ "error": "storage unavailable" }));
    };
    if !verify(&headers, &body, &secrets.webhook_secret) {
        return reply(StatusCode::UNAUTHORIZED, json!({ "error": "bad signature — check the webhook secret" }));
    }
    let event = header(&headers, "x-github-event").or_else(|| header(&headers, "x-gitea-event")).or_else(|| header(&headers, "x-gitlab-event")).unwrap_or("");
    if event == "ping" {
        return reply(StatusCode::OK, json!({ "ok": true, "message": "timika is listening" }));
    }
    if event != "push" && event != "Push Hook" {
        return reply(StatusCode::ACCEPTED, json!({ "ignored": format!("event `{event}`") }));
    }
    let Ok(p) = serde_json::from_slice::<Value>(&body) else {
        return reply(StatusCode::BAD_REQUEST, json!({ "error": "the payload must be JSON (content type application/json)" }));
    };
    let want = format!("refs/heads/{}", repo.branch);
    if p["ref"].as_str() != Some(want.as_str()) {
        return reply(StatusCode::ACCEPTED, json!({ "ignored": format!("push to {}, not {}", p["ref"].as_str().unwrap_or("?"), repo.branch) }));
    }
    let sha = p["after"].as_str().or_else(|| p["checkout_sha"].as_str()).unwrap_or("").to_string();
    let by = p["pusher"]["name"].as_str().or_else(|| p["user_username"].as_str()).or_else(|| p["pusher"]["login"].as_str()).unwrap_or("someone").to_string();
    let changed = changed_files(&p);
    crate::audit::target(format!("repository {} (push {} by {by})", repo.name, &sha[..sha.len().min(7)]));

    tokio::spawn(async move {
        let result = match auto::sync_repo(&core, &id).await {
            Err(e) => format!("pull failed: {e}"),
            Ok(repo) if repo.auto_plan => {
                let mut started = Vec::new();
                for p in auto::touched(&repo.projects, &changed) {
                    let start = auto::Start { project: &p.id, action: auto::safe_action(&p.kind), user: &by, trigger: "push", plan_run: None, options: Default::default() };
                    match auto::start_run(&core, &repo, start).await {
                        Ok(_) => started.push(format!("{} {}", auto::safe_action(&p.kind), p.name)),
                        Err(e) => tracing::info!(repo = %repo.name, project = %p.name, "push: not started: {e}"),
                    }
                }
                if started.is_empty() { "pulled — no projects changed".into() } else { format!("pulled — started {}", started.join(", ")) }
            }
            Ok(_) => "pulled".into(),
        };
        let delivery = HookDelivery { time: Utc::now(), sha, by, result };
        let _ = auto::update_repo(&core, &id, |r| r.last_hook = Some(delivery.clone())).await;
    });
    reply(StatusCode::ACCEPTED, json!({ "ok": true }))
}

/// `POST /v1/automation/trigger/<repo>` — start a run from CI.
///
/// `Authorization: Bearer <the repository's CI token>`, JSON body:
/// `{"project": "envs/prod" | "terraform:envs/prod", "action": "plan",
///   "plan": "<run id>" | "latest" (apply), "options": {…}, "by": "github-actions",
///   "wait": true}`.
/// Without `wait`: 202 and the run. With it: the finished run — 200 when it
/// succeeded, 409 when it failed / was cancelled / waits for approval, so
/// `curl --fail` fails the CI step.
pub async fn trigger(State(st): State<AppState>, Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let core = st.core.clone();
    if core.is_sealed().await {
        return reply(StatusCode::SERVICE_UNAVAILABLE, json!({ "error": "vault is sealed" }));
    }
    let Ok(repo) = auto::get_repo(&core, &id).await else {
        return reply(StatusCode::NOT_FOUND, json!({ "error": "unknown repository" }));
    };
    crate::audit::target(format!("repository {} (CI trigger)", repo.name));
    let Ok(secrets) = auto::get_secrets(&core, &id).await else {
        return reply(StatusCode::SERVICE_UNAVAILABLE, json!({ "error": "storage unavailable" }));
    };
    let token = header(&headers, "authorization").and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").trim();
    let (a, b) = (token.as_bytes(), secrets.trigger_token.as_bytes());
    if b.is_empty() || a.len() != b.len() || a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) != 0 {
        return reply(StatusCode::UNAUTHORIZED, json!({ "error": "send the repository's CI token: Authorization: Bearer tmkci.…" }));
    }
    let Ok(p) = serde_json::from_slice::<Value>(&body) else {
        return reply(StatusCode::BAD_REQUEST, json!({ "error": "the body must be JSON" }));
    };
    let want = p["project"].as_str().unwrap_or("").trim();
    let matches: Vec<&auto::Project> = repo.projects.iter().filter(|x| x.id == want || x.name == want || (!x.dir.is_empty() && x.dir == want && x.kind != "ansible")).collect();
    let project = match matches.as_slice() {
        [one] => (*one).clone(),
        [] => return reply(StatusCode::NOT_FOUND, json!({ "error": format!("no project `{want}`"), "projects": repo.projects.iter().map(|x| &x.id).collect::<Vec<_>>() })),
        _ => return reply(StatusCode::BAD_REQUEST, json!({ "error": format!("`{want}` matches several projects — use the id"), "projects": matches.iter().map(|x| &x.id).collect::<Vec<_>>() })),
    };
    let action = p["action"].as_str().unwrap_or(auto::safe_action(&project.kind)).to_string();
    let options: auto::RunOptions = match serde_json::from_value(p["options"].clone()) {
        Ok(o) => o,
        Err(_) if p["options"].is_null() => Default::default(),
        Err(e) => return reply(StatusCode::BAD_REQUEST, json!({ "error": format!("options: {e}") })),
    };
    let by = p["by"].as_str().unwrap_or("ci").chars().filter(|c| !c.is_control()).take(60).collect::<String>();
    let plan_run = match p["plan"].as_str() {
        Some("latest") => {
            let runs = auto::list_runs(&core, Some(&repo.id)).await.unwrap_or_default();
            match runs.iter().find(|r| r.project == project.id && r.action == "plan") {
                Some(r) => Some(r.id.clone()),
                None => return reply(StatusCode::BAD_REQUEST, json!({ "error": "no plan to apply — run a plan first" })),
            }
        }
        Some(x) => Some(x.to_string()),
        None => None,
    };
    crate::audit::target(format!("repository {} (CI trigger: {action} {} by {by})", repo.name, project.name));
    let start = auto::Start { project: &project.id, action: &action, user: &by, trigger: "ci", plan_run, options };
    let run = match auto::start_run(&core, &repo, start).await {
        Ok(r) => r,
        Err(e) => {
            let code = match &e {
                crate::error::AppError::Conflict(_) => StatusCode::CONFLICT,
                crate::error::AppError::NotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::BAD_REQUEST,
            };
            return reply(code, json!({ "error": e.to_string() }));
        }
    };
    let out = |r: &auto::Run| json!({ "id": r.id, "status": r.shown_status(), "summary": r.summary, "changes": r.changes, "error": r.error, "url": auto::notify::link(&r.id) });
    if !p["wait"].as_bool().unwrap_or(false) || run.status == "approval" {
        let code = if run.status == "approval" && p["wait"].as_bool().unwrap_or(false) { StatusCode::CONFLICT } else { StatusCode::ACCEPTED };
        return reply(code, json!({ "run": out(&run) }));
    }
    let limit = std::time::Duration::from_secs(std::env::var("AUTOMATION_RUN_TIMEOUT_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(60u64) * 60 + 60);
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let Ok(r) = auto::get_run(&core, &run.id).await else { break };
        if r.finished() || r.shown_status() == "lost" || started.elapsed() > limit {
            let ok = r.status == "succeeded";
            return reply(if ok { StatusCode::OK } else { StatusCode::CONFLICT }, json!({ "run": out(&r) }));
        }
    }
    reply(StatusCode::ACCEPTED, json!({ "run": out(&run) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        let body = br#"{"ref":"refs/heads/main"}"#;
        let mut m = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
        m.update(body);
        let sig = format!("sha256={}", hex::encode(m.finalize().into_bytes()));
        let mut h = HeaderMap::new();
        h.insert("x-hub-signature-256", sig.parse().unwrap());
        assert!(verify(&h, body, "s3cret"));
        assert!(!verify(&h, body, "other"));
        assert!(!verify(&h, b"tampered", "s3cret"));
        let mut g = HeaderMap::new();
        g.insert("x-gitlab-token", "s3cret".parse().unwrap());
        assert!(verify(&g, body, "s3cret"));
        assert!(!verify(&g, body, "s3cre"));
        assert!(!verify(&HeaderMap::new(), body, "s3cret"), "unsigned");
    }

    #[test]
    fn changed() {
        let p = json!({ "commits": [{ "added": ["a/x.tf"], "modified": ["b.yml"], "removed": [] }, { "modified": ["c"] }] });
        assert_eq!(changed_files(&p), ["a/x.tf", "b.yml", "c"]);
    }
}
