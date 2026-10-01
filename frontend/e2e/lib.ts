// End-to-end harness: real backend processes (fresh Raft data per node), real
// clients — gRPC-Web (what the UI uses), native gRPC (HTTP/2), plain HTTP, the
// terminal WebSocket, and the `timika operator` CLI.

import { createHash } from 'node:crypto'
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync, existsSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createClient, Code, ConnectError, type Interceptor, type Transport } from '@connectrpc/connect'
import { createGrpcWebTransport } from '@connectrpc/connect-web'
import { createGrpcTransport } from '@connectrpc/connect-node'
import { SysService } from '../src/gen/timika/v1/sys_pb'
import { KvService } from '../src/gen/timika/v1/kv_pb'
import { AuthService } from '../src/gen/timika/v1/auth_pb'
import { BastionService } from '../src/gen/timika/v1/bastion_pb'
import { ClusterService } from '../src/gen/timika/v1/cluster_pb'
import { AuditService } from '../src/gen/timika/v1/audit_pb'
import { AutomationService } from '../src/gen/timika/v1/automation_pb'
import { Health } from './gen/grpc/health/v1/health_pb'

export { Code, ConnectError }

export const ROOT = resolve(import.meta.dir, '../..')
export const BIN = join(ROOT, 'backend/target/debug/anveesa-timika-backend')
export const SSH_BIN = join(ROOT, 'backend/target/debug/examples/ssh_target')
export const WORK = process.env.E2E_DIR ?? mkdtempSync(join(tmpdir(), 'timika-e2e-'))
mkdirSync(WORK, { recursive: true })

export const STRONG = 'Correct-Horse-Battery-9!'
export const STRONG2 = 'Another-Long-Passphrase-7#'

// ─── assertions ──────────────────────────────────────────────────────────────

export class AssertError extends Error {}

export function ok(cond: unknown, msg = 'assertion failed'): asserts cond {
  if (!cond) throw new AssertError(msg)
}

/** Deep equality on the JSON form, ignoring object key order. */
export function eq<T>(actual: T, expected: T, what = 'value') {
  const a = j(actual), e = j(expected)
  if (a !== e) throw new AssertError(`${what}: expected ${e}, got ${a}`)
}

const big = (_: string, v: unknown) => (typeof v === 'bigint' ? v.toString() : v)

/** Canonical JSON: bigint-safe, object keys sorted. */
export function j(v: unknown): string {
  return JSON.stringify(v, (k, x) => {
    x = big(k, x)
    if (x && typeof x === 'object' && !Array.isArray(x) && !(x instanceof Uint8Array)) {
      return Object.fromEntries(Object.keys(x).sort().map((key) => [key, (x as Record<string, unknown>)[key]]))
    }
    return x
  })
}

/** Expect a gRPC failure with `code` (and optionally a message fragment). */
export async function fails(p: Promise<unknown>, code: Code, fragment?: string): Promise<ConnectError> {
  try {
    await p
  } catch (e) {
    const err = ConnectError.from(e)
    if (err.code !== code) throw new AssertError(`expected ${Code[code]}, got ${Code[err.code]}: ${err.rawMessage}`)
    if (fragment && !err.rawMessage.toLowerCase().includes(fragment.toLowerCase())) {
      throw new AssertError(`expected message containing "${fragment}", got "${err.rawMessage}"`)
    }
    return err
  }
  throw new AssertError(`expected ${Code[code]}, but the call succeeded`)
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

export async function waitFor<T>(what: string, fn: () => Promise<T | undefined | false>, ms = 15000): Promise<T> {
  const end = Date.now() + ms
  let last: unknown
  while (Date.now() < end) {
    try {
      const v = await fn()
      if (v) return v
    } catch (e) {
      last = e
    }
    await sleep(150)
  }
  throw new AssertError(`timed out waiting for ${what}${last ? ` (last error: ${errText(last)})` : ''}`)
}

export const errText = (e: unknown) => (e instanceof ConnectError ? `${Code[e.code]}: ${e.rawMessage}` : e instanceof Error ? e.message : String(e))

// ─── ports & processes ───────────────────────────────────────────────────────

export function freePort(): Promise<number> {
  return new Promise((res, rej) => {
    const s = createServer()
    s.listen(0, '127.0.0.1', () => {
      const p = (s.address() as { port: number }).port
      s.close(() => res(p))
    })
    s.on('error', rej)
  })
}

const procs = new Set<{ kill: () => void }>()
export function killAll() {
  for (const p of procs) p.kill()
  procs.clear()
}
process.on('exit', killAll)

export type Node = {
  name: string
  url: string
  api: number
  cluster: number
  dir: string
  env: Record<string, string>
  stop: () => Promise<void>
  start: () => Promise<void>
  log: () => string
  auditFile: string
}

let nodeSeq = 0

/** Start a backend on fresh ports and a fresh data dir. `env` overrides defaults. */
export async function startNode(env: Record<string, string> = {}, opts: { name?: string; ports?: [number, number]; wait?: boolean } = {}): Promise<Node> {
  const name = opts.name ?? `n${++nodeSeq}`
  const [api, cluster] = opts.ports ?? [await freePort(), await freePort()]
  const dir = join(WORK, `${name}-${api}`)
  mkdirSync(dir, { recursive: true })
  const scheme = env.TLS_CERT_FILE ? 'https' : 'http'
  const fullEnv: Record<string, string> = {
    PATH: process.env.PATH ?? '',
    HOME: process.env.HOME ?? '',
    STORAGE: 'raft',
    RAFT_NODE_ID: name,
    RAFT_PATH: join(dir, 'raft'),
    BIND_ADDR: `127.0.0.1:${api}`,
    CLUSTER_BIND_ADDR: `127.0.0.1:${cluster}`,
    API_ADDR: `${scheme}://127.0.0.1:${api}`,
    CLUSTER_ADDR: `${scheme}://127.0.0.1:${cluster}`,
    LOG_DIR: join(dir, 'logs'),
    LOG_FORMAT: 'json',
    LOG_LEVEL: 'info,anveesa_timika_backend=debug,openraft=warn',
    AUDIT_FILE: join(dir, 'logs', 'audit-{instance}.log'),
    LOGIN_RATE_LIMIT: '100000',
    CAPTCHA_PROVIDER: 'pow',
    CAPTCHA_POW_MAX_NUMBER: '2000',
    SESSION_RETENTION_DAYS: '90',
    ...env,
  }
  let proc: ReturnType<typeof Bun.spawn> | null = null
  const out: string[] = []
  const node: Node = {
    name, api, cluster, dir, env: fullEnv,
    url: `${scheme}://127.0.0.1:${api}`,
    auditFile: join(dir, 'logs', `audit-${name}.log`),
    log: () => out.join(''),
    async start() {
      proc = Bun.spawn([BIN], { cwd: dir, env: fullEnv, stdout: 'pipe', stderr: 'pipe' })
      const p = proc
      const handle = { kill: () => p.kill() }
      procs.add(handle)
      p.exited.then(() => procs.delete(handle))
      for (const s of [p.stdout, p.stderr] as ReadableStream<Uint8Array>[]) {
        ;(async () => {
          const dec = new TextDecoder()
          for await (const chunk of s as unknown as AsyncIterable<Uint8Array>) out.push(dec.decode(chunk))
        })()
      }
      if (opts.wait === false) return
      await waitFor(`${name} to listen`, async () => {
        if (p.exitCode !== null) throw new AssertError(`${name} exited (${p.exitCode}): ${out.join('').slice(-800)}`)
        const r = await fetch(`${node.url}/v1/sys/health?uninitok=true&sealedok=true&standbyok=true`, { tls: { rejectUnauthorized: false } } as RequestInit).catch(() => null)
        return r !== null
      }, 20000)
    },
    async stop() {
      if (!proc) return
      proc.kill()
      await proc.exited
      proc = null
    },
  }
  await node.start()
  return node
}

// ─── clients ─────────────────────────────────────────────────────────────────

function tokenInterceptor(token?: string): Interceptor {
  return (next) => (req) => {
    if (token) req.header.set('x-timika-token', token)
    return next(req)
  }
}

export type Clients = ReturnType<typeof clientsFor>

function clientsFor(t: Transport) {
  return {
    sys: createClient(SysService, t),
    kv: createClient(KvService, t),
    auth: createClient(AuthService, t),
    bastion: createClient(BastionService, t),
    cluster: createClient(ClusterService, t),
    audit: createClient(AuditService, t),
    automation: createClient(AutomationService, t),
    health: createClient(Health, t),
  }
}

/** gRPC-Web clients (the browser path). */
export const web = (n: Node | string, token?: string) =>
  clientsFor(createGrpcWebTransport({ baseUrl: typeof n === 'string' ? n : n.url, interceptors: [tokenInterceptor(token)] }))

/** Native gRPC over HTTP/2 (services, CLI). */
export const grpc = (n: Node | string, token?: string) =>
  clientsFor(
    createGrpcTransport({
      baseUrl: typeof n === 'string' ? n : n.url,
      interceptors: [tokenInterceptor(token)],
      nodeOptions: { rejectUnauthorized: false },
    }),
  )

// ─── vault helpers ───────────────────────────────────────────────────────────

export type Vault = { node: Node; keys: string[]; root: string; threshold: number }

export async function initVault(node: Node, shares = 3, threshold = 2): Promise<Vault> {
  const r = await web(node).sys.init({ secretShares: shares, secretThreshold: threshold })
  return { node, keys: r.keys, root: r.rootToken, threshold }
}

export async function unseal(v: Vault, node: Node = v.node) {
  const c = web(node)
  for (const k of v.keys.slice(0, v.threshold)) await c.sys.unseal({ key: k })
  await waitFor('unsealed + leader', async () => {
    const h = await fetch(`${node.url}/v1/sys/health`, { tls: { rejectUnauthorized: false } } as RequestInit)
    return h.status === 200
  })
}

export async function freshVault(env: Record<string, string> = {}, shares = 3, threshold = 2): Promise<Vault> {
  const node = await startNode(env)
  const v = await initVault(node, shares, threshold)
  await unseal(v)
  return v
}

/** Solve the built-in proof-of-work captcha like the browser does. */
export async function solveCaptcha(n: Node | string) {
  const c = await web(n).auth.getCaptchaChallenge({})
  for (let i = 0; i <= Number(c.maxnumber); i++) {
    if (createHash('sha256').update(c.salt + i).digest('hex') === c.challenge) {
      const payload = { algorithm: c.algorithm, challenge: c.challenge, number: i, salt: c.salt, signature: c.signature }
      return { provider: 'pow', token: Buffer.from(JSON.stringify(payload)).toString('base64') }
    }
  }
  throw new AssertError('could not solve captcha')
}

export async function login(n: Node | string, username: string, password: string) {
  return web(n).auth.login({ username, password, captcha: await solveCaptcha(n), bannerAck: true })
}

/** Create a user who can sign in straight away (no forced password change). */
export async function createUser(v: Vault, username: string, policies: string[] = ['read-only'], password = STRONG) {
  await web(v.node, v.root).auth.upsertUser({ username, password, policies, mustChangePassword: false })
  const r = await login(v.node, username, password)
  return r.token
}

export async function health(n: Node, query = '') {
  const r = await fetch(`${n.url}/v1/sys/health${query}`, { tls: { rejectUnauthorized: false } } as RequestInit)
  return { status: r.status, body: (await r.json().catch(() => ({}))) as Record<string, unknown> }
}

export function auditEntries(n: Node): Record<string, any>[] {
  if (!existsSync(n.auditFile)) return []
  return readFileSync(n.auditFile, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l))
}

// ─── CLI ─────────────────────────────────────────────────────────────────────

export async function cli(args: string[], env: Record<string, string> = {}, stdin?: string) {
  const p = Bun.spawn([BIN, 'operator', ...args], {
    env: { PATH: process.env.PATH ?? '', HOME: process.env.HOME ?? '', ...env },
    stdin: stdin === undefined ? 'ignore' : new TextEncoder().encode(stdin),
    stdout: 'pipe',
    stderr: 'pipe',
    cwd: WORK,
  })
  const [out, err] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text()])
  return { code: await p.exited, out, err }
}

// ─── SSH target + web terminal ───────────────────────────────────────────────

export type SshTarget = { port: number; user: string; password: string; hostKey: string; stop: () => Promise<void>; start: () => Promise<void>; privateKey: string; publicKey: string; files: string }

export function sshKeygen(path: string, type = 'ed25519', passphrase = '') {
  if (!existsSync(path)) {
    const r = Bun.spawnSync(['ssh-keygen', '-q', '-t', type, '-N', passphrase, '-f', path, '-C', 'e2e'])
    if (r.exitCode !== 0) throw new Error(`ssh-keygen failed: ${r.stderr}`)
  }
  return { privateKey: readFileSync(path, 'utf8'), publicKey: readFileSync(`${path}.pub`, 'utf8') }
}

export async function startSsh(opts: { user?: string; password?: string; port?: number; hostKeyName?: string } = {}): Promise<SshTarget> {
  const port = opts.port ?? (await freePort())
  const user = opts.user ?? 'deploy'
  const password = opts.password ?? 'target-pass'
  const keyDir = join(WORK, 'ssh')
  mkdirSync(keyDir, { recursive: true })
  const hostKey = join(keyDir, opts.hostKeyName ?? 'host_ed25519')
  sshKeygen(hostKey)
  const client = sshKeygen(join(keyDir, 'client_ed25519'))
  // What the server serves over SFTP (as `/`).
  const files = join(WORK, `sftp-${port}`)
  mkdirSync(files, { recursive: true })
  let proc: ReturnType<typeof Bun.spawn> | null = null
  const t: SshTarget = {
    port, user, password, hostKey, ...client, files,
    async start() {
      proc = Bun.spawn([SSH_BIN, String(port), hostKey, user, password, join(keyDir, 'client_ed25519.pub'), files], { stdout: 'pipe', stderr: 'pipe' })
      const p = proc
      procs.add({ kill: () => p.kill() })
      await waitFor('ssh target', async () => {
        const s = await Bun.connect({ hostname: '127.0.0.1', port, socket: { data() {} } }).catch(() => null)
        s?.end()
        return !!s
      })
    },
    async stop() {
      proc?.kill()
      await proc?.exited
      proc = null
    },
  }
  await t.start()
  return t
}

export type Term = {
  ws: WebSocket
  events: Record<string, any>[]
  output: () => string
  send: (s: string) => void
  resize: (cols: number, rows: number) => void
  waitOutput: (needle: string, ms?: number) => Promise<string>
  waitEvent: (type: string, ms?: number) => Promise<Record<string, any>>
  closed: Promise<{ code: number; reason: string }>
  close: () => void
}

/** Open the web terminal like the browser does (token as a subprotocol). */
export function terminal(n: Node, token: string | null, q: Record<string, string | number>): Promise<Term> {
  const qs = new URLSearchParams(Object.entries(q).map(([k, v]) => [k, String(v)])).toString()
  const url = `${n.url.replace(/^http/, 'ws')}/v1/bastion/connect?${qs}`
  const ws = new WebSocket(url, token ? ['timika', token] : ['timika'])
  ws.binaryType = 'arraybuffer'
  const events: Record<string, any>[] = []
  let out = ''
  const dec = new TextDecoder()
  let resolveClosed!: (v: { code: number; reason: string }) => void
  const closed = new Promise<{ code: number; reason: string }>((r) => (resolveClosed = r))
  ws.onmessage = (m) => {
    if (typeof m.data === 'string') events.push(JSON.parse(m.data))
    else out += dec.decode(new Uint8Array(m.data as ArrayBuffer), { stream: true })
  }
  ws.onclose = (e) => resolveClosed({ code: e.code, reason: e.reason })
  const term: Term = {
    ws, events, closed,
    output: () => out,
    send: (s) => ws.send(new TextEncoder().encode(s)),
    resize: (cols, rows) => ws.send(JSON.stringify({ type: 'resize', cols, rows })),
    waitOutput: (needle, ms = 8000) => waitFor(`terminal output "${needle}" (have: ${JSON.stringify(out.slice(-200))})`, async () => out.includes(needle) && out, ms),
    waitEvent: (type, ms = 8000) => waitFor(`terminal event ${type} (have: ${JSON.stringify(events)})`, async () => events.find((e) => e.type === type), ms),
    close: () => ws.close(),
  }
  return new Promise((res, rej) => {
    ws.onopen = () => res(term)
    ws.onerror = () => rej(new AssertError('websocket refused'))
  })
}

/** The HTTP status of a WebSocket handshake that is expected to be refused. */
export async function wsRefusal(n: Node, token: string | null, q: Record<string, string>) {
  const qs = new URLSearchParams(q).toString()
  const headers: Record<string, string> = {
    connection: 'Upgrade',
    upgrade: 'websocket',
    'sec-websocket-version': '13',
    'sec-websocket-key': Buffer.from(crypto.getRandomValues(new Uint8Array(16))).toString('base64'),
    'sec-websocket-protocol': token ? `timika, ${token}` : 'timika',
  }
  const r = await fetch(`${n.url}/v1/bastion/connect?${qs}`, { headers })
  return { status: r.status, body: await r.text() }
}

export function writeJson(path: string, v: unknown) {
  writeFileSync(path, JSON.stringify(v, big, 2))
}
