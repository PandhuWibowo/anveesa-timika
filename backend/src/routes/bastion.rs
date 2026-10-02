//! `GET /v1/bastion/connect` — the web terminal. A WebSocket, not gRPC:
//! browsers can't do bidirectional gRPC streaming. Everything else about the
//! bastion is `timika.v1.BastionService`.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Query, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;

use crate::bastion::{self, Secret};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

// ─── web terminal ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ConnectQuery {
    asset: String,
    account: String,
    #[serde(default = "default_cols")]
    cols: u32,
    #[serde(default = "default_rows")]
    rows: u32,
    /// Open a shell inside this container (admins) instead of on the server.
    #[serde(default)]
    container: Option<String>,
}
fn default_cols() -> u32 { 120 }
fn default_rows() -> u32 { 32 }

/// `GET /v1/bastion/connect` — WebSocket upgrade. Served by whichever
/// instance receives it: the SSH connection lives here.
pub async fn connect(
    State(st): State<AppState>,
    ws: WebSocketUpgrade,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Query(q): Query<ConnectQuery>,
) -> AppResult<Response> {
    crate::audit::target(match &q.container {
        Some(c) => format!("{}@{} → container {c}", q.account, q.asset),
        None => format!("{}@{}", q.account, q.asset),
    });
    let token = crate::audit::ws_token(&headers).ok_or_else(|| AppError::Auth("missing token".into()))?;
    let (me, _) = crate::token::resolve(&st, &token).await?;
    if me.is_restricted() {
        return Err(AppError::Forbidden("password change required".into()));
    }
    let asset = bastion::get_asset(&st.core, &q.asset).await?;
    if !bastion::allowed_accounts(&st.core, &me, &asset).await?.contains(&q.account) {
        return Err(AppError::Forbidden(format!("you don't have access to {}@{}", q.account, asset.name)));
    }
    let container = q.container.clone().filter(|c| !c.is_empty());
    if let Some(c) = &container {
        if !me.is_admin() {
            return Err(AppError::Forbidden("only administrators can open a shell in a container".into()));
        }
        crate::containers::check_name(c)?;
    }
    let secret: Secret = st
        .core
        .get_json(&bastion::cred_path(&asset.id, &q.account))
        .await?
        .ok_or_else(|| AppError::NotFound("account credentials".into()))?;
    let ip = st.auth.client_ip(&headers, peer.map(|c| c.0));
    let (cols, rows) = (q.cols.clamp(20, 1000), q.rows.clamp(5, 1000));
    let entry = me.clone();
    let mut resp = ws
        .protocols(["timika"])
        .on_upgrade(move |socket| bastion::session::run(st, socket, me, asset, q.account, secret, cols, rows, ip, container));
    resp.extensions_mut().insert(entry);
    Ok(resp)
}

// ─── file transfer (SFTP) ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DownloadQuery {
    t: String,
}

/// RFC 5987 filename for Content-Disposition.
fn disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._- ".contains(c) { c } else { '_' }).collect();
    let encoded: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"._-".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Who a file transfer is attributed to in the audit log.
fn audit_who(display_name: &str, username: Option<String>, policies: Vec<String>) -> crate::token::TokenEntry {
    crate::token::TokenEntry {
        display_name: display_name.to_string(),
        policies,
        created_at: chrono::Utc::now(),
        username,
        expires_at: None,
        max_expires_at: None,
        idle_secs: None,
    }
}

/// `GET /v1/bastion/files/download?t=<link>` — streams the file. The link
/// (from BastionService.DownloadLink) names one file for one person, for a minute.
pub async fn download(State(st): State<AppState>, Query(q): Query<DownloadQuery>) -> AppResult<Response> {
    use axum::response::IntoResponse;
    let key = st.core.derive_key(b"timika files ticket v1").await.ok_or(AppError::Sealed)?;
    let t = bastion::files::verify(&key, &q.t, chrono::Utc::now())?;
    if let Some(format) = t.format.clone() {
        return download_archive(&st, t, &format).await;
    }
    crate::audit::target(format!("{}@{}:{} (download)", t.account, t.asset, t.path));
    let asset = bastion::get_asset(&st.core, &t.asset).await?;
    let conn = st.files.get(&st.core, &asset, &t.account, &t.who).await?;
    let meta = conn.sftp.metadata(t.path.clone()).await.map_err(|e| bastion::files::sftp_error(&t.path, e))?;
    let file = conn.sftp.open(t.path.clone()).await.map_err(|e| bastion::files::sftp_error(&t.path, e))?;
    let name = t.path.rsplit('/').next().unwrap_or("download").to_string();
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024));
    let mut resp = (
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, disposition(&name)),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response();
    if let Some(size) = meta.size {
        resp.headers_mut().insert(axum::http::header::CONTENT_LENGTH, size.into());
    }
    resp.extensions_mut().insert(audit_who(&t.who, t.username, t.policies));
    Ok(resp)
}

/// Stream `t.names` in folder `t.path` as one archive, made on the fly by the
/// server's tar / zip (nothing is written on the server).
async fn download_archive(st: &AppState, t: bastion::files::Ticket, format: &str) -> AppResult<Response> {
    use axum::response::IntoResponse;
    use bastion::archive::{self, Format};
    let f = Format::parse(format)?;
    crate::audit::target(format!("{}@{}:{} (download {} as {format})", t.account, t.asset, t.path, t.names.join(", ")));
    let asset = bastion::get_asset(&st.core, &t.asset).await?;
    let conn = st.files.get(&st.core, &asset, &t.account, &t.who).await?;
    let tool = if f == Format::Zip { "zip" } else { "tar" };
    let (have, _) = bastion::ssh::exec(&conn.handle, &format!("command -v {tool}"), b"").await.map_err(AppError::Unavailable)?;
    if have != 0 {
        return Err(AppError::BadRequest(format!("`{tool}` isn't installed on the server — choose another format")));
    }
    let cmd = archive::stream_cmd(&t.path, &t.names, f)?;
    let ch = bastion::ssh::exec_stream(&conn.handle, &cmd).await.map_err(AppError::Unavailable)?;
    // stdout → the response; a failing command aborts the download (rather
    // than leaving the user with a silently truncated archive).
    let stream = futures_util::stream::unfold(Some(ch), |state| async move {
        let mut ch = state?;
        loop {
            match ch.wait().await {
                Some(russh::ChannelMsg::Data { data }) => return Some((Ok(bytes::Bytes::copy_from_slice(&data)), Some(ch))),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                    return Some((Err(std::io::Error::other(format!("archiving failed (exit {exit_status})"))), None))
                }
                Some(russh::ChannelMsg::Close) | None => return None,
                _ => {}
            }
        }
    });
    let base = if t.names.len() == 1 { t.names[0].clone() } else { t.path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("files").to_string() };
    let mut resp = (
        [
            (axum::http::header::CONTENT_TYPE, f.mime().to_string()),
            (axum::http::header::CONTENT_DISPOSITION, disposition(&format!("{base}{}", f.ext()))),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response();
    resp.extensions_mut().insert(audit_who(&t.who, t.username, t.policies));
    Ok(resp)
}

#[derive(Deserialize)]
pub struct UploadQuery {
    asset: String,
    account: String,
    /// Full remote path of the file to write (overwritten if it exists).
    path: String,
}

/// `PUT /v1/bastion/files/upload?asset&account&path` with the token in
/// `x-timika-token` — streams the body into the file.
pub async fn upload(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<UploadQuery>, body: axum::body::Body) -> AppResult<Response> {
    use axum::response::IntoResponse;
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    crate::audit::target(format!("{}@{}:{} (upload)", q.account, q.asset, q.path));
    let token = crate::grpc::token_from(&tonic::metadata::MetadataMap::from_headers(headers.clone()))
        .ok_or_else(|| AppError::Auth("missing token".into()))?;
    let (me, _) = crate::token::resolve(&st, &token).await?;
    if me.is_restricted() {
        return Err(AppError::Forbidden("password change required".into()));
    }
    let asset = bastion::get_asset(&st.core, &q.asset).await?;
    if !bastion::allowed_accounts(&st.core, &me, &asset).await?.contains(&q.account) {
        return Err(AppError::Forbidden(format!("you don't have access to {}@{}", q.account, asset.name)));
    }
    let path = bastion::files::clean_remote(&q.path)?;
    let max: u64 = std::env::var("FILES_MAX_UPLOAD_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(4096) * 1024 * 1024;
    let who = me.username.clone().unwrap_or_else(|| me.display_name.clone());
    let conn = st.files.get(&st.core, &asset, &q.account, &who).await?;
    let mut file = conn.sftp.create(path.clone()).await.map_err(|e| bastion::files::sftp_error(&path, e))?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::BadRequest(format!("upload interrupted: {e}")))?;
        written += chunk.len() as u64;
        if written > max {
            drop(file);
            let _ = conn.sftp.remove_file(path.clone()).await;
            return Err(AppError::BadRequest(format!("file is larger than the {} MB upload limit", max / 1024 / 1024)));
        }
        file.write_all(&chunk).await.map_err(|e| AppError::Unavailable(format!("writing `{path}` failed: {e}")))?;
    }
    file.shutdown().await.map_err(|e| AppError::Unavailable(format!("finishing `{path}` failed: {e}")))?;
    crate::audit::target(format!("{}@{}:{} (upload {written} bytes)", q.account, q.asset, path));
    let mut resp = axum::Json(serde_json::json!({ "path": path, "size": written })).into_response();
    resp.extensions_mut().insert(me);
    Ok(resp)
}
