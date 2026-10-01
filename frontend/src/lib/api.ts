import { createClient, Code, ConnectError, type Interceptor } from '@connectrpc/connect'
import { createGrpcWebTransport } from '@connectrpc/connect-web'
import { SysService } from '../gen/timika/v1/sys_pb'
import { KvService } from '../gen/timika/v1/kv_pb'
import { AuthService } from '../gen/timika/v1/auth_pb'
import { BastionService } from '../gen/timika/v1/bastion_pb'
import { ClusterService } from '../gen/timika/v1/cluster_pb'
import { AuditService } from '../gen/timika/v1/audit_pb'
import { session, clearToken, markSealed } from './session.svelte'

export { Code, ConnectError }

/** Attach the vault token, and react to the two global states every page
 *  cares about: sealed and unauthenticated. */
const auth: Interceptor = (next) => async (req) => {
  if (session.token && !req.header.has('x-timika-token')) req.header.set('x-timika-token', session.token)
  try {
    return await next(req)
  } catch (e) {
    const err = ConnectError.from(e)
    if (err.code === Code.FailedPrecondition && err.rawMessage === 'vault is sealed') markSealed()
    if (err.code === Code.Unauthenticated && session.token) clearToken(err.rawMessage || 'signed out')
    throw err
  }
}

// gRPC-Web over same-origin HTTP/1.1 (the backend speaks it natively; Vite
// proxies it in dev). Binary protobuf on the wire.
const transport = createGrpcWebTransport({ baseUrl: location.origin, interceptors: [auth] })

export const sys = createClient(SysService, transport)
export const kv = createClient(KvService, transport)
export const authApi = createClient(AuthService, transport)
export const bastion = createClient(BastionService, transport)
export const cluster = createClient(ClusterService, transport)
export const audit = createClient(AuditService, transport)

export const isCode = (e: unknown, code: Code) => e instanceof ConnectError && e.code === code

/** The browser couldn't reach the server at all (restart, network blip). */
export const isNetworkError = (e: unknown) =>
  (e instanceof ConnectError && /failed to fetch|networkerror|load failed|network connection/i.test(e.rawMessage)) ||
  (e instanceof TypeError && /fetch|network/i.test(e.message))

export const errMsg = (e: unknown) =>
  isNetworkError(e)
    ? "Can't reach the server — check your connection; it retries on its own."
    : e instanceof ConnectError ? e.rawMessage || Code[e.code] : e instanceof Error ? e.message : String(e)
