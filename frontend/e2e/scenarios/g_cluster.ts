import { scenario, fixture } from '../suite'
import { main } from '../fixtures'
import { startNode, freePort, initVault, web, health, cli, ok, eq, fails, waitFor, Code, type Node, type Vault } from '../lib'

/** Three nodes that find each other via RAFT_RETRY_JOIN; node 0 is initialized,
 *  then the CLI's `unseal --all` brings the others in. */
const cluster3 = fixture(async () => {
  const ports = await Promise.all([0, 1, 2].map(async () => [await freePort(), await freePort()] as [number, number]))
  const join = ports.map(([api]) => `http://127.0.0.1:${api}`).join(',')
  const nodes: Node[] = []
  for (let i = 0; i < 3; i++) nodes.push(await startNode({ RAFT_RETRY_JOIN: join }, { name: `c${i}`, ports: ports[i] }))
  const v = await initVault(nodes[0], 3, 2)
  return { v, nodes }
})

async function unsealAll(v: Vault, via: Node) {
  for (const k of v.keys.slice(0, v.threshold)) {
    const r = await cli(['unseal', k, '--all', '--addr', via.url])
    eq(r.code, 0, `unseal --all exit (${r.err})`)
  }
}

async function leaderOf(nodes: Node[]) {
  for (const n of nodes) {
    const h = await health(n).catch(() => null)
    if (h?.status === 200) return n
  }
  return undefined
}

scenario('G', 'a single node is its own leader', async () => {
  const m = await main()
  const peers = (await web(m.node, m.root).cluster.configuration({})).peers
  eq(peers.map((p) => [p.name, p.leader, p.voter, p.self]), [[m.node.name, true, true, true]], 'peers')
})

scenario('G', 'a snapshot streams the whole (encrypted) key space as JSON', async () => {
  const m = await main()
  const chunks: Uint8Array[] = []
  for await (const c of web(m.node, m.root).cluster.snapshot({})) chunks.push(c.data)
  const text = Buffer.concat(chunks).toString()
  ok(text.length > 100, `snapshot ${text.length} bytes`)
  JSON.parse(text)
  ok(!text.includes('"password"'), 'plaintext field names in snapshot')
})

scenario('G', 'snapshots are admin-only', async () => {
  const m = await main()
  const err = await (async () => { for await (const _ of web(m.node, m.reader).cluster.snapshot({})) void _ })().then(() => null, (e) => e)
  ok(err && String(err).includes('administrators only'), `got ${err}`)
})

scenario('G', 'removing an unknown peer is NOT_FOUND; removing the leader itself is refused', async () => {
  const m = await main()
  await fails(web(m.node, m.root).cluster.removePeer({ nodeId: 'ghost-node' }), Code.NotFound)
  await fails(web(m.node, m.root).cluster.removePeer({ nodeId: m.node.name }), Code.InvalidArgument, 'leader')
})

scenario('G', 'a join request with a bad node descriptor is refused', async () => {
  const m = await main()
  await fails(web(m.node).cluster.joinChallenge({ node: { name: 'x y', apiAddr: 'ftp://a', clusterAddr: 'b' } }), Code.InvalidArgument, 'invalid node')
  await fails(web(m.node).cluster.joinChallenge({}), Code.InvalidArgument)
})

scenario('G', 'a join answer without a challenge is refused', async () => {
  const m = await main()
  await fails(web(m.node).cluster.joinAnswer({ node: { name: 'intruder', apiAddr: 'http://127.0.0.1:1', clusterAddr: 'http://127.0.0.1:2' }, answer: 'AAAA' }), Code.InvalidArgument)
  eq((await web(m.node, m.root).cluster.configuration({})).peers.length, 1, 'membership unchanged')
})

scenario('G', 'three nodes form a cluster with retry_join + `operator unseal --all`', async () => {
  const { v, nodes } = await cluster3()
  for (const k of v.keys.slice(0, v.threshold)) await web(nodes[0]).sys.unseal({ key: k })
  await waitFor('peers found', async () => (await web(nodes[0]).sys.instances({})).instances.length === 3)
  await unsealAll(v, nodes[0])
  await waitFor('3 voters', async () => {
    const peers = (await web(nodes[0], v.root).cluster.configuration({})).peers
    return peers.length === 3 && peers.every((p) => p.voter)
  }, 30000)
})

scenario('G', 'followers report standby (429) for health; ?standbyok=true makes it 200', async () => {
  const { nodes } = await cluster3()
  const leader = await waitFor('leader', () => leaderOf(nodes))
  for (const n of nodes.filter((x) => x !== leader)) {
    await waitFor(`${n.name} standby`, async () => (await health(n)).status === 429)
    eq((await health(n, '?standbyok=true')).status, 200, `${n.name} standbyok`)
  }
})

scenario('G', 'writes on one follower are immediately readable on another', async () => {
  const { v, nodes } = await cluster3()
  const leader = await waitFor('leader', () => leaderOf(nodes))
  const [f1, f2] = nodes.filter((n) => n !== leader)
  for (let i = 0; i < 20; i++) {
    await web(i % 2 ? f1 : f2, v.root).kv.write({ path: 'lin/x', data: { i } })
    eq((await web(i % 2 ? f2 : f1, v.root).kv.read({ path: 'lin/x' })).data?.i, i, `read ${i}`)
  }
})

scenario('G', 'the cluster survives losing its leader; the old leader rejoins and catches up', async () => {
  const { v, nodes } = await cluster3()
  const leader = await waitFor('leader', () => leaderOf(nodes))
  const rest = nodes.filter((n) => n !== leader)
  await web(rest[0], v.root).kv.write({ path: 'failover/before', data: { v: 1 } })
  await leader.stop()
  await waitFor('write after failover', async () => {
    await web(rest[1], v.root).kv.write({ path: 'failover/after', data: { v: 2 } })
    return true
  }, 20000)
  await leader.start()
  for (const k of v.keys.slice(0, v.threshold)) await web(leader).sys.unseal({ key: k })
  await waitFor('old leader caught up', async () => (await web(leader, v.root).kv.read({ path: 'failover/after' })).data?.v === 2, 20000)
  eq((await web(leader, v.root).kv.read({ path: 'failover/before' })).data, { v: 1 }, 'older data')
})
