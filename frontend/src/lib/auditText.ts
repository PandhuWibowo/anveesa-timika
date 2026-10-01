// Plain-language labels for audit events ("Wrote secret" instead of
// "KvService/Write"), and the category each belongs to.

import type { AuditEvent } from '../gen/timika/v1/audit_pb'

export type Category = 'signin' | 'secrets' | 'servers' | 'sessions' | 'files' | 'users' | 'system'

const LABELS: Record<string, [string, Category]> = {
  'KvService/Write': ['Wrote secret', 'secrets'],
  'KvService/Read': ['Read secret', 'secrets'],
  'KvService/List': ['Listed folder', 'secrets'],
  'KvService/GetMetadata': ['Viewed secret history', 'secrets'],
  'KvService/Delete': ['Deleted secret version', 'secrets'],
  'KvService/Destroy': ['Destroyed secret', 'secrets'],
  'AuthService/Login': ['Signed in', 'signin'],
  'AuthService/RevokeSelf': ['Signed out', 'signin'],
  'AuthService/RenewSelf': ['Session kept alive', 'signin'],
  'AuthService/LookupSelf': ['Session check', 'signin'],
  'AuthService/ChangePassword': ['Changed own password', 'signin'],
  'AuthService/UpsertUser': ['Created or updated user', 'users'],
  'AuthService/DeleteUser': ['Deleted user', 'users'],
  'AuthService/UnlockUser': ['Unlocked user', 'users'],
  'AuthService/GetUser': ['Viewed user', 'users'],
  'AuthService/ListUsers': ['Listed users', 'users'],
  'BastionService/CreateAsset': ['Added server', 'servers'],
  'BastionService/UpdateAsset': ['Edited server', 'servers'],
  'BastionService/DeleteAsset': ['Deleted server', 'servers'],
  'BastionService/TestAsset': ['Tested server login', 'servers'],
  'BastionService/ResetHostKey': ['Reset host key', 'servers'],
  'BastionService/CreateGrant': ['Granted server access', 'servers'],
  'BastionService/DeleteGrant': ['Revoked server access', 'servers'],
  'BastionService/ListAssets': ['Listed servers', 'servers'],
  'BastionService/ListGrants': ['Viewed server access', 'servers'],
  'BastionService/ListAllGrants': ['Viewed server access', 'servers'],
  'BastionService/ListSessions': ['Viewed sessions', 'sessions'],
  'BastionService/KillSession': ['Ended a session', 'sessions'],
  'BastionService/GetRecording': ['Replayed a recording', 'sessions'],
  'GET /v1/bastion/connect': ['Opened terminal', 'sessions'],
  'BastionService/ListFiles': ['Browsed files', 'files'],
  'BastionService/MakeDir': ['Created a folder', 'files'],
  'BastionService/RenameFile': ['Renamed a file', 'files'],
  'BastionService/DeleteFiles': ['Deleted files', 'files'],
  'BastionService/DownloadLink': ['Prepared a download', 'files'],
  'GET /v1/bastion/files/download': ['Downloaded a file', 'files'],
  'PUT /v1/bastion/files/upload': ['Uploaded a file', 'files'],
  'BastionService/Compress': ['Compressed files', 'files'],
  'BastionService/Extract': ['Extracted an archive', 'files'],
  'BastionService/ArchiveLink': ['Prepared an archive download', 'files'],
  'BastionService/ListCommands': ['Viewed commands', 'sessions'],
  'session live': ['Terminal session started', 'sessions'],
  'session closed': ['Terminal session ended', 'sessions'],
  'session killed': ['Session ended by an admin', 'sessions'],
  'session failed': ['Terminal connection failed', 'sessions'],
  'SysService/Init': ['Initialized vault', 'system'],
  'SysService/Unseal': ['Submitted unseal key', 'system'],
  'SysService/Seal': ['Sealed vault', 'system'],
  'SysService/Rotate': ['Rotated encryption key', 'system'],
  'SysService/GetKeyStatus': ['Viewed key status', 'system'],
  'SysService/Storage': ['Viewed storage status', 'system'],
  'SysService/Audit': ['Viewed audit settings', 'system'],
  'SysService/AuditHash': ['Looked up a value in the audit log', 'system'],
  'AuditService/ListEvents': ['Viewed audit trail', 'system'],
  'AuditService/Verify': ['Verified audit log integrity', 'system'],
  'ClusterService/Configuration': ['Viewed cluster', 'system'],
  'ClusterService/RemovePeer': ['Removed cluster node', 'system'],
  'ClusterService/Snapshot': ['Downloaded snapshot', 'system'],
  'ClusterService/JoinChallenge': ['Node asked to join', 'system'],
  'ClusterService/JoinAnswer': ['Node joined', 'system'],
}

/** gRPC status codes → short names. */
export const CODE_NAMES: Record<number, string> = {
  1: 'cancelled', 2: 'unknown', 3: 'invalid', 4: 'timeout', 5: 'not found', 6: 'exists', 7: 'denied',
  8: 'rate limited', 9: 'precondition', 10: 'conflict', 12: 'unimplemented', 13: 'internal', 14: 'unavailable', 16: 'unauthenticated',
}

export function describe(e: Pick<AuditEvent, 'action' | 'ok' | 'kind' | 'code'>): { label: string; category: Category } {
  const hit = LABELS[e.action]
  if (!hit) return { label: e.action, category: 'system' }
  let [label, category] = hit
  if (!e.ok && e.action === 'AuthService/Login') label = 'Sign-in failed'
  if (!e.ok && e.action === 'GET /v1/bastion/connect') label = 'Terminal refused'
  return { label, category }
}

export const outcome = (e: Pick<AuditEvent, 'ok' | 'code' | 'kind'>) =>
  e.ok ? 'ok' : e.kind === 'call' && e.code < 100 ? CODE_NAMES[e.code] ?? `code ${e.code}` : e.kind === 'call' ? `HTTP ${e.code}` : 'failed'
