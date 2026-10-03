//! nginx on a server or inside a container (docs/NGINX.md): what it serves,
//! its config files with a safe apply, version history, certificates, logs.
//!
//! Everything is a fixed `sh` script; paths and names reach it as quoted
//! arguments, and each script checks on the server that a path is inside
//! nginx's own config folder. A change is only kept when `nginx -t` accepts
//! it — otherwise the script puts the previous state back itself.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bastion::archive::q;
use crate::containers::runtime;
use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::grpc::pb;
use crate::kv;

/// The largest config file that can be edited, and the most history kept per file.
pub const MAX_FILE: usize = 256 * 1024;
const MAX_VERSIONS: usize = 20;
const MAX_HISTORY: usize = 1024 * 1024;

// Finds nginx and its config folder ($C, $D); `inside` refuses any other path.
const PRE: &str = r##"PATH="$PATH:/usr/sbin:/usr/local/sbin:/sbin:/usr/local/nginx/sbin"
command -v nginx >/dev/null 2>&1 || { echo '#@missing'; exit 127; }
V=$(nginx -V 2>&1)
opt() { printf '%s\n' "$V" | tr ' ' '\n' | sed -n "s/^--$1=//p" | head -n 1; }
C=$(opt conf-path); [ -n "$C" ] || C=/etc/nginx/nginx.conf
D=${C%/*}
inside() { case "$1" in "$D"/*) return 0;; esac; exit 5; }
check() { OUT=$(nginx -t 2>&1); RC=$?; if [ $RC = 0 ]; then echo '#@test ok'; else echo '#@test fail'; fi; printf '%s\n' "$OUT"; return $RC; }
reload() { echo '#@reload'; nginx -s reload 2>&1; echo "#@reloaded $?"; }
"##;

const OVERVIEW: &str = r##"echo "#@version $(printf '%s\n' "$V" | sed -n 's/^nginx version: //p' | head -n 1)"
echo "#@conf $C"
echo "#@prefix $(opt prefix)"
P=$(opt pid-path); [ -n "$P" ] || P=/run/nginx.pid
R=0; if [ -s "$P" ] && [ -d "/proc/$(cat "$P" 2>/dev/null)" ]; then R=1; elif pgrep -x nginx >/dev/null 2>&1; then R=1; fi
echo "#@running $R"
echo '#@files'
find "$D" -maxdepth 3 \( -type f -o -type l \) 2>/dev/null | head -n 400 | while IFS= read -r f; do
  if [ -L "$f" ]; then echo "l|$f|$(readlink "$f")"; else echo "f|$f"; fi
done
echo '#@off'
for f in "$D"/sites-available/* "$D"/*.disabled "$D"/conf.d/*.disabled "$D"/http.d/*.disabled; do
  [ -f "$f" ] || continue
  echo "# configuration file $f:"; head -c 200000 "$f"; echo
done
echo '#@dump'
nginx -T 2>&1
echo "#@test $?"
"##;

const READ: &str = r##"F=$1; inside "$F"
[ -f "$F" ] || exit 6
cat "$F"
"##;

// $1 file · $2 create|update · $3 link to make for a new site ('' = none) · stdin: the content
const SAVE: &str = r##"F=$1; L=$3; inside "$F"; [ -z "$L" ] || inside "$L"
T=$(mktemp) && B=$(mktemp) || exit 3
cat > "$T" || { rm -f "$T" "$B"; exit 3; }
HAD=0; [ -e "$F" ] && HAD=1
if [ "$2" = create ] && [ $HAD = 1 ]; then rm -f "$T" "$B"; exit 4; fi
if [ "$2" = update ] && [ $HAD = 0 ]; then rm -f "$T" "$B"; exit 6; fi
[ $HAD = 1 ] && cat "$F" > "$B"
# Written in place: the owner, mode and (bind-mounted) file stay what they were.
cat "$T" > "$F" || { rm -f "$T" "$B"; exit 3; }
rm -f "$T"
LINKED=0; if [ -n "$L" ] && [ ! -e "$L" ]; then ln -s "$F" "$L" && LINKED=1; fi
if ! check; then
  [ $LINKED = 1 ] && rm -f "$L"
  if [ $HAD = 1 ]; then cat "$B" > "$F"; else rm -f "$F"; fi
  rm -f "$B"; exit 0
fi
rm -f "$B"
reload
"##;

// $1 link|rename · $2 on|off · $3 file · $4 the link, or the file's new name
const SWITCH: &str = r##"F=$3; O=$4; inside "$F"; inside "$O"
[ "$F" = "$C" ] && exit 7
case "$1$2" in
  linkon) [ -f "$F" ] || exit 6; [ -e "$O" ] || ln -s "$F" "$O" || exit 3 ;;
  linkoff) [ -e "$O" ] || [ -L "$O" ] || exit 6; [ -L "$O" ] || exit 8; rm -f "$O" || exit 3 ;;
  renameon|renameoff) [ -f "$F" ] || exit 6; [ -e "$O" ] && exit 4; mv "$F" "$O" || exit 3 ;;
  *) exit 2 ;;
esac
if ! check; then
  case "$1$2" in
    linkon) rm -f "$O" ;;
    linkoff) ln -s "$F" "$O" ;;
    *) mv "$O" "$F" ;;
  esac
  exit 0
fi
reload
"##;

// $1 file · $2 its sites-enabled link ('' = none)
const DELETE: &str = r##"F=$1; L=$2; inside "$F"; [ -z "$L" ] || inside "$L"
[ "$F" = "$C" ] && exit 7
[ -f "$F" ] || exit 6
B=$(mktemp) || exit 3
cat "$F" > "$B"
WAS=0; if [ -n "$L" ] && [ -L "$L" ]; then rm -f "$L"; WAS=1; fi
rm -f "$F" || { rm -f "$B"; exit 3; }
if ! check; then
  cat "$B" > "$F"; [ $WAS = 1 ] && ln -s "$F" "$L"
  rm -f "$B"; exit 0
fi
rm -f "$B"
reload
"##;

const RELOAD: &str = "check || exit 0\nreload\n";

// $1 file · $2 lines
const LOG: &str = r##"F=$1; X=$(opt prefix)
case "$F" in /var/log/*|"$D"/*|"${X%/}"/logs/*) ;; *) exit 5;; esac
if [ ! -f "$F" ]; then [ -e "$F" ] && exit 8; exit 4; fi
tail -n "$2" "$F"
"##;

/// Which nginx: on the server itself, or in a container.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub container: String,
    /// docker · podman (only used for a container)
    pub runtime: String,
}

impl Target {
    /// For storage paths: the container's name, or `-` for the server itself.
    pub fn slot(&self) -> &str {
        if self.container.is_empty() { "-" } else { &self.container }
    }

    /// `sh -c <script> sh <args…>` as root on the server, or inside the container.
    fn wrap(&self, body: &str, args: &[&str]) -> String {
        let script = q(&format!("{PRE}{body}"));
        let args: String = args.iter().map(|a| format!(" {}", q(a))).collect();
        if self.container.is_empty() {
            format!("cd -- '.' && S=; [ \"$(id -u)\" = 0 ] || S='sudo -n'; $S sh -c {script} sh{args}")
        } else {
            format!("cd -- '.' && {} exec -i {} sh -c {script} sh{args}", runtime(&self.runtime), q(&self.container))
        }
    }

    pub fn overview_cmd(&self) -> String {
        self.wrap(OVERVIEW, &[])
    }
    pub fn read_cmd(&self, path: &str) -> String {
        self.wrap(READ, &[path])
    }
    pub fn save_cmd(&self, path: &str, create: bool, link: &str) -> String {
        self.wrap(SAVE, &[path, if create { "create" } else { "update" }, link])
    }
    pub fn switch_cmd(&self, s: &Switch) -> String {
        self.wrap(SWITCH, &[if s.link { "link" } else { "rename" }, if s.on { "on" } else { "off" }, &s.file, &s.other])
    }
    pub fn delete_cmd(&self, path: &str) -> String {
        self.wrap(DELETE, &[path, &enabled_link(path).unwrap_or_default()])
    }
    pub fn reload_cmd(&self) -> String {
        self.wrap(RELOAD, &[])
    }
    pub fn log_cmd(&self, path: &str, tail: u32) -> String {
        self.wrap(LOG, &[path, &tail.to_string()])
    }
    /// Certificates are public: read where nginx is, decoded with the server's openssl.
    pub fn certs_cmd(&self, paths: &[String]) -> String {
        let list: String = paths.iter().map(|p| format!(" {}", q(p))).collect();
        let cat = if self.container.is_empty() {
            "S=; [ \"$(id -u)\" = 0 ] || S='sudo -n'; $S cat".to_string()
        } else {
            format!("{} exec {} cat", runtime(&self.runtime), q(&self.container))
        };
        format!("cd -- '.' && for f in{list}; do echo \"#@cert $f\"; ({cat} \"$f\" | openssl x509 -noout -subject -issuer -enddate) 2>&1; done")
    }
}

/// A container that looks like nginx (the image says so).
pub fn is_nginx_image(image: &str) -> bool {
    let i = image.to_lowercase();
    i.contains("nginx") || i.contains("openresty")
}

/// An absolute path with nothing a shell, or `..`, could bend.
pub fn clean_path(p: &str) -> AppResult<String> {
    let ok = p.starts_with('/')
        && p.len() <= 400
        && !p.ends_with('/')
        && !p.split('/').any(|c| c == ".." || c == ".")
        && !p.contains("//")
        && p.chars().all(|c| !c.is_control() && c != '\\');
    if ok { Ok(p.to_string()) } else { Err(AppError::BadRequest("not a valid file path".into())) }
}

/// Certificates and keys live next to the config; they are not config, and a
/// private key is never read into the browser or the history.
pub fn is_key_material(path: &str) -> bool {
    let n = name(path).to_lowercase();
    [".key", ".pem", ".crt", ".cer", ".der", ".p12", ".pfx", ".csr", ".jks"].iter().any(|e| n.ends_with(e))
}

/// File content as nginx should get it: text, unix line ends, a final newline.
pub fn clean_content(c: &str) -> AppResult<String> {
    if c.len() > MAX_FILE {
        return Err(AppError::BadRequest(format!("the file is larger than {} KiB", MAX_FILE / 1024)));
    }
    if c.contains('\0') {
        return Err(AppError::BadRequest("the file must be text".into()));
    }
    let mut c = c.replace("\r\n", "\n");
    if !c.ends_with('\n') {
        c.push('\n');
    }
    Ok(c)
}

pub fn rev(content: &str) -> String {
    hex::encode(&Sha256::digest(content.as_bytes())[..8])
}

fn parent(p: &str) -> &str {
    p.rsplit_once('/').map(|x| x.0).unwrap_or("")
}
fn name(p: &str) -> &str {
    p.rsplit_once('/').map(|x| x.1).unwrap_or(p)
}

/// `…/sites-available/x` → `…/sites-enabled/x`.
pub fn enabled_link(path: &str) -> Option<String> {
    let dir = parent(path);
    (name(dir) == "sites-available").then(|| format!("{}/sites-enabled/{}", parent(dir), name(path)))
}

#[derive(Debug, PartialEq)]
pub struct Switch {
    pub link: bool,
    pub on: bool,
    pub file: String,
    /// The link (sites-enabled), or the file's name afterwards.
    pub other: String,
}

impl Switch {
    /// How a file is switched on or off: Debian's sites-enabled link, or a
    /// `.disabled` suffix anywhere else.
    pub fn new(path: &str, on: bool) -> AppResult<Self> {
        if let Some(link) = enabled_link(path) {
            return Ok(Self { link: true, on, file: path.into(), other: link });
        }
        let other = match (on, path.strip_suffix(".disabled")) {
            (true, Some(base)) => base.to_string(),
            (false, None) => format!("{path}.disabled"),
            (true, None) => return Err(AppError::BadRequest("this file is already on".into())),
            (false, Some(_)) => return Err(AppError::BadRequest("this file is already off".into())),
        };
        Ok(Self { link: false, on, file: path.into(), other })
    }
    /// Where the file is afterwards.
    pub fn path(&self) -> &str {
        if self.link { &self.file } else { &self.other }
    }
}

/// What a script's exit code means (0 = it ran; the test result is in the output).
pub fn expect_ran(code: u32, out: &str, t: &Target) -> AppResult<()> {
    let low = out.to_lowercase();
    Err(match code {
        0 => return Ok(()),
        127 if out.contains("#@missing") => AppError::NotFound(if t.container.is_empty() { "nginx is not installed on this server".into() } else { format!("there is no nginx in the container `{}`", t.container) }),
        _ if low.contains("sudo:") => AppError::Forbidden("nginx's files need root: sign the monitoring account in as root, or give it passwordless sudo".into()),
        _ if !t.container.is_empty() && (low.contains("is not running") || low.contains("no such container") || low.contains("no container with name")) => AppError::BadRequest(format!("the container `{}` is not running", t.container)),
        2 => AppError::BadRequest("unknown action".into()),
        3 => AppError::Unavailable(format!("the file could not be written: {}", last_line(out))),
        4 => AppError::BadRequest("a file with that name already exists".into()),
        5 => AppError::BadRequest("that path is outside nginx's configuration folder".into()),
        6 => AppError::NotFound("that file is not there (any more)".into()),
        7 => AppError::BadRequest("the main configuration file can't be switched off or deleted".into()),
        8 => AppError::BadRequest("this site is a real file in sites-enabled, not a link — edit or delete it instead".into()),
        _ => AppError::Unavailable(last_line(out)),
    })
}

fn last_line(out: &str) -> String {
    out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("#@")).last().unwrap_or("the command failed").chars().take(300).collect()
}

/// The answer of SAVE / SWITCH / DELETE / RELOAD.
pub fn applied(out: &str) -> pb::NginxApply {
    let mut r = pb::NginxApply::default();
    let mut part = "";
    let (mut test, mut reload) = (Vec::new(), Vec::new());
    for l in out.lines() {
        if let Some(t) = l.strip_prefix("#@test ") {
            r.ok = t.trim() == "ok";
            part = "test";
        } else if l.starts_with("#@reload ") || l == "#@reload" {
            part = "reload";
        } else if let Some(c) = l.strip_prefix("#@reloaded ") {
            r.reloaded = c.trim() == "0";
            part = "";
        } else if part == "test" {
            test.push(l);
        } else if part == "reload" {
            reload.push(l);
        }
    }
    r.test_output = test.join("\n").trim().to_string();
    r.reload_output = reload.join("\n").trim().to_string();
    r
}

// ── the configuration ───────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Open,
    Close,
    Semi,
}

/// nginx's own lexing, near enough: words, quotes, comments, `{ } ;`.
fn tokens(src: &str) -> Vec<(Tok, u32)> {
    let mut out = Vec::new();
    let mut line = 1u32;
    let mut it = src.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' => line += 1,
            c if c.is_whitespace() => {}
            '#' => {
                while it.peek().is_some_and(|c| *c != '\n') {
                    it.next();
                }
            }
            '{' => out.push((Tok::Open, line)),
            '}' => out.push((Tok::Close, line)),
            ';' => out.push((Tok::Semi, line)),
            '"' | '\'' => {
                let (start, mut w) = (line, String::new());
                while let Some(d) = it.next() {
                    match d {
                        '\\' => w.extend(it.next()),
                        d if d == c => break,
                        '\n' => { line += 1; w.push(d) }
                        d => w.push(d),
                    }
                }
                out.push((Tok::Word(w), start));
            }
            c => {
                let mut w = String::from(c);
                while let Some(&d) = it.peek() {
                    // "${var}" belongs to the word.
                    if d == '{' && w.ends_with('$') {
                        while let Some(e) = it.next() {
                            w.push(e);
                            if e == '}' { break }
                        }
                        continue;
                    }
                    if d.is_whitespace() || matches!(d, '{' | ';') || (d == '}' && !w.contains("${")) {
                        break;
                    }
                    w.push(d);
                    it.next();
                }
                out.push((Tok::Word(w), line));
            }
        }
    }
    out
}

#[derive(Default)]
struct Parsed {
    sites: Vec<pb::NginxSite>,
    /// upstream name → its servers (host:port)
    upstreams: Vec<(String, Vec<String>)>,
    certs: Vec<String>,
    logs: Vec<String>,
}

/// A path as nginx resolves it; nothing for variables and non-files.
fn resolve(v: &str, base: &str) -> Option<String> {
    if v.is_empty() || v.contains('$') || v == "off" || v.contains(':') && !v.starts_with('/') {
        return None;
    }
    Some(if v.starts_with('/') { v.to_string() } else { format!("{}/{v}", base.trim_end_matches('/')) })
}

fn add(list: &mut Vec<String>, v: String) {
    if !list.contains(&v) && list.len() < 60 {
        list.push(v);
    }
}

/// The `server { … }` blocks of one file.
fn parse_file(file: &str, src: &str, enabled: bool, conf_dir: &str, prefix: &str, out: &mut Parsed) {
    // (block name, index of the site it opened)
    let mut stack: Vec<(String, Option<usize>)> = Vec::new();
    let mut upstream: Option<usize> = None;
    let mut words: Vec<String> = Vec::new();
    let mut first = 0u32;
    let mut redirect: Vec<Option<String>> = Vec::new();
    let base = out.sites.len();
    for (t, line) in tokens(src) {
        match t {
            Tok::Word(w) => {
                if words.is_empty() {
                    first = line;
                }
                words.push(w);
            }
            Tok::Open => {
                let head = words.first().cloned().unwrap_or_default();
                let in_upstream = stack.last().is_some_and(|b| b.0 == "upstream");
                let site = (head == "server" && !in_upstream && out.sites.len() < 300).then(|| {
                    let stream = stack.iter().any(|b| b.0 == "stream");
                    out.sites.push(pb::NginxSite { file: file.into(), line: first, enabled, kind: if stream { "stream".into() } else { String::new() }, ..Default::default() });
                    redirect.push(None);
                    out.sites.len() - 1
                });
                if head == "upstream" && out.upstreams.len() < 200 {
                    out.upstreams.push((words.get(1).cloned().unwrap_or_default(), Vec::new()));
                    upstream = Some(out.upstreams.len() - 1);
                }
                stack.push((head, site));
                words.clear();
            }
            Tok::Close => {
                if stack.pop().is_some_and(|b| b.0 == "upstream") {
                    upstream = None;
                }
                words.clear();
            }
            Tok::Semi => {
                let d = std::mem::take(&mut words);
                let Some((key, args)) = d.split_first() else { continue };
                let arg = args.first().map(String::as_str).unwrap_or_default();
                if let ("server", Some(u)) = (key.as_str(), upstream) {
                    if let Some(b) = backend(arg, 80) {
                        add(&mut out.upstreams[u].1, b);
                    }
                    continue;
                }
                let site = stack.iter().rev().find_map(|b| b.1);
                let direct = stack.last().is_some_and(|b| b.1.is_some());
                match key.as_str() {
                    "access_log" | "error_log" => {
                        if let Some(p) = resolve(arg, prefix).filter(|p| !p.starts_with("/dev/")) {
                            add(&mut out.logs, p.clone());
                            if let Some(i) = site {
                                add(&mut out.sites[i].logs, p);
                            }
                        }
                    }
                    "ssl_certificate" => {
                        if let Some(p) = resolve(arg, conf_dir) {
                            add(&mut out.certs, p.clone());
                            if let Some(i) = site {
                                add(&mut out.sites[i].certificates, p);
                            }
                        }
                    }
                    _ => {}
                }
                let Some(i) = site else { continue };
                let s = &mut out.sites[i];
                match key.as_str() {
                    "listen" if direct => {
                        s.tls |= args.iter().any(|a| a == "ssl" || a == "quic");
                        add(&mut s.listens, args.join(" "));
                    }
                    "server_name" if direct => args.iter().for_each(|a| add(&mut s.names, a.clone())),
                    "proxy_pass" | "fastcgi_pass" | "grpc_pass" | "uwsgi_pass" | "scgi_pass" => {
                        if s.kind.is_empty() {
                            s.kind = "proxy".into();
                        }
                        if s.kind == "proxy" || s.kind == "stream" {
                            add(&mut s.targets, arg.to_string());
                        }
                    }
                    "root" | "alias" if s.kind.is_empty() || s.kind == "static" => {
                        s.kind = "static".into();
                        add(&mut s.targets, arg.to_string());
                    }
                    "return" if direct && matches!(arg, "301" | "302" | "303" | "307" | "308") => redirect[i - base] = args.get(1).cloned(),
                    _ => {}
                }
            }
        }
    }
    for (s, r) in out.sites[base..].iter_mut().zip(redirect) {
        // A proxy with a few static locations is a proxy; a redirect is one only when it is all the site does.
        if s.kind == "static" && s.targets.is_empty() || s.kind.is_empty() {
            s.kind = "other".into();
        }
        if let Some(to) = r.filter(|_| s.kind == "other") {
            s.kind = "redirect".into();
            s.targets = vec![to];
        }
    }
}

/// "http://10.0.0.5:3000/api", "app:8080", "[::1]:9000" → "host:port" (nothing for sockets and variables).
fn backend(target: &str, default_port: u16) -> Option<String> {
    let (scheme, rest) = target.split_once("://").map(|(s, r)| (s, r)).unwrap_or(("", target));
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('$') || authority.starts_with("unix:") {
        return None;
    }
    let port = if matches!(scheme, "https" | "grpcs") { 443 } else { default_port };
    let has_port = authority.rsplit_once(':').is_some_and(|(h, p)| p.parse::<u16>().is_ok() && (!h.contains(':') || h.ends_with(']')));
    Some(if has_port { authority.to_string() } else { format!("{authority}:{port}") })
}

/// "# configuration file /etc/nginx/x.conf:" sections → (path, content).
fn split_dump<'a>(lines: &[&'a str]) -> (Vec<&'a str>, Vec<(String, String)>) {
    let mut before = Vec::new();
    let mut files: Vec<(String, String)> = Vec::new();
    for l in lines {
        if let Some(p) = l.strip_prefix("# configuration file ").and_then(|r| r.strip_suffix(':')) {
            files.push((p.to_string(), String::new()));
        } else if let Some(f) = files.last_mut() {
            f.1.push_str(l);
            f.1.push('\n');
        } else {
            before.push(*l);
        }
    }
    (before, files)
}

/// `a/b/../c` → `a/c` (a link's target, relative to where the link is).
fn join(dir: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').collect();
    for p in target.split('/') {
        match p {
            ".." => { parts.pop(); }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// What OVERVIEW printed → the instance (certificates are filled in afterwards).
pub fn overview(out: &str) -> pb::NginxInstance {
    let mut r = pb::NginxInstance::default();
    let mut prefix = String::new();
    let mut part = "";
    let (mut files, mut off, mut dump) = (Vec::new(), Vec::new(), Vec::new());
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("#@") {
            let (k, v) = rest.split_once(' ').unwrap_or((rest, ""));
            match k {
                "version" => r.version = v.trim().trim_start_matches("nginx/").chars().take(60).collect(),
                "conf" => r.conf_path = v.trim().to_string(),
                "prefix" => prefix = v.trim().to_string(),
                "running" => r.running = v.trim() == "1",
                "test" => r.test_ok = v.trim() == "0",
                "files" | "off" | "dump" => part = k,
                _ => {}
            }
            continue;
        }
        match part {
            "files" => files.push(l),
            "off" => off.push(l),
            "dump" => dump.push(l),
            _ => {}
        }
    }
    let dir = parent(&r.conf_path).to_string();
    // link → the file it points at
    let links: Vec<(String, String)> = files.iter().filter_map(|l| { let mut f = l.splitn(3, '|'); (f.next()? == "l").then_some(())?; let link = f.next()?; Some((link.to_string(), join(parent(link), f.next()?))) }).collect();
    let real = |p: &str| links.iter().find(|l| l.0 == p).map(|l| l.1.clone()).unwrap_or_else(|| p.to_string());

    let (before, loaded) = split_dump(&dump);
    r.test_output = before.join("\n").trim().chars().take(4000).collect();
    let mut parsed = Parsed::default();
    let loaded: Vec<(String, String)> = loaded.into_iter().map(|(p, c)| (real(&p), c)).collect();
    for (p, c) in &loaded {
        parse_file(p, c, true, &dir, &prefix, &mut parsed);
    }
    for (p, c) in split_dump(&off).1 {
        if !loaded.iter().any(|l| l.0 == p) {
            parse_file(&p, &c, false, &dir, &prefix, &mut parsed);
        }
    }
    // Where each proxy leads: an upstream's servers, or the address itself.
    for s in parsed.sites.iter_mut().filter(|s| s.kind == "proxy" || s.kind == "stream") {
        for t in s.targets.clone() {
            let Some(b) = backend(&t, 80) else { continue };
            let name = b.rsplit_once(':').map(|x| x.0).unwrap_or(&b);
            match parsed.upstreams.iter().find(|u| u.0 == name) {
                Some(u) => u.1.iter().for_each(|x| add(&mut s.backends, x.clone())),
                None => add(&mut s.backends, b),
            }
        }
    }
    parsed.sites.sort_by(|a, b| (!a.enabled, a.names.first(), &a.file, a.line).cmp(&(!b.enabled, b.names.first(), &b.file, b.line)));
    r.sites = parsed.sites;
    r.logs = parsed.logs;
    r.certificates = parsed.certs.into_iter().map(|path| pb::NginxCert { used_by: r.sites.iter().filter(|s| s.certificates.contains(&path)).flat_map(|s| s.names.first().cloned()).collect(), path, ..Default::default() }).collect();

    for l in &files {
        let Some(path) = l.strip_prefix("f|").filter(|p| !is_key_material(p)) else { continue };
        let is_loaded = loaded.iter().any(|x| x.0 == path);
        let (switchable, enabled) = if let Some(link) = enabled_link(path) {
            (true, links.iter().any(|x| x.0 == link))
        } else if path.ends_with(".disabled") {
            (true, false)
        } else {
            (is_loaded && path != r.conf_path && path.ends_with(".conf") && matches!(name(parent(path)), "conf.d" | "http.d" | "stream.d"), true)
        };
        r.files.push(pb::NginxConfFile { path: path.to_string(), loaded: is_loaded, switchable, enabled });
    }
    r.files.sort_by(|a, b| (a.path != r.conf_path, &a.path).cmp(&(b.path != r.conf_path, &b.path)));
    let has = |d: &str| files.iter().any(|l| l.split('|').nth(1).is_some_and(|p| p.starts_with(&format!("{dir}/{d}/"))));
    r.sites_dir = format!("{dir}/{}", if has("sites-available") || has("sites-enabled") { "sites-available" } else if has("http.d") && !has("conf.d") { "http.d" } else { "conf.d" });
    r
}

/// Fill in what `certs_cmd` printed.
pub fn certs(out: &str, list: &mut [pb::NginxCert], now: DateTime<Utc>) {
    let mut cur: Option<usize> = None;
    for l in out.lines() {
        if let Some(p) = l.strip_prefix("#@cert ") {
            cur = list.iter().position(|c| c.path == p);
            continue;
        }
        let Some(c) = cur.map(|i| &mut list[i]) else { continue };
        // The readable end of "C = US, O = Let's Encrypt, CN = R11".
        let short = |dn: &str| dn.rsplit([',', '/']).find_map(|p| p.trim().strip_prefix("CN").map(|v| v.trim_start_matches([' ', '=']).to_string())).unwrap_or_else(|| dn.trim().to_string());
        if let Some(v) = l.strip_prefix("subject=") {
            c.subject = short(v);
        } else if let Some(v) = l.strip_prefix("issuer=") {
            c.issuer = short(v);
        } else if let Some(v) = l.strip_prefix("notAfter=") {
            if let Ok(t) = NaiveDateTime::parse_from_str(&v.trim_end_matches("GMT").split_whitespace().collect::<Vec<_>>().join(" "), "%b %d %H:%M:%S %Y") {
                c.not_after = t.and_utc().to_rfc3339();
                c.days_left = Some((t - now.naive_utc()).num_days() as i32);
                c.error.clear();
            }
        } else if c.not_after.is_empty() && c.error.is_empty() && !l.trim().is_empty() {
            let low = l.to_lowercase();
            c.error = if low.contains("no such file") { "the file is not there".into() } else if low.contains("openssl") && low.contains("not found") { "openssl is not installed on the server".into() } else { "could not be read as a certificate".into() };
        }
    }
}

// ── history ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    /// Unix milliseconds.
    pub id: i64,
    pub at: DateTime<Utc>,
    pub by: String,
    pub note: String,
    pub content: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    pub path: String,
    pub versions: Vec<Version>,
}

pub fn history_prefix(asset: &str) -> String {
    format!("nginx/h/{asset}/")
}

pub fn history_path(asset: &str, t: &Target, path: &str) -> String {
    format!("{}{}/{}", history_prefix(asset), t.slot(), hex::encode(&Sha256::digest(path.as_bytes())[..16]))
}

pub async fn history(core: &Core, asset: &str, t: &Target, path: &str) -> AppResult<History> {
    Ok(core.get_json(&history_path(asset, t, path)).await?.unwrap_or_default())
}

/// Add versions (oldest first) to a file's history; content equal to the newest one is skipped.
pub async fn remember(core: &Core, asset: &str, t: &Target, path: &str, add: Vec<(String, String, String)>) -> AppResult<()> {
    let key = history_path(asset, t, path);
    kv::retrying(|| async {
        let (cur, guard) = core.get_json_guarded::<History>(&key).await?;
        let mut h = cur.unwrap_or_default();
        h.path = path.to_string();
        for (content, by, note) in &add {
            if h.versions.last().is_some_and(|v| v.content == *content) {
                continue;
            }
            let now = Utc::now();
            let id = now.timestamp_millis().max(h.versions.last().map(|v| v.id + 1).unwrap_or(0));
            h.versions.push(Version { id, at: now, by: by.clone(), note: note.clone(), content: content.clone() });
        }
        while h.versions.len() > MAX_VERSIONS || (h.versions.len() > 1 && h.versions.iter().map(|v| v.content.len()).sum::<usize>() > MAX_HISTORY) {
            h.versions.remove(0);
        }
        core.commit(vec![(key.clone(), serde_json::to_vec(&h)?)], vec![], vec![guard]).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = r##"#@version nginx/1.27.2
#@conf /etc/nginx/nginx.conf
#@prefix /usr/share/nginx
#@running 1
#@files
f|/etc/nginx/nginx.conf
f|/etc/nginx/mime.types
f|/etc/nginx/sites-available/shop
f|/etc/nginx/sites-available/old
l|/etc/nginx/sites-enabled/shop|../sites-available/shop
f|/etc/nginx/conf.d/api.conf
f|/etc/nginx/conf.d/beta.conf.disabled
#@off
# configuration file /etc/nginx/sites-available/shop:
server { listen 80; server_name shop.example.com; }

# configuration file /etc/nginx/sites-available/old:
server {
    listen 80;
    server_name old.example.com;
    return 301 https://shop.example.com$request_uri;
}

# configuration file /etc/nginx/conf.d/beta.conf.disabled:
server { listen 8080; server_name beta.example.com; root /srv/beta; }

#@dump
nginx: the configuration file /etc/nginx/nginx.conf syntax is ok
nginx: configuration file /etc/nginx/nginx.conf test is successful
# configuration file /etc/nginx/nginx.conf:
user nginx;
error_log /var/log/nginx/error.log notice;
events { worker_connections 1024; }
http {
    include mime.types;
    log_format main '$remote_addr - "$request" {x}';
    access_log /var/log/nginx/access.log main;
    upstream app { server 10.0.0.5:3000; server 10.0.0.6:3000; }
    include /etc/nginx/conf.d/*.conf;
    include /etc/nginx/sites-enabled/*;
}
stream {
    server { listen 5432; proxy_pass 10.0.0.9:5432; }
}
# configuration file /etc/nginx/mime.types:
types { text/html html; }
# configuration file /etc/nginx/conf.d/api.conf:
server {
    listen 443 ssl http2;
    listen [::]:443 ssl;
    server_name api.example.com "www.api.example.com";
    ssl_certificate certs/api.pem;   # relative to the conf folder
    ssl_certificate_key certs/api.key;
    access_log logs/api.log;
    access_log off;
    location / { proxy_pass http://app; }
    location /static/ { root /srv/static; }
    location = /old { return 301 /new; }
}
# configuration file /etc/nginx/sites-enabled/shop:
server {
    listen 80;
    server_name shop.example.com;
    root /srv/shop;
    error_log /dev/stderr;
    set $x "${host}";
}
#@test 0
"##;

    #[test]
    fn sites_files_and_where_new_ones_go() {
        let r = overview(OUT);
        assert_eq!((r.version.as_str(), r.conf_path.as_str(), r.running, r.test_ok), ("1.27.2", "/etc/nginx/nginx.conf", true, true));
        assert!(r.test_output.ends_with("test is successful"), "{}", r.test_output);
        let sites: Vec<_> = r.sites.iter().map(|s| (s.names.join(" "), s.listens.join(" | "), s.tls, s.kind.as_str(), s.targets.join(" "), s.file.as_str(), s.line, s.enabled)).collect();
        assert_eq!(
            sites,
            [
                ("".into(), "5432".into(), false, "stream", "10.0.0.9:5432".into(), "/etc/nginx/nginx.conf", 13, true),
                ("api.example.com www.api.example.com".into(), "443 ssl http2 | [::]:443 ssl".into(), true, "proxy", "http://app".into(), "/etc/nginx/conf.d/api.conf", 1, true),
                // Loaded through its sites-enabled link, edited where the file is.
                ("shop.example.com".into(), "80".into(), false, "static", "/srv/shop".into(), "/etc/nginx/sites-available/shop", 1, true),
                ("beta.example.com".into(), "8080".into(), false, "static", "/srv/beta".into(), "/etc/nginx/conf.d/beta.conf.disabled", 1, false),
                ("old.example.com".into(), "80".into(), false, "redirect", "https://shop.example.com$request_uri".into(), "/etc/nginx/sites-available/old", 1, false),
            ]
        );
        assert_eq!(r.sites[1].certificates, ["/etc/nginx/certs/api.pem"]);
        assert_eq!((r.sites[0].backends.clone(), r.sites[1].backends.clone(), r.sites[2].backends.len()), (vec!["10.0.0.9:5432".to_string()], vec!["10.0.0.5:3000".to_string(), "10.0.0.6:3000".to_string()], 0), "the upstream's servers");
        assert_eq!([backend("https://api.internal/x", 80), backend("http://[::1]:9000", 80), backend("unix:/run/app.sock", 80), backend("http://$up", 80), backend("app", 80)], [Some("api.internal:443".into()), Some("[::1]:9000".into()), None, None, Some("app:80".into())]);
        assert_eq!(r.sites[1].logs, ["/usr/share/nginx/logs/api.log"], "relative to the prefix; `off` is not a file");
        assert_eq!(r.logs, ["/var/log/nginx/error.log", "/var/log/nginx/access.log", "/usr/share/nginx/logs/api.log"], "not /dev/stderr");
        assert_eq!(r.certificates.iter().map(|c| (c.path.as_str(), c.used_by.join(","))).collect::<Vec<_>>(), [("/etc/nginx/certs/api.pem", "api.example.com".to_string())]);
        let files: Vec<_> = r.files.iter().map(|f| (f.path.as_str(), f.loaded, f.switchable, f.enabled)).collect();
        assert_eq!(
            files,
            [
                ("/etc/nginx/nginx.conf", true, false, true),
                ("/etc/nginx/conf.d/api.conf", true, true, true),
                ("/etc/nginx/conf.d/beta.conf.disabled", false, true, false),
                ("/etc/nginx/mime.types", true, false, true),
                ("/etc/nginx/sites-available/old", false, true, false),
                ("/etc/nginx/sites-available/shop", true, true, true),
            ]
        );
        assert_eq!(r.sites_dir, "/etc/nginx/sites-available");
        let rhel = overview("#@conf /etc/nginx/nginx.conf\n#@files\nf|/etc/nginx/nginx.conf\nf|/etc/nginx/conf.d/default.conf\n#@dump\nnginx: [emerg] unexpected \"}\" in /etc/nginx/conf.d/default.conf:3\n#@test 1\n");
        assert_eq!((rhel.sites_dir.as_str(), rhel.test_ok, rhel.test_output.as_str(), rhel.files.len()), ("/etc/nginx/conf.d", false, "nginx: [emerg] unexpected \"}\" in /etc/nginx/conf.d/default.conf:3", 2), "a broken config can still be opened and fixed");
    }

    #[test]
    fn lexing() {
        let t = tokens("a 'b c' \"d\\\"e\"; # x { y\nmap $a $b { ~^/x(.*)$ /y$1; }\nset $v \"${h}\";");
        let words: Vec<String> = t.iter().filter_map(|x| if let Tok::Word(w) = &x.0 { Some(w.clone()) } else { None }).collect();
        assert_eq!(words, ["a", "b c", "d\"e", "map", "$a", "$b", "~^/x(.*)$", "/y$1", "set", "$v", "${h}"]);
        assert_eq!(t.iter().filter(|x| x.0 == Tok::Open).count(), 1, "a brace in a comment is not a block");
        assert_eq!(t.last().unwrap().1, 3, "lines are counted");
    }

    #[test]
    fn paths_and_switches() {
        for bad in ["etc/nginx/x", "/etc/nginx/../shadow", "/etc/nginx/./x", "/etc//x", "/etc/nginx/", "/etc/x\n", "/a\\b", ""] {
            assert!(clean_path(bad).is_err(), "{bad:?}");
        }
        assert!(clean_path("/etc/nginx/conf.d/my site's.conf").is_ok(), "quoted, so spaces and quotes are fine");
        assert_eq!(Switch::new("/etc/nginx/sites-available/shop", false).unwrap(), Switch { link: true, on: false, file: "/etc/nginx/sites-available/shop".into(), other: "/etc/nginx/sites-enabled/shop".into() });
        let off = Switch::new("/etc/nginx/conf.d/api.conf", false).unwrap();
        assert_eq!((off.link, off.path()), (false, "/etc/nginx/conf.d/api.conf.disabled"));
        assert_eq!(Switch::new("/etc/nginx/conf.d/api.conf.disabled", true).unwrap().path(), "/etc/nginx/conf.d/api.conf");
        assert!(Switch::new("/etc/nginx/conf.d/api.conf", true).is_err());
        assert!(Switch::new("/etc/nginx/conf.d/api.conf.disabled", false).is_err());
        assert_eq!(clean_content("a\r\nb").unwrap(), "a\nb\n");
        assert!(clean_content("a\0b").is_err());
        assert!(clean_content(&"x".repeat(MAX_FILE + 1)).is_err());
        assert_eq!(rev("a\n").len(), 16);
        assert!(is_key_material("/etc/nginx/certs/API.key") && is_key_material("/etc/nginx/x.pem") && !is_key_material("/etc/nginx/conf.d/keys.conf"));
    }

    #[test]
    fn commands_quote_everything() {
        let host = Target { container: String::new(), runtime: String::new() };
        let c = host.save_cmd("/etc/nginx/conf.d/it's.conf", true, "");
        assert!(c.starts_with("cd -- '.' && S=; [ \"$(id -u)\" = 0 ] || S='sudo -n'; $S sh -c '"), "{c}");
        assert!(c.ends_with(" sh '/etc/nginx/conf.d/it'\\''s.conf' 'create' ''"), "{c}");
        let ctr = Target { container: "web".into(), runtime: "podman".into() };
        assert!(ctr.reload_cmd().starts_with("cd -- '.' && podman exec -i 'web' sh -c '"));
        assert!(Target { container: "web".into(), runtime: "rm -rf".into() }.reload_cmd().starts_with("cd -- '.' && docker exec -i 'web' "), "the runtime is docker or podman, nothing else");
        assert_eq!(ctr.certs_cmd(&["/etc/ssl/a b.pem".into()]), "cd -- '.' && for f in '/etc/ssl/a b.pem'; do echo \"#@cert $f\"; (podman exec 'web' cat \"$f\" | openssl x509 -noout -subject -issuer -enddate) 2>&1; done");
        assert!(host.delete_cmd("/etc/nginx/sites-available/shop").ends_with(" sh '/etc/nginx/sites-available/shop' '/etc/nginx/sites-enabled/shop'"));
    }

    #[test]
    fn apply_results_and_exit_codes() {
        let r = applied("#@test ok\nnginx: the configuration file /etc/nginx/nginx.conf syntax is ok\nnginx: configuration file /etc/nginx/nginx.conf test is successful\n#@reload\n#@reloaded 0\n");
        assert_eq!((r.ok, r.reloaded, r.reload_output.as_str()), (true, true, ""));
        assert!(r.test_output.ends_with("test is successful"));
        let r = applied("#@test fail\nnginx: [emerg] unknown directive \"servr\" in /etc/nginx/conf.d/api.conf:1\nnginx: configuration file /etc/nginx/nginx.conf test failed\n");
        assert_eq!((r.ok, r.reloaded), (false, false));
        assert!(r.test_output.starts_with("nginx: [emerg] unknown directive"));
        let r = applied("#@test ok\nok\n#@reload\nnginx: [error] invalid PID number \"\" in \"/run/nginx.pid\"\n#@reloaded 1\n");
        assert_eq!((r.ok, r.reloaded, r.reload_output.as_str()), (true, false, "nginx: [error] invalid PID number \"\" in \"/run/nginx.pid\""));

        let host = Target { container: String::new(), runtime: String::new() };
        let ctr = Target { container: "web".into(), runtime: "docker".into() };
        assert!(expect_ran(0, "", &host).is_ok());
        assert!(matches!(expect_ran(127, "#@missing\n", &host), Err(AppError::NotFound(m)) if m == "nginx is not installed on this server"));
        assert!(matches!(expect_ran(1, "sudo: a password is required\n", &host), Err(AppError::Forbidden(_))));
        assert!(matches!(expect_ran(1, "Error response from daemon: container abc is not running\n", &ctr), Err(AppError::BadRequest(m)) if m.contains("not running")));
        assert!(matches!(expect_ran(5, "", &host), Err(AppError::BadRequest(m)) if m.contains("outside")));
        assert!(matches!(expect_ran(4, "", &host), Err(AppError::BadRequest(m)) if m.contains("already exists")));
    }

    #[test]
    fn certificate_dates() {
        let mut list = vec![pb::NginxCert { path: "/a.pem".into(), ..Default::default() }, pb::NginxCert { path: "/b.pem".into(), ..Default::default() }, pb::NginxCert { path: "/c.pem".into(), ..Default::default() }];
        let now = DateTime::parse_from_rfc3339("2026-10-02T00:00:00Z").unwrap().with_timezone(&Utc);
        certs("#@cert /a.pem\nsubject=CN = api.example.com\nissuer=C = US, O = Let's Encrypt, CN = R11\nnotAfter=Oct 12 10:00:00 2026 GMT\n#@cert /b.pem\ncat: /b.pem: No such file or directory\nUnable to load certificate\n#@cert /c.pem\nsubject= /CN=old.example.com\nnotAfter=Sep  1 00:00:00 2026 GMT\n", &mut list, now);
        assert_eq!((list[0].subject.as_str(), list[0].issuer.as_str(), list[0].days_left, list[0].not_after.as_str()), ("api.example.com", "R11", Some(10), "2026-10-12T10:00:00+00:00"));
        assert_eq!((list[1].days_left, list[1].error.as_str()), (None, "the file is not there"));
        assert_eq!((list[2].subject.as_str(), list[2].days_left), ("old.example.com", Some(-31)));
    }
}
