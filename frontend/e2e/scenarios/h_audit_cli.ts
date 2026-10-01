import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { createGrpcWebTransport } from '@connectrpc/connect-web'
import { createClient } from '@connectrpc/connect'
import { scenario, fixture } from '../suite'
import { main, uniq } from '../fixtures'
import { SysService } from '../../src/gen/timika/v1/sys_pb'
import { startNode, freshVault, web, login, cli, auditEntries, freePort, ok, eq, fails, Code, STRONG, WORK } from '../lib'

/** Self-signed cert for 127.0.0.1 (its own CA). */
const cert = fixture(async () => {
  const dir = join(WORK, 'tls')
  // A CA, and a server certificate for 127.0.0.1 signed by it (rustls rejects a
  // CA certificate used directly as the server's).
  const script = [
    `mkdir -p ${dir} && cd ${dir}`,
    `openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 2 -subj /CN=e2e-ca -keyout ca.key -out ca.pem`,
    `openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -subj /CN=127.0.0.1 -keyout key.pem -out server.csr`,
    `printf 'subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' > ext.cnf`,
    `openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 2 -extfile ext.cnf -out cert.pem`,
  ].join(' && ')
  const r = Bun.spawnSync(['sh', '-c', script])
  if (r.exitCode !== 0) throw new Error(`openssl: ${r.stderr}`)
  return { cert: join(dir, 'cert.pem'), key: join(dir, 'key.pem'), ca: join(dir, 'ca.pem') }
})

const audited = async () => {
  const m = await main()
  return { m, entries: () => auditEntries(m.node) }
}

scenario('H', 'every audited call has a request and a response entry with the same id', async () => {
  const { m, entries } = await audited()
  await web(m.node, m.root).sys.getKeyStatus({})
  const all = entries()
  const reqs = all.filter((e) => e.type === 'request' && e.request?.rpc === 'timika.v1.SysService/GetKeyStatus')
  const last = reqs.at(-1)!
  const resp = all.find((e) => e.type === 'response' && e.id === last.id)
  ok(resp && resp.response.grpc_status === 0, `response ${JSON.stringify(resp)}`)
  eq(last.request.protocol, 'grpc-web', 'protocol')
})

scenario('H', "tokens appear only as HMACs matching AuditHash; never in the clear", async () => {
  const { m, entries } = await audited()
  await web(m.node, m.reader).kv.list({ folder: '' }).catch(() => {})
  const h = (await web(m.node, m.root).sys.auditHash({ input: m.reader })).hash
  ok(entries().some((e) => e.auth?.token_hmac === h), 'hmac found')
  const raw = readFileSync(m.node.auditFile, 'utf8')
  ok(!raw.includes(m.reader) && !raw.includes(m.root), 'raw token in audit log')
})

scenario('H', 'failed calls are audited with their gRPC status and message', async () => {
  const { m, entries } = await audited()
  await web(m.node, 'tmk.bogus').kv.list({ folder: '' }).catch(() => {})
  const e = entries().filter((x) => x.type === 'response' && x.request?.rpc === 'timika.v1.KvService/List').at(-1)
  eq([e?.response.grpc_status, e?.response.error], [Code.Unauthenticated, 'permission denied'], 'audited failure')
})

scenario('H', 'probe/polling calls are not audited', async () => {
  const { m, entries } = await audited()
  for (let i = 0; i < 3; i++) await web(m.node).sys.getSealStatus({})
  await fetch(`${m.node.url}/v1/sys/health`)
  ok(!entries().some((e) => e.request?.rpc === 'timika.v1.SysService/GetSealStatus' || e.request?.path === '/v1/sys/health'), 'polling audited')
})

scenario('H', 'a sign-in is audited with who signed in, but never the password', async () => {
  const { m, entries } = await audited()
  const name = uniq('auditee')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG, mustChangePassword: false })
  await login(m.node, name, STRONG)
  const e = entries().filter((x) => x.type === 'response' && x.request?.rpc === 'timika.v1.AuthService/Login').at(-1)
  eq(e?.auth.display_name, name, 'display name')
  ok(!readFileSync(m.node.auditFile, 'utf8').includes(STRONG), 'password in audit log')
})

scenario('H', '`operator audit-verify` confirms an intact hash chain', async () => {
  const { m } = await audited()
  const r = await cli(['audit-verify', m.node.auditFile])
  eq(r.code, 0, `exit (${r.out}${r.err})`)
  ok(r.out.includes('hash chain intact'), r.out)
})

scenario('H', '`operator audit-verify` detects an edited entry', async () => {
  const v = await freshVault()
  await web(v.node, v.root).kv.write({ path: 'tamper', data: { a: 1 } })
  const copy = join(v.node.dir, 'tampered.log')
  const lines = readFileSync(v.node.auditFile, 'utf8').split('\n')
  const i = lines.findIndex((l) => l.includes('KvService/Write'))
  lines[i] = lines[i].replace('KvService/Write', 'KvService/Read')
  writeFileSync(copy, lines.join('\n'))
  const r = await cli(['audit-verify', copy])
  eq(r.code, 1, 'exit')
  ok(r.out.includes('TAMPERED'), r.out)
})

scenario('H', 'fail-closed: with no working audit sink, audited calls are refused (UNAVAILABLE)', async () => {
  const dead = await freePort()
  const n = await startNode({ AUDIT_FILE: '', AUDIT_SYSLOG: `tcp://127.0.0.1:${dead}` })
  // Even init: nothing that changes the vault may go unrecorded.
  await fails(web(n).sys.init({ secretShares: 1, secretThreshold: 1 }), Code.Unavailable, 'audit log unavailable')
  // Probes are not audited, so the node still reports its state.
  eq((await web(n).sys.getSealStatus({})).initialized, false, 'status still answers')
})

scenario('H', '`operator status`, `instances` and `health` talk to a node', async () => {
  const { m } = await audited()
  const s = await cli(['status', '--addr', m.node.url])
  ok(s.code === 0 && s.out.includes('sealed:      false'), s.out + s.err)
  const i = await cli(['instances', '--addr', m.node.url])
  ok(i.code === 0 && i.out.includes(m.node.name), i.out + i.err)
  eq((await cli(['health', '--addr', m.node.url])).code, 0, 'health exit')
  eq((await cli(['health', '--addr', `http://127.0.0.1:${await freePort()}`])).code, 1, 'health exit for a dead node')
})

scenario('H', '`operator` with an unknown command exits 2', async () => {
  const r = await cli(['frobnicate'])
  eq(r.code, 2, 'exit')
  ok(r.err.includes('unknown command'), r.err)
})

scenario('H', '`operator seal` needs $TIMIKA_TOKEN', async () => {
  const v = await freshVault()
  const no = await cli(['seal', '--addr', v.node.url])
  ok(no.code !== 0 && no.err.includes('missing token'), no.err)
  const yes = await cli(['seal', '--addr', v.node.url], { TIMIKA_TOKEN: v.root })
  eq(yes.code, 0, `exit (${yes.err})`)
  eq((await web(v.node).sys.getSealStatus({})).sealed, true, 'sealed')
})

scenario('H', '`operator init` + `unseal` (key on stdin) bring up a vault; a refused key exits 1', async () => {
  const n = await startNode()
  const init = await cli(['init', '--shares', '2', '--threshold', '2', '--addr', n.url])
  eq(init.code, 0, `init exit (${init.err})`)
  const keys = [...init.out.matchAll(/unseal key \d+: (\S+)/g)].map((x) => x[1])
  eq(keys.length, 2, 'keys printed')
  const bad = await cli(['unseal', 'AA==', '--addr', n.url])
  eq(bad.code, 1, `a refused key exits 1 (${bad.out})`)
  for (const k of keys) eq((await cli(['unseal', '--addr', n.url], {}, `${k}\n`)).code, 0, 'unseal exit')
  eq((await web(n).sys.getSealStatus({})).sealed, false, 'sealed')
})

scenario('H', 'TLS: the CLI and browsers need the CA, or --tls-skip-verify for the CLI', async () => {
  const c = await cert()
  const n = await startNode({ TLS_CERT_FILE: c.cert, TLS_KEY_FILE: c.key })
  ok(n.url.startsWith('https://'), n.url)
  const untrusted = await cli(['status', '--addr', n.url])
  ok(untrusted.code !== 0, `expected failure without the CA: ${untrusted.out}`)
  const withCa = await cli(['init', '--shares', '1', '--threshold', '1', '--addr', n.url], { TLS_CA_FILE: c.ca })
  eq(withCa.code, 0, `with CA (${withCa.err})`)
  const skip = await cli(['status', '--addr', n.url, '--tls-skip-verify'])
  ok(skip.code === 0 && skip.out.includes('initialized: true'), `skip-verify (${skip.err})`)
  const ca = readFileSync(c.ca, 'utf8')
  const sys = createClient(SysService, createGrpcWebTransport({ baseUrl: n.url, fetch: ((u: string, init: RequestInit) => fetch(u, { ...init, tls: { ca } } as unknown as RequestInit)) as unknown as typeof fetch }))
  eq((await sys.getSealStatus({})).initialized, true, 'gRPC-Web over TLS')
})
