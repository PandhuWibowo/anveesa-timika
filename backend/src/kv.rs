//! Versioned key/value secrets engine (KV-v2 semantics).
//!
//! Storage layout (all barrier-encrypted):
//!   kv/meta/<path>        → Metadata (version table, timestamps)
//!   kv/ver/<path>@<n>     → the secret's data map for version n
//!
//! A write puts the new version blob and the updated metadata in one Redis
//! MULTI, and prunes versions beyond `max_versions` in the same transaction.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::{Core, MAX_RETRIES};
use crate::error::{AppError, AppResult};

const META: &str = "kv/meta/";

#[derive(Serialize, Deserialize, Clone)]
pub struct VersionMeta {
    pub created_time: DateTime<Utc>,
    pub deletion_time: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Metadata {
    pub current_version: u32,
    pub oldest_version: u32,
    pub max_versions: u32,
    pub created_time: DateTime<Utc>,
    pub updated_time: DateTime<Utc>,
    pub versions: BTreeMap<u32, VersionMeta>,
}

#[derive(Serialize)]
pub struct Secret {
    pub data: Map<String, Value>,
    pub version: u32,
    pub metadata: VersionMeta,
}

/// Normalize and validate a secret path: `a/b/c`, segments of `[A-Za-z0-9._-]`.
pub fn clean_path(raw: &str) -> AppResult<String> {
    let p = raw.trim_matches('/');
    if p.is_empty() {
        return Err(AppError::BadRequest("secret path is empty".into()));
    }
    for seg in p.split('/') {
        let ok = !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok {
            return Err(AppError::BadRequest(format!(
                "invalid path segment `{seg}` (allowed: letters, digits, . _ -)"
            )));
        }
    }
    Ok(p.to_string())
}

fn meta_path(p: &str) -> String {
    format!("{META}{p}")
}

fn ver_path(p: &str, n: u32) -> String {
    format!("kv/ver/{p}@{n}")
}

pub async fn metadata(core: &Core, path: &str) -> AppResult<Metadata> {
    core.get_json(&meta_path(path))
        .await?
        .ok_or_else(|| AppError::NotFound(format!("secret `{path}`")))
}

pub async fn read(core: &Core, path: &str, version: Option<u32>) -> AppResult<Secret> {
    let meta = metadata(core, path).await?;
    let n = version.unwrap_or(meta.current_version);
    let vm = meta
        .versions
        .get(&n)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("version {n} of `{path}`")))?;
    if vm.deletion_time.is_some() {
        return Err(AppError::NotFound(format!("version {n} of `{path}` is deleted")));
    }
    let data = core
        .get_json(&ver_path(path, n))
        .await?
        .ok_or_else(|| AppError::NotFound(format!("version {n} of `{path}`")))?;
    Ok(Secret { data, version: n, metadata: vm })
}

/// Run a read-modify-write until its guarded commit wins. With several timika
/// instances on shared storage, a lost race just re-reads and tries again.
pub async fn retrying<T, F, Fut>(mut attempt: F) -> AppResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    for i in 0..MAX_RETRIES {
        match attempt().await {
            Err(AppError::WriteConflict) => {
                // Small, growing, jittered backoff so contenders spread out.
                let ms = (5u64 << i.min(5)) + rand::random::<u64>() % 10;
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            }
            other => return other,
        }
    }
    Err(AppError::WriteConflict)
}

pub async fn write(
    core: &Core,
    path: &str,
    data: Map<String, Value>,
    cas: Option<u32>,
    max_versions: u32,
) -> AppResult<Metadata> {
    // The lock only avoids pointless conflicts between requests in this process;
    // correctness across instances comes from the guard on the metadata.
    let _local = core.write_lock.lock().await;
    let data = serde_json::to_vec(&data)?;
    retrying(|| async {
        let now = Utc::now();
        let (meta, guard) = core.get_json_guarded::<Metadata>(&meta_path(path)).await?;
        let mut meta = meta.unwrap_or(Metadata {
            current_version: 0,
            oldest_version: 1,
            max_versions,
            created_time: now,
            updated_time: now,
            versions: BTreeMap::new(),
        });
        if let Some(expected) = cas {
            if expected != meta.current_version {
                return Err(AppError::Conflict(format!(
                    "check-and-set failed: expected version {expected}, current is {}",
                    meta.current_version
                )));
            }
        }

        let n = meta.current_version + 1;
        meta.current_version = n;
        meta.updated_time = now;
        meta.versions.insert(n, VersionMeta { created_time: now, deletion_time: None });

        let mut deletes = Vec::new();
        while meta.versions.len() > meta.max_versions as usize {
            let (old, _) = meta.versions.pop_first().expect("non-empty");
            deletes.push(ver_path(path, old));
        }
        meta.oldest_version = *meta.versions.keys().next().expect("non-empty");

        core.commit(
            vec![(ver_path(path, n), data.clone()), (meta_path(path), serde_json::to_vec(&meta)?)],
            deletes,
            vec![guard],
        )
        .await?;
        Ok(meta)
    })
    .await
}

/// Soft-delete: mark versions deleted (latest by default). Data blobs are kept
/// so the version can be inspected in metadata; `destroy` removes everything.
pub async fn soft_delete(core: &Core, path: &str, versions: Option<Vec<u32>>) -> AppResult<Metadata> {
    let _local = core.write_lock.lock().await;
    retrying(|| async {
        let (meta, guard) = core.get_json_guarded::<Metadata>(&meta_path(path)).await?;
        let mut meta = meta.ok_or_else(|| AppError::NotFound(format!("secret `{path}`")))?;
        let targets = versions.clone().unwrap_or_else(|| vec![meta.current_version]);
        let now = Utc::now();
        for n in targets {
            if let Some(vm) = meta.versions.get_mut(&n) {
                vm.deletion_time.get_or_insert(now);
            }
        }
        meta.updated_time = now;
        core.commit(vec![(meta_path(path), serde_json::to_vec(&meta)?)], vec![], vec![guard]).await?;
        Ok(meta)
    })
    .await
}

/// Permanently remove a secret and every version of it.
pub async fn destroy(core: &Core, path: &str) -> AppResult<()> {
    let _local = core.write_lock.lock().await;
    retrying(|| async {
        let (meta, guard) = core.get_json_guarded::<Metadata>(&meta_path(path)).await?;
        let meta = meta.ok_or_else(|| AppError::NotFound(format!("secret `{path}`")))?;
        let mut deletes: Vec<String> = meta.versions.keys().map(|n| ver_path(path, *n)).collect();
        deletes.push(meta_path(path));
        core.commit(vec![], deletes, vec![guard]).await
    })
    .await
}

#[derive(Serialize)]
pub struct Listing {
    /// Immediate children of the folder; sub-folders end in `/`.
    pub keys: Vec<String>,
}

/// List the immediate children of a folder (`""` = root).
pub async fn list(core: &Core, folder: &str) -> AppResult<Listing> {
    let folder = folder.trim_matches('/');
    let prefix = if folder.is_empty() { META.to_string() } else { format!("{META}{folder}/") };
    let mut keys: Vec<String> = core
        .list(&prefix)
        .await?
        .into_iter()
        .filter_map(|full| {
            let rest = full.strip_prefix(&prefix)?;
            Some(match rest.split_once('/') {
                Some((dir, _)) => format!("{dir}/"),
                None => rest.to_string(),
            })
        })
        .collect();
    keys.dedup();
    if keys.is_empty() && !folder.is_empty() {
        return Err(AppError::NotFound(format!("folder `{folder}/`")));
    }
    Ok(Listing { keys })
}
