import { scenario } from '../suite'
import { main, ssh, uniq, addServer } from '../fixtures'
import { web, createUser, terminal, wsRefusal, startSsh, sshKeygen, ok, eq, j, fails, waitFor, sleep, Code, WORK } from '../lib'
import { join } from 'node:path'

const bastion = async (who: 'root' | 'reader' | 'admin' = 'root') => {
  const m = await main()
  return { m, b: web(m.node, who === 'root' ? m.root : who === 'admin' ? m.admin : m.reader).bastion }
}

const connect = async (token: string, assetId: string, account: string, size = { cols: 100, rows: 30 }) => {
  const m = await main()
  const t = await terminal(m.node, token, { asset: assetId, account, ...size })
  await t.waitEvent('ready')
  await t.waitOutput('$ ')
  return t
}

// ─── servers ─────────────────────────────────────────────────────────────────

scenario('F', 'admins can manage servers', async () => {
  const { b } = await bastion()
  eq((await b.listAssets({})).canManage, true, 'canManage')
})

scenario('F', 'a server needs at least one account', async () => {
  const { b } = await bastion()
  await fails(b.createAsset({ name: uniq('s'), host: '127.0.0.1', accounts: [] }), Code.InvalidArgument, 'at least one account')
})

scenario('F', 'a host with spaces is refused', async () => {
  const { b } = await bastion()
  await fails(b.createAsset({ name: uniq('s'), host: 'ex ample.com', accounts: [{ username: 'u', password: 'p' }] }), Code.InvalidArgument, 'host')
})

scenario('F', 'a server needs a name', async () => {
  const { b } = await bastion()
  await fails(b.createAsset({ name: '  ', host: '10.0.0.1', accounts: [{ username: 'u', password: 'p' }] }), Code.InvalidArgument, 'name')
})

scenario('F', 'an account without a password or key is refused', async () => {
  const { b } = await bastion()
  await fails(b.createAsset({ name: uniq('s'), host: '10.0.0.1', accounts: [{ username: 'u' }] }), Code.InvalidArgument, 'password or a private key')
})

scenario('F', 'an invalid account name is refused', async () => {
  const { b } = await bastion()
  await fails(b.createAsset({ name: uniq('s'), host: '10.0.0.1', accounts: [{ username: 'bad user', password: 'p' }] }), Code.InvalidArgument, 'invalid account')
})

scenario('F', 'a port above 65535 is refused', async () => {
  const { b } = await bastion()
  await fails(b.createAsset({ name: uniq('s'), host: '10.0.0.1', port: 70000, accounts: [{ username: 'u', password: 'p' }] }), Code.InvalidArgument, 'port')
})

scenario('F', '"Test & save" logs in, pins the host key and saves', async () => {
  const { asset, test } = await addServer()
  ok(test?.ok && test.hostKey?.startsWith('SHA256:'), `test ${j(test)}`)
  eq(asset.hostKey, test!.hostKey, 'pinned key')
  ok(Number(test!.connectMs) >= 0, 'connectMs')
})

scenario('F', '"Test & save" with a wrong password saves nothing', async () => {
  const { b } = await bastion()
  const t = await ssh()
  const name = uniq('badpw')
  await fails(b.createAsset({ name, host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, password: 'wrong' }], test: true }), Code.InvalidArgument, 'rejected the credentials')
  ok(!(await b.listAssets({})).assets.some((a) => a.name === name), 'not saved')
})

scenario('F', '"Test & save" against a closed port saves nothing', async () => {
  const { b } = await bastion()
  const name = uniq('closed')
  await fails(b.createAsset({ name, host: '127.0.0.1', port: 1, accounts: [{ username: 'u', password: 'p' }], test: true }), Code.InvalidArgument, 'could not connect')
  ok(!(await b.listAssets({})).assets.some((a) => a.name === name), 'not saved')
})

scenario('F', 'two servers cannot share a name', async () => {
  const { asset } = await addServer(uniq('dup'), { test: false })
  const { b } = await bastion()
  await fails(b.createAsset({ name: asset.name, host: '10.0.0.2', accounts: [{ username: 'u', password: 'p' }] }), Code.AlreadyExists)
})

scenario('F', 'the server id is a slug of its name', async () => {
  const { b } = await bastion()
  const r = await b.createAsset({ name: `Web Server #1 ${Date.now()}`, host: '10.0.0.3', accounts: [{ username: 'u', password: 'p' }] })
  ok(/^web-server-1-\d+$/.test(r.asset!.id), `id ${r.asset!.id}`)
})

scenario('F', 'stored credentials are never returned', async () => {
  const { b } = await bastion()
  const t = await ssh()
  await addServer(uniq('nocreds'), { test: false })
  const json = JSON.stringify(await b.listAssets({}))
  ok(!json.includes(t.password), 'password leaked')
  ok(json.includes('"auth":"password"'), 'auth kind shown')
})

scenario('F', 'testing a saved server pins its host key on first contact', async () => {
  const { asset } = await addServer(uniq('pin'), { test: false })
  eq(asset.hostKey, undefined, 'no key yet')
  const { b } = await bastion()
  const r = await b.testAsset({ id: asset.id })
  ok(r.ok && r.hostKey, `test ${j(r)}`)
  eq((await b.listAssets({})).assets.find((a) => a.id === asset.id)?.hostKey, r.hostKey, 'pinned')
})

scenario('F', 'a changed host key is detected (possible MITM) and the reset re-pins it', async () => {
  const t = await startSsh({ hostKeyName: `rotating_${Date.now()}` })
  const { b } = await bastion()
  const r = await b.createAsset({ name: uniq('mitm'), host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, password: t.password }], test: true })
  const id = r.asset!.id
  await t.stop()
  const t2 = await startSsh({ port: t.port, hostKeyName: `rotated_${Date.now()}` })
  const bad = await b.testAsset({ id })
  ok(!bad.ok && /HOST KEY CHANGED/.test(bad.error ?? ''), `test ${j(bad)}`)
  await b.resetHostKey({ id })
  const good = await b.testAsset({ id })
  ok(good.ok && good.hostKey !== r.asset!.hostKey, 'new key pinned')
  await t2.stop()
})

scenario('F', 'updating a server without new secrets keeps the stored credentials', async () => {
  const { asset, t } = await addServer()
  const { b } = await bastion()
  await b.updateAsset({ id: asset.id, asset: { name: asset.name, host: asset.host, port: asset.port, description: 'edited', accounts: [{ username: t.user }] } })
  ok((await b.testAsset({ id: asset.id })).ok, 'still logs in')
})

scenario('F', 'removing an account from a server removes access as it', async () => {
  const { asset, t } = await addServer()
  const { b, m } = await bastion()
  await b.updateAsset({ id: asset.id, asset: { name: asset.name, host: asset.host, port: asset.port, accounts: [{ username: t.user }, { username: 'extra', password: 'x' }] } })
  await b.updateAsset({ id: asset.id, asset: { name: asset.name, host: asset.host, port: asset.port, accounts: [{ username: t.user }] } })
  const a = (await b.listAssets({})).assets.find((x) => x.id === asset.id)!
  eq(a.accounts.map((x) => x.username), [t.user], 'accounts')
  eq((await wsRefusal(m.node, m.root, { asset: asset.id, account: 'extra' })).status, 403, 'connect as removed account')
})

scenario('F', 'moving a server to another host clears its pinned key', async () => {
  const { asset, t } = await addServer()
  const { b } = await bastion()
  const u = await b.updateAsset({ id: asset.id, asset: { name: asset.name, host: 'localhost', port: asset.port, accounts: [{ username: t.user }] } })
  eq(u.hostKey, undefined, 'host key')
})

scenario('F', 'key-based accounts log in with a private key', async () => {
  const t = await ssh()
  const { b } = await bastion()
  const r = await b.createAsset({ name: uniq('keyauth'), host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, privateKey: t.privateKey }], test: true })
  ok(r.test?.ok, 'test ok')
  eq(r.asset!.accounts[0].auth, 'key', 'auth kind')
})

scenario('F', 'a passphrase-protected key works with its passphrase', async () => {
  const t = await ssh()
  const k = sshKeygen(join(WORK, 'ssh', 'enc_ed25519'), 'ed25519', 'pp-secret')
  const { b } = await bastion()
  // The target only trusts client_ed25519, so expect "rejected" — but not "could not be read".
  const err = await fails(b.createAsset({ name: uniq('enc'), host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, privateKey: k.privateKey, passphrase: 'pp-secret' }], test: true }), Code.InvalidArgument)
  ok(err.rawMessage.includes('rejected the credentials'), err.rawMessage)
  const wrong = await fails(b.createAsset({ name: uniq('enc2'), host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, privateKey: k.privateKey, passphrase: 'nope' }], test: true }), Code.InvalidArgument)
  ok(wrong.rawMessage.includes('could not be read'), wrong.rawMessage)
})

// ─── grants ──────────────────────────────────────────────────────────────────

scenario('F', 'without a grant, a read-only user sees no servers', async () => {
  const m = await main()
  const { asset } = await addServer(uniq('hidden2'), { test: false })
  const t = await createUser(m, uniq('nogrant'))
  ok(!(await web(m.node, t).bastion.listAssets({})).assets.some((a) => a.id === asset.id), 'visible without a grant')
})

scenario('F', 'a user grant shows the server with the allowed accounts', async () => {
  const { asset, m, t } = await addServer()
  const name = uniq('granted')
  const tok = await createUser(m, name)
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  const a = (await web(m.node, tok).bastion.listAssets({})).assets
  eq([a.length, a[0]?.id, a[0]?.allowedAccounts], [1, asset.id, [t.user]], 'visible')
  eq((await web(m.node, tok).bastion.listAssets({})).canManage, false, 'canManage')
})

scenario('F', 'a grant can be limited to some accounts', async () => {
  const { asset, m, t } = await addServer()
  const b = web(m.node, m.root).bastion
  await b.updateAsset({ id: asset.id, asset: { name: asset.name, host: asset.host, port: asset.port, accounts: [{ username: t.user }, { username: 'ops', password: 'x' }] } })
  const name = uniq('limited')
  const tok = await createUser(m, name)
  await b.createGrant({ asset: asset.id, subjectType: 'user', subject: name, accounts: ['ops'] })
  eq((await web(m.node, tok).bastion.listAssets({})).assets[0]?.allowedAccounts, ['ops'], 'allowed')
  eq((await wsRefusal(m.node, tok, { asset: asset.id, account: t.user })).status, 403, 'other account refused')
})

scenario('F', 'a role grant applies to everyone with that role', async () => {
  const { asset, m } = await addServer()
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'role', subject: 'read-only' })
  const tok = await createUser(m, uniq('rolemember'))
  ok((await web(m.node, tok).bastion.listAssets({})).assets.some((a) => a.id === asset.id), 'visible via role')
})

scenario('F', 'a grant for an unknown user is refused', async () => {
  const { asset, m } = await addServer(uniq('g'), { test: false })
  await fails(web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: 'no-such-user' }), Code.InvalidArgument, 'no user')
})

scenario('F', 'a grant for an account the server lacks is refused', async () => {
  const { asset, m } = await addServer(uniq('g'), { test: false })
  await fails(web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'role', subject: 'read-only', accounts: ['nobody'] }), Code.InvalidArgument, 'has no account')
})

scenario('F', 'an expiring grant reports its expiry; hours=0 means permanent', async () => {
  const { asset, m } = await addServer(uniq('exp'), { test: false })
  const b = web(m.node, m.root).bastion
  const g1 = await b.createGrant({ asset: asset.id, subjectType: 'role', subject: 'read-only', hours: 2n })
  const g2 = await b.createGrant({ asset: asset.id, subjectType: 'role', subject: 'admin', hours: 0n })
  const in2h = Date.now() + 2 * 3600e3
  ok(Math.abs(new Date(g1.expiresAt!).getTime() - in2h) < 60e3, `expires ${g1.expiresAt}`)
  eq([g2.expiresAt, g1.expired, (await b.listGrants({ id: asset.id })).grants.length], [undefined, false, 2], 'grants')
})

scenario('F', 'deleting a grant revokes access', async () => {
  const { asset, m } = await addServer(uniq('revoke'), { test: false })
  const name = uniq('revokee')
  const tok = await createUser(m, name)
  const g = await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  ok((await web(m.node, tok).bastion.listAssets({})).assets.some((a) => a.id === asset.id), 'visible with the grant')
  await web(m.node, m.root).bastion.deleteGrant({ asset: asset.id, grantId: g.id })
  ok(!(await web(m.node, tok).bastion.listAssets({})).assets.some((a) => a.id === asset.id), 'still visible')
})

scenario('F', 'read-only users cannot create servers or grants', async () => {
  const { b } = await bastion('reader')
  await fails(b.createAsset({ name: uniq('x'), host: '10.0.0.1', accounts: [{ username: 'u', password: 'p' }] }), Code.PermissionDenied)
  await fails(b.createGrant({ asset: 'x', subjectType: 'role', subject: 'read-only' }), Code.PermissionDenied)
})

scenario('F', 'deleting a server deletes its grants (a new server with the same name starts clean)', async () => {
  const name = uniq('reuse')
  const { asset, m } = await addServer(name, { test: false })
  const b = web(m.node, m.root).bastion
  await b.createGrant({ asset: asset.id, subjectType: 'role', subject: 'read-only' })
  await b.deleteAsset({ id: asset.id })
  await addServer(name, { test: false })
  eq((await b.listGrants({ id: asset.id })).grants.length, 0, 'grants')
})

// ─── web terminal ────────────────────────────────────────────────────────────

scenario('F', 'the terminal refuses a connection without a token (401)', async () => {
  const { asset, m, t } = await addServer(uniq('ws'), { test: false })
  eq((await wsRefusal(m.node, null, { asset: asset.id, account: t.user })).status, 401, 'status')
})

scenario('F', 'the terminal refuses a user without a grant (403)', async () => {
  const { asset, m, t } = await addServer(uniq('ws'), { test: false })
  eq((await wsRefusal(m.node, m.reader, { asset: asset.id, account: t.user })).status, 403, 'status')
})

scenario('F', 'a terminal session runs commands and follows resizes', async () => {
  const { asset, t } = await addServer()
  const m = await main()
  const term = await connect(m.root, asset.id, t.user, { cols: 100, rows: 30 })
  ok(term.output().includes(`welcome ${t.user}`), 'welcome banner')
  term.send('whoami\r')
  await term.waitOutput(`\r\n${t.user}\r\n`)
  term.send('size\r')
  await term.waitOutput('100x30')
  term.resize(132, 43)
  await sleep(200)
  term.send('size\r')
  await term.waitOutput('132x43')
  term.close()
})

scenario('F', 'a 200 KiB burst of output arrives intact', async () => {
  const { asset, t } = await addServer()
  const m = await main()
  const term = await connect(m.root, asset.id, t.user)
  term.send('big\r')
  await term.waitOutput('x'.repeat(200 * 1024), 15000)
  term.close()
})

scenario('F', 'a finished session is listed and its recording replays the output', async () => {
  const { asset, t } = await addServer()
  const m = await main()
  const term = await connect(m.root, asset.id, t.user)
  const sid = (await term.waitEvent('ready')).session as string
  term.send('echo-marker-123\r')
  await term.waitOutput('ok: echo-marker-123')
  term.send('exit\r')
  await term.waitEvent('closed')
  const b = web(m.node, m.root).bastion
  const s = await waitFor('session closed', async () => (await b.listSessions({})).sessions.find((x) => x.id === sid && x.status === 'closed'))
  eq([s.asset, s.account, s.user], [asset.id, t.user, 'root'], 'session')
  let rec = ''
  for await (const c of b.getRecording({ id: sid })) rec += new TextDecoder().decode(c.data)
  const [header, ...events] = rec.trim().split('\n')
  eq(JSON.parse(header).version, 2, 'asciicast header')
  ok(events.map((l) => JSON.parse(l)[2]).join('').includes('ok: echo-marker-123'), 'recording has the output')
})

scenario('F', 'an administrator can kill a live session', async () => {
  const { asset, t } = await addServer()
  const m = await main()
  const term = await connect(m.root, asset.id, t.user)
  const sid = (await term.waitEvent('ready')).session as string
  await web(m.node, m.root).bastion.killSession({ id: sid })
  await term.waitEvent('closed', 10000)
  const s = await waitFor('killed', async () => (await web(m.node, m.root).bastion.listSessions({})).sessions.find((x) => x.id === sid && x.status === 'killed'))
  ok(s.endedAt, 'ended')
})

scenario('F', "users see only their own sessions and can't replay others'", async () => {
  const { asset, m, t } = await addServer()
  const name = uniq('viewer')
  const tok = await createUser(m, name)
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  const mine = await connect(tok, asset.id, t.user)
  const mySid = (await mine.waitEvent('ready')).session as string
  mine.send('exit\r')
  await mine.waitEvent('closed')
  const theirs = await connect(m.root, asset.id, t.user)
  const rootSid = (await theirs.waitEvent('ready')).session as string
  theirs.close()
  const seen = (await web(m.node, tok).bastion.listSessions({})).sessions.map((s) => s.id)
  ok(seen.includes(mySid) && !seen.includes(rootSid), `seen ${seen}`)
  const err = await (async () => { for await (const _ of web(m.node, tok).bastion.getRecording({ id: rootSid })) void _ })().then(() => null, (e) => e)
  ok(err && String(err).includes('not your session'), `expected permission denied, got ${err}`)
})

// ─── command log ─────────────────────────────────────────────────────────────

async function runCommands(token: string, assetId: string, account: string, cmds: string[]) {
  const m = await main()
  const term = await connect(token, assetId, account)
  const sid = (await term.waitEvent('ready')).session as string
  for (const c of cmds) {
    term.send(`${c}\r`)
    await term.waitOutput(c === 'whoami' ? `\r\n${account}\r\n` : `ok: ${c}`)
  }
  term.send('exit\r')
  await term.waitEvent('closed')
  // `exit` is a command too (the shell echoed it).
  await waitFor('commands stored', async () => (await web(m.node, m.root).bastion.listSessions({})).sessions.find((s) => s.id === sid)?.commands === cmds.length + 1)
  return sid
}

scenario('F', 'commands typed in a terminal are logged per server, newest first, with risk levels', async () => {
  const { asset, m, t } = await addServer()
  const sid = await runCommands(m.root, asset.id, t.user, ['whoami', 'sudo systemctl restart nginx', 'rm -rf /var/tmp/cache'])
  const r = await web(m.node, m.root).bastion.listCommands({ asset: asset.id })
  eq(r.commands.map((c) => [c.command, c.risk ?? null]), [['exit', null], ['rm -rf /var/tmp/cache', 'high'], ['sudo systemctl restart nginx', 'medium'], ['whoami', null]], 'commands')
  ok(r.commands.every((c) => c.session === sid && c.user === 'root' && c.account === t.user && c.offset > 0 && c.source === 'typed'), j(r.commands))
})

scenario('F', 'the command log can be searched and narrowed to risky commands', async () => {
  const { asset, m, t } = await addServer()
  await runCommands(m.root, asset.id, t.user, ['ls -la', 'shutdown -r now', 'cat /etc/hosts'])
  const b = web(m.node, m.root).bastion
  eq((await b.listCommands({ asset: asset.id, query: 'HOSTS' })).commands.map((c) => c.command), ['cat /etc/hosts'], 'search')
  eq((await b.listCommands({ asset: asset.id, riskyOnly: true })).commands.map((c) => c.command), ['shutdown -r now'], 'risky')
})

scenario('F', "people see their own commands only, never someone else's", async () => {
  const { asset, m, t } = await addServer()
  const name = uniq('cmd-user')
  const tok = await createUser(m, name, ['ssh'])
  await web(m.node, m.root).bastion.createGrant({ asset: asset.id, subjectType: 'user', subject: name })
  await runCommands(tok, asset.id, t.user, ['mine-only'])
  const rootSid = await runCommands(m.root, asset.id, t.user, ['admins-only'])
  eq((await web(m.node, tok).bastion.listCommands({ asset: asset.id })).commands.map((c) => c.command), ['exit', 'mine-only'], 'own commands')
  await fails(web(m.node, tok).bastion.listCommands({ session: rootSid }), Code.PermissionDenied)
  ok((await web(m.node, m.root).bastion.listCommands({ asset: asset.id })).commands.length === 4, 'admin sees both sessions')
})

// ─── creating accounts on the server ─────────────────────────────────────────

scenario('F', 'a new account with a generated password is created on the server and works', async () => {
  const t = await ssh()
  const { b, m } = await bastion()
  const user = `svc${Date.now() % 100000}`
  const pw = `Gen-${crypto.randomUUID()}`
  const r = await b.createAsset({
    name: uniq('prov'), host: '127.0.0.1', port: t.port, test: true,
    accounts: [{ username: t.user, password: t.password }, { username: user, password: pw, provision: true, provisionVia: t.user }],
  })
  eq(r.provisioned, [`${user} (created)`], 'provisioned')
  ok(r.asset?.hostKey, 'host key pinned')
  const term = await connect(m.root, r.asset!.id, user)
  term.send('whoami\r')
  await term.waitOutput(`\r\n${user}\r\n`)
  term.close()
})

scenario('F', 'if the admin login fails, nothing is created or saved', async () => {
  const t = await ssh()
  const { b } = await bastion()
  const name = uniq('provfail')
  await fails(b.createAsset({
    name, host: '127.0.0.1', port: t.port,
    accounts: [{ username: t.user, password: 'wrong' }, { username: 'svcx', password: 'Some-Long-Password-1!', provision: true, provisionVia: t.user }],
  }), Code.InvalidArgument, `signing in as \`${t.user}\``)
  ok(!(await b.listAssets({})).assets.some((a) => a.name === name), 'not saved')
})

scenario("F", "an existing account's password can be reset on the server", async () => {
  const t = await ssh()
  const { b } = await bastion()
  const user = `rot${Date.now() % 100000}`
  const r = await b.createAsset({
    name: uniq('rotate'), host: '127.0.0.1', port: t.port,
    accounts: [{ username: t.user, password: t.password }, { username: user, password: 'First-Password-123!', provision: true, provisionVia: t.user }],
  })
  const a = r.asset!
  await b.updateAsset({ id: a.id, asset: { name: a.name, host: a.host, port: a.port, accounts: [{ username: t.user }, { username: user, password: 'Second-Password-456!', provision: true, provisionVia: t.user }] } })
  const test = await b.testAsset({ id: a.id, account: user })
  ok(test.ok, `login with the new password: ${j(test)}`)
  const audit = (await web((await main()).node, (await main()).root).audit.listEvents({ server: a.id })).events
  ok(audit.some((e) => e.action === 'BastionService/UpdateAsset' && (e.target ?? '').includes(`${user} (updated · new password)`)), 'audited')
})

scenario('F', 'creating an account on the server needs a password and an account to sign in with', async () => {
  const t = await ssh()
  const { b } = await bastion()
  await fails(b.createAsset({ name: uniq('p1'), host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, password: t.password }, { username: 'nopw', privateKey: 'x', provision: true, provisionVia: t.user }] }), Code.InvalidArgument, 'password')
  await fails(b.createAsset({ name: uniq('p2'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'solo', password: 'Long-Password-123!', provision: true, provisionVia: '' }] }), Code.InvalidArgument, 'choose the account')
})

scenario('F', "a saved account can change its own password on the server (signing in with the current one)", async () => {
  const t = await startSsh({ user: 'opsroot', password: 'Current-Pass-1!', hostKeyName: `self_${Date.now()}` })
  const { b } = await bastion()
  const r = await b.createAsset({ name: uniq('selfrot'), host: '127.0.0.1', port: t.port, test: true, accounts: [{ username: 'opsroot', password: 'Current-Pass-1!' }] })
  const a = r.asset!
  // Only one account: it signs in as itself.
  await b.updateAsset({ id: a.id, asset: { name: a.name, host: a.host, port: a.port, accounts: [{ username: 'opsroot', password: 'Rotated-Pass-2!', provision: true, provisionVia: 'opsroot' }] } })
  ok((await b.testAsset({ id: a.id })).ok, 'logs in with the new password')
  // A brand-new account can't sign in as itself — there's no current password.
  await fails(b.updateAsset({ id: a.id, asset: { name: a.name, host: a.host, port: a.port, accounts: [{ username: 'opsroot' }, { username: 'fresh', password: 'Fresh-Pass-3!', provision: true, provisionVia: 'fresh' }] } }), Code.InvalidArgument, 'no saved password yet')
  await t.stop()
})

scenario('F', 'a new account gets passwordless sudo and the groups that exist', async () => {
  const t = await ssh()
  const { b } = await bastion()
  const user = `ops${Date.now() % 100000}`
  const r = await b.createAsset({
    name: uniq('sudo'), host: '127.0.0.1', port: t.port,
    accounts: [{ username: t.user, password: t.password }, { username: user, password: 'Ops-Pass-12345!', provision: true, provisionVia: t.user, sudo: 'nopasswd', groups: ['docker', 'nosuchgroup'] }],
  })
  eq(r.provisioned, [`${user} (created · sudo without password · groups docker · no such group nosuchgroup)`], 'summary')
  const acc = r.asset!.accounts.find((a) => a.username === user)!
  eq([acc.sudo, acc.groups], ['nopasswd', ['docker']], 'remembered on the account')
})

scenario("F", "an existing account's sudo and groups change without touching its password", async () => {
  const t = await ssh()
  const { b } = await bastion()
  const user = `perm${Date.now() % 100000}`
  const r = await b.createAsset({
    name: uniq('perm'), host: '127.0.0.1', port: t.port,
    accounts: [{ username: t.user, password: t.password }, { username: user, password: 'Perm-Pass-12345!', provision: true, provisionVia: t.user, sudo: 'password' }],
  })
  const a = r.asset!
  const u = await b.updateAsset({ id: a.id, asset: { name: a.name, host: a.host, port: a.port, accounts: [{ username: t.user }, { username: user, provision: true, provisionVia: t.user, sudo: 'none', groups: ['adm'] }] } })
  const acc = u.accounts.find((x) => x.username === user)!
  eq([acc.sudo, acc.groups], ['none', ['adm']], 'updated permissions')
  ok((await b.testAsset({ id: a.id, account: user })).ok, 'old password still works')
})

scenario('F', 'unsafe group names and unknown sudo levels are refused', async () => {
  const t = await ssh()
  const { b } = await bastion()
  const base = { host: '127.0.0.1', port: t.port }
  await fails(b.createAsset({ ...base, name: uniq('g'), accounts: [{ username: t.user, password: t.password }, { username: 'gx', password: 'Gx-Pass-12345!', provision: true, provisionVia: t.user, groups: ['docker; rm -rf /'] }] }), Code.InvalidArgument, 'invalid group')
  await fails(b.createAsset({ ...base, name: uniq('s'), accounts: [{ username: t.user, password: t.password }, { username: 'sx', password: 'Sx-Pass-12345!', provision: true, provisionVia: t.user, sudo: 'root' }] }), Code.InvalidArgument, 'sudo must be')
})
