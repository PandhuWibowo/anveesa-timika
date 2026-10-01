//! Outbound SSH to a server: host-key pinning, password or key auth, PTY shell.

use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::client::{self, Handle, Msg};
use russh::keys::{decode_secret_key, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{Channel, Disconnect};
use tokio::sync::Mutex;

use super::{Asset, Secret};

pub struct Pinned {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

fn fingerprint(key: &PublicKeyOrCertificate) -> String {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256).to_string(),
        PublicKeyOrCertificate::Certificate(c) => {
            PublicKey::new(c.public_key().clone(), "").fingerprint(HashAlg::Sha256).to_string()
        }
    }
}

impl client::Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = fingerprint(key);
        let ok = self.expected.as_deref().is_none_or(|e| e == fp);
        *self.seen.lock().await = Some(fp);
        Ok(ok)
    }
}

pub struct Connected {
    pub handle: Handle<Pinned>,
    /// Fingerprint the server presented.
    pub host_key: String,
    /// True when no key was pinned yet (trust on first use).
    pub first_seen: bool,
    pub connect_ms: u128,
}

fn config() -> Arc<client::Config> {
    Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    })
}

/// Connect and authenticate. Errors are user-facing messages.
pub async fn connect(asset: &Asset, username: &str, secret: &Secret) -> Result<Connected, String> {
    let started = Instant::now();
    let seen = Arc::new(Mutex::new(None));
    let handler = Pinned { expected: asset.host_key.clone(), seen: seen.clone() };
    let addr = (asset.host.as_str(), asset.port);
    let mut handle = match tokio::time::timeout(Duration::from_secs(10), client::connect(config(), addr, handler)).await {
        Err(_) => return Err(format!("{}:{} did not answer within 10s", asset.host, asset.port)),
        Ok(Err(e)) => {
            let presented = seen.lock().await.clone();
            if let (Some(expected), Some(got)) = (&asset.host_key, &presented) {
                if expected != got {
                    return Err(format!(
                        "HOST KEY CHANGED for {} — expected {expected}, server presented {got}. This can mean a \
                         man-in-the-middle attack. If the server was legitimately reinstalled, an administrator must reset its host key.",
                        asset.name
                    ));
                }
            }
            return Err(format!("could not connect to {}:{}: {e}", asset.host, asset.port));
        }
        Ok(Ok(h)) => h,
    };
    let host_key = seen.lock().await.clone().unwrap_or_default();

    let auth = if let Some(pem) = secret.private_key.as_deref().filter(|k| !k.trim().is_empty()) {
        let key = decode_secret_key(pem, secret.passphrase.as_deref())
            .map_err(|e| format!("private key could not be read: {e}"))?;
        let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
        handle.authenticate_publickey(username, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await
    } else {
        handle.authenticate_password(username, secret.password.clone().unwrap_or_default()).await
    }
    .map_err(|e| format!("authentication error: {e}"))?;
    if !auth.success() {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        return Err(format!("the server rejected the credentials for `{username}`"));
    }
    Ok(Connected {
        handle,
        first_seen: asset.host_key.is_none(),
        host_key,
        connect_ms: started.elapsed().as_millis(),
    })
}

/// Open an interactive shell with a PTY of the given size.
pub async fn shell(handle: &Handle<Pinned>, cols: u32, rows: u32) -> Result<Channel<Msg>, String> {
    let ch = handle.channel_open_session().await.map_err(|e| format!("could not open a session: {e}"))?;
    ch.request_pty(false, "xterm-256color", cols, rows, 0, 0, &[])
        .await
        .map_err(|e| format!("PTY request failed: {e}"))?;
    ch.request_shell(false).await.map_err(|e| format!("shell request failed: {e}"))?;
    Ok(ch)
}

pub async fn disconnect(handle: &Handle<Pinned>) {
    let _ = handle.disconnect(Disconnect::ByApplication, "session ended", "en").await;
}

/// Run `command` without a PTY, feeding `stdin`, and collect its exit status
/// and (bounded) output.
pub async fn exec(handle: &Handle<Pinned>, command: &str, stdin: &[u8]) -> Result<(u32, String), String> {
    exec_for(handle, command, stdin, Duration::from_secs(30)).await
}

/// `exec` with a custom time limit (archiving big folders takes a while).
pub async fn exec_for(handle: &Handle<Pinned>, command: &str, stdin: &[u8], limit: Duration) -> Result<(u32, String), String> {
    let mut ch = handle.channel_open_session().await.map_err(|e| format!("could not open a session: {e}"))?;
    ch.exec(true, command).await.map_err(|e| format!("exec failed: {e}"))?;
    if !stdin.is_empty() {
        ch.data(stdin).await.map_err(|e| format!("could not send input: {e}"))?;
    }
    ch.eof().await.map_err(|e| format!("could not send input: {e}"))?;
    let mut out = Vec::new();
    let mut status = None;
    let run = async {
        while let Some(msg) = ch.wait().await {
            match msg {
                russh::ChannelMsg::Data { data } | russh::ChannelMsg::ExtendedData { data, .. } => {
                    if out.len() < 16 * 1024 {
                        out.extend_from_slice(&data);
                    }
                }
                russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
    };
    tokio::time::timeout(limit, run).await.map_err(|_| format!("the command did not finish within {}s", limit.as_secs()))?;
    Ok((status.unwrap_or(255), String::from_utf8_lossy(&out).into_owned()))
}

/// Start `command` and return its channel, to stream stdout as it comes.
pub async fn exec_stream(handle: &Handle<Pinned>, command: &str) -> Result<Channel<Msg>, String> {
    let ch = handle.channel_open_session().await.map_err(|e| format!("could not open a session: {e}"))?;
    ch.exec(true, command).await.map_err(|e| format!("exec failed: {e}"))?;
    ch.eof().await.map_err(|e| format!("exec failed: {e}"))?;
    Ok(ch)
}
