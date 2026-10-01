//! Per-node persistent storage for Raft, on a single redb file:
//!
//!   logs  u64 → Entry (JSON)            the replicated log
//!   meta  str → JSON                    vote, committed, purged, applied, membership, snapshot
//!   data  str → bytes                   the state machine = the vault's key space (ciphertext)
//!
//! The state machine is persisted on every `apply` (in the same redb
//! transaction as `last_applied`), so a restart never re-applies or loses
//! entries, and snapshots only exist for shipping state to lagging peers.

use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

use openraft::storage::{LogFlushed, LogState, RaftLogStorage, RaftStateMachine, Snapshot};
use openraft::{
    Entry, EntryPayload, LogId, OptionalSend, RaftLogReader, RaftSnapshotBuilder, SnapshotMeta, StorageError,
    StoredMembership, Vote,
};
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::types::{ClusterNode, NodeId, Request, SnapshotData, TypeConfig};

const LOGS: TableDefinition<u64, &[u8]> = TableDefinition::new("logs");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const DATA: TableDefinition<&str, &[u8]> = TableDefinition::new("data");

const K_VOTE: &str = "vote";
const K_COMMITTED: &str = "committed";
const K_PURGED: &str = "last_purged";
const K_APPLIED: &str = "sm_last_applied";
const K_MEMBERSHIP: &str = "sm_membership";
const K_SNAPSHOT_META: &str = "snapshot_meta";
const K_SNAPSHOT_DATA: &str = "snapshot_data";

type SE = StorageError<NodeId>;
type Membership = StoredMembership<NodeId, ClusterNode>;

/// Open (or create) the node's database and make sure every table exists.
pub fn open(path: &std::path::Path) -> anyhow::Result<Arc<Database>> {
    std::fs::create_dir_all(path)?;
    let db = Database::create(path.join("raft.redb"))?;
    let tx = db.begin_write()?;
    tx.open_table(LOGS)?;
    tx.open_table(META)?;
    tx.open_table(DATA)?;
    tx.commit()?;
    Ok(Arc::new(db))
}

fn to_json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("raft types always serialize")
}

fn read_meta<T: DeserializeOwned>(db: &Database, key: &str) -> Result<Option<T>, String> {
    let tx = db.begin_read().map_err(|e| e.to_string())?;
    let t = tx.open_table(META).map_err(|e| e.to_string())?;
    match t.get(key).map_err(|e| e.to_string())? {
        Some(v) => serde_json::from_slice(v.value()).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

fn write_meta<T: Serialize>(db: &Database, key: &str, v: &T) -> Result<(), String> {
    let tx = db.begin_write().map_err(|e| e.to_string())?;
    {
        let mut t = tx.open_table(META).map_err(|e| e.to_string())?;
        t.insert(key, to_json(v).as_slice()).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn io<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// `StorageError` constructors from anything displayable.
mod sio {
    use std::fmt::Display;

    use openraft::storage::SnapshotSignature;
    use openraft::{AnyError, StorageError, StorageIOError};

    use super::NodeId;

    type SE = StorageError<NodeId>;

    fn any(e: impl Display) -> AnyError {
        AnyError::error(e.to_string())
    }

    pub fn read_logs(e: impl Display) -> SE {
        SE::IO { source: StorageIOError::read_logs(any(e)) }
    }
    pub fn write_logs(e: impl Display) -> SE {
        SE::IO { source: StorageIOError::write_logs(any(e)) }
    }
    pub fn read_vote(e: impl Display) -> SE {
        SE::IO { source: StorageIOError::read_vote(any(e)) }
    }
    pub fn write_vote(e: impl Display) -> SE {
        SE::IO { source: StorageIOError::write_vote(any(e)) }
    }
    pub fn read_state_machine(e: impl Display) -> SE {
        SE::IO { source: StorageIOError::read_state_machine(any(e)) }
    }
    pub fn write_state_machine(e: impl Display) -> SE {
        SE::IO { source: StorageIOError::write_state_machine(any(e)) }
    }
    pub fn read_snapshot(sig: Option<SnapshotSignature<NodeId>>, e: impl Display) -> SE {
        SE::IO { source: StorageIOError::read_snapshot(sig, any(e)) }
    }
    pub fn write_snapshot(sig: Option<SnapshotSignature<NodeId>>, e: impl Display) -> SE {
        SE::IO { source: StorageIOError::write_snapshot(sig, any(e)) }
    }
}

// ─── Direct state-machine reads (used by the vault while sealed or as follower) ─

pub fn data_get(db: &Database, key: &str) -> Result<Option<Vec<u8>>, String> {
    let tx = db.begin_read().map_err(|e| e.to_string())?;
    let t = tx.open_table(DATA).map_err(|e| e.to_string())?;
    Ok(t.get(key).map_err(|e| e.to_string())?.map(|v| v.value().to_vec()))
}

pub fn data_list(db: &Database, prefix: &str) -> Result<Vec<String>, String> {
    let tx = db.begin_read().map_err(|e| e.to_string())?;
    let t = tx.open_table(DATA).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for item in t.range::<&str>(prefix..).map_err(|e| e.to_string())? {
        let (k, _) = item.map_err(|e| e.to_string())?;
        let k = k.value();
        if !k.starts_with(prefix) {
            break;
        }
        out.push(k.to_string());
    }
    Ok(out)
}

pub fn data_dump(db: &Database) -> Result<Vec<(String, Vec<u8>)>, String> {
    let tx = db.begin_read().map_err(|e| e.to_string())?;
    let t = tx.open_table(DATA).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for item in t.iter().map_err(|e| e.to_string())? {
        let (k, v) = item.map_err(|e| e.to_string())?;
        out.push((k.value().to_string(), v.value().to_vec()));
    }
    Ok(out)
}

pub fn data_len(db: &Database) -> Result<u64, String> {
    let tx = db.begin_read().map_err(|e| e.to_string())?;
    let t = tx.open_table(DATA).map_err(|e| e.to_string())?;
    t.len().map_err(|e| e.to_string())
}

/// Last membership applied to this node's state machine (survives restarts,
/// readable while sealed).
pub fn applied_membership(db: &Database) -> Result<Option<Membership>, String> {
    read_meta(db, K_MEMBERSHIP)
}

/// True once this node has any Raft state — i.e. it was bootstrapped or joined.
pub fn has_state(db: &Database) -> Result<bool, String> {
    if read_meta::<Vote<NodeId>>(db, K_VOTE)?.is_some() {
        return Ok(true);
    }
    let tx = db.begin_read().map_err(|e| e.to_string())?;
    let t = tx.open_table(LOGS).map_err(|e| e.to_string())?;
    Ok(!t.is_empty().map_err(|e| e.to_string())?)
}

// ─── Log store ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LogStore {
    db: Arc<Database>,
}

impl LogStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, SE> {
        let tx = self.db.begin_read().map_err(sio::read_logs)?;
        let t = tx.open_table(LOGS).map_err(sio::read_logs)?;
        let start = match range.start_bound() {
            Bound::Included(i) => Bound::Included(*i),
            Bound::Excluded(i) => Bound::Excluded(*i),
            Bound::Unbounded => Bound::Unbounded,
        };
        let end = match range.end_bound() {
            Bound::Included(i) => Bound::Included(*i),
            Bound::Excluded(i) => Bound::Excluded(*i),
            Bound::Unbounded => Bound::Unbounded,
        };
        let mut out = Vec::new();
        for item in t.range::<u64>((start, end)).map_err(sio::read_logs)? {
            let (_, v) = item.map_err(sio::read_logs)?;
            out.push(serde_json::from_slice(v.value()).map_err(sio::read_logs)?);
        }
        Ok(out)
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, SE> {
        let last_purged: Option<LogId<NodeId>> =
            read_meta(&self.db, K_PURGED).map_err(sio::read_logs)?;
        let tx = self.db.begin_read().map_err(sio::read_logs)?;
        let t = tx.open_table(LOGS).map_err(sio::read_logs)?;
        let last = match t.last().map_err(sio::read_logs)? {
            Some((_, v)) => {
                let e: Entry<TypeConfig> = serde_json::from_slice(v.value()).map_err(sio::read_logs)?;
                Some(e.log_id)
            }
            None => last_purged,
        };
        Ok(LogState { last_purged_log_id: last_purged, last_log_id: last })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), SE> {
        write_meta(&self.db, K_VOTE, vote).map_err(sio::write_vote)
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, SE> {
        read_meta(&self.db, K_VOTE).map_err(sio::read_vote)
    }

    async fn save_committed(&mut self, committed: Option<LogId<NodeId>>) -> Result<(), SE> {
        write_meta(&self.db, K_COMMITTED, &committed).map_err(sio::write_logs)
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, SE> {
        Ok(read_meta::<Option<LogId<NodeId>>>(&self.db, K_COMMITTED)
            .map_err(sio::read_logs)?
            .flatten())
    }

    async fn append<I>(&mut self, entries: I, callback: LogFlushed<TypeConfig>) -> Result<(), SE>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let tx = self.db.begin_write().map_err(sio::write_logs)?;
        {
            let mut t = tx.open_table(LOGS).map_err(sio::write_logs)?;
            for e in entries {
                t.insert(e.log_id.index, to_json(&e).as_slice()).map_err(sio::write_logs)?;
            }
        }
        // redb commits are fsynced (Durability::Immediate), so the entries are
        // durable before we tell openraft they are.
        let res = tx.commit().map_err(io);
        let ok = res.is_ok();
        callback.log_io_completed(res);
        if ok { Ok(()) } else { Err(sio::write_logs("log append commit failed")) }
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), SE> {
        let tx = self.db.begin_write().map_err(sio::write_logs)?;
        {
            let mut t = tx.open_table(LOGS).map_err(sio::write_logs)?;
            t.retain_in::<u64, _>(log_id.index.., |_, _| false).map_err(sio::write_logs)?;
        }
        tx.commit().map_err(sio::write_logs)
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), SE> {
        let tx = self.db.begin_write().map_err(sio::write_logs)?;
        {
            let mut t = tx.open_table(LOGS).map_err(sio::write_logs)?;
            t.retain_in::<u64, _>(..=log_id.index, |_, _| false).map_err(sio::write_logs)?;
            let mut m = tx.open_table(META).map_err(sio::write_logs)?;
            m.insert(K_PURGED, to_json(&log_id).as_slice()).map_err(sio::write_logs)?;
        }
        tx.commit().map_err(sio::write_logs)
    }
}

// ─── State machine ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
}

impl StateMachine {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn snapshot_from_db(&self) -> Result<Option<Snapshot<TypeConfig>>, String> {
        let meta: Option<SnapshotMeta<NodeId, ClusterNode>> = read_meta(&self.db, K_SNAPSHOT_META)?;
        let Some(meta) = meta else { return Ok(None) };
        let tx = self.db.begin_read().map_err(|e| e.to_string())?;
        let t = tx.open_table(META).map_err(|e| e.to_string())?;
        let data = t.get(K_SNAPSHOT_DATA).map_err(|e| e.to_string())?.map(|v| v.value().to_vec()).unwrap_or_default();
        Ok(Some(Snapshot { meta, snapshot: Box::new(data) }))
    }
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, SE> {
        let err = |e: String| sio::write_snapshot(None, e);
        // One read transaction = a consistent view of data + applied state.
        let tx = self.db.begin_read().map_err(|e| err(e.to_string()))?;
        let meta_t = tx.open_table(META).map_err(|e| err(e.to_string()))?;
        let get = |k: &str| -> Result<Option<Vec<u8>>, String> {
            Ok(meta_t.get(k).map_err(|e| e.to_string())?.map(|v| v.value().to_vec()))
        };
        let last_applied: Option<LogId<NodeId>> = match get(K_APPLIED).map_err(err)? {
            Some(b) => serde_json::from_slice(&b).map_err(|e| err(e.to_string()))?,
            None => None,
        };
        let membership: Membership = match get(K_MEMBERSHIP).map_err(err)? {
            Some(b) => serde_json::from_slice(&b).map_err(|e| err(e.to_string()))?,
            None => Membership::default(),
        };
        let data_t = tx.open_table(DATA).map_err(|e| err(e.to_string()))?;
        let mut entries = Vec::new();
        for item in data_t.iter().map_err(|e| err(e.to_string()))? {
            let (k, v) = item.map_err(|e| err(e.to_string()))?;
            entries.push((k.value().to_string(), v.value().to_vec()));
        }
        drop(data_t);
        drop(meta_t);
        drop(tx);

        let bytes = serde_json::to_vec(&SnapshotData { entries }).map_err(|e| err(e.to_string()))?;
        let snapshot_id = format!(
            "{}-{}",
            last_applied.map(|l| l.index).unwrap_or(0),
            chrono::Utc::now().timestamp_millis()
        );
        let meta = SnapshotMeta { last_log_id: last_applied, last_membership: membership, snapshot_id };

        let wtx = self.db.begin_write().map_err(|e| err(e.to_string()))?;
        {
            let mut m = wtx.open_table(META).map_err(|e| err(e.to_string()))?;
            m.insert(K_SNAPSHOT_META, to_json(&meta).as_slice()).map_err(|e| err(e.to_string()))?;
            m.insert(K_SNAPSHOT_DATA, bytes.as_slice()).map_err(|e| err(e.to_string()))?;
        }
        wtx.commit().map_err(|e| err(e.to_string()))?;
        Ok(Snapshot { meta, snapshot: Box::new(bytes) })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(&mut self) -> Result<(Option<LogId<NodeId>>, Membership), SE> {
        let applied: Option<LogId<NodeId>> = read_meta::<Option<LogId<NodeId>>>(&self.db, K_APPLIED)
            .map_err(sio::read_state_machine)?
            .flatten();
        let membership = applied_membership(&self.db).map_err(sio::read_state_machine)?.unwrap_or_default();
        Ok((applied, membership))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<bool>, SE>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let werr = |e: String| sio::write_state_machine(e);
        let tx = self.db.begin_write().map_err(|e| werr(e.to_string()))?;
        let mut responses = Vec::new();
        {
            let mut data = tx.open_table(DATA).map_err(|e| werr(e.to_string()))?;
            let mut meta = tx.open_table(META).map_err(|e| werr(e.to_string()))?;
            for entry in entries {
                match entry.payload {
                    EntryPayload::Blank => {}
                    EntryPayload::Normal(Request::Commit { puts, deletes, guards }) => {
                        // Deterministic on every node: all see the same prior state.
                        let mut ok = true;
                        for g in &guards {
                            let cur = data.get(g.path.as_str()).map_err(|e| werr(e.to_string()))?;
                            let cur = cur.as_ref().map(|v| v.value());
                            if cur != g.expect.as_deref() {
                                ok = false;
                                break;
                            }
                        }
                        if ok {
                            for (k, v) in &puts {
                                data.insert(k.as_str(), v.as_slice()).map_err(|e| werr(e.to_string()))?;
                            }
                            for k in &deletes {
                                data.remove(k.as_str()).map_err(|e| werr(e.to_string()))?;
                            }
                        }
                        meta.insert(K_APPLIED, to_json(&Some(entry.log_id)).as_slice())
                            .map_err(|e| werr(e.to_string()))?;
                        responses.push(ok);
                        continue;
                    }
                    EntryPayload::Membership(m) => {
                        let sm = Membership::new(Some(entry.log_id), m);
                        meta.insert(K_MEMBERSHIP, to_json(&sm).as_slice()).map_err(|e| werr(e.to_string()))?;
                    }
                }
                meta.insert(K_APPLIED, to_json(&Some(entry.log_id)).as_slice())
                    .map_err(|e| werr(e.to_string()))?;
                responses.push(true);
            }
        }
        tx.commit().map_err(|e| werr(e.to_string()))?;
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<Vec<u8>>, SE> {
        Ok(Box::default())
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, ClusterNode>,
        snapshot: Box<Vec<u8>>,
    ) -> Result<(), SE> {
        let err = |e: String| sio::write_snapshot(Some(meta.signature()), e);
        let decoded: SnapshotData = serde_json::from_slice(&snapshot).map_err(|e| err(e.to_string()))?;
        // Replace the whole key space, applied state and stored snapshot atomically.
        let tx = self.db.begin_write().map_err(|e| err(e.to_string()))?;
        {
            let mut data = tx.open_table(DATA).map_err(|e| err(e.to_string()))?;
            data.retain(|_, _| false).map_err(|e| err(e.to_string()))?;
            for (k, v) in &decoded.entries {
                data.insert(k.as_str(), v.as_slice()).map_err(|e| err(e.to_string()))?;
            }
            let mut m = tx.open_table(META).map_err(|e| err(e.to_string()))?;
            m.insert(K_APPLIED, to_json(&meta.last_log_id).as_slice()).map_err(|e| err(e.to_string()))?;
            m.insert(K_MEMBERSHIP, to_json(&meta.last_membership).as_slice()).map_err(|e| err(e.to_string()))?;
            m.insert(K_SNAPSHOT_META, to_json(meta).as_slice()).map_err(|e| err(e.to_string()))?;
            m.insert(K_SNAPSHOT_DATA, snapshot.as_slice()).map_err(|e| err(e.to_string()))?;
        }
        tx.commit().map_err(|e| err(e.to_string()))
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<TypeConfig>>, SE> {
        self.snapshot_from_db().map_err(|e| sio::read_snapshot(None, e))
    }
}
