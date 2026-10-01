import { scenario } from '../suite'
import { createUser } from '../lib'
import { startNode, initVault, unseal, freshVault, web, ok, eq, fails, waitFor, health, Code, type Vault } from '../lib'

scenario('B', 'status of a new node: uninitialized, sealed, raft, node name', async () => {
  const n = await startNode()
  const s = await web(n).sys.getSealStatus({})
  eq([s.initialized, s.sealed, s.storage, s.node, s.sealType], [false, true, 'raft', n.name, 'shamir'], 'status')
})

scenario('B', 'init rejects threshold > shares', async () => {
  const n = await startNode()
  await fails(web(n).sys.init({ secretShares: 2, secretThreshold: 3 }), Code.InvalidArgument)
})

scenario('B', 'init rejects zero shares', async () => {
  const n = await startNode()
  await fails(web(n).sys.init({ secretShares: 0, secretThreshold: 0 }), Code.InvalidArgument)
})

scenario('B', 'init rejects more than 16 shares', async () => {
  const n = await startNode()
  await fails(web(n).sys.init({ secretShares: 17, secretThreshold: 3 }), Code.InvalidArgument)
})

scenario('B', 'init rejects share counts that overflow (300)', async () => {
  const n = await startNode()
  await fails(web(n).sys.init({ secretShares: 300, secretThreshold: 2 }), Code.InvalidArgument)
  // …and the node is still uninitialized.
  eq((await web(n).sys.getSealStatus({})).initialized, false, 'initialized')
})

scenario('B', 'init returns N distinct keys and a tmk. root token, and leaves the vault sealed', async () => {
  const n = await startNode()
  const r = await web(n).sys.init({ secretShares: 5, secretThreshold: 3 })
  eq(new Set(r.keys).size, 5, 'distinct keys')
  ok(r.rootToken.startsWith('tmk.'), 'root token prefix')
  const s = await web(n).sys.getSealStatus({})
  eq([s.initialized, s.sealed, s.t, s.n, s.progress], [true, true, 3, 5, 0], 'status')
})

scenario('B', 'a 1-of-1 vault unseals with its single key', async () => {
  const n = await startNode()
  const v = await initVault(n, 1, 1)
  eq(v.keys.length, 1, 'keys')
  eq((await web(n).sys.unseal({ key: v.keys[0] })).status?.sealed, false, 'sealed')
})

scenario('B', 'init twice is refused', async () => {
  const n = await startNode()
  await initVault(n)
  await fails(web(n).sys.init({ secretShares: 3, secretThreshold: 2 }), Code.InvalidArgument, 'already initialized')
})

scenario('B', 'unseal rejects a key that is not base64', async () => {
  const n = await startNode()
  await initVault(n)
  await fails(web(n).sys.unseal({ key: 'not base64 !!' }), Code.InvalidArgument, 'base64')
})

scenario('B', 'unseal rejects base64 that is not a share', async () => {
  const n = await startNode()
  await initVault(n)
  await fails(web(n).sys.unseal({ key: 'AA==' }), Code.InvalidArgument)
  eq((await web(n).sys.getSealStatus({})).progress, 0, 'progress')
})

scenario('B', 'one key moves progress to 1 of the threshold', async () => {
  const n = await startNode()
  const v = await initVault(n, 3, 2)
  const r = await web(n).sys.unseal({ key: v.keys[0] })
  eq([r.status?.sealed, r.status?.progress, r.status?.t], [true, 1, 2], 'status')
})

scenario('B', 'submitting the same key twice does not count twice', async () => {
  const n = await startNode()
  const v = await initVault(n, 3, 2)
  await web(n).sys.unseal({ key: v.keys[0] })
  const r = await web(n).sys.unseal({ key: v.keys[0] })
  eq([r.status?.sealed, r.status?.progress], [true, 1], 'status')
})

scenario('B', 'reset clears unseal progress', async () => {
  const n = await startNode()
  const v = await initVault(n, 3, 2)
  await web(n).sys.unseal({ key: v.keys[0] })
  eq((await web(n).sys.unseal({ reset: true })).status?.progress, 0, 'progress')
})

scenario('B', "another vault's keys are rejected and progress resets", async () => {
  const a = await initVault(await startNode(), 3, 2)
  const b = await initVault(await startNode(), 3, 2)
  await web(a.node).sys.unseal({ key: b.keys[0] })
  await fails(web(a.node).sys.unseal({ key: b.keys[1] }), Code.InvalidArgument)
  const s = await web(a.node).sys.getSealStatus({})
  eq([s.sealed, s.progress], [true, 0], 'status')
})

scenario('B', 'unseal on an uninitialized node is FAILED_PRECONDITION', async () => {
  const n = await startNode()
  await fails(web(n).sys.unseal({ key: 'AA==' }), Code.FailedPrecondition, 'not initialized')
})

scenario('B', 'unseal on an unsealed vault is a harmless no-op', async () => {
  const v = await freshVault()
  const r = await web(v.node).sys.unseal({ key: 'garbage' })
  eq(r.status?.sealed, false, 'sealed')
})

scenario('B', 'any threshold-sized subset of keys unseals (keys 2 and 3)', async () => {
  const v = await initVault(await startNode(), 3, 2)
  await unseal({ ...v, keys: v.keys.slice(1) })
  eq((await web(v.node).sys.getSealStatus({})).sealed, false, 'sealed')
})

scenario('B', 'seal without a token is UNAUTHENTICATED', async () => {
  const v = await freshVault()
  await fails(web(v.node).sys.seal({}), Code.Unauthenticated)
})

scenario('B', 'seal with a read-only token is PERMISSION_DENIED', async () => {
  const v = await freshVault()
  const reader = await createUser(v, 'ro-sealer')
  await fails(web(v.node, reader).sys.seal({}), Code.PermissionDenied)
  eq((await web(v.node).sys.getSealStatus({})).sealed, false, 'still unsealed')
})

scenario('B', 'seal by root seals; data calls then fail with "vault is sealed"', async () => {
  const v = await freshVault()
  const r = await web(v.node, v.root).sys.seal({})
  eq(r.status?.sealed, true, 'sealed')
  await fails(web(v.node, v.root).kv.list({ folder: '' }), Code.FailedPrecondition, 'sealed')
})

scenario('B', 'a sealed vault reports sealed, not "bad token", for token calls', async () => {
  const v = await freshVault()
  await web(v.node, v.root).sys.seal({})
  await fails(web(v.node, v.root).auth.lookupSelf({}), Code.FailedPrecondition, 'sealed')
})

scenario('B', 'seal → unseal keeps every secret', async () => {
  const v = await freshVault()
  await web(v.node, v.root).kv.write({ path: 'keep/me', data: { v: 'x1' } })
  await web(v.node, v.root).sys.seal({})
  await unseal(v)
  eq((await web(v.node, v.root).kv.read({ path: 'keep/me' })).data, { v: 'x1' }, 'data')
})

scenario('B', 'a restarted process comes back sealed; its data survives', async () => {
  const v = await freshVault()
  await web(v.node, v.root).kv.write({ path: 'persist/me', data: { v: 'disk' } })
  await v.node.stop()
  await v.node.start()
  eq((await web(v.node).sys.getSealStatus({})).sealed, true, 'sealed after restart')
  await unseal(v)
  eq((await web(v.node, v.root).kv.read({ path: 'persist/me' })).data, { v: 'disk' }, 'data')
})

scenario('B', 'rotate adds a key term; old and new secrets stay readable', async () => {
  const v = await freshVault()
  const c = web(v.node, v.root)
  await c.kv.write({ path: 'rot/old', data: { v: 'before' } })
  const before = await c.sys.getKeyStatus({})
  const after = await c.sys.rotate({})
  eq([after.term, after.terms], [before.term + 1, before.terms + 1], 'term')
  await c.kv.write({ path: 'rot/new', data: { v: 'after' } })
  eq((await c.kv.read({ path: 'rot/old' })).data, { v: 'before' }, 'old')
  eq((await c.kv.read({ path: 'rot/new' })).data, { v: 'after' }, 'new')
})

scenario('B', 'a rotated keyring survives seal + unseal', async () => {
  const v: Vault = await freshVault()
  const c = web(v.node, v.root)
  await c.kv.write({ path: 'rot2/a', data: { v: 1 } })
  await c.sys.rotate({})
  await c.kv.write({ path: 'rot2/b', data: { v: 2 } })
  await c.sys.seal({})
  await unseal(v)
  eq((await c.sys.getKeyStatus({})).terms, 2, 'terms')
  eq([(await c.kv.read({ path: 'rot2/a' })).data, (await c.kv.read({ path: 'rot2/b' })).data], [{ v: 1 }, { v: 2 }], 'data')
  await waitFor('healthy', async () => (await health(v.node)).status === 200)
})
