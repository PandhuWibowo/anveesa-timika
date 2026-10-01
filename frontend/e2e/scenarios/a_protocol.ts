import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { main } from '../fixtures'
import { startNode, initVault, health, web, grpc, ok, eq, fails, waitFor, Code } from '../lib'
import { HealthCheckResponse_ServingStatus as Serving } from '../gen/grpc/health/v1/health_pb'

const uninit = fixture(() => startNode())
const sealedVault = fixture(async () => {
  const n = await startNode()
  return initVault(n)
})

/** A raw gRPC-Web unary call (5-byte frame header + message), for malformed / unknown calls. */
async function rawGrpcWeb(url: string, path: string, body: Uint8Array, headers: Record<string, string> = {}) {
  const frame = new Uint8Array(5 + body.length)
  new DataView(frame.buffer).setUint32(1, body.length)
  frame.set(body, 5)
  const r = await fetch(`${url}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/grpc-web+proto', 'x-grpc-web': '1', ...headers },
    body: frame,
  })
  const buf = new Uint8Array(await r.arrayBuffer())
  // grpc-status lives in a header (trailers-only) or in the trailer frame of the body.
  let status = r.headers.get('grpc-status')
  if (status === null) {
    const text = new TextDecoder().decode(buf)
    status = /grpc-status:\s*(\d+)/i.exec(text)?.[1] ?? null
  }
  return { http: r.status, status: status === null ? null : Number(status), contentType: r.headers.get('content-type'), headers: r.headers }
}

scenario('A', 'health is 501 on an uninitialized node', async () => {
  const h = await health(await uninit())
  eq(h.status, 501, 'status')
  eq(h.body.initialized, false, 'initialized')
})

scenario('A', 'health ?uninitok=true turns 501 into 200', async () => {
  eq((await health(await uninit(), '?uninitok=true')).status, 200, 'status')
})

scenario('A', 'health is 503 while sealed', async () => {
  const v = await sealedVault()
  const h = await health(v.node)
  eq(h.status, 503, 'status')
  eq(h.body.sealed, true, 'sealed')
})

scenario('A', 'health ?sealedok=true turns 503 into 200', async () => {
  eq((await health((await sealedVault()).node, '?sealedok=true')).status, 200, 'status')
})

scenario('A', 'health is 200 on the unsealed leader, with engine and node', async () => {
  const m = await main()
  const h = await health(m.node)
  eq(h.status, 200, 'status')
  eq(h.body.standby, false, 'standby')
  eq((h.body.storage as any).engine, 'raft', 'engine')
  eq(h.body.node, m.node.name, 'node')
})

scenario('A', 'health reports a semver version', async () => {
  ok(/^\d+\.\d+\.\d+/.test(String((await health(await main().then((m) => m.node))).body.version)), 'version')
})

scenario('A', 'grpc.health.v1 says NOT_SERVING while sealed', async () => {
  const v = await sealedVault()
  await waitFor('NOT_SERVING', async () => (await grpc(v.node).health.check({})).status === Serving.NOT_SERVING, 6000)
})

scenario('A', 'grpc.health.v1 says SERVING once unsealed', async () => {
  const m = await main()
  await waitFor('SERVING', async () => (await grpc(m.node).health.check({})).status === Serving.SERVING, 6000)
})

scenario('A', 'grpc.health.v1 answers over gRPC-Web too (browser-based probes)', async () => {
  const m = await main()
  await waitFor('SERVING via gRPC-Web', async () => (await web(m.node).health.check({})).status === Serving.SERVING, 6000)
})

scenario('A', 'native gRPC (HTTP/2) and gRPC-Web return the same seal status', async () => {
  const m = await main()
  const [a, b] = await Promise.all([grpc(m.node).sys.getSealStatus({}), web(m.node).sys.getSealStatus({})])
  eq([a.initialized, a.sealed, a.t, a.n, a.node], [b.initialized, b.sealed, b.t, b.n, b.node], 'status')
})

scenario('A', 'an unknown method returns UNIMPLEMENTED', async () => {
  const m = await main()
  const r = await rawGrpcWeb(m.node.url, '/timika.v1.SysService/DoesNotExist', new Uint8Array())
  eq(r.status, Code.Unimplemented, 'grpc-status')
})

scenario('A', 'an unknown service returns UNIMPLEMENTED, not the SPA', async () => {
  const m = await main()
  const r = await rawGrpcWeb(m.node.url, '/timika.v1.NoSuchService/Call', new Uint8Array())
  eq(r.status, Code.Unimplemented, 'grpc-status')
  ok(!String(r.contentType).includes('text/html'), `content-type was ${r.contentType}`)
})

scenario('A', 'a malformed protobuf body is rejected and the server stays up', async () => {
  const m = await main()
  const r = await rawGrpcWeb(m.node.url, '/timika.v1.SysService/Unseal', new Uint8Array([0xff, 0xff, 0xff, 0xff, 0x0f]))
  ok(r.status !== null && r.status !== 0, `grpc-status ${r.status}`)
  eq((await health(m.node)).status, 200, 'still healthy')
})

scenario('A', 'CORS preflight for gRPC-Web is allowed', async () => {
  const m = await main()
  const r = await fetch(`${m.node.url}/timika.v1.SysService/GetSealStatus`, {
    method: 'OPTIONS',
    headers: { origin: 'https://ui.example', 'access-control-request-method': 'POST', 'access-control-request-headers': 'content-type,x-grpc-web,x-timika-token' },
  })
  ok(r.status < 300, `status ${r.status}`)
  ok(r.headers.get('access-control-allow-origin'), 'allow-origin')
})

scenario('A', 'CORS exposes the grpc-status header to browsers', async () => {
  const m = await main()
  const r = await rawGrpcWeb(m.node.url, '/timika.v1.SysService/GetSealStatus', new Uint8Array(), { origin: 'https://ui.example' })
  const exposed = (r.headers.get('access-control-expose-headers') ?? '').toLowerCase()
  ok(exposed === '*' || exposed.includes('grpc-status'), `expose-headers: "${exposed}"`)
})

scenario('A', 'the SPA is served for / and for client-side routes', async () => {
  const m = await main()
  mkdirSync(join(m.node.dir, 'static'), { recursive: true })
  writeFileSync(join(m.node.dir, 'static', 'index.html'), '<!doctype html><title>timika</title>')
  for (const p of ['/', '/secrets/app/db']) {
    const r = await fetch(`${m.node.url}${p}`)
    eq(r.status, 200, `${p} status`)
    ok((await r.text()).includes('<title>timika</title>'), `${p} body`)
  }
})

scenario('A', 'an unknown /v1 path is a 404, not the SPA', async () => {
  const m = await main()
  const r = await fetch(`${m.node.url}/v1/kv/data/app`)
  eq(r.status, 404, 'status')
})

scenario('A', '200 concurrent gRPC-Web calls all succeed', async () => {
  const m = await main()
  const c = web(m.node, m.root)
  const rs = await Promise.allSettled(Array.from({ length: 200 }, () => c.sys.getKeyStatus({})))
  eq(rs.filter((r) => r.status === 'rejected').length, 0, 'failures')
})

scenario('A', 'responses carry an x-timika-request-id for correlation with the audit log', async () => {
  const m = await main()
  let id: string | null = null
  await web(m.node, m.root).sys.getKeyStatus({}, { onHeader: (h) => (id = h.get('x-timika-request-id')) })
  ok(id && /^[0-9a-f]{24}$/.test(id), `request id ${id}`)
})

scenario('A', 'a call to a sealed node fails with FAILED_PRECONDITION "vault is sealed"', async () => {
  const v = await sealedVault()
  await fails(web(v.node, v.root).kv.list({ folder: '' }), Code.FailedPrecondition, 'sealed')
})
