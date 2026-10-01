import { sys } from './api'
import type { SealStatus, Instance } from '../gen/timika/v1/sys_pb'
import type { LastLogin } from '../gen/timika/v1/auth_pb'

export type { SealStatus, Instance }

// The token lives in sessionStorage (not localStorage like mikko's JWT): a
// vault token should not outlive the browser tab.
const KEY = 'timika_token'

function readToken(): string | null {
  try { return sessionStorage.getItem(KEY) } catch { return null }
}

/** Who is signed in. `username` undefined = the root token (break-glass). */
export type Me = {
  username?: string
  policies: string[]
  expiresAt?: string
  maxExpiresAt?: string
  lastLogin?: Pick<LastLogin, 'time' | 'ip'>
  lastFailedLogin?: Pick<LastLogin, 'time' | 'ip'>
}

const ME_KEY = 'timika_me_v2'

function readMe(): Me | null {
  try { return JSON.parse(sessionStorage.getItem(ME_KEY) ?? 'null') } catch { return null }
}

export const session = $state({
  token: readToken(),
  me: readMe(),
  status: null as SealStatus | null,
  loaded: false,
  /** Why the user was signed out, shown on the login page. */
  signedOutReason: '' as string,
})

export function setToken(t: string, me: Me | null = null) {
  session.token = t
  session.me = me
  session.signedOutReason = ''
  try {
    sessionStorage.setItem(KEY, t)
    sessionStorage.setItem(ME_KEY, JSON.stringify(me))
  } catch { /* private mode */ }
}

export function updateMe(patch: Partial<Me>) {
  if (!session.me) return
  session.me = { ...session.me, ...patch }
  try { sessionStorage.setItem(ME_KEY, JSON.stringify(session.me)) } catch { /* private mode */ }
}

export function clearToken(reason = '') {
  session.token = null
  session.me = null
  session.signedOutReason = reason
  try {
    sessionStorage.removeItem(KEY)
    sessionStorage.removeItem(ME_KEY)
  } catch { /* private mode */ }
}

export const isAdmin = () => !session.me || session.me.policies.some((p) => p === 'root' || p === 'admin')
export const isReadOnly = () => !!session.me?.policies.includes('read-only')
/** May see vault data (secrets, keys, storage) — not the `ssh` role. */
export const canReadVault = () => isAdmin() || isReadOnly()

export function markSealed() {
  if (session.status) session.status.sealed = true
}

export async function refreshStatus() {
  try {
    session.status = await sys.getSealStatus({})
  } finally {
    session.loaded = true
  }
}
