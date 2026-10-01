//! Schedules: run a project's action on a cron schedule — a nightly plan to
//! catch drift, a weekly playbook run.
//!
//! Every instance checks every 20 s (Raft: only the leader). A due time is
//! claimed with a guarded write of `last_fired`, so with N instances each time
//! fires exactly once. Times missed while timika was down or sealed for more
//! than 10 minutes are skipped (and noted), not replayed in a burst.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::cron::{parse_offset, Cron};
use super::{schedule_path, RunOptions, Start, SCHEDULES};
use crate::core::Core;
use crate::error::{AppError, AppResult};

const LATE: i64 = 10 * 60;

#[derive(Serialize, Deserialize, Clone)]
pub struct Schedule {
    pub id: String,
    pub repo: String,
    pub project: String,
    pub action: String,
    pub name: String,
    pub cron: String,
    /// UTC or a fixed offset (+07:00).
    pub timezone: String,
    #[serde(default)]
    pub options: RunOptions,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    /// The last time slot claimed (fired or skipped).
    #[serde(default)]
    pub last_fired: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_run: Option<String>,
    #[serde(default)]
    pub last_result: Option<String>,
}

impl Schedule {
    pub fn next(&self) -> Option<DateTime<Utc>> {
        if !self.enabled {
            return None;
        }
        let c = Cron::parse(&self.cron).ok()?;
        let tz = parse_offset(&self.timezone).ok()?;
        c.next(self.last_fired.unwrap_or(self.created_at).max(Utc::now() - Duration::seconds(1)), tz)
    }

    /// The slot due now (not yet claimed), and whether it is too late to run.
    fn due(&self, now: DateTime<Utc>) -> Option<(DateTime<Utc>, bool)> {
        let c = Cron::parse(&self.cron).ok()?;
        let tz = parse_offset(&self.timezone).ok()?;
        let mut slot = c.next(self.last_fired.unwrap_or(self.created_at), tz)?;
        if slot > now {
            return None;
        }
        // Several missed: jump to the latest one.
        while let Some(n) = c.next(slot, tz) {
            if n > now {
                break;
            }
            slot = n;
        }
        Some((slot, (now - slot).num_seconds() > LATE))
    }
}

pub async fn list(core: &Core, repo: Option<&str>) -> AppResult<Vec<Schedule>> {
    let mut out = Vec::new();
    for key in core.list(SCHEDULES).await? {
        if let Some(s) = core.get_json::<Schedule>(&key).await? {
            if repo.is_none_or(|r| r == s.repo) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// Claim `slot` (guarded): false if another instance already did.
async fn claim(core: &Core, id: &str, slot: DateTime<Utc>, result: Option<String>) -> AppResult<Option<Schedule>> {
    let (s, guard) = core.get_json_guarded::<Schedule>(&schedule_path(id)).await?;
    let Some(mut s) = s else { return Ok(None) };
    if s.last_fired.is_some_and(|f| f >= slot) || !s.enabled {
        return Ok(None);
    }
    s.last_fired = Some(slot);
    if let Some(r) = result {
        s.last_result = Some(r);
    }
    match core.commit(vec![(schedule_path(id), serde_json::to_vec(&s)?)], vec![], vec![guard]).await {
        Ok(()) => Ok(Some(s)),
        Err(AppError::WriteConflict) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn record(core: &Core, id: &str, run: Option<String>, result: String) {
    for _ in 0..crate::core::MAX_RETRIES {
        let Ok((Some(mut s), guard)) = core.get_json_guarded::<Schedule>(&schedule_path(id)).await else { return };
        s.last_run = run.clone().or(s.last_run);
        s.last_result = Some(result.clone());
        match core.commit(vec![(schedule_path(id), serde_json::to_vec(&s).unwrap_or_default())], vec![], vec![guard]).await {
            Err(AppError::WriteConflict) => continue,
            _ => return,
        }
    }
}

/// Start the schedule's run now (used by the loop and by "Run now").
pub async fn fire(core: &Arc<Core>, s: &Schedule, user: &str) -> AppResult<super::Run> {
    let repo = super::get_repo(core, &s.repo).await?;
    let r = super::start_run(
        core,
        &repo,
        Start { project: &s.project, action: &s.action, user, trigger: "schedule", plan_run: None, options: s.options.clone() },
    )
    .await;
    match &r {
        Ok(run) => record(core, &s.id, Some(run.id.clone()), format!("started run {}", &run.id[..8])).await,
        Err(e) => record(core, &s.id, None, format!("not started: {e}")).await,
    }
    r
}

async fn tick(core: &Arc<Core>) -> AppResult<()> {
    let now = Utc::now();
    for s in list(core, None).await? {
        if !s.enabled {
            continue;
        }
        let Some((slot, late)) = s.due(now) else { continue };
        let note = late.then(|| format!("skipped {} — timika was down or sealed", slot.format("%Y-%m-%d %H:%M UTC")));
        let Some(claimed) = claim(core, &s.id, slot, note).await? else { continue };
        if !late {
            let _ = fire(core, &claimed, &format!("schedule “{}”", claimed.name)).await;
        }
    }
    Ok(())
}

pub fn spawn(core: Arc<Core>) {
    tokio::spawn(async move {
        loop {
            let every = std::env::var("AUTOMATION_SCHEDULE_TICK_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(20u64).max(1);
            tokio::time::sleep(std::time::Duration::from_secs(every)).await;
            if core.is_sealed().await {
                continue;
            }
            if let Some(r) = core.storage.raft() {
                if !r.is_leader().await {
                    continue;
                }
            }
            if let Err(e) = tick(&core).await {
                tracing::warn!("automation: schedule check failed: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(cron: &str, created: &str, last: Option<&str>) -> Schedule {
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        Schedule {
            id: "s".into(), repo: "r".into(), project: "p".into(), action: "plan".into(), name: "n".into(),
            cron: cron.into(), timezone: "UTC".into(), options: Default::default(), enabled: true,
            created_by: "x".into(), created_at: t(created), last_fired: last.map(t), last_run: None, last_result: None,
        }
    }

    #[test]
    fn due_slots() {
        let now = DateTime::parse_from_rfc3339("2026-10-01T10:05:30Z").unwrap().with_timezone(&Utc);
        // Every 5 minutes, last fired 10:00 → 10:05 is due, on time.
        let (slot, late) = sched("*/5 * * * *", "2026-09-01T00:00:00Z", Some("2026-10-01T10:00:00Z")).due(now).unwrap();
        assert_eq!((slot.to_rfc3339().as_str(), late), ("2026-10-01T10:05:00+00:00", false));
        // Already claimed.
        assert!(sched("*/5 * * * *", "2026-09-01T00:00:00Z", Some("2026-10-01T10:05:00Z")).due(now).is_none());
        // Down for a day: only the latest slot, and it's late? no — 10:05 is recent.
        let (slot, late) = sched("*/5 * * * *", "2026-09-01T00:00:00Z", Some("2026-09-30T10:00:00Z")).due(now).unwrap();
        assert_eq!((slot.to_rfc3339().as_str(), late), ("2026-10-01T10:05:00+00:00", false));
        // Daily 02:00, last ran yesterday 02:00, now 10:05 → today's 02:00 is 8 h late: skipped.
        let (_, late) = sched("0 2 * * *", "2026-09-01T00:00:00Z", Some("2026-09-30T02:00:00Z")).due(now).unwrap();
        assert!(late);
        // A new schedule never fires for times before it existed.
        assert!(sched("0 2 * * *", "2026-10-01T09:00:00Z", None).due(now).is_none());
    }
}
