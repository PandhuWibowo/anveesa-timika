// Session keep-alive and idle sign-out.
//
// Session tokens expire after the policy's idle timeout (30 min in both
// profiles). While the user is active we renew at most once a minute; when
// they're not, the token lapses and they're signed out — the "automatic
// logout on inactivity" both NIST and 等保 ask for. A warning shows 2 minutes
// before the end.
import { authApi } from './api'
import { session, updateMe, clearToken } from './session.svelte'

export const idle = $state({ secondsLeft: null as number | null })

let lastActivity = Date.now()
let lastRenew = 0
let timer: ReturnType<typeof setInterval> | null = null
const EVENTS = ['mousemove', 'mousedown', 'keydown', 'scroll', 'touchstart'] as const

function onActivity() { lastActivity = Date.now() }

async function tick() {
  const me = session.me
  if (!session.token || !me?.expiresAt) { idle.secondsLeft = null; return }
  const now = Date.now()
  // Active within the last minute and not renewed for a minute → renew.
  if (now - lastActivity < 60_000 && now - lastRenew > 60_000) {
    lastRenew = now
    try {
      const r = await authApi.renewSelf({})
      updateMe({ expiresAt: r.expiresAt, maxExpiresAt: r.maxExpiresAt })
    } catch { /* unauthenticated already signs us out (api.ts) */ }
  }
  const left = Math.floor((new Date(session.me!.expiresAt!).getTime() - Date.now()) / 1000)
  idle.secondsLeft = left
  if (left <= 0) {
    const max = session.me?.maxExpiresAt && new Date(session.me.maxExpiresAt).getTime() <= Date.now()
    clearToken(max ? 'Your session reached its maximum length — please sign in again.' : 'Signed out after inactivity.')
  }
}

export function startIdleWatch() {
  stopIdleWatch()
  EVENTS.forEach((e) => window.addEventListener(e, onActivity, { passive: true }))
  timer = setInterval(tick, 5_000)
  tick()
}

export function stopIdleWatch() {
  EVENTS.forEach((e) => window.removeEventListener(e, onActivity))
  if (timer) clearInterval(timer)
  timer = null
}

/** "I'm still here" from the warning banner. */
export function stayActive() {
  lastActivity = Date.now()
  lastRenew = 0
  tick()
}
