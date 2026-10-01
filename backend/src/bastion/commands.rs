//! Command log: the commands a user ran in a web-terminal session.
//!
//! A command is taken when the server answers Enter with a line break — from
//! what the **screen** shows on that line at that moment (fast typing and
//! pastes are echoed before the line break, so this is never too early) — so tab completion and ↑ history recall come out right —
//! and only if it was visible: typed text the server did not echo (a password
//! at a `sudo` prompt) is never recorded. Nothing here is not already in the
//! session's recording; this just makes it searchable.
//!
//! Full-screen programs (vim, less, top — the alternate screen) are skipped.
//! Commands are buffered and written through the barrier in chunks, like the
//! recording: `bastion/cmds/<sid>/<seq>` = JSON array.

use std::collections::VecDeque;
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::core::Core;
use crate::error::AppResult;

const MAX_COMMANDS: u32 = 20_000;
const MAX_LEN: usize = 2_000;

pub fn cmd_prefix(sid: &str) -> String {
    format!("bastion/cmds/{sid}/")
}

#[derive(Clone, Copy, PartialEq)]
enum Esc {
    None,
    Esc,
    Csi,
    Osc { saw_esc: bool },
    Charset,
}

#[derive(Clone, Copy, PartialEq)]
enum InEsc {
    None,
    Esc,
    Seq,
}

pub struct CommandLog {
    sid: String,
    start: Instant,
    started_at: DateTime<Utc>,
    // The terminal's current line, as the screen shows it.
    line: Vec<char>,
    cursor: usize,
    esc: Esc,
    csi: String,
    alt_screen: bool,
    carry: Vec<u8>,
    // What was typed since the last Enter.
    input: String,
    /// Typed with plain keys only (no tab, arrows, ctrl-keys): `input` is exact.
    plain: bool,
    in_esc: InEsc,
    /// Enters pressed whose line break the server hasn't echoed yet: (typed, plain, at).
    entered: VecDeque<(String, bool, std::time::Duration)>,
    pending: Vec<Value>,
    seq: u32,
    pub count: u32,
}

/// How dangerous a command looks: "high", "medium" or none.
pub fn risk(cmd: &str) -> Option<&'static str> {
    let c = cmd.to_lowercase();
    let words: Vec<&str> = c.split_whitespace().map(|w| w.trim_start_matches("sudo")).collect();
    let has = |w: &str| words.iter().any(|x| *x == w);
    let first = words.iter().find(|w| !w.is_empty() && **w != "sudo").copied().unwrap_or("");
    let high = (first == "rm" && (c.contains(" -rf") || c.contains(" -fr") || c.contains(" -r ") && c.contains(" -f")))
        || matches!(first, "mkfs" | "shutdown" | "reboot" | "halt" | "poweroff" | "userdel" | "visudo" | "wipefs" | "fdisk" | "parted")
        || first.starts_with("mkfs.")
        || (first == "dd" && c.contains("of=/dev/"))
        || c.contains(":(){")
        || (first == "init" && (has("0") || has("6")))
        || (first == "chmod" && c.contains("-r") && c.contains("777"))
        || (first == "iptables" && (has("-f") || has("--flush")))
        || (first == "crontab" && has("-r"))
        || c.contains("> /dev/sd")
        || ((c.contains("curl ") || c.contains("wget ")) && (c.contains("| sh") || c.contains("| bash") || c.contains("|sh") || c.contains("|bash")));
    if high {
        return Some("high");
    }
    let medium = c.starts_with("sudo") || c.starts_with("su ") || c == "su"
        || matches!(first, "rm" | "kill" | "killall" | "pkill" | "passwd" | "useradd" | "usermod" | "chown" | "chmod" | "mount" | "umount")
        || (first == "systemctl" && words.iter().any(|w| matches!(*w, "stop" | "restart" | "disable" | "mask" | "kill")))
        || ((first == "apt" || first == "apt-get" || first == "yum" || first == "dnf") && words.iter().any(|w| matches!(*w, "remove" | "purge" | "erase" | "autoremove")))
        || ((first == "docker" || first == "kubectl") && words.iter().any(|w| matches!(*w, "rm" | "rmi" | "delete" | "kill" | "prune")))
        || (first == "git" && (has("push") && (has("-f") || has("--force"))));
    medium.then_some("medium")
}

/// For a line edited with completion/history: drop the shell prompt.
fn strip_prompt(screen: &str) -> &str {
    let at = ["$ ", "# ", "> ", "% "].iter().filter_map(|p| screen.find(p).map(|i| i + p.len())).min();
    match at {
        Some(i) => screen[i..].trim(),
        None => screen.trim(),
    }
}

impl CommandLog {
    pub fn new(sid: &str, started_at: DateTime<Utc>) -> Self {
        Self {
            sid: sid.into(),
            start: Instant::now(),
            started_at,
            line: Vec::new(),
            cursor: 0,
            esc: Esc::None,
            csi: String::new(),
            alt_screen: false,
            carry: Vec::new(),
            input: String::new(),
            plain: true,
            in_esc: InEsc::None,
            entered: VecDeque::new(),
            pending: Vec::new(),
            seq: 0,
            count: 0,
        }
    }

    // ── what the server printed ──

    pub fn output(&mut self, data: &[u8]) {
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(data);
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(e) if e.error_len().is_none() => {
                self.carry = bytes[e.valid_up_to()..].to_vec();
                String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
            }
            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
        };
        for c in text.chars() {
            self.out_char(c);
        }
    }

    fn out_char(&mut self, c: char) {
        match self.esc {
            Esc::None => match c {
                '\x1b' => self.esc = Esc::Esc,
                '\r' => self.cursor = 0,
                '\n' => {
                    self.line_done();
                    self.line.clear();
                    self.cursor = 0;
                }
                '\x08' => self.cursor = self.cursor.saturating_sub(1),
                c if (c as u32) < 0x20 || c == '\x7f' => {}
                c => self.put(c),
            },
            Esc::Esc => {
                self.esc = match c {
                    '[' => {
                        self.csi.clear();
                        Esc::Csi
                    }
                    ']' => Esc::Osc { saw_esc: false },
                    '(' | ')' => Esc::Charset,
                    _ => Esc::None,
                }
            }
            Esc::Charset => self.esc = Esc::None,
            Esc::Osc { saw_esc } => {
                self.esc = match c {
                    '\x07' => Esc::None,
                    '\\' if saw_esc => Esc::None,
                    '\x1b' => Esc::Osc { saw_esc: true },
                    _ => Esc::Osc { saw_esc: false },
                }
            }
            Esc::Csi => {
                if ('\x40'..='\x7e').contains(&c) {
                    let params = std::mem::take(&mut self.csi);
                    self.csi_apply(c, &params);
                    self.esc = Esc::None;
                } else if self.csi.len() < 32 {
                    self.csi.push(c);
                }
            }
        }
    }

    fn csi_apply(&mut self, f: char, params: &str) {
        if let Some(mode) = params.strip_prefix('?') {
            if matches!(mode, "1049" | "1047" | "47") {
                match f {
                    'h' => self.alt_screen = true,
                    'l' => {
                        self.alt_screen = false;
                        self.entered.clear();
                        self.line.clear();
                        self.cursor = 0;
                    }
                    _ => {}
                }
            }
            return;
        }
        let nums: Vec<usize> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let n = nums.first().copied().unwrap_or(0).max(1);
        match f {
            'K' => match nums.first().copied().unwrap_or(0) {
                0 => self.line.truncate(self.cursor),
                1 => (0..=self.cursor.min(self.line.len().saturating_sub(1))).for_each(|i| self.line[i] = ' '),
                _ => self.line.clear(),
            },
            'C' => self.cursor += n,
            'D' => self.cursor = self.cursor.saturating_sub(n),
            'G' => self.cursor = n - 1,
            'H' | 'f' => self.cursor = nums.get(1).copied().unwrap_or(1).max(1) - 1,
            'P' => {
                if self.cursor < self.line.len() {
                    let end = (self.cursor + n).min(self.line.len());
                    self.line.drain(self.cursor..end);
                }
            }
            '@' => {
                if self.cursor <= self.line.len() {
                    for _ in 0..n {
                        self.line.insert(self.cursor, ' ');
                    }
                }
            }
            'X' => (self.cursor..(self.cursor + n).min(self.line.len())).for_each(|i| self.line[i] = ' '),
            'J' if nums.first().copied().unwrap_or(0) >= 2 => {
                self.line.clear();
                self.cursor = 0;
            }
            _ => {}
        }
    }

    fn put(&mut self, c: char) {
        if self.cursor > 4096 {
            return;
        }
        while self.line.len() < self.cursor {
            self.line.push(' ');
        }
        if self.cursor < self.line.len() {
            self.line[self.cursor] = c;
        } else {
            self.line.push(c);
        }
        self.cursor += 1;
    }

    // ── what the user typed ──

    pub fn input(&mut self, data: &[u8]) {
        for c in String::from_utf8_lossy(data).chars() {
            match self.in_esc {
                InEsc::Esc => {
                    self.in_esc = if c == '[' || c == 'O' { InEsc::Seq } else { InEsc::None };
                    continue;
                }
                InEsc::Seq => {
                    if c.is_ascii_alphabetic() || c == '~' {
                        self.in_esc = InEsc::None;
                    }
                    continue;
                }
                InEsc::None => {}
            }
            match c {
                '\r' | '\n' => self.enter(),
                '\x7f' | '\x08' => {
                    self.input.pop();
                }
                '\x03' | '\x15' => {
                    // Ctrl-C / Ctrl-U: the line is abandoned / cleared.
                    self.input.clear();
                    self.plain = true;
                }
                '\x1b' => {
                    self.in_esc = InEsc::Esc;
                    self.plain = false;
                }
                c if (c as u32) < 0x20 => self.plain = false, // tab, ctrl-r, ctrl-a…
                c => {
                    if self.input.len() < MAX_LEN {
                        self.input.push(c);
                    }
                }
            }
        }
    }

    /// Enter: remember what was typed; the decision waits for the line break.
    fn enter(&mut self) {
        if !self.alt_screen && self.entered.len() < 16 {
            self.entered.push_back((std::mem::take(&mut self.input), self.plain, self.start.elapsed()));
        }
        self.input.clear();
        self.plain = true;
    }

    /// The server ended a line: if it answers an Enter, log what the line shows.
    fn line_done(&mut self) {
        let Some((typed, plain, at)) = self.entered.pop_front() else { return };
        let screen: String = self.line.iter().collect();
        let screen = screen.trim_end();
        let typed = typed.trim();
        let found = if plain {
            // Plain typing is exact — but only if it was echoed (not a password).
            (!typed.is_empty() && screen.ends_with(typed)).then(|| (typed.to_string(), "typed"))
        } else {
            let s = strip_prompt(screen);
            (!s.is_empty()).then(|| (s.to_string(), "screen"))
        };
        if let Some((command, source)) = found {
            if self.count < MAX_COMMANDS {
                let t = at;
                let command: String = command.chars().take(MAX_LEN).collect();
                self.pending.push(json!({
                    "t": (t.as_secs_f64() * 1000.0).round() / 1000.0,
                    "time": self.started_at + Duration::milliseconds(t.as_millis() as i64),
                    "command": command,
                    "source": source,
                    "risk": risk(&command),
                }));
                self.count += 1;
            }
        }
    }

    pub async fn flush(&mut self, core: &Core) -> AppResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let chunk = serde_json::to_vec(&std::mem::take(&mut self.pending))?;
        let path = format!("{}{:08}", cmd_prefix(&self.sid), self.seq);
        self.seq += 1;
        core.commit(vec![(path, chunk)], vec![], vec![]).await
    }

    #[cfg(test)]
    fn take(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|v| (v["command"].as_str().unwrap().to_string(), v["source"].as_str().unwrap().to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> CommandLog {
        let mut l = CommandLog::new("s", Utc::now());
        l.output(b"user@web-1:~$ ");
        l
    }

    /// Type `keys` with the server echoing `echo` back, then press Enter.
    fn run(l: &mut CommandLog, keys: &str, echo: &str) {
        l.input(keys.as_bytes());
        l.output(echo.as_bytes());
        l.input(b"\r");
        l.output(b"\r\n");
        l.output(b"user@web-1:~$ ");
    }

    #[test]
    fn plain_command() {
        let mut l = log();
        run(&mut l, "ls -la /etc", "ls -la /etc");
        assert_eq!(l.take(), vec![("ls -la /etc".into(), "typed".into())]);
    }

    #[test]
    fn fast_typing_before_the_echo() {
        let mut l = log();
        // The whole command and Enter arrive before the server echoes anything.
        l.input(b"uptime\r");
        l.output(b"uptime\r\n 10:01 up 3 days\r\nuser@web-1:~$ ");
        assert_eq!(l.take(), vec![("uptime".into(), "typed".into())]);
    }

    #[test]
    fn backspace_edits() {
        let mut l = log();
        run(&mut l, "lss\x7f -l", "lss\x08\x1b[K -l");
        assert_eq!(l.take()[0].0, "ls -l");
    }

    #[test]
    fn tab_completion_uses_the_screen() {
        let mut l = log();
        // typed "cat /etc/hostn<TAB>", shell completed to "cat /etc/hostname "
        run(&mut l, "cat /etc/hostn\t", "cat /etc/hostname ");
        assert_eq!(l.take(), vec![("cat /etc/hostname".into(), "screen".into())]);
    }

    #[test]
    fn history_recall_uses_the_screen() {
        let mut l = log();
        // ↑ : bash redraws the line with the previous command
        run(&mut l, "\x1b[A", "\r\x1b[Kuser@web-1:~$ systemctl status nginx");
        assert_eq!(l.take(), vec![("systemctl status nginx".into(), "screen".into())]);
    }

    #[test]
    fn unechoed_input_is_not_recorded() {
        let mut l = CommandLog::new("s", Utc::now());
        l.output(b"[sudo] password for deploy: ");
        l.input(b"hunter2\r");
        l.output(b"\r\n");
        assert!(l.take().is_empty());
    }

    #[test]
    fn full_screen_programs_are_skipped() {
        let mut l = log();
        run(&mut l, "vim notes", "vim notes");
        l.output(b"\x1b[?1049h");
        l.input(b"ihello");
        l.output(b"hello");
        l.input(b"\r");
        l.output(b"\x1b[?1049l");
        run(&mut l, "ls", "ls");
        assert_eq!(l.take().into_iter().map(|c| c.0).collect::<Vec<_>>(), vec!["vim notes", "ls"]);
    }

    #[test]
    fn empty_and_ctrl_c_lines_are_skipped() {
        let mut l = log();
        run(&mut l, "", "");
        l.input(b"rm -rf /tmp/x\x03");
        l.output(b"rm -rf /tmp/x^C\r\n$ ");
        l.input(b"\r");
        assert!(l.take().is_empty());
    }

    #[test]
    fn risk_levels() {
        assert_eq!(risk("rm -rf /var/lib"), Some("high"));
        assert_eq!(risk("sudo shutdown -h now"), Some("high"));
        assert_eq!(risk("curl https://x.sh | bash"), Some("high"));
        assert_eq!(risk("sudo systemctl restart nginx"), Some("medium"));
        assert_eq!(risk("rm notes.txt"), Some("medium"));
        assert_eq!(risk("ls -la"), None);
        assert_eq!(risk("cat /etc/hostname"), None);
    }
}
