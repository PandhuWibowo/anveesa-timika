//! Manage a login account on a server: create it, set its password, give it
//! sudo and extra groups — by signing in with an account that can administer
//! the server (root, or a user with sudo), possibly the account itself with its
//! current password.
//!
//! - The password never appears on a command line (`ps` would show it): the
//!   script — username and password included — goes over the channel's stdin
//!   to `sh -s`, and `chpasswd` reads `user:password` from a pipe.
//! - Sudo is a drop-in `/etc/sudoers.d/timika-<user>`, checked with
//!   `visudo -c` before it is installed, so a bad rule can never break sudo.
//! - Groups are only joined if they exist; missing ones are reported.

use super::ssh::{self, Connected};
use super::{valid_account, Asset, Secret};

/// Sudo for the account: leave as is, none, with its password, or without.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sudo {
    Keep,
    None,
    Password,
    NoPassword,
}

impl Sudo {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "" | "keep" => Sudo::Keep,
            "none" => Sudo::None,
            "password" => Sudo::Password,
            "nopasswd" => Sudo::NoPassword,
            other => return Err(format!("sudo must be none, password or nopasswd — got `{other}`")),
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Sudo::Keep => "",
            Sudo::None => "no sudo",
            Sudo::Password => "sudo",
            Sudo::NoPassword => "sudo without password",
        }
    }
    pub fn stored(self) -> Option<&'static str> {
        match self {
            Sudo::Keep => None,
            Sudo::None => Some("none"),
            Sudo::Password => Some("password"),
            Sudo::NoPassword => Some("nopasswd"),
        }
    }
}

pub struct Plan<'a> {
    pub username: &'a str,
    /// None: keep the current password (the account must exist).
    pub password: Option<&'a str>,
    pub sudo: Sudo,
    pub groups: &'a [String],
}

/// What was done, for the UI and the audit trail.
#[derive(Debug, Default)]
pub struct Outcome {
    pub created: bool,
    pub password_set: bool,
    pub sudo: Option<&'static str>,
    pub groups_added: Vec<String>,
    pub groups_missing: Vec<String>,
}

impl Outcome {
    pub fn summary(&self, username: &str) -> String {
        let mut parts = vec![if self.created { "created".to_string() } else { "updated".to_string() }];
        if self.password_set && !self.created {
            parts.push("new password".into());
        }
        if let Some(s) = self.sudo {
            parts.push(s.into());
        }
        if !self.groups_added.is_empty() {
            parts.push(format!("groups {}", self.groups_added.join(", ")));
        }
        if !self.groups_missing.is_empty() {
            parts.push(format!("no such group {}", self.groups_missing.join(", ")));
        }
        format!("{username} ({})", parts.join(" · "))
    }
}

/// Single-quote for sh: 'it'\''s'.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn valid_group(g: &str) -> bool {
    !g.is_empty() && g.len() <= 32 && g.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) && !g.starts_with('-')
}

fn script(p: &Plan) -> String {
    // sudoers.d ignores files whose names contain '.', so map those away.
    let sudo_file = format!("/etc/sudoers.d/timika-{}", p.username.replace(['.', '@'], "_"));
    let rule = match p.sudo {
        Sudo::Password => format!("{} ALL=(ALL:ALL) ALL", p.username),
        Sudo::NoPassword => format!("{} ALL=(ALL:ALL) NOPASSWD: ALL", p.username),
        _ => String::new(),
    };
    let mut s = format!("set -e\nU={}\n", quote(p.username));
    // Create the account if needed (only with a password to give it).
    s.push_str("if ! id -u \"$U\" >/dev/null 2>&1; then\n");
    if p.password.is_some() {
        s.push_str(
            "  if command -v useradd >/dev/null 2>&1; then useradd -m -s /bin/bash \"$U\" 2>/dev/null || useradd -m \"$U\"\n  else adduser -D \"$U\"; fi\n  echo created\n",
        );
    } else {
        s.push_str("  echo 'no-such-user'; exit 4\n");
    }
    s.push_str("fi\n");
    if let Some(pw) = p.password {
        s.push_str(&format!("P={}\nprintf '%s:%s\\n' \"$U\" \"$P\" | chpasswd\necho password-set\n", quote(pw)));
    }
    for g in p.groups {
        s.push_str(&format!(
            "if getent group {g} >/dev/null 2>&1; then\n  if command -v usermod >/dev/null 2>&1; then usermod -aG {g} \"$U\"; else addgroup \"$U\" {g}; fi\n  echo 'group-ok {g}'\nelse\n  echo 'group-missing {g}'\nfi\n",
        ));
    }
    match p.sudo {
        Sudo::Keep => {}
        Sudo::None => s.push_str(&format!("rm -f {sudo_file}\necho sudo-ok\n")),
        _ => s.push_str(&format!(
            "command -v visudo >/dev/null 2>&1 || {{ echo 'no-sudo-installed'; exit 5; }}\n\
             [ -d /etc/sudoers.d ] || {{ echo 'no-sudoers-d'; exit 6; }}\n\
             T=$(mktemp)\n\
             printf '%s\\n' {rule} > \"$T\"\n\
             if visudo -cf \"$T\" >/dev/null 2>&1; then chmod 0440 \"$T\"; mv \"$T\" {sudo_file}; echo sudo-ok; else rm -f \"$T\"; echo 'sudo-invalid'; exit 7; fi\n",
            rule = quote(&rule),
        )),
    }
    s.push_str("echo done-ok\n");
    s
}

fn explain(out: &str, status: u32, username: &str, via: &str) -> String {
    if out.contains("no-such-user") {
        return format!("`{username}` doesn't exist on the server — set a password to create it");
    }
    if out.contains("no-sudo-installed") {
        return "sudo isn't installed on the server (install it, or choose “No sudo”)".into();
    }
    if out.contains("no-sudoers-d") {
        return "the server has no /etc/sudoers.d — can't add a sudo rule safely".into();
    }
    if out.contains("sudo-invalid") {
        return format!("the sudo rule for `{username}` didn't pass visudo — nothing was changed");
    }
    let tail: String = out.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").chars().take(200).collect();
    format!("updating `{username}` via `{via}` failed (exit {status}){}", if tail.is_empty() { String::new() } else { format!(": {tail}") })
}

pub async fn provision(asset: &Asset, via: &str, via_secret: &Secret, p: &Plan<'_>) -> Result<(Outcome, Connected), String> {
    if !valid_account(p.username) {
        return Err(format!("invalid account name `{}`", p.username));
    }
    if p.username.contains('@') && matches!(p.sudo, Sudo::Password | Sudo::NoPassword) {
        return Err(format!("`{}`: sudo can't be granted to a name with '@'", p.username));
    }
    if let Some(g) = p.groups.iter().find(|g| !valid_group(g)) {
        return Err(format!("invalid group name `{g}`"));
    }
    let c = ssh::connect(asset, via, via_secret).await.map_err(|e| format!("signing in as `{via}`: {e}"))?;
    let body = script(p);
    let (status, out) = if via == "root" {
        ssh::exec(&c.handle, "sh -s", body.as_bytes()).await?
    } else {
        // Passwordless sudo first; otherwise feed sudo the account's password.
        let (nopw, _) = ssh::exec(&c.handle, "sudo -n true", b"").await?;
        if nopw == 0 {
            ssh::exec(&c.handle, "sudo -n sh -s", body.as_bytes()).await?
        } else if let Some(pw) = via_secret.password.as_deref() {
            let mut input = format!("{pw}\n").into_bytes();
            input.extend_from_slice(body.as_bytes());
            ssh::exec(&c.handle, "sudo -S -p '' sh -s", &input).await?
        } else {
            ssh::disconnect(&c.handle).await;
            return Err(format!("`{via}` needs sudo without a password (or use root) to manage accounts"));
        }
    };
    if status != 0 || !out.contains("done-ok") {
        ssh::disconnect(&c.handle).await;
        return Err(explain(&out, status, p.username, via));
    }
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let outcome = Outcome {
        created: lines.contains(&"created"),
        password_set: lines.contains(&"password-set"),
        sudo: p.sudo.stored().map(|_| p.sudo.label()).filter(|_| lines.contains(&"sudo-ok")),
        groups_added: lines.iter().filter_map(|l| l.strip_prefix("group-ok ")).map(String::from).collect(),
        groups_missing: lines.iter().filter_map(|l| l.strip_prefix("group-missing ")).map(String::from).collect(),
    };
    Ok((outcome, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan<'a>(pw: Option<&'a str>, sudo: Sudo, groups: &'a [String]) -> Plan<'a> {
        Plan { username: "deploy", password: pw, sudo, groups }
    }

    #[test]
    fn quoting_survives_hostile_passwords() {
        assert_eq!(quote("a'b"), r"'a'\''b'");
        let s = script(&plan(Some("x'; rm -rf / #"), Sudo::Keep, &[]));
        assert!(s.contains(r"P='x'\''; rm -rf / #'"));
    }

    #[test]
    fn sudo_rules_are_checked_with_visudo() {
        let s = script(&plan(None, Sudo::NoPassword, &[]));
        assert!(s.contains("'deploy ALL=(ALL:ALL) NOPASSWD: ALL'"));
        assert!(s.contains("visudo -cf"));
        assert!(s.contains("/etc/sudoers.d/timika-deploy"));
        // Without a password nothing is created: the account must exist.
        assert!(s.contains("no-such-user") && !s.contains("useradd"));
        let none = script(&plan(None, Sudo::None, &[]));
        assert!(none.contains("rm -f /etc/sudoers.d/timika-deploy"));
    }

    #[test]
    fn dotted_names_get_a_sudoers_file_sudo_reads() {
        let p = Plan { username: "jane.doe", password: None, sudo: Sudo::Password, groups: &[] };
        assert!(script(&p).contains("/etc/sudoers.d/timika-jane_doe"));
    }

    #[test]
    fn group_names_are_validated() {
        assert!(valid_group("docker") && valid_group("systemd-journal") && valid_group("www_data"));
        assert!(!valid_group("a b") && !valid_group("x;rm") && !valid_group("-g") && !valid_group(""));
    }

    #[test]
    fn summaries() {
        let o = Outcome { created: true, password_set: true, sudo: Some("sudo without password"), groups_added: vec!["docker".into()], groups_missing: vec!["nope".into()] };
        assert_eq!(o.summary("deploy"), "deploy (created · sudo without password · groups docker · no such group nope)");
    }
}
