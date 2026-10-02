//! A tiny SSH server for the bastion end-to-end tests (`e2e/`): no real shell,
//! just enough terminal behaviour to assert on.
//!
//!   cargo run --example ssh_target -- <port> <host_key_file> <user> <password> [authorized_pubkey_file] [files_root]
//!
//! SFTP: serves `files_root` (default: a temp folder) as `/`.
//!
//! Shell: echoes keystrokes; on Enter runs one of
//!   `whoami` → the user · `size` → "<cols>x<rows>" · `big` → 200 KiB of output ·
//!   `exit` → closes the channel · anything else → "ok: <line>".
//! Exec: answers timika's monitoring script from `<files_root>/.timika-metrics`;
//! understands timika's account provisioning (`sh -s`, `sudo -n true`,
//! `sudo -n sh -s`, `sudo -S -p '' sh -s`): the script's U/P become a user
//! that can then sign in with that password.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh::keys::{load_secret_key, ssh_key};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Pty};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle as SftpHandle, Name, OpenFlags, Status, StatusCode, Version};

#[derive(Clone)]
struct Target {
    user: String,
    password: String,
    pubkey: Option<ssh_key::PublicKey>,
    line: String,
    cols: u32,
    rows: u32,
    authed_as: String,
    users: Arc<Mutex<HashMap<String, String>>>,
    exec: Option<String>,
    stdin: Vec<u8>,
    files_root: std::path::PathBuf,
    /// Session channels of this connection, until a subsystem claims one.
    channels: Arc<tokio::sync::Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    sftp_channels: Arc<Mutex<std::collections::HashSet<ChannelId>>>,
}

/// `U='name'` → name (undoing sh single-quote escaping).
fn sh_value(script: &str, var: &str) -> Option<String> {
    let line = script.lines().find(|l| l.starts_with(&format!("{var}=")))?;
    let q = &line[var.len() + 1..];
    Some(q.trim_matches('\'').replace(r"'\''", "'"))
}

impl russh::server::Server for Target {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        let mut t = self.clone();
        t.channels = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        t.sftp_channels = Arc::new(Mutex::new(std::collections::HashSet::new()));
        t
    }
}

impl russh::server::Handler for Target {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let extra = self.users.lock().unwrap().get(user).is_some_and(|p| p == password);
        if (user == self.user && password == self.password) || extra {
            self.authed_as = user.to_string();
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey_offered(&mut self, user: &str, key: &ssh_key::PublicKey) -> Result<Auth, Self::Error> {
        Ok(if user == self.user && self.pubkey.as_ref().is_some_and(|k| k.key_data() == key.key_data()) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: &ssh_key::PublicKey) -> Result<Auth, Self::Error> {
        let ok = user == self.user && self.pubkey.as_ref().is_some_and(|k| k.key_data() == key.key_data());
        if ok {
            self.authed_as = user.to_string();
        }
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, channel: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        let ch = self.channels.lock().await.remove(&channel);
        match (name, ch) {
            ("sftp", Some(ch)) => {
                self.sftp_channels.lock().unwrap().insert(channel);
                session.channel_success(channel)?;
                let fs = Fs { root: self.files_root.clone(), handles: HashMap::new(), next: 0 };
                tokio::spawn(russh_sftp::server::run(ch.into_stream(), fs));
            }
            _ => session.channel_failure(channel)?,
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.cols = cols;
        self.rows = rows;
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, format!("welcome {}\r\n$ ", self.authed_as).into_bytes())?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.cols = cols;
        self.rows = rows;
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.exec = Some(String::from_utf8_lossy(data).into_owned());
        session.channel_success(channel)?;
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let Some(cmd) = self.exec.take() else { return Ok(()) };
        let raw = std::mem::take(&mut self.stdin);
        let input = String::from_utf8_lossy(&raw).into_owned();
        let status = if cmd == "sudo -n true" {
            0
        } else if cmd == "sh -s" && input.starts_with("# timika-metrics") {
            // Monitoring: answer with what the test put in `.timika-metrics`.
            let out = std::fs::read_to_string(self.files_root.join(".timika-metrics")).unwrap_or_else(|_| "unsupported=TestTarget\n".into());
            session.data(channel, out.into_bytes())?;
            0
        } else if cmd.ends_with("sh -s") {
            // `sudo -S` reads the password line first.
            let script = if cmd.starts_with("sudo -S") { input.split_once('\n').map(|x| x.1).unwrap_or("").to_string() } else { input };
            match sh_value(&script, "U") {
                Some(u) => {
                    let mut out = String::new();
                    let exists = u == self.user || self.users.lock().unwrap().contains_key(&u);
                    let mut status = 0;
                    match sh_value(&script, "P") {
                        Some(p) => {
                            if !exists {
                                out.push_str("created\n");
                            }
                            self.users.lock().unwrap().insert(u.clone(), p);
                            out.push_str("password-set\n");
                        }
                        None if !exists => {
                            out.push_str("no-such-user\n");
                            status = 4;
                        }
                        None => {}
                    }
                    if status == 0 {
                        // Groups that "exist" on this pretend server.
                        for line in script.lines().filter_map(|l| l.strip_prefix("if getent group ")) {
                            let g = line.split_whitespace().next().unwrap_or("");
                            let known = matches!(g, "docker" | "adm" | "sudo" | "wheel" | "systemd-journal");
                            out.push_str(&format!("{} {g}\n", if known { "group-ok" } else { "group-missing" }));
                        }
                        if script.contains("visudo -cf") || script.contains("rm -f /etc/sudoers.d/") {
                            out.push_str("sudo-ok\n");
                        }
                        out.push_str("done-ok\n");
                    }
                    session.data(channel, out.into_bytes())?;
                    status
                }
                None => 1,
            }
        } else {
            // Anything else (tar, zip, command -v …) runs for real, inside the
            // served folder: `cd -- '/x'` is mapped onto it. Output streams back.
            let root = self.files_root.clone();
            let real = match cmd.strip_prefix("cd -- '").and_then(|r| r.split_once("' && ")) {
                Some((vpath, rest)) => {
                    let p = root.join(vpath.replace(r"'\''", "'").trim_start_matches('/'));
                    format!("cd -- '{}' && {rest}", p.display().to_string().replace('\'', r"'\''"))
                }
                None => cmd.clone(),
            };
            let handle = session.handle();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut child = match tokio::process::Command::new("sh").arg("-c").arg(&real).current_dir(&root)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn() {
                    Ok(c) => c,
                    Err(_) => { let _ = handle.exit_status_request(channel, 127).await; let _ = handle.close(channel).await; return; }
                };
                // What the client sent before EOF is the command's stdin (uploads).
                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    let _ = stdin.write_all(&raw).await;
                }
                let mut out = child.stdout.take().unwrap();
                let mut err = child.stderr.take().unwrap();
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match out.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => { if handle.data(channel, bytes::Bytes::copy_from_slice(&buf[..n])).await.is_err() { break; } }
                    }
                }
                let mut e = Vec::new();
                let _ = err.read_to_end(&mut e).await;
                if !e.is_empty() { let _ = handle.extended_data(channel, 1, bytes::Bytes::from(e)).await; }
                let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(1) as u32;
                let _ = handle.exit_status_request(channel, code).await;
                let _ = handle.eof(channel).await;
                let _ = handle.close(channel).await;
            });
            return Ok(());
        };
        session.exit_status_request(channel, status)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if self.sftp_channels.lock().unwrap().contains(&channel) {
            return Ok(()); // the SFTP task reads it from the channel
        }
        if self.exec.is_some() {
            self.stdin.extend_from_slice(data);
            return Ok(());
        }
        for &b in data {
            if b == b'\r' || b == b'\n' {
                let line = std::mem::take(&mut self.line);
                let out = match line.trim() {
                    "whoami" => self.authed_as.clone(),
                    "size" => format!("{}x{}", self.cols, self.rows),
                    "big" => "x".repeat(200 * 1024),
                    "exit" => {
                        session.data(channel, b"\r\nbye\r\n".to_vec())?;
                        session.exit_status_request(channel, 0)?;
                        session.eof(channel)?;
                        session.close(channel)?;
                        return Ok(());
                    }
                    other => format!("ok: {other}"),
                };
                session.data(channel, format!("\r\n{out}\r\n$ ").into_bytes())?;
            } else {
                self.line.push(b as char);
                session.data(channel, vec![b])?;
            }
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        anyhow::bail!("usage: ssh_target <port> <host_key_file> <user> <password> [authorized_pubkey_file]");
    }
    let key = load_secret_key(&a[1], None)?;
    let pubkey = match a.get(4) {
        Some(p) => Some(ssh_key::PublicKey::from_openssh(std::fs::read_to_string(p)?.trim())?),
        None => None,
    };
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        auth_rejection_time: std::time::Duration::from_millis(50),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        ..Default::default()
    });
    let mut t = Target {
        user: a[2].clone(),
        password: a[3].clone(),
        pubkey,
        line: String::new(),
        cols: 0,
        rows: 0,
        authed_as: String::new(),
        users: Arc::new(Mutex::new(HashMap::new())),
        exec: None,
        stdin: Vec::new(),
        files_root: match a.get(5) {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let p = std::env::temp_dir().join(format!("ssh_target_files_{}", a[0]));
                std::fs::create_dir_all(&p)?;
                p
            }
        },
        channels: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        sftp_channels: Arc::new(Mutex::new(std::collections::HashSet::new())),
    };
    let socket = tokio::net::TcpListener::bind(("127.0.0.1", a[0].parse::<u16>()?)).await?;
    println!("ssh_target listening on 127.0.0.1:{}", a[0]);
    t.run_on_socket(config, &socket).await?;
    Ok(())
}

// ─── SFTP over a real folder ──────────────────────────────────────────────────

enum Open {
    File(tokio::fs::File),
    Dir(Option<Vec<File>>),
}

struct Fs {
    root: std::path::PathBuf,
    handles: HashMap<String, Open>,
    next: u64,
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn io(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl Fs {
    /// Client path → normalized absolute path ("." and "" are "/").
    fn virt(p: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for seg in p.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        format!("/{}", parts.join("/"))
    }
    fn real(&self, p: &str) -> std::path::PathBuf {
        self.root.join(Self::virt(p).trim_start_matches('/'))
    }
    fn handle(&mut self, h: Open) -> String {
        self.next += 1;
        let k = self.next.to_string();
        self.handles.insert(k.clone(), h);
        k
    }
}

impl russh_sftp::server::Handler for Fs {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name { id, files: vec![File::dummy(Self::virt(&path))] })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<SftpHandle, Self::Error> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(self.real(&path)).map_err(io)? {
            let e = e.map_err(io)?;
            let meta = e.metadata().map_err(io)?;
            files.push(File::new(e.file_name().to_string_lossy().into_owned(), FileAttributes::from(&meta)));
        }
        let handle = self.handle(Open::Dir(Some(files)));
        Ok(SftpHandle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) => match files.take() {
                Some(files) => Ok(Name { id, files }),
                None => Err(StatusCode::Eof),
            },
            _ => Err(StatusCode::Failure),
        }
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, _: FileAttributes) -> Result<SftpHandle, Self::Error> {
        let f = tokio::fs::OpenOptions::new()
            .read(pflags.contains(OpenFlags::READ))
            .write(pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND))
            .append(pflags.contains(OpenFlags::APPEND))
            .create(pflags.contains(OpenFlags::CREATE))
            .truncate(pflags.contains(OpenFlags::TRUNCATE))
            .open(self.real(&filename))
            .await
            .map_err(io)?;
        let handle = self.handle(Open::File(f));
        Ok(SftpHandle { id, handle })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(std::io::SeekFrom::Start(offset)).await.map_err(io)?;
        let mut buf = vec![0u8; len as usize];
        let n = f.read(&mut buf).await.map_err(io)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(std::io::SeekFrom::Start(offset)).await.map_err(io)?;
        f.write_all(&data).await.map_err(io)?;
        Ok(ok(id))
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let Some(Open::File(f)) = self.handles.get(&handle) else { return Err(StatusCode::Failure) };
        let meta = f.metadata().await.map_err(io)?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::metadata(self.real(&path)).map_err(io)?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::symlink_metadata(self.real(&path)).map_err(io)?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.real(&path)).map_err(io)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_dir(self.real(&path)).map_err(io)?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.real(&filename)).map_err(io)?;
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, Self::Error> {
        std::fs::rename(self.real(&oldpath), self.real(&newpath)).map_err(io)?;
        Ok(ok(id))
    }
}
