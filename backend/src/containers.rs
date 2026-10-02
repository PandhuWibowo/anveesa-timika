//! Containers on a monitored server, driven with the `docker` or `podman` CLI
//! (they take the same commands) over SSH as the server's monitoring account:
//! container details and files, images, volumes. Which one a container
//! belongs to comes from the monitoring reading (`Container.runtime`).
//!
//! Every value that reaches a shell is one single-quoted word: container
//! names must match Docker's own rule (`valid_container`), paths go through
//! `q()` and are passed to `sh -c` as `$1`, never spliced into a script.
//! Commands start with `cd -- '.' &&` (harmless; the test SSH target runs
//! only such commands for real).

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::automation::q;
use crate::error::{AppError, AppResult};
use crate::monitor::collect::valid_container;

const CD: &str = "cd -- '.' && ";

/// An absolute, normalized path inside a container.
pub fn clean_path(p: &str) -> AppResult<String> {
    let p = p.trim();
    let p = if p.is_empty() { "/" } else { p };
    if !p.starts_with('/') || p.len() > 4096 || p.contains('\0') || p.contains('\n') {
        return Err(AppError::BadRequest("a path inside the container starts with /".into()));
    }
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
    Ok(format!("/{}", parts.join("/")))
}

/// docker · podman — the only two words that may start a command here.
pub fn runtime(rt: &str) -> &'static str {
    if rt == "podman" { "podman" } else { "docker" }
}

pub fn check_name(name: &str) -> AppResult<()> {
    if valid_container(name) { Ok(()) } else { Err(AppError::BadRequest("invalid container name".into())) }
}

/// Image references and volume names: what Docker allows, nothing a shell reads.
pub fn check_ref(r: &str) -> AppResult<()> {
    let ok = !r.is_empty() && r.len() <= 300 && !r.starts_with('-') && r.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/' | ':' | '@'));
    if ok { Ok(()) } else { Err(AppError::BadRequest("invalid image or volume name".into())) }
}

// ── commands ────────────────────────────────────────────────────────────────

/// The interactive shell of the web terminal: bash if the image has it, else sh.
pub fn shell_cmd(rt: &str, name: &str) -> String {
    format!("{} exec -it {} sh -c 'if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi'", runtime(rt), q(name))
}

pub fn inspect_cmd(rt: &str, name: &str) -> String {
    format!("{CD}{} inspect {}", runtime(rt), q(name))
}

/// One line per entry: `d|type|size|mtime|mode|name` — `d` is 1 for a folder
/// or a link to one (`/bin` → `usr/bin`), the rest is `stat` (GNU and busybox
/// both have it). `!nodir` when the path isn't a folder.
pub fn list_cmd(rt: &str, name: &str, path: &str) -> String {
    let script = r#"cd -- "$1" 2>/dev/null || { echo "!nodir"; exit 0; }; for f in .[!.]* ..?* *; do [ -e "$f" ] || [ -L "$f" ] || continue; if [ -d "$f" ]; then d=1; else d=0; fi; stat -c "$d|%F|%s|%Y|%a|%n" -- "$f" 2>/dev/null; done"#;
    format!("{CD}{} exec {} sh -c {} sh {} 2>&1", runtime(rt), q(name), q(script), q(path))
}

pub fn mkdir_cmd(rt: &str, name: &str, path: &str) -> String {
    format!("{CD}{} exec {} mkdir -p -- {} 2>&1", runtime(rt), q(name), q(path))
}

pub fn delete_cmd(rt: &str, name: &str, paths: &[String]) -> String {
    format!("{CD}{} exec {} rm -rf -- {} 2>&1", runtime(rt), q(name), paths.iter().map(|p| q(p)).collect::<Vec<_>>().join(" "))
}

/// A file's bytes on stdout.
pub fn cat_cmd(rt: &str, name: &str, path: &str) -> String {
    format!("{CD}{} exec {} cat -- {}", runtime(rt), q(name), q(path))
}

/// A folder as a tar stream on stdout (works without a shell in the image).
pub fn tar_cmd(rt: &str, name: &str, path: &str) -> String {
    format!("{CD}{} cp {}:{} -", runtime(rt), q(name), q(path))
}

/// stdin → a file in the container (its folder is created).
pub fn upload_cmd(rt: &str, name: &str, path: &str) -> String {
    let script = r#"mkdir -p -- "$(dirname -- "$1")" && cat > "$1""#;
    format!("{CD}{} exec -i {} sh -c {} sh {}", runtime(rt), q(name), q(script), q(path))
}

pub fn kind_cmd(rt: &str, name: &str, path: &str) -> String {
    let script = r#"if [ -d "$1" ]; then echo dir; elif [ -e "$1" ]; then echo file; else echo none; fi"#;
    format!("{CD}{} exec {} sh -c {} sh {} 2>&1", runtime(rt), q(name), q(script), q(path))
}

pub fn images_cmd(rt: &str) -> String {
    let d = runtime(rt);
    format!("{CD}{d} images --no-trunc --format 'img={{{{.Repository}}}}|{{{{.Tag}}}}|{{{{.ID}}}}|{{{{.Size}}}}|{{{{.CreatedSince}}}}' && {d} ps -a --no-trunc --format 'use={{{{.Names}}}}|{{{{.Image}}}}'")
}

pub fn volumes_cmd(rt: &str) -> String {
    let d = runtime(rt);
    format!(
        "{CD}{d} volume ls --format 'vol={{{{.Name}}}}|{{{{.Driver}}}}|{{{{.Mountpoint}}}}' && {{ ids=$({d} ps -aq); if [ -n \"$ids\" ]; then {d} inspect --format 'use={{{{.Name}}}}|{{{{range .Mounts}}}}{{{{if eq .Type \"volume\"}}}}{{{{.Name}}}},{{{{end}}}}{{{{end}}}}' $ids; fi; }} && {{ {d} system df -v --format '{{{{range .Volumes}}}}siz={{{{.Name}}}}|{{{{.Size}}}}{{{{\"\\n\"}}}}{{{{end}}}}' 2>/dev/null || true; }}"
    )
}

pub fn rmi_cmd(rt: &str, image: &str) -> String {
    format!("{CD}{} rmi {} 2>&1", runtime(rt), q(image))
}
pub fn image_prune_cmd(rt: &str) -> String {
    format!("{CD}{} image prune -f 2>&1", runtime(rt))
}
pub fn volume_rm_cmd(rt: &str, name: &str) -> String {
    format!("{CD}{} volume rm {} 2>&1", runtime(rt), q(name))
}
pub fn volume_prune_cmd(rt: &str) -> String {
    format!("{CD}{} volume prune -f 2>&1", runtime(rt))
}
pub fn logs_cmd(rt: &str, name: &str, tail: u32) -> String {
    format!("{CD}{} logs --tail {tail} --timestamps {} 2>&1", runtime(rt), q(name))
}
pub fn action_cmd(rt: &str, name: &str, action: &str) -> String {
    format!("{CD}{} {action} {} 2>&1", runtime(rt), q(name))
}

// ── parsers ─────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Default)]
pub struct FileEntry {
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub modified: i64,
    pub mode: String,
}

fn rwx(octal: &str) -> String {
    let n = u32::from_str_radix(octal, 8).unwrap_or(0);
    let mut s = String::new();
    for shift in [6, 3, 0] {
        let b = (n >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// The runtime's own complaints, said plainly.
fn explain(out: &str) -> String {
    let o = out.to_lowercase();
    if o.contains("is not running") {
        "the container is not running — start it first".into()
    } else if o.contains("no such container") || o.contains("no container with name or id") {
        "that container no longer exists".into()
    } else if o.contains("executable file not found") || o.contains("no such file or directory: unknown") {
        "this image has no shell or basic tools (sh, stat) — files can't be listed; a folder can still be downloaded by its path".into()
    } else if o.contains("permission denied") && (o.contains("docker") || o.contains("podman")) {
        "the monitoring account may not run docker / podman on this server".into()
    } else {
        out.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("the container runtime failed").chars().take(300).collect()
    }
}

pub fn parse_list(code: u32, out: &str) -> AppResult<Vec<FileEntry>> {
    if out.trim() == "!nodir" {
        return Err(AppError::NotFound("that folder (inside the container)".into()));
    }
    if code != 0 {
        return Err(AppError::BadRequest(explain(out)));
    }
    let mut entries: Vec<FileEntry> = out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(6, '|').collect();
            if f.len() != 6 {
                return None;
            }
            let t = f[1].to_lowercase();
            // A link to a folder opens like a folder.
            let kind = if f[0] == "1" || t.contains("directory") { "dir" } else if t.contains("link") { "link" } else if t.contains("regular") { "file" } else { "other" };
            Some(FileEntry { name: f[5].to_string(), kind: kind.into(), size: f[2].parse().unwrap_or(0), modified: f[3].parse().unwrap_or(0), mode: rwx(f[4]) })
        })
        .take(5000)
        .collect();
    entries.sort_by(|a, b| (a.kind != "dir").cmp(&(b.kind != "dir")).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

/// A command that should just work: its output is the error otherwise.
pub fn expect_ok(code: u32, out: &str) -> AppResult<()> {
    if code == 0 { Ok(()) } else { Err(AppError::BadRequest(explain(out))) }
}

/// "187MB", "1.2GB", "0B", podman's "187 MB" (decimal units) → bytes.
pub fn size_bytes(s: &str) -> u64 {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let n: f64 = s[..split].trim().parse().unwrap_or(0.0);
    let mult = match s[split..].trim().to_ascii_uppercase().as_str() {
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KIB" => 1024.0,
        "MIB" => 1024.0 * 1024.0,
        "GIB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (n * mult) as u64
}

#[derive(Debug, PartialEq, Default)]
pub struct Image {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: u64,
    pub created: String,
    pub used_by: Vec<String>,
    pub dangling: bool,
}

pub fn parse_images(out: &str) -> Vec<Image> {
    let uses: Vec<(&str, &str)> = out.lines().filter_map(|l| l.strip_prefix("use=")).filter_map(|l| l.split_once('|')).collect();
    let mut images: Vec<Image> = out
        .lines()
        .filter_map(|l| l.strip_prefix("img="))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').collect();
            if f.len() != 5 {
                return None;
            }
            let full_id = f[2];
            let id: String = full_id.trim_start_matches("sha256:").chars().take(12).collect();
            let dangling = f[0] == "<none>";
            let name = format!("{}:{}", f[0], f[1]);
            let used_by = uses
                .iter()
                .filter(|(_, img)| *img == name || (f[1] == "latest" && *img == f[0]) || *img == full_id || (!id.is_empty() && img.trim_start_matches("sha256:").starts_with(&id)))
                .map(|(c, _)| c.to_string())
                .collect();
            Some(Image { id, repository: f[0].into(), tag: f[1].into(), size: size_bytes(f[3]), created: f[4].into(), used_by, dangling })
        })
        .collect();
    images.sort_by(|a, b| a.dangling.cmp(&b.dangling).then(a.repository.cmp(&b.repository)).then(a.tag.cmp(&b.tag)));
    images
}

#[derive(Debug, PartialEq, Default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub size: Option<u64>,
    pub used_by: Vec<String>,
}

pub fn parse_volumes(out: &str) -> Vec<Volume> {
    let mut vols: Vec<Volume> = out
        .lines()
        .filter_map(|l| l.strip_prefix("vol="))
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(3, '|').collect();
            (f.len() == 3).then(|| Volume { name: f[0].into(), driver: f[1].into(), mountpoint: f[2].into(), ..Default::default() })
        })
        .collect();
    for l in out.lines() {
        if let Some((container, names)) = l.strip_prefix("use=").and_then(|x| x.split_once('|')) {
            for n in names.split(',').filter(|n| !n.is_empty()) {
                if let Some(v) = vols.iter_mut().find(|v| v.name == n) {
                    v.used_by.push(container.trim_start_matches('/').to_string());
                }
            }
        }
        if let Some((name, size)) = l.strip_prefix("siz=").and_then(|x| x.split_once('|')) {
            if let Some(v) = vols.iter_mut().find(|v| v.name == name) {
                v.size = Some(size_bytes(size));
            }
        }
    }
    vols.sort_by(|a, b| a.name.cmp(&b.name));
    vols
}

// ── download links ──────────────────────────────────────────────────────────

/// What a download link allows: one path in one container, for one person, for a minute.
#[derive(Serialize, Deserialize)]
pub struct Ticket {
    pub asset: String,
    pub container: String,
    pub path: String,
    /// docker · podman
    #[serde(default)]
    pub runtime: String,
    /// file · dir
    pub kind: String,
    pub who: String,
    pub username: Option<String>,
    pub policies: Vec<String>,
    pub exp: i64,
}

pub fn sign(key: &[u8; 32], t: &Ticket) -> String {
    let body = B64URL.encode(serde_json::to_vec(t).unwrap_or_default());
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    m.update(body.as_bytes());
    format!("{body}.{}", B64URL.encode(m.finalize().into_bytes()))
}

pub fn verify(key: &[u8; 32], ticket: &str, now: i64) -> AppResult<Ticket> {
    let bad = || AppError::Auth("this download link is invalid or has expired".into());
    let (body, sig) = ticket.split_once('.').ok_or_else(bad)?;
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    m.update(body.as_bytes());
    m.verify_slice(&B64URL.decode(sig).map_err(|_| bad())?).map_err(|_| bad())?;
    let t: Ticket = serde_json::from_slice(&B64URL.decode(body).map_err(|_| bad())?).map_err(|_| bad())?;
    if t.exp < now {
        return Err(bad());
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(clean_path("").unwrap(), "/");
        assert_eq!(clean_path("/usr/share/../share/nginx/./html/").unwrap(), "/usr/share/nginx/html");
        assert_eq!(clean_path("/../../etc").unwrap(), "/etc");
        assert!(clean_path("relative").is_err());
        assert!(clean_path("/a\nb").is_err());
        assert!(check_name("web-1.app_2").is_ok());
        assert!(check_name("web; rm -rf /").is_err());
        assert!(check_ref("ghcr.io/acme/app:1.8@sha256:abc").is_ok());
        assert!(check_ref("nginx; id").is_err());
        assert!(check_ref("-f").is_err());
    }

    #[test]
    fn commands_quote_everything() {
        let evil = "/tmp/it's $(id) `x`";
        let c = list_cmd("docker", "web", evil);
        assert!(c.starts_with("cd -- '.' && docker exec 'web' sh -c '"), "{c}");
        assert!(c.contains(r"sh '/tmp/it'\''s $(id) `x`' 2>&1"), "the path is one quoted word: {c}");
        assert!(delete_cmd("docker", "web", &["/a b".into(), "/c".into()]).ends_with("rm -rf -- '/a b' '/c' 2>&1"));
        assert_eq!(tar_cmd("podman", "web", "/var/log"), "cd -- '.' && podman cp 'web':'/var/log' -");
        assert!(list_cmd("podman", "web", "/").starts_with("cd -- '.' && podman exec 'web' sh -c '"));
        assert_eq!(runtime("podman; rm -rf /"), "docker", "anything else is docker — never text from outside");
        assert!(images_cmd("podman").contains("podman images --no-trunc --format 'img={{.Repository}}|{{.Tag}}|{{.ID}}|{{.Size}}|{{.CreatedSince}}' && podman ps -a"));
        assert!(volumes_cmd("docker").contains("docker inspect --format 'use={{.Name}}|{{range .Mounts}}{{if eq .Type \"volume\"}}{{.Name}},{{end}}{{end}}' $ids"), "{}", volumes_cmd("docker"));
        assert_eq!(logs_cmd("podman", "web", 50), "cd -- '.' && podman logs --tail 50 --timestamps 'web' 2>&1");
        assert!(upload_cmd("docker", "web", "/app/x").contains("docker exec -i 'web' sh -c '") && upload_cmd("docker", "web", "/app/x").ends_with("sh '/app/x'"));
        assert!(shell_cmd("docker", "web").starts_with("docker exec -it 'web' sh -c '"));
        assert!(shell_cmd("podman", "web").starts_with("podman exec -it 'web' sh -c '"));
    }

    #[test]
    fn listings() {
        let out = "1|directory|4096|1790000000|755|html\n0|regular file|615|1790000100|644|index.html\n0|symbolic link|7|1790000200|777|current\n0|regular empty file|0|1|600|.env\n0|regular file|3|2|644|odd|name\n1|symbolic link|7|3|777|bin\n";
        let e = parse_list(0, out).unwrap();
        assert_eq!(e.iter().map(|x| (x.name.as_str(), x.kind.as_str())).collect::<Vec<_>>(), [("bin", "dir"), ("html", "dir"), (".env", "file"), ("current", "link"), ("index.html", "file"), ("odd|name", "file")]);
        assert_eq!((e[4].size, e[4].modified, e[4].mode.as_str()), (615, 1790000100, "rw-r--r--"));
        // BSD stat (macOS test hosts) capitalizes the type.
        assert_eq!(parse_list(0, "1|Directory|64|1|755|a\n0|Regular File|1|1|644|b\n").unwrap().iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(), ["dir", "file"]);
        assert!(matches!(parse_list(0, "!nodir\n"), Err(AppError::NotFound(_))));
        let no_sh = parse_list(127, "OCI runtime exec failed: exec: \"sh\": executable file not found in $PATH: unknown").unwrap_err().to_string();
        assert!(no_sh.contains("has no shell"), "{no_sh}");
        assert!(parse_list(1, "Error response from daemon: container abc is not running").unwrap_err().to_string().contains("not running"));
        assert!(parse_list(0, "").unwrap().is_empty(), "an empty folder");
    }

    #[test]
    fn images_and_volumes() {
        let out = "img=nginx|latest|sha256:aaaaaaaaaaaa1111|187MB|3 weeks ago\nimg=acme/app|1.8|sha256:bbbbbbbbbbbb2222|1.2GB|2 days ago\nimg=<none>|<none>|sha256:cccccccccccc3333|64.5 MB|5 days ago\nuse=web|nginx\nuse=api|acme/app:1.8\nuse=old|sha256:cccccccccccc3333\n";
        let i = parse_images(out);
        assert_eq!(i.iter().map(|x| (x.repository.as_str(), x.id.as_str(), x.size, x.used_by.clone(), x.dangling)).collect::<Vec<_>>(), [
            ("acme/app", "bbbbbbbbbbbb", 1_200_000_000, vec!["api".to_string()], false),
            ("nginx", "aaaaaaaaaaaa", 187_000_000, vec!["web".to_string()], false),
            ("<none>", "cccccccccccc", 64_500_000, vec!["old".to_string()], true),
        ]);
        let out = "vol=pgdata|local|/var/lib/docker/volumes/pgdata/_data\nvol=cache|local|/var/lib/docker/volumes/cache/_data\nuse=/db|pgdata,\nuse=/web|\nsiz=pgdata|1.5GB\n";
        let v = parse_volumes(out);
        assert_eq!(v.iter().map(|x| (x.name.as_str(), x.size, x.used_by.clone())).collect::<Vec<_>>(), [("cache", None, vec![]), ("pgdata", Some(1_500_000_000), vec!["db".to_string()])]);
    }

    #[test]
    fn tickets() {
        let key = [3u8; 32];
        let t = Ticket { asset: "a".into(), container: "web".into(), path: "/etc/nginx".into(), runtime: "docker".into(), kind: "dir".into(), who: "jane".into(), username: None, policies: vec![], exp: 1000 };
        let s = sign(&key, &t);
        assert_eq!(verify(&key, &s, 999).unwrap().path, "/etc/nginx");
        assert!(verify(&key, &s, 1001).is_err(), "expired");
        assert!(verify(&[4u8; 32], &s, 999).is_err(), "another key");
        assert!(verify(&key, &s.replace('.', "x."), 999).is_err(), "tampered");
    }
}
