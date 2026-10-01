//! Web terminal: bridges a browser WebSocket to an SSH shell.
//!
//! Wire protocol (kept tiny for latency):
//!   browser → server  binary = keystrokes · text = JSON control {"type":"resize","cols","rows"}
//!   server → browser  binary = terminal output · text = JSON events
//!                     {"type":"status"|"ready"|"error"|"closed", …}

use axum::extract::ws::{Message, WebSocket};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use russh::ChannelMsg;
use serde_json::{json, Map, Value};

use super::commands::CommandLog;
use super::recorder::Recorder;
use super::{session_path, ssh, Asset, Secret, Session};
use crate::core::MAX_RETRIES;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::token::TokenEntry;

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

async fn save(st: &AppState, s: &Session) -> AppResult<()> {
    st.core.commit(vec![(session_path(&s.id), serde_json::to_vec(s)?)], vec![], vec![]).await
}

/// Read the admin kill flag (the only field another instance changes).
async fn kill_requested(st: &AppState, sid: &str) -> bool {
    st.core.get_json::<Session>(&session_path(sid)).await.ok().flatten().is_some_and(|s| s.kill)
}

/// Persist our view of the session without clobbering a concurrent kill flag.
async fn save_keep_kill(st: &AppState, s: &mut Session) -> AppResult<()> {
    for _ in 0..MAX_RETRIES {
        let (cur, guard) = st.core.get_json_guarded::<Session>(&session_path(&s.id)).await?;
        if let Some(c) = cur {
            s.kill = s.kill || c.kill;
        }
        match st.core.commit(vec![(session_path(&s.id), serde_json::to_vec(&*s)?)], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            other => return other,
        }
    }
    Err(AppError::WriteConflict)
}

async fn audit(st: &AppState, s: &Session) {
    if let Some(a) = &st.audit {
        let mut e = Map::new();
        e.insert("type".into(), json!("bastion-session"));
        e.insert("time".into(), json!(Utc::now()));
        e.insert("id".into(), json!(s.id));
        e.insert("auth".into(), json!({ "display_name": s.user }));
        e.insert(
            "session".into(),
            json!({
                "status": s.status, "asset": s.asset, "host": s.host, "account": s.account,
                "client_ip": s.client_ip, "started_at": s.started_at, "ended_at": s.ended_at,
                "bytes_recorded": s.bytes, "error": s.error,
            }),
        );
        a.record(e).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    st: AppState,
    socket: WebSocket,
    me: TokenEntry,
    asset: Asset,
    account: String,
    secret: Secret,
    cols: u32,
    rows: u32,
    ip: Option<String>,
) {
    let (mut tx, mut rx) = socket.split();
    let send_json = |v: Value| Message::Text(v.to_string());

    let now = Utc::now();
    let mut s = Session {
        id: super::random_id(10),
        user: me.username.clone().unwrap_or_else(|| me.display_name.clone()),
        asset: asset.id.clone(),
        asset_name: asset.name.clone(),
        host: format!("{}:{}", asset.host, asset.port),
        account: account.clone(),
        client_ip: ip,
        instance: st.core.identity.id.clone(),
        started_at: now,
        ended_at: None,
        status: "connecting".into(),
        error: None,
        cols,
        rows,
        bytes: 0,
        chunks: 0,
        truncated: false,
        commands: 0,
        kill: false,
        heartbeat_at: now,
    };
    if let Err(e) = save(&st, &s).await {
        let _ = tx.send(send_json(json!({ "type": "error", "message": format!("could not start session: {e}") }))).await;
        return;
    }
    let _ = tx.send(send_json(json!({ "type": "status", "message": format!("Connecting to {}…", asset.name) }))).await;

    let connected = match ssh::connect(&asset, &account, &secret).await {
        Ok(c) => c,
        Err(msg) => {
            let _ = tx.send(send_json(json!({ "type": "error", "message": msg }))).await;
            s.status = "failed".into();
            s.error = Some(msg);
            s.ended_at = Some(Utc::now());
            let _ = save_keep_kill(&st, &mut s).await;
            audit(&st, &s).await;
            return;
        }
    };
    if connected.first_seen && !connected.host_key.is_empty() {
        let _ = super::pin_host_key(&st.core, &asset.id, &connected.host_key).await;
    }
    let channel = match ssh::shell(&connected.handle, cols, rows).await {
        Ok(c) => c,
        Err(msg) => {
            let _ = tx.send(send_json(json!({ "type": "error", "message": msg }))).await;
            s.status = "failed".into();
            s.error = Some(msg);
            s.ended_at = Some(Utc::now());
            let _ = save_keep_kill(&st, &mut s).await;
            ssh::disconnect(&connected.handle).await;
            return;
        }
    };

    s.status = "live".into();
    let _ = save_keep_kill(&st, &mut s).await;
    audit(&st, &s).await;
    let _ = tx
        .send(send_json(json!({
            "type": "ready", "session": s.id, "host_key": connected.host_key,
            "first_seen": connected.first_seen, "connect_ms": connected.connect_ms,
        })))
        .await;

    let mut rec = Recorder::new(&s.id, env_u64("RECORDING_MAX_MB", 100) * 1024 * 1024);
    let mut cmds = CommandLog::new(&s.id, Utc::now());
    let (mut read, write) = channel.split();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut end = "closed";
    let mut end_msg = String::from("connection closed");

    loop {
        tokio::select! {
            msg = rx.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    cmds.input(&b);
                    if write.data_bytes(b).await.is_err() { break; }
                }
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v["type"] == "resize" {
                            let (c, r) = (v["cols"].as_u64().unwrap_or(80) as u32, v["rows"].as_u64().unwrap_or(24) as u32);
                            if c > 0 && r > 0 && c <= 1000 && r <= 1000 {
                                let _ = write.window_change(c, r, 0, 0).await;
                                rec.resize(c, r);
                                s.cols = c;
                                s.rows = r;
                            }
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => { end_msg = "browser closed the terminal".into(); break; }
                _ => {}
            },
            m = read.wait() => match m {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    rec.output(&data);
                    cmds.output(&data);
                    if tx.send(Message::Binary(data.to_vec())).await.is_err() { break; }
                    if rec.wants_flush() { let _ = rec.flush(&st.core).await; }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => { end_msg = format!("shell exited ({exit_status})"); }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            },
            _ = tick.tick() => {
                let _ = rec.flush(&st.core).await;
                let _ = cmds.flush(&st.core).await;
                s.commands = cmds.count;
                if kill_requested(&st, &s.id).await {
                    end = "killed";
                    end_msg = "session terminated by an administrator".into();
                    let _ = tx.send(send_json(json!({ "type": "error", "message": end_msg }))).await;
                    break;
                }
                s.heartbeat_at = Utc::now();
                s.bytes = rec.bytes;
                s.chunks = rec.seq;
                s.truncated = rec.truncated;
                let _ = save_keep_kill(&st, &mut s).await;
            }
        }
    }

    let _ = rec.flush(&st.core).await;
    let _ = cmds.flush(&st.core).await;
    s.commands = cmds.count;
    ssh::disconnect(&connected.handle).await;
    s.status = end.into();
    s.ended_at = Some(Utc::now());
    s.heartbeat_at = Utc::now();
    s.bytes = rec.bytes;
    s.chunks = rec.seq;
    s.truncated = rec.truncated;
    let _ = save_keep_kill(&st, &mut s).await;
    audit(&st, &s).await;
    let _ = tx.send(send_json(json!({ "type": "closed", "message": end_msg }))).await;
    let _ = tx.close().await;
}
