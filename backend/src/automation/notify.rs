//! Run notifications: Slack, Microsoft Teams, Discord, Telegram or any JSON
//! webhook, per repository, for chosen events:
//!
//! - `failed`    a run failed (or was lost);
//! - `drift`     a scheduled plan / check / preview found changes;
//! - `approval`  an Apply / Run / Up waits for another admin;
//! - `succeeded` a run succeeded.
//!
//! The target (webhook URL, bot token) is a secret in the repository's
//! secrets; only a redacted label is shown. Links point at `PUBLIC_URL`.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

use super::{Notifier, NotifyTarget, Run};
use crate::core::Core;

pub const KINDS: &[&str] = &["slack", "teams", "discord", "telegram", "webhook"];
pub const EVENTS: &[&str] = &["failed", "drift", "approval", "succeeded"];

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default())
}

/// `https://timika.example.com/#/automation/run/<id>` when PUBLIC_URL is set.
pub fn link(run: &str) -> Option<String> {
    std::env::var("PUBLIC_URL").ok().map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty()).map(|u| format!("{u}/#/automation/run/{run}"))
}

/// Check a target and make the label shown in the UI.
pub fn check(kind: &str, t: &NotifyTarget) -> Result<String, String> {
    let url = t.url.trim();
    match kind {
        "telegram" => {
            let ok = url.split_once(':').is_some_and(|(id, rest)| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && rest.len() >= 20);
            if !ok {
                return Err("Telegram: the bot token from @BotFather (123456:ABC…)".into());
            }
            if t.chat_id.trim().is_empty() {
                return Err("Telegram: the chat id (e.g. -1001234567890)".into());
            }
            Ok(format!("bot {}… → chat {}", &url[..url.find(':').unwrap_or(0)], t.chat_id.trim()))
        }
        _ if !KINDS.contains(&kind) => Err(format!("unknown notification type `{kind}`")),
        _ => {
            let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).ok_or("the webhook URL starts with https://")?;
            if url.len() > 2000 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err("invalid URL".into());
            }
            let host = rest.split('/').next().unwrap_or("");
            let tail: String = url.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            Ok(format!("{host}/…{tail}"))
        }
    }
}

fn action_label(a: &str) -> String {
    let mut c = a.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// One line describing the event.
pub fn text(run: &Run, event: &str) -> String {
    let what = format!("{} {} {}", run.repo_name, action_label(&run.action), run.project_name);
    let opts = run.options.describe();
    let opts = if opts.is_empty() { String::new() } else { format!(" ({opts})") };
    let by = match run.trigger.as_str() {
        "schedule" => " · scheduled".to_string(),
        "push" => format!(" · push by {}", run.user),
        "ci" => format!(" · CI ({})", run.user),
        _ => format!(" · by {}", run.user),
    };
    let short = &run.sha[..run.sha.len().min(7)];
    let mut s = match event {
        "failed" => format!("✗ {what}{opts} failed{}{by} · {short}", run.error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()),
        "drift" => format!("⚠ {what} found changes: {}{by} · {short}", run.summary.clone().unwrap_or_default()),
        "approval" => format!("⏳ {what}{opts} waits for approval{by} · {short}"),
        _ => format!("✓ {what}{opts} succeeded{}{by} · {short}", run.summary.as_ref().map(|x| format!(": {x}")).unwrap_or_default()),
    };
    if let Some(l) = link(&run.id) {
        s.push_str(&format!("\n{l}"));
    }
    s
}

fn payload(kind: &str, t: &NotifyTarget, event: &str, text: &str, run: Option<&Run>) -> (String, Value) {
    match kind {
        "discord" => (t.url.clone(), json!({ "content": text })),
        "telegram" => (
            format!("https://api.telegram.org/bot{}/sendMessage", t.url.trim()),
            json!({ "chat_id": t.chat_id.trim(), "text": text, "disable_web_page_preview": true }),
        ),
        "webhook" => (
            t.url.clone(),
            json!({
                "event": event,
                "text": text,
                "run": run.map(|r| json!({
                    "id": r.id, "repo": r.repo_name, "project": r.project_name, "kind": r.kind, "action": r.action,
                    "status": r.shown_status(), "summary": r.summary, "error": r.error, "user": r.user,
                    "trigger": r.trigger, "commit": r.sha, "options": r.options.describe(), "url": link(&r.id),
                })),
            }),
        ),
        // Slack, Teams (incoming webhook), Mattermost, Rocket.Chat all take {"text"}.
        _ => (t.url.clone(), json!({ "text": text })),
    }
}

pub async fn send(kind: &str, t: &NotifyTarget, event: &str, text: &str, run: Option<&Run>) -> Result<(), String> {
    let (url, body) = payload(kind, t, event, text, run);
    let r = client().post(&url).json(&body).send().await.map_err(|e| format!("could not reach it: {}", e.without_url()))?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("it answered HTTP {}", r.status().as_u16()))
    }
}

/// Send `event` for `run` to every notifier of its repository that wants it.
pub async fn run_event(core: &Core, run: &Run, event: &str) {
    let Ok(repo) = super::get_repo(core, &run.repo).await else { return };
    let wanted: Vec<&Notifier> = repo.notifiers.iter().filter(|n| n.on.iter().any(|e| e == event)).collect();
    if wanted.is_empty() {
        return;
    }
    let Ok(secrets) = super::get_secrets(core, &run.repo).await else { return };
    let msg = text(run, event);
    for n in wanted {
        if let Some(t) = secrets.notify.get(&n.id) {
            if let Err(e) = send(&n.kind, t, event, &msg, Some(run)).await {
                tracing::warn!(repo = %repo.name, notifier = %n.label, "notification failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        let t = |url: &str| NotifyTarget { url: url.into(), chat_id: String::new() };
        assert_eq!(check("slack", &t("https://hooks.slack.com/services/T0/B0/abcdWXYZ")).unwrap(), "hooks.slack.com/…WXYZ");
        assert!(check("slack", &t("hooks.slack.com/x")).is_err());
        assert!(check("teams", &t("https://x y")).is_err());
        assert!(check("pager", &t("https://x")).is_err());
        assert!(check("telegram", &NotifyTarget { url: "123456:ABCdefGHIjklMNOpqrSTUvwx".into(), chat_id: "-100123".into() }).unwrap().starts_with("bot 123456"));
        assert!(check("telegram", &NotifyTarget { url: "123456:ABCdefGHIjklMNOpqrSTUvwx".into(), chat_id: String::new() }).is_err());
        assert!(check("telegram", &t("not-a-token")).is_err());
    }
}
