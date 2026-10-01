// Shapes of the free-form reports (SysService.Storage) and shared helpers.
// Everything else is typed by the generated code in src/gen.


export type RedisInfo = {
  engine: 'redis'
  mode: 'standalone' | 'sentinel' | 'cluster'
  role: string | null
  replicas: number
  wait_replicas: number
  sentinel_master: string | null
  sentinels: number | null
  cluster: { state: string | null; known_nodes: number | null; size: number | null; slot: number } | null
  redis_version: string | null
  prefix: string
  keys: number
  aof_enabled: boolean
  appendfsync: string | null
  rdb_save: string | null
  rdb_last_save: number | null
  rdb_changes_since_last_save: number | null
  aof_last_write_status: string | null
  maxmemory_policy: string | null
  used_memory_human: string | null
  warnings: string[]
}

export type RaftPeer = {
  name: string
  api_addr: string
  cluster_addr: string
  voter: boolean
  leader: boolean
  self: boolean
}

export type RaftInfo = {
  engine: 'raft'
  node: { name: string; id: string; api_addr: string; cluster_addr: string }
  path: string
  running: boolean
  state: 'leader' | 'follower' | 'candidate' | 'learner' | 'shutdown' | null
  term: number | null
  last_log_index: number | null
  last_applied: number | null
  snapshot_index: number | null
  leader: RaftPeer | null
  voters: number
  peers: RaftPeer[]
  keys: number
  warnings: string[]
}

export type StorageInfo = RedisInfo | RaftInfo

export const fmtTime = (iso: string | null | undefined) =>
  iso ? new Date(iso).toLocaleString() : '—'

