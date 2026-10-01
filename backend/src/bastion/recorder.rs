//! Session recording in asciicast v2 (https://docs.asciinema.org/manual/asciicast/v2/).
//!
//! Only *output* is recorded (what the user saw — including echoed commands).
//! Keystrokes are not: they'd capture passwords typed at `sudo` prompts.
//! Events are buffered and written in chunks (every ~2s or 64 KiB) through the
//! barrier, so recording is encrypted at rest and never slows the terminal.

use std::time::Instant;

use crate::core::Core;
use crate::error::AppResult;

const FLUSH_BYTES: usize = 64 * 1024;

pub struct Recorder {
    sid: String,
    start: Instant,
    buf: String,
    /// Bytes of an incomplete UTF-8 sequence carried to the next chunk.
    carry: Vec<u8>,
    pub seq: u32,
    pub bytes: u64,
    max: u64,
    pub truncated: bool,
}

impl Recorder {
    pub fn new(sid: &str, max_bytes: u64) -> Self {
        Self { sid: sid.into(), start: Instant::now(), buf: String::new(), carry: Vec::new(), seq: 0, bytes: 0, max: max_bytes, truncated: false }
    }

    fn event(&mut self, kind: &str, data: &str) {
        let t = self.start.elapsed().as_secs_f64();
        let line = serde_json::json!([(t * 1_000_000.0).round() / 1_000_000.0, kind, data]).to_string();
        self.buf.push_str(&line);
        self.buf.push('\n');
    }

    pub fn output(&mut self, data: &[u8]) {
        if self.truncated {
            return;
        }
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(data);
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(e) => {
                let valid = e.valid_up_to();
                if e.error_len().is_none() {
                    // Split multi-byte character: keep the tail for the next chunk.
                    self.carry = bytes[valid..].to_vec();
                    String::from_utf8_lossy(&bytes[..valid]).into_owned()
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            }
        };
        if !text.is_empty() {
            self.event("o", &text);
        }
    }

    pub fn resize(&mut self, cols: u32, rows: u32) {
        if !self.truncated {
            self.event("r", &format!("{cols}x{rows}"));
        }
    }

    pub fn wants_flush(&self) -> bool {
        self.buf.len() >= FLUSH_BYTES
    }

    /// Write buffered events as the next chunk.
    pub async fn flush(&mut self, core: &Core) -> AppResult<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buf);
        let path = format!("{}{:08}", super::rec_prefix(&self.sid), self.seq);
        self.bytes += chunk.len() as u64;
        self.seq += 1;
        core.commit(vec![(path, chunk.into_bytes())], vec![], vec![]).await?;
        if self.bytes >= self.max && !self.truncated {
            self.event("o", "\r\n[timika: recording limit reached — the rest of this session is not recorded]\r\n");
            self.truncated = true;
            let tail = std::mem::take(&mut self.buf);
            let path = format!("{}{:08}", super::rec_prefix(&self.sid), self.seq);
            self.seq += 1;
            core.commit(vec![(path, tail.into_bytes())], vec![], vec![]).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_utf8_across_chunks() {
        let mut r = Recorder::new("s", 1 << 20);
        let s = "héllo 中文".as_bytes();
        let (a, b) = s.split_at(9); // cuts through a multi-byte char
        r.output(a);
        r.output(b);
        let text: String = r
            .buf
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()[2].as_str().unwrap().to_string())
            .collect();
        assert_eq!(text, "héllo 中文");
    }
}
