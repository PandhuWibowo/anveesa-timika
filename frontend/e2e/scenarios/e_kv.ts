import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { main, uniq } from '../fixtures'
import { freshVault, web, grpc, health, auditEntries, ok, eq, fails, Code } from '../lib'

const kvOf = async (token?: 'admin' | 'reader' | 'none') => {
  const m = await main()
  const t = token === 'none' ? undefined : token === 'reader' ? m.reader : m.root
  return { m, kv: web(m.node, t).kv }
}

/** Every file under a directory, recursively. */
function files(dir: string): string[] {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f)
    return statSync(p).isDirectory() ? files(p) : [p]
  })
}

const pruneVault = fixture(() => freshVault({ KV_MAX_VERSIONS: '3' }))

scenario('E', 'write then read a secret', async () => {
  const { kv } = await kvOf()
  const p = uniq('app/db')
  const meta = await kv.write({ path: p, data: { user: 'admin', password: 's3cret' } })
  eq(meta.currentVersion, 1, 'version')
  const s = await kv.read({ path: p })
  eq([s.data, s.version], [{ user: 'admin', password: 's3cret' }, 1], 'secret')
})

scenario('E', 'JSON types survive the round trip (numbers, bools, null, nested, arrays)', async () => {
  const { kv } = await kvOf()
  const p = uniq('types')
  const data = { i: 42, f: 3.14159, neg: -7, t: true, f2: false, n: null, nested: { a: { b: [1, 'two', { c: null }] } }, empty: {}, list: [], s: '' }
  await kv.write({ path: p, data })
  eq((await kv.read({ path: p })).data, data, 'data')
})

scenario('E', 'unicode and emoji values survive', async () => {
  const { kv } = await kvOf()
  const p = uniq('uni')
  const data = { zh: '密码', ja: 'パスワード', emoji: '🔐🗝️', rtl: 'كلمة السر', nul: 'a\u0000b', nl: 'line1\nline2' }
  await kv.write({ path: p, data })
  eq((await kv.read({ path: p })).data, data, 'data')
})

scenario('E', 'versions count up with every write', async () => {
  const { kv } = await kvOf()
  const p = uniq('ver')
  for (let i = 1; i <= 3; i++) eq((await kv.write({ path: p, data: { i } })).currentVersion, i, `write ${i}`)
})

scenario('E', 'an older version can be read', async () => {
  const { kv } = await kvOf()
  const p = uniq('old')
  await kv.write({ path: p, data: { v: 'one' } })
  await kv.write({ path: p, data: { v: 'two' } })
  eq((await kv.read({ path: p, version: 1 })).data, { v: 'one' }, 'v1')
  eq((await kv.read({ path: p })).data, { v: 'two' }, 'latest')
})

scenario('E', 'reading a secret that does not exist is NOT_FOUND', async () => {
  const { kv } = await kvOf()
  await fails(kv.read({ path: uniq('missing') }), Code.NotFound)
})

scenario('E', 'reading a version that does not exist is NOT_FOUND', async () => {
  const { kv } = await kvOf()
  const p = uniq('nover')
  await kv.write({ path: p, data: { a: 1 } })
  await fails(kv.read({ path: p, version: 9 }), Code.NotFound)
  await fails(kv.read({ path: p, version: 0 }), Code.NotFound)
})

scenario('E', 'cas=0 creates a new secret', async () => {
  const { kv } = await kvOf()
  eq((await kv.write({ path: uniq('cas0'), data: { a: 1 }, cas: 0 })).currentVersion, 1, 'version')
})

scenario('E', 'cas=0 refuses to overwrite an existing secret (ABORTED)', async () => {
  const { kv } = await kvOf()
  const p = uniq('cas0b')
  await kv.write({ path: p, data: { a: 1 } })
  await fails(kv.write({ path: p, data: { a: 2 }, cas: 0 }), Code.Aborted, 'check-and-set')
  eq((await kv.read({ path: p })).data, { a: 1 }, 'unchanged')
})

scenario('E', 'cas=current succeeds', async () => {
  const { kv } = await kvOf()
  const p = uniq('casok')
  await kv.write({ path: p, data: { a: 1 } })
  eq((await kv.write({ path: p, data: { a: 2 }, cas: 1 })).currentVersion, 2, 'version')
})

scenario('E', 'a stale cas is ABORTED', async () => {
  const { kv } = await kvOf()
  const p = uniq('casold')
  await kv.write({ path: p, data: { a: 1 } })
  await kv.write({ path: p, data: { a: 2 } })
  await fails(kv.write({ path: p, data: { a: 3 }, cas: 1 }), Code.Aborted)
})

scenario('E', '`..` in a path is refused', async () => {
  const { kv } = await kvOf()
  await fails(kv.write({ path: 'app/../etc', data: { a: 1 } }), Code.InvalidArgument, 'invalid path segment')
})

scenario('E', 'an empty path is refused', async () => {
  const { kv } = await kvOf()
  await fails(kv.write({ path: '', data: { a: 1 } }), Code.InvalidArgument, 'empty')
  await fails(kv.write({ path: '///', data: { a: 1 } }), Code.InvalidArgument, 'empty')
})

scenario('E', 'spaces in a path are refused', async () => {
  const { kv } = await kvOf()
  await fails(kv.write({ path: 'my secret', data: { a: 1 } }), Code.InvalidArgument)
})

scenario('E', 'leading and trailing slashes are ignored', async () => {
  const { kv } = await kvOf()
  const p = uniq('slash')
  await kv.write({ path: `/${p}/x/`, data: { a: 1 } })
  eq((await kv.read({ path: `${p}/x` })).data, { a: 1 }, 'data')
})

scenario('E', 'an empty segment (a//b) is refused', async () => {
  const { kv } = await kvOf()
  await fails(kv.write({ path: 'a//b', data: { a: 1 } }), Code.InvalidArgument)
})

scenario('E', 'non-ASCII path segments are refused', async () => {
  const { kv } = await kvOf()
  await fails(kv.write({ path: 'café/db', data: { a: 1 } }), Code.InvalidArgument)
})

scenario('E', 'a `.` segment is refused', async () => {
  const { kv } = await kvOf()
  await fails(kv.write({ path: 'a/./b', data: { a: 1 } }), Code.InvalidArgument)
})

scenario('E', 'dots, dashes and underscores are fine in names', async () => {
  const { kv } = await kvOf()
  const p = `${uniq('ok')}/db.prod-1_x`
  await kv.write({ path: p, data: { a: 1 } })
  eq((await kv.read({ path: p })).version, 1, 'version')
})

scenario('E', 'listing a folder shows sub-folders with a trailing slash and secrets', async () => {
  const { kv } = await kvOf()
  const base = uniq('list')
  await kv.write({ path: `${base}/one`, data: { a: 1 } })
  await kv.write({ path: `${base}/sub/two`, data: { a: 1 } })
  eq((await kv.list({ folder: base })).keys.sort(), ['one', 'sub/'], 'keys')
})

scenario('E', 'a listing shows only immediate children, once each', async () => {
  const { kv } = await kvOf()
  const base = uniq('deep')
  for (const p of ['a/b/c', 'a/b/d', 'a/e', 'a-f', 'a.g']) await kv.write({ path: `${base}/${p}`, data: { p } })
  eq((await kv.list({ folder: base })).keys.sort(), ['a-f', 'a.g', 'a/'], 'level 1')
  eq((await kv.list({ folder: `${base}/a` })).keys.sort(), ['b/', 'e'], 'level 2')
  eq((await kv.list({ folder: `${base}/a/b/` })).keys.sort(), ['c', 'd'], 'level 3 (trailing slash)')
})

scenario('E', 'the root listing includes top-level folders', async () => {
  const { kv } = await kvOf()
  const top = uniq('top')
  await kv.write({ path: `${top}/x`, data: { a: 1 } })
  ok((await kv.list({ folder: '' })).keys.includes(`${top}/`), 'top folder listed')
})

scenario('E', 'listing a folder that does not exist is NOT_FOUND', async () => {
  const { kv } = await kvOf()
  await fails(kv.list({ folder: uniq('nofolder') }), Code.NotFound)
})

scenario('E', 'soft delete with no versions deletes the latest', async () => {
  const { kv } = await kvOf()
  const p = uniq('sd')
  await kv.write({ path: p, data: { v: 1 } })
  await kv.write({ path: p, data: { v: 2 } })
  const meta = await kv.delete({ path: p })
  ok(meta.versions[2].deletionTime && !meta.versions[1].deletionTime, 'v2 deleted only')
  await fails(kv.read({ path: p }), Code.NotFound, 'deleted')
  eq((await kv.read({ path: p, version: 1 })).data, { v: 1 }, 'v1 still readable')
})

scenario('E', 'soft delete of chosen versions leaves the others', async () => {
  const { kv } = await kvOf()
  const p = uniq('sd2')
  for (let i = 1; i <= 3; i++) await kv.write({ path: p, data: { i } })
  await kv.delete({ path: p, versions: [1, 2] })
  await fails(kv.read({ path: p, version: 1 }), Code.NotFound)
  await fails(kv.read({ path: p, version: 2 }), Code.NotFound)
  eq((await kv.read({ path: p })).data, { i: 3 }, 'v3')
})

scenario('E', 'writing after a soft delete makes a new readable version', async () => {
  const { kv } = await kvOf()
  const p = uniq('sd3')
  await kv.write({ path: p, data: { v: 1 } })
  await kv.delete({ path: p })
  eq((await kv.write({ path: p, data: { v: 2 } })).currentVersion, 2, 'version')
  eq((await kv.read({ path: p })).data, { v: 2 }, 'data')
})

scenario('E', 'destroy removes every version and the metadata', async () => {
  const { kv } = await kvOf()
  const p = uniq('gone')
  await kv.write({ path: `${p}/s`, data: { v: 1 } })
  await kv.write({ path: `${p}/s`, data: { v: 2 } })
  await kv.destroy({ path: `${p}/s` })
  await fails(kv.getMetadata({ path: `${p}/s` }), Code.NotFound)
  await fails(kv.read({ path: `${p}/s`, version: 1 }), Code.NotFound)
  // The folder disappears with its last secret.
  ok(!(await kv.list({ folder: '' })).keys.includes(`${p}/`), 'folder gone')
})

scenario('E', 'destroying a secret that does not exist is NOT_FOUND', async () => {
  const { kv } = await kvOf()
  await fails(kv.destroy({ path: uniq('never') }), Code.NotFound)
})

scenario('E', 'old versions are pruned beyond KV_MAX_VERSIONS', async () => {
  const v = await pruneVault()
  const kv = web(v.node, v.root).kv
  for (let i = 1; i <= 5; i++) await kv.write({ path: 'pruned', data: { i } })
  const m = await kv.getMetadata({ path: 'pruned' })
  eq([m.currentVersion, m.oldestVersion, m.maxVersions, Object.keys(m.versions)], [5, 3, 3, ['3', '4', '5']], 'metadata')
  await fails(kv.read({ path: 'pruned', version: 1 }), Code.NotFound)
})

scenario('E', 'metadata carries versions and timestamps in order', async () => {
  const { kv } = await kvOf()
  const p = uniq('meta')
  await kv.write({ path: p, data: { a: 1 } })
  await kv.write({ path: p, data: { a: 2 } })
  const m = await kv.getMetadata({ path: p })
  eq([m.currentVersion, m.oldestVersion], [2, 1], 'versions')
  ok(new Date(m.createdTime) <= new Date(m.updatedTime), 'created ≤ updated')
  ok(new Date(m.versions[1].createdTime) <= new Date(m.versions[2].createdTime), 'version times')
})

scenario('E', 'read-only users can read and list', async () => {
  const { kv: admin } = await kvOf()
  const { kv } = await kvOf('reader')
  const p = uniq('ro')
  await admin.write({ path: `${p}/s`, data: { a: 1 } })
  eq((await kv.read({ path: `${p}/s` })).data, { a: 1 }, 'read')
  eq((await kv.list({ folder: p })).keys, ['s'], 'list')
})

scenario('E', 'read-only users cannot write', async () => {
  const { kv } = await kvOf('reader')
  await fails(kv.write({ path: uniq('rw'), data: { a: 1 } }), Code.PermissionDenied)
})

scenario('E', 'read-only users cannot delete or destroy', async () => {
  const { kv: admin } = await kvOf()
  const { kv } = await kvOf('reader')
  const p = uniq('rodel')
  await admin.write({ path: p, data: { a: 1 } })
  await fails(kv.delete({ path: p }), Code.PermissionDenied)
  await fails(kv.destroy({ path: p }), Code.PermissionDenied)
})

scenario('E', 'every KV call needs a token', async () => {
  const { kv } = await kvOf('none')
  await fails(kv.list({ folder: '' }), Code.Unauthenticated)
  await fails(kv.read({ path: 'a' }), Code.Unauthenticated)
  await fails(kv.write({ path: 'a', data: {} }), Code.Unauthenticated)
})

scenario('E', 'a 1 MiB value round-trips', async () => {
  const { kv } = await kvOf()
  const p = uniq('big')
  const blob = 'x'.repeat(1024 * 1024)
  await kv.write({ path: p, data: { blob } })
  eq((await kv.read({ path: p })).data?.blob === blob, true, 'blob intact')
})

scenario('E', 'an oversized (8 MiB) write is refused cleanly and the server stays up', async () => {
  const { m, kv } = await kvOf()
  const err = await kv.write({ path: uniq('huge'), data: { blob: 'x'.repeat(8 * 1024 * 1024) } }).then(() => null, (e) => e)
  ok(err, 'expected a failure')
  eq((await health(m.node)).status, 200, 'still healthy')
})

scenario('E', 'a secret with 1000 keys round-trips', async () => {
  const { kv } = await kvOf()
  const p = uniq('many')
  const data = Object.fromEntries(Array.from({ length: 1000 }, (_, i) => [`k${i}`, `v${i}`]))
  await kv.write({ path: p, data })
  eq(Object.keys((await kv.read({ path: p })).data ?? {}).length, 1000, 'keys')
})

scenario('E', '20 concurrent writes to one secret all land (no lost versions)', async () => {
  const { kv } = await kvOf()
  const p = uniq('conc')
  const rs = await Promise.allSettled(Array.from({ length: 20 }, (_, i) => kv.write({ path: p, data: { i } })))
  eq(rs.filter((r) => r.status === 'rejected').length, 0, 'failures')
  const m = await kv.getMetadata({ path: p })
  eq(m.currentVersion, 20, 'current version')
})

scenario('E', 'of 10 concurrent create-if-absent (cas=0) writes exactly one wins', async () => {
  const { kv } = await kvOf()
  const p = uniq('race')
  const rs = await Promise.allSettled(Array.from({ length: 10 }, (_, i) => kv.write({ path: p, data: { i }, cas: 0 })))
  eq(rs.filter((r) => r.status === 'fulfilled').length, 1, 'winners')
})

scenario('E', 'secrets are encrypted at rest (no plaintext in the Raft files)', async () => {
  const { m, kv } = await kvOf()
  const marker = `PLAINTEXT-MARKER-${Date.now()}`
  await kv.write({ path: uniq('rest'), data: { v: marker } })
  const hits = files(join(m.node.dir, 'raft')).filter((f) => readFileSync(f).includes(marker))
  eq(hits, [], 'files containing the plaintext')
})

scenario('E', 'secret values never reach the audit or server logs', async () => {
  const { m, kv } = await kvOf()
  const marker = `LOG-MARKER-${Date.now()}`
  await kv.write({ path: uniq('logs'), data: { v: marker } })
  ok(!JSON.stringify(auditEntries(m.node)).includes(marker), 'in audit log')
  ok(!m.node.log().includes(marker), 'in server log')
  const logDir = join(m.node.dir, 'logs')
  ok(!files(logDir).some((f) => readFileSync(f).includes(marker)), 'in a log file')
})

scenario('E', 'native gRPC writes and gRPC-Web reads see the same data', async () => {
  const m = await main()
  const p = uniq('mixed')
  await grpc(m.node, m.root).kv.write({ path: p, data: { via: 'h2' } })
  eq((await web(m.node, m.root).kv.read({ path: p })).data, { via: 'h2' }, 'data')
})
