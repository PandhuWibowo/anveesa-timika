import { scenario } from '../suite'
import { main, uniq, addServer } from '../fixtures'
import { freshVault, web, login, createUser, terminal, ok, eq, j, fails, waitFor, Code, STRONG } from '../lib'

const events = async (q: Record<string, unknown> = {}) => {
  const m = await main()
  return (await web(m.node, m.root).audit.listEvents({ limit: 500, ...q })).events
}

// ─── the ssh role ────────────────────────────────────────────────────────────

scenario('I', 'the `ssh` role is offered and can be assigned', async () => {
  const m = await main()
  ok((await web(m.node, m.root).auth.listUsers({})).roles.includes('ssh'), 'roles')
  const u = await web(m.node, m.root).auth.upsertUser({ username: uniq('sshy'), password: STRONG, policies: ['ssh'], mustChangePassword: false })
  eq(u.policies, ['ssh'], 'policies')
})

scenario('I', 'an ssh-only user cannot read vault data', async () => {
  const m = await main()
  const t = await createUser(m, uniq('ssh-nokv'), ['ssh'])
  await fails(web(m.node, t).kv.list({ folder: '' }), Code.PermissionDenied)
  await fails(web(m.node, t).kv.read({ path: 'anything' }), Code.PermissionDenied)
  await fails(web(m.node, t).sys.getKeyStatus({}), Code.PermissionDenied)
})

scenario('I', 'an ssh-only user sees and opens only granted servers', async () => {
  const { asset, m, t } = await addServer()
  const { asset: other } = await addServer(uniq('other'), { test: false })
  const name = uniq('ssh-term')
  const tok = await createUser(m, name, ['ssh'])
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  const ids = (await web(m.node, tok).bastion.listAssets({})).assets.map((a) => a.id)
  ok(ids.includes(asset.id) && !ids.includes(other.id), `visible ${ids}`)
  const term = await terminal(m.node, tok, { asset: asset.id, account: t.user })
  await term.waitEvent('ready')
  term.send('whoami\r')
  await term.waitOutput(`\r\n${t.user}\r\n`)
  term.close()
})

scenario('I', 'an ssh-only user lists their own sessions and cannot manage servers', async () => {
  const m = await main()
  const tok = await createUser(m, uniq('ssh-ses'), ['ssh'])
  eq((await web(m.node, tok).bastion.listSessions({})).sessions.length, 0, 'own sessions')
  await fails(web(m.node, tok).bastion.createAsset({ name: uniq('x'), host: '10.0.0.1', accounts: [{ username: 'u', password: 'p' }] }), Code.PermissionDenied)
  await fails(web(m.node, tok).bastion.listAllGrants({}), Code.PermissionDenied)
})

scenario('I', 'all grants across servers are listed for administrators', async () => {
  const { asset, m } = await addServer(uniq('allg'), { test: false })
  const name = uniq('allg-u')
  await createUser(m, name, ['ssh'])
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  const all = (await web(m.node, m.root).bastion.listAllGrants({})).grants
  ok(all.some((g) => g.asset === asset.id && g.subject === name), 'grant listed')
})

// ─── audit trail ─────────────────────────────────────────────────────────────

scenario('I', 'the audit trail is for administrators only', async () => {
  const m = await main()
  await fails(web(m.node, m.reader).audit.listEvents({}), Code.PermissionDenied)
  await fails(web(m.node, m.reader).audit.verify({}), Code.PermissionDenied)
})

scenario('I', 'a secret write shows who, what, the path and the outcome', async () => {
  const m = await main()
  const p = uniq('trail/db')
  await web(m.node, m.admin).kv.write({ path: p, data: { v: 1 } })
  const e = (await events({ query: p })).find((x) => x.action === 'KvService/Write')
  ok(e, 'event found')
  eq([e.user, e.target, e.ok, e.code, e.kind], ['ops-admin', p, true, 0, 'call'], 'event')
  ok(e.remoteAddr === '127.0.0.1', `remote ${e.remoteAddr}`)
})

scenario('I', 'a failed sign-in names the targeted account', async () => {
  const m = await main()
  const name = uniq('victim')
  await login(m.node, name, 'Wrong-Password-123456').catch(() => {})
  const e = (await events({ query: name })).find((x) => x.action === 'AuthService/Login')
  ok(e, 'event found')
  eq([e.ok, e.code, e.target], [false, Code.Unauthenticated, `user ${name}`], 'event')
})

scenario('I', 'successful routine reads are hidden unless asked for', async () => {
  const m = await main()
  await web(m.node, m.root).auth.lookupSelf({})
  // Successful routine reads are hidden; failed ones always show (worth seeing).
  ok(!(await events()).some((e) => e.action === 'AuthService/LookupSelf' && e.ok), 'hidden by default')
  ok((await events({ includeRoutine: true })).some((e) => e.action === 'AuthService/LookupSelf' && e.ok), 'shown on request')
})

scenario('I', 'filter by user returns only that person', async () => {
  const m = await main()
  await web(m.node, m.reader).kv.read({ path: uniq('nope') }).catch(() => {})
  const evs = await events({ user: 'READER' })
  ok(evs.length > 0 && evs.every((e) => e.user === 'reader'), `users ${[...new Set(evs.map((e) => e.user))]}`)
})

scenario('I', 'filter by category and outcome', async () => {
  const m = await main()
  await web(m.node, m.root).kv.read({ path: uniq('missing') }).catch(() => {})
  const evs = await events({ category: 'secrets', outcome: 'error' })
  ok(evs.length > 0 && evs.every((e) => e.action.startsWith('KvService/') && !e.ok), j(evs.map((e) => [e.action, e.ok])))
})

scenario('I', 'paging with beforeSeq continues without overlap', async () => {
  const m = await main()
  for (let i = 0; i < 6; i++) await web(m.node, m.root).kv.write({ path: uniq('page'), data: { i } })
  const first = await web(m.node, m.root).audit.listEvents({ limit: 3 })
  ok(first.events.length === 3 && first.nextBeforeSeq, 'first page')
  const second = await web(m.node, m.root).audit.listEvents({ limit: 3, beforeSeq: first.nextBeforeSeq })
  const a = first.events.map((e) => e.seq), b = second.events.map((e) => e.seq)
  ok(b.length > 0 && Math.max(...b.map(Number)) < Math.min(...a.map(Number)), `pages ${a} / ${b}`)
})

scenario('I', 'the trail never contains secret values or passwords', async () => {
  const m = await main()
  const marker = `VALUE-${Date.now()}`
  await web(m.node, m.root).kv.write({ path: uniq('noval'), data: { v: marker } })
  await web(m.node, m.root).auth.upsertUser({ username: uniq('pw'), password: `Pass-${marker}-x!`, mustChangePassword: false })
  ok(!j(await events({ includeRoutine: true })).includes(marker), 'value in the trail')
})

scenario('I', 'creating a user records the role change but not the password', async () => {
  const m = await main()
  const name = uniq('newbie')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG, policies: ['ssh'] })
  const e = (await events({ query: name })).find((x) => x.action === 'AuthService/UpsertUser')
  eq(e?.target, `user ${name} (password, role ssh)`, 'target')
})

scenario('I', 'granting access records who got which server', async () => {
  const { asset, m } = await addServer(uniq('gtrail'), { test: false })
  const name = uniq('gt-u')
  await createUser(m, name, ['ssh'])
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  const e = (await events({ query: name })).find((x) => x.action === 'BastionService/CreateGrant')
  eq(e?.target, `user ${name} → server ${asset.id}`, 'target')
})

scenario('I', 'terminal sessions appear in the trail with account@server', async () => {
  const { asset, m, t } = await addServer()
  const term = await terminal(m.node, m.root, { asset: asset.id, account: t.user })
  await term.waitEvent('ready')
  term.send('exit\r')
  await term.waitEvent('closed')
  const target = `${t.user}@${asset.id}`
  const evs = await waitFor('session events', async () => {
    const e = await events({ query: target, category: 'sessions' })
    return e.some((x) => x.action === 'session closed') && e
  })
  ok(evs.some((x) => x.action === 'GET /v1/bastion/connect' && x.target === target), 'connect call')
  ok(evs.find((x) => x.action === 'session closed')?.details, 'session details')
})

scenario('I', 'very long targets are truncated', async () => {
  const m = await main()
  const p = `long/${'a'.repeat(400)}`
  await web(m.node, m.root).kv.write({ path: p, data: { v: 1 } })
  const e = (await events({ query: 'long/aaaa' })).find((x) => x.action === 'KvService/Write')
  ok(e?.target && e.target.length <= 201 && e.target.endsWith('…'), `target length ${e?.target?.length}`)
})

scenario('I', 'verify confirms the chain over the API', async () => {
  const m = await main()
  const v = await web(m.node, m.root).audit.verify({})
  ok(v.ok && Number(v.entries) > 10 && v.problems.length === 0, j(v))
})

scenario('I', 'without a local audit file the trail says so', async () => {
  const v = await freshVault({ AUDIT_FILE: '' })
  const r = await web(v.node, v.root).audit.listEvents({})
  eq([r.stored, r.events.length], [false, 0], 'stored')
  await fails(web(v.node, v.root).audit.verify({}), Code.FailedPrecondition)
})

scenario('I', 'audit events carry their instance and stay newest first', async () => {
  const m = await main()
  const evs = await events({ limit: 50 })
  ok(evs.every((e) => e.instance === m.node.name), 'instance')
  for (let i = 1; i < evs.length; i++) ok(evs[i - 1].seq > evs[i].seq, `order at ${i}`)
})

// ─── per-server activity ─────────────────────────────────────────────────────

scenario('I', "a server's activity covers its setup, access, tests, terminals and sessions", async () => {
  const base = uniq('vm')
  const { asset, m, t } = await addServer(base) // created with a login test
  const name = uniq('vm-user')
  await createUser(m, name, ['ssh'])
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  await web(m.node, m.root).bastion.testAsset({ id: asset.id })
  const term = await terminal(m.node, m.root, { asset: asset.id, account: t.user })
  await term.waitEvent('ready')
  term.send('exit\r')
  await term.waitEvent('closed')
  const actions = await waitFor('session in the server trail', async () => {
    const evs = await events({ server: asset.id })
    const a = evs.map((e) => e.action)
    return a.includes('session closed') && a
  })
  for (const want of ['BastionService/CreateAsset', 'BastionService/CreateGrant', 'BastionService/TestAsset', 'GET /v1/bastion/connect', 'session live', 'session closed']) {
    ok(actions.includes(want), `missing ${want} in ${actions}`)
  }
})

scenario('I', "a server's activity excludes other servers, even ones with a longer similar id", async () => {
  const base = uniq('twin')
  const { asset: a } = await addServer(base, { test: false })
  const { asset: b } = await addServer(`${base}0`, { test: false })
  ok(b.id.startsWith(a.id), `ids ${a.id} / ${b.id}`)
  const m = await main()
  await web(m.node, m.root).bastion.resetHostKey({ id: b.id })
  const evs = await events({ server: a.id, includeRoutine: true })
  ok(evs.length > 0, 'own events')
  ok(evs.every((e) => !(e.target ?? '').includes(b.id)), `leaked: ${evs.map((e) => e.target)}`)
})
