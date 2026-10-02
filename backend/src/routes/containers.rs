//! File transfer with a container (gRPC-Web can't stream):
//!
//! - `GET /v1/containers/download?t=<link>` — a file as it is (`docker exec cat`),
//!   a folder as a tar (`docker cp … -`). The link comes from
//!   ContainerService.DownloadLink and names one path for one admin for a minute.
//! - `PUT /v1/containers/upload?asset&container&path` (token in `x-timika-token`,
//!   admins) — the body becomes that file; its folder is created.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::bastion::ssh;
use crate::containers as dk;
use crate::error::{AppError, AppResult};
use crate::monitor::{self as mon, SystemCfg};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct DownloadQuery {
    t: String,
}

fn disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._- ".contains(c) { c } else { '_' }).collect();
    let encoded: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"._-".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

async fn connect(st: &AppState, asset: &str) -> AppResult<russh::client::Handle<ssh::Pinned>> {
    let cfg: SystemCfg = st.core.get_json(&mon::system_path(asset)).await?.ok_or_else(|| AppError::NotFound("that server is not monitored".into()))?;
    mon::engine::connect(&st.core, &cfg).await.map_err(AppError::Unavailable)
}

pub async fn download(State(st): State<AppState>, Query(q): Query<DownloadQuery>) -> AppResult<Response> {
    let key = st.core.derive_key(b"timika container ticket v1").await.ok_or(AppError::Sealed)?;
    let t = dk::verify(&key, &q.t, chrono::Utc::now().timestamp())?;
    crate::audit::target(format!("container {} on {}:{} (download)", t.container, t.asset, t.path));
    let handle = connect(&st, &t.asset).await?;
    let dir = t.kind == "dir";
    let cmd = if dir { dk::tar_cmd(&t.runtime, &t.container, &t.path) } else { dk::cat_cmd(&t.runtime, &t.container, &t.path) };
    let ch = ssh::exec_stream(&handle, &cmd).await.map_err(AppError::Unavailable)?;
    // The connection lives as long as the stream.
    let stream = futures_util::stream::unfold(Some((ch, handle)), |state| async move {
        let (mut ch, handle) = state?;
        loop {
            match ch.wait().await {
                Some(russh::ChannelMsg::Data { data }) => return Some((Ok(bytes::Bytes::copy_from_slice(&data)), Some((ch, handle)))),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                    ssh::disconnect(&handle).await;
                    return Some((Err(std::io::Error::other(format!("docker failed (exit {exit_status})"))), None));
                }
                Some(russh::ChannelMsg::Close) | None => {
                    ssh::disconnect(&handle).await;
                    return None;
                }
                _ => {}
            }
        }
    });
    let base = t.path.rsplit('/').find(|x| !x.is_empty()).unwrap_or(&t.container).to_string();
    let name = if dir { format!("{base}.tar") } else { base };
    let mut resp = (
        [
            (axum::http::header::CONTENT_TYPE, if dir { "application/x-tar" } else { "application/octet-stream" }.to_string()),
            (axum::http::header::CONTENT_DISPOSITION, disposition(&name)),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response();
    resp.extensions_mut().insert(crate::token::TokenEntry {
        display_name: t.who,
        policies: t.policies,
        created_at: chrono::Utc::now(),
        username: t.username,
        expires_at: None,
        max_expires_at: None,
        idle_secs: None,
    });
    Ok(resp)
}

#[derive(Deserialize)]
pub struct UploadQuery {
    asset: String,
    container: String,
    path: String,
}

pub async fn upload(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<UploadQuery>, body: axum::body::Body) -> AppResult<Response> {
    use futures_util::StreamExt;
    crate::audit::target(format!("container {} on {}:{} (upload)", q.container, q.asset, q.path));
    let token = crate::grpc::token_from(&tonic::metadata::MetadataMap::from_headers(headers.clone())).ok_or_else(|| AppError::Auth("missing token".into()))?;
    let (me, _) = crate::token::resolve(&st, &token).await?;
    if me.is_restricted() || !me.is_admin() {
        return Err(AppError::Forbidden("administrators only".into()));
    }
    dk::check_name(&q.container)?;
    let path = dk::clean_path(&q.path)?;
    if path == "/" {
        return Err(AppError::BadRequest("name the file to write".into()));
    }
    let max: u64 = std::env::var("FILES_MAX_UPLOAD_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(4096) * 1024 * 1024;
    let handle = connect(&st, &q.asset).await?;
    let fail = |m: String| AppError::Unavailable(m);
    let mut ch = handle.channel_open_session().await.map_err(|e| fail(format!("could not open a session: {e}")))?;
    let rt = mon::runtime_of(&st.core, &q.asset, &q.container).await;
    ch.exec(true, dk::upload_cmd(&rt, &q.container, &path)).await.map_err(|e| fail(format!("exec failed: {e}")))?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::BadRequest(format!("upload interrupted: {e}")))?;
        written += chunk.len() as u64;
        if written > max {
            ssh::disconnect(&handle).await;
            return Err(AppError::BadRequest(format!("file is larger than the {} MB upload limit", max / 1024 / 1024)));
        }
        ch.data(&chunk[..]).await.map_err(|e| fail(format!("sending to the container failed: {e}")))?;
    }
    ch.eof().await.map_err(|e| fail(format!("finishing the upload failed: {e}")))?;
    let (mut code, mut out) = (None, Vec::new());
    while let Some(msg) = ch.wait().await {
        match msg {
            russh::ChannelMsg::Data { data } | russh::ChannelMsg::ExtendedData { data, .. } if out.len() < 8192 => out.extend_from_slice(&data),
            russh::ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    ssh::disconnect(&handle).await;
    dk::expect_ok(code.unwrap_or(255), &String::from_utf8_lossy(&out))?;
    crate::audit::target(format!("container {} on {}:{path} (upload {written} bytes)", q.container, q.asset));
    let mut resp = axum::Json(serde_json::json!({ "path": path, "size": written })).into_response();
    resp.extensions_mut().insert(me);
    Ok(resp)
}
