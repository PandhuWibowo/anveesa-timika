//! Git, through the `git` binary, on a bare clone per repository in
//! `AUTOMATION_DIR` (default `./data/automation`). Nothing is ever checked
//! out here: runs get a `git archive` of one commit.
//!
//! Hardening: no system or user git config, no hooks, only https / http / ssh
//! remotes (file:// only with `AUTOMATION_ALLOW_FILE_URLS=true`, for tests),
//! no prompts, a time limit. Tokens go in an HTTP header via the environment
//! (never on a command line or in a stored URL); deploy keys exist on disk
//! (0600) only while git runs. SSH host keys are trusted on first use, per
//! repository.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;

use super::Commit;

const LIMIT: Duration = Duration::from_secs(180);
const FORMAT: &str = "--format=%H%x1f%s%x1f%an%x1f%cI%x1e";

pub fn base_dir() -> PathBuf {
    PathBuf::from(std::env::var("AUTOMATION_DIR").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "data/automation".into()))
}

fn repo_dir(id: &str) -> PathBuf {
    base_dir().join(format!("{id}.git"))
}

/// One git operation per repository at a time on this instance.
fn lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(id.to_string()).or_default().clone()
}

#[derive(Clone, Default)]
pub struct Auth {
    pub token: Option<String>,
    pub private_key: Option<String>,
}

fn allow_file() -> bool {
    std::env::var("AUTOMATION_ALLOW_FILE_URLS").is_ok_and(|v| v == "true")
}

/// https://…, http://…, ssh://…, git@host:owner/repo(.git).
pub fn check_url(url: &str) -> Result<(), String> {
    let bad = || Err("enter a Git URL like https://github.com/acme/infra.git or git@github.com:acme/infra.git".to_string());
    if url.is_empty() || url.len() > 500 || url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad();
    }
    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("ssh://") {
        return Ok(());
    }
    if url.starts_with("file://") {
        return if allow_file() { Ok(()) } else { Err("local (file://) repositories are not allowed".into()) };
    }
    // scp-like: user@host:path
    match url.split_once(':') {
        Some((userhost, path)) if userhost.contains('@') && !userhost.contains('/') && !path.is_empty() && !path.starts_with("//") => Ok(()),
        _ => bad(),
    }
}

pub fn check_branch(b: &str) -> Result<(), String> {
    let ok = !b.is_empty()
        && b.len() <= 200
        && !b.starts_with('-')
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
        && !b.contains("..")
        && !b.contains("//")
        && b.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'));
    if ok { Ok(()) } else { Err(format!("`{b}` is not a valid branch name")) }
}

/// A new Ed25519 key pair: (private key in OpenSSH format, public key line).
pub fn new_key_pair(comment: &str) -> Result<(String, String), String> {
    use russh::keys::ssh_key::{private::Ed25519Keypair, LineEnding, PrivateKey};
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut seed);
    let mut key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    seed.fill(0);
    key.set_comment(comment);
    let private = key.to_openssh(LineEnding::LF).map_err(|e| e.to_string())?.to_string();
    let public = key.public_key().to_openssh().map_err(|e| e.to_string())?;
    Ok((private, public))
}

/// Deletes the key file when git is done.
struct KeyFile(PathBuf);
impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn humanize(stderr: &str, url: &str) -> String {
    let s = stderr.to_lowercase();
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', ':']).next().unwrap_or(url).rsplit('@').next().unwrap_or(url);
    if s.contains("authentication failed")
        || s.contains("could not read username")
        || s.contains("permission denied (publickey")
        || s.contains("repository not found")
        || s.contains("not found") && s.contains("repository")
        || s.contains("403")
        || s.contains("401")
    {
        return "access denied — check the token or deploy key, and that it can read this repository".into();
    }
    if s.contains("could not resolve host") || s.contains("name or service not known") || s.contains("nodename nor servname") {
        return format!("can't reach {host} (DNS)");
    }
    if s.contains("couldn't find remote ref") {
        return "that branch doesn't exist in the repository".into();
    }
    if s.contains("host key verification failed") || s.contains("remote host identification has changed") {
        return format!("the SSH host key of {host} changed — refusing to connect");
    }
    if s.contains("connection refused") || s.contains("timed out") || s.contains("connection reset") {
        return format!("can't connect to {host}");
    }
    let last = stderr.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("hint:")).last().unwrap_or("git failed");
    last.trim_start_matches("fatal: ").trim_start_matches("error: ").chars().take(300).collect()
}

/// Run git; returns stdout. `url` is only used to explain errors.
async fn git(id: &str, args: &[&str], auth: &Auth, url: &str) -> Result<Vec<u8>, String> {
    let base = base_dir();
    std::fs::create_dir_all(&base).map_err(|e| format!("can't create {}: {e}", base.display()))?;
    let mut cmd = tokio::process::Command::new("git");
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        // No ~/.gitconfig or /etc/gitconfig: this repository's settings only.
        .env("HOME", &base)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("LANG", "C")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "protocol.allow=never", "-c", "protocol.https.allow=always", "-c", "protocol.http.allow=always", "-c", "protocol.ssh.allow=always", "-c", "credential.helper="])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if allow_file() {
        cmd.args(["-c", "protocol.file.allow=always"]);
    }
    if let Some(t) = auth.token.as_deref().filter(|t| !t.is_empty()) {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{t}"));
        cmd.env("GIT_CONFIG_COUNT", "1").env("GIT_CONFIG_KEY_0", "http.extraHeader").env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {basic}"));
    }
    let known_hosts = base.join(format!("{id}.known_hosts"));
    let mut _key = None;
    let mut ssh = format!(
        "ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile='{}'",
        known_hosts.display()
    );
    if let Some(k) = auth.private_key.as_deref().filter(|k| !k.is_empty()) {
        let path = base.join(format!("{id}.{}.key", crate::bastion::random_id(6)));
        write_private(&path, k).map_err(|e| format!("can't write the deploy key: {e}"))?;
        ssh.push_str(&format!(" -i '{}'", path.display()));
        _key = Some(KeyFile(path));
    }
    cmd.env("GIT_SSH_COMMAND", ssh);
    cmd.args(args);
    let out = match tokio::time::timeout(LIMIT, cmd.output()).await {
        Err(_) => return Err(format!("git took longer than {}s", LIMIT.as_secs())),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return Err("git is not installed on the timika server".into()),
        Ok(Err(e)) => return Err(format!("git failed to start: {e}")),
        Ok(Ok(o)) => o,
    };
    if !out.status.success() {
        return Err(humanize(&String::from_utf8_lossy(&out.stderr), url));
    }
    Ok(out.stdout)
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    Ok(())
}

fn parse_commits(out: &[u8]) -> Vec<Commit> {
    String::from_utf8_lossy(out)
        .split('\x1e')
        .filter_map(|rec| {
            let mut f = rec.trim_start_matches('\n').split('\x1f');
            let sha = f.next()?.trim().to_string();
            if sha.is_empty() {
                return None;
            }
            Some(Commit { sha, message: f.next().unwrap_or("").to_string(), author: f.next().unwrap_or("").to_string(), time: f.next().unwrap_or("").trim().to_string() })
        })
        .collect()
}

/// The remote's default branch (`main` if it doesn't say).
pub async fn default_branch(id: &str, url: &str, auth: &Auth) -> Result<String, String> {
    let out = git(id, &["ls-remote", "--symref", url, "HEAD"], auth, url).await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/").and_then(|r| r.split('\t').next()).map(String::from))
        .unwrap_or_else(|| "main".into()))
}

pub struct Synced {
    pub head: Commit,
    pub projects: Vec<super::Project>,
}

/// Fetch `branch` (shallow) and detect the projects at its head.
pub async fn sync(id: &str, url: &str, branch: &str, auth: &Auth) -> Result<Synced, String> {
    check_url(url)?;
    check_branch(branch)?;
    let lock = lock(id);
    let _held = lock.lock().await;
    let dir = repo_dir(id);
    let d = dir.to_string_lossy().to_string();
    if !dir.join("HEAD").exists() {
        git(id, &["init", "--bare", "-q", &d], auth, url).await?;
    }
    let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
    git(id, &["-C", &d, "fetch", "--depth=50", "--no-tags", "--update-head-ok", "-q", url, &refspec], auth, url).await?;
    let head = parse_commits(&git(id, &["-C", &d, "log", "-1", FORMAT, &format!("refs/heads/{branch}"), "--"], auth, url).await?)
        .into_iter()
        .next()
        .ok_or("the branch has no commits")?;
    let tree = git(id, &["-C", &d, "ls-tree", "-r", "-z", "--name-only", &head.sha], auth, url).await?;
    let files: Vec<String> = String::from_utf8_lossy(&tree).split('\0').filter(|s| !s.is_empty()).take(50_000).map(String::from).collect();
    // Read the YAML that might be playbooks (detect() decides).
    let mut texts: HashMap<String, String> = HashMap::new();
    for f in files.iter().filter(|f| (f.ends_with(".yml") || f.ends_with(".yaml")) && !f.contains("/roles/") && !f.starts_with("roles/")).take(300) {
        if let Ok(b) = git(id, &["-C", &d, "cat-file", "blob", &format!("{}:{f}", head.sha)], auth, url).await {
            texts.insert(f.clone(), String::from_utf8_lossy(&b[..b.len().min(64 * 1024)]).into_owned());
        }
    }
    let projects = super::detect(&files, |f| texts.get(f).cloned());
    Ok(Synced { head, projects })
}

/// Recent commits on the branch (from the local clone; empty if never synced here).
pub async fn log(id: &str, branch: &str, n: usize) -> Vec<Commit> {
    let dir = repo_dir(id);
    if !dir.join("HEAD").exists() {
        return Vec::new();
    }
    let d = dir.to_string_lossy().to_string();
    git(id, &["-C", &d, "log", &format!("-{n}"), FORMAT, &format!("refs/heads/{branch}"), "--"], &Auth::default(), "")
        .await
        .map(|o| parse_commits(&o))
        .unwrap_or_default()
}

fn full_sha(sha: &str) -> Result<(), String> {
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) { Ok(()) } else { Err("invalid commit".into()) }
}

/// Make sure this instance's clone has commit `sha` (fetching it if not).
pub async fn ensure_commit(id: &str, url: &str, branch: &str, auth: &Auth, sha: &str) -> Result<(), String> {
    full_sha(sha)?;
    let dir = repo_dir(id);
    let d = dir.to_string_lossy().to_string();
    let have = || {
        let d = d.clone();
        async move { git(id, &["-C", &d, "cat-file", "-e", &format!("{sha}^{{commit}}")], &Auth::default(), "").await.is_ok() }
    };
    if !dir.join("HEAD").exists() || !have().await {
        sync(id, url, branch, auth).await?;
        if !have().await {
            let lock = lock(id);
            let _held = lock.lock().await;
            git(id, &["-C", &d, "fetch", "--depth=1", "--no-tags", "-q", url, sha], auth, url).await.map_err(|e| format!("commit {} is not available: {e}", &sha[..7]))?;
        }
    }
    Ok(())
}

/// A .tar.gz of the repository at `sha` (fetched first if this instance doesn't have it).
pub async fn archive(id: &str, url: &str, branch: &str, auth: &Auth, sha: &str) -> Result<Vec<u8>, String> {
    ensure_commit(id, url, branch, auth, sha).await?;
    let d = repo_dir(id).to_string_lossy().to_string();
    git(id, &["-C", &d, "archive", "--format=tar.gz", sha], &Auth::default(), "").await
}

/// A path inside the repository: no `..`, no NUL, no leading `-`; slashes trimmed.
pub fn check_path(p: &str) -> Result<String, String> {
    let p = p.trim().trim_matches('/');
    if p.len() > 1000 || p.contains('\0') || p.starts_with('-') || p.split('/').any(|seg| seg == ".." || seg == "." || seg.is_empty() && !p.is_empty()) {
        return Err("invalid path".into());
    }
    Ok(p.to_string())
}

pub struct TreeEntry {
    pub name: String,
    /// dir · file · link · submodule
    pub kind: String,
    pub size: u64,
}

pub enum Node {
    Dir(Vec<TreeEntry>),
    File,
}

fn not_found(e: String, path: &str) -> String {
    if e.contains("does not exist") || e.contains("Not a valid object") || e.contains("not a tree") {
        format!("`{path}` is not in this commit")
    } else {
        e
    }
}

/// A folder's entries, or "file" when `path` is one. `sha` must already be in the clone.
pub async fn browse(id: &str, sha: &str, path: &str) -> Result<Node, String> {
    full_sha(sha)?;
    let path = check_path(path)?;
    let d = repo_dir(id).to_string_lossy().to_string();
    let spec = format!("{sha}:{path}");
    let kind = git(id, &["-C", &d, "cat-file", "-t", &spec], &Auth::default(), "").await.map_err(|e| not_found(e, &path))?;
    if String::from_utf8_lossy(&kind).trim() != "tree" {
        return Ok(Node::File);
    }
    let out = git(id, &["-C", &d, "ls-tree", "-z", "-l", &spec], &Auth::default(), "").await?;
    let mut entries: Vec<TreeEntry> = String::from_utf8_lossy(&out)
        .split('\0')
        .filter_map(|rec| {
            let (meta, name) = rec.split_once('\t')?;
            let mut f = meta.split_whitespace();
            let (mode, ty, _sha, size) = (f.next()?, f.next()?, f.next()?, f.next()?);
            let kind = match ty {
                "tree" => "dir",
                "commit" => "submodule",
                _ if mode == "120000" => "link",
                _ => "file",
            };
            Some(TreeEntry { name: name.to_string(), kind: kind.into(), size: size.parse().unwrap_or(0) })
        })
        .collect();
    entries.sort_by(|a, b| (a.kind != "dir").cmp(&(b.kind != "dir")).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(Node::Dir(entries))
}

pub struct Blob {
    pub size: u64,
    pub binary: bool,
    pub too_large: bool,
    pub text: String,
}

pub const VIEW_MAX: u64 = 1024 * 1024;

/// A file at `sha`: text up to `VIEW_MAX`, otherwise only its size.
pub async fn read_blob(id: &str, sha: &str, path: &str) -> Result<Blob, String> {
    full_sha(sha)?;
    let path = check_path(path)?;
    if path.is_empty() {
        return Err("choose a file".into());
    }
    let d = repo_dir(id).to_string_lossy().to_string();
    let spec = format!("{sha}:{path}");
    let ty = git(id, &["-C", &d, "cat-file", "-t", &spec], &Auth::default(), "").await.map_err(|e| not_found(e, &path))?;
    if String::from_utf8_lossy(&ty).trim() != "blob" {
        return Err(format!("`{path}` is a folder"));
    }
    let size: u64 = String::from_utf8_lossy(&git(id, &["-C", &d, "cat-file", "-s", &spec], &Auth::default(), "").await?).trim().parse().unwrap_or(0);
    if size > VIEW_MAX {
        return Ok(Blob { size, binary: false, too_large: true, text: String::new() });
    }
    let bytes = git(id, &["-C", &d, "cat-file", "blob", &spec], &Auth::default(), "").await?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Ok(Blob { size, binary: true, too_large: false, text: String::new() });
    }
    Ok(Blob { size, binary: false, too_large: false, text: String::from_utf8_lossy(&bytes).into_owned() })
}

/// Forget the local clone.
pub fn remove(id: &str) {
    let base = base_dir();
    let _ = std::fs::remove_dir_all(repo_dir(id));
    let _ = std::fs::remove_file(base.join(format!("{id}.known_hosts")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in ["https://github.com/acme/infra.git", "git@github.com:acme/infra.git", "ssh://git@gitlab.com/acme/infra", "http://git.internal/x"] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-oProxyCommand=x", "ext::sh -c x", "github.com/acme", "file:///etc", "https://x y", "/tmp/repo", "host:path"] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn branches() {
        for ok in ["main", "release/1.2", "feature_x-y"] {
            assert!(check_branch(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "a..b", "a b", "x.lock", "/a", "a/"] {
            assert!(check_branch(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths() {
        assert_eq!(check_path("/envs/prod/").unwrap(), "envs/prod");
        assert_eq!(check_path("").unwrap(), "");
        for bad in ["../x", "a/../b", "a//b", "-rf", "a\0b", "./a"] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn key_pairs() {
        let (private, public) = new_key_pair("timika").unwrap();
        assert!(private.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(public.starts_with("ssh-ed25519 ") && public.ends_with(" timika"));
        assert!(russh::keys::decode_secret_key(&private, None).is_ok());
    }

    #[test]
    fn errors() {
        assert!(humanize("remote: Repository not found.\nfatal: repository 'https://github.com/a/b/' not found", "https://github.com/a/b").starts_with("access denied"));
        assert_eq!(humanize("fatal: unable to access: Could not resolve host: nope.example", "https://nope.example/x"), "can't reach nope.example (DNS)");
        assert_eq!(humanize("fatal: couldn't find remote ref refs/heads/x", "u"), "that branch doesn't exist in the repository");
        assert_eq!(humanize("fatal: something odd", "u"), "something odd");
    }
}
