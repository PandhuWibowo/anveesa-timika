import { createHash } from 'node:crypto'
import { scenario, fixture } from '../suite'
import { main, uniq } from '../fixtures'
import { startNode, freshVault, web, login, createUser, solveCaptcha, ok, eq, fails, Code, STRONG, STRONG2, sleep } from '../lib'

/** A vault with a low lockout threshold for lockout scenarios. */
const lockVault = fixture(() => freshVault({ LOCKOUT_THRESHOLD: '3', LOCKOUT_MINUTES: '15' }))

async function attempt(url: string, username: string, password: string) {
  return web(url).auth.login({ username, password, captcha: await solveCaptcha(url), bannerAck: true })
}

scenario('C', 'login config is public and names the policy profile and captcha', async () => {
  const m = await main()
  const c = await web(m.node).auth.getLoginConfig({})
  eq([c.policy?.profile, c.captcha?.provider, c.policy?.minLength], ['us-nist', 'pow', 15], 'config')
})

scenario('C', 'the cn-mlps profile is reported with its own rules', async () => {
  const n = await startNode({ LOGIN_POLICY: 'cn-mlps' })
  const c = await web(n).auth.getLoginConfig({})
  eq([c.policy?.profile, c.policy?.minLength, c.policy?.lockoutThreshold], ['cn-mlps', 8, 5], 'policy')
})

scenario('C', 'login config never exposes captcha secrets', async () => {
  const n = await startNode({ CAPTCHA_PROVIDER: 'turnstile', CAPTCHA_SITE_KEY: 'site-123', CAPTCHA_SECRET: 'very-secret-456' })
  const c = await web(n).auth.getLoginConfig({})
  eq([c.captcha?.provider, c.captcha?.siteKey], ['turnstile', 'site-123'], 'public part')
  ok(!JSON.stringify(c).includes('very-secret-456'), 'secret leaked')
})

scenario('C', 'a captcha challenge is signed and expires', async () => {
  const m = await main()
  const ch = await web(m.node).auth.getCaptchaChallenge({})
  ok(/^[0-9a-f]{64}$/.test(ch.challenge) && /^[0-9a-f]{64}$/.test(ch.signature), 'hex fields')
  ok(/expires=\d+/.test(ch.salt), `salt ${ch.salt}`)
  eq(ch.algorithm, 'SHA-256', 'algorithm')
})

scenario('C', 'no captcha challenges while sealed (the key is derived from the root key)', async () => {
  const n = await startNode()
  await web(n).sys.init({ secretShares: 1, secretThreshold: 1 })
  await fails(web(n).auth.getCaptchaChallenge({}), Code.FailedPrecondition, 'sealed')
})

scenario('C', 'login without a captcha is refused', async () => {
  const m = await main()
  await fails(web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: true }), Code.PermissionDenied, 'captcha')
})

scenario('C', 'a captcha for a different provider is refused', async () => {
  const m = await main()
  await fails(web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: true, captcha: { provider: 'turnstile', token: 'x' } }), Code.PermissionDenied)
})

scenario('C', 'a wrong proof-of-work answer is refused', async () => {
  const m = await main()
  const sub = await solveCaptcha(m.node)
  const p = JSON.parse(Buffer.from(sub.token, 'base64').toString())
  p.number += 1
  await fails(web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: true, captcha: { provider: 'pow', token: Buffer.from(JSON.stringify(p)).toString('base64') } }), Code.PermissionDenied, 'captcha failed')
})

scenario('C', 'a solved captcha cannot be replayed', async () => {
  const m = await main()
  const captcha = await solveCaptcha(m.node)
  await web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: true, captcha })
  await fails(web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: true, captcha }), Code.PermissionDenied, 'already used')
})

scenario('C', 'a self-made challenge with a forged signature is refused', async () => {
  const m = await main()
  const salt = `abc?expires=${Math.floor(Date.now() / 1000) + 600}`
  const challenge = createHash('sha256').update(salt + 7).digest('hex')
  const token = Buffer.from(JSON.stringify({ algorithm: 'SHA-256', challenge, number: 7, salt, signature: '00'.repeat(32) })).toString('base64')
  await fails(web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: true, captcha: { provider: 'pow', token } }), Code.PermissionDenied)
})

scenario('C', 'the system-use notice must be acknowledged (us-nist)', async () => {
  const m = await main()
  await fails(web(m.node).auth.login({ username: 'reader', password: STRONG, bannerAck: false, captcha: await solveCaptcha(m.node) }), Code.InvalidArgument, 'acknowledge')
})

scenario('C', 'a good sign-in returns a session token with expiry', async () => {
  const m = await main()
  const r = await login(m.node, 'reader', STRONG)
  ok(r.token.startsWith('tmk.'), 'token')
  eq([r.username, r.policies, r.mustChangePassword], ['reader', ['read-only'], false], 'session')
  ok(r.expiresAt && r.maxExpiresAt && r.idleTimeoutSecs > 0n, 'expiry')
})

scenario('C', 'a wrong password is UNAUTHENTICATED with a generic message', async () => {
  const m = await main()
  await fails(login(m.node, 'reader', 'Wrong-Password-123456'), Code.Unauthenticated, 'invalid username or password')
})

scenario('C', 'an unknown user gets exactly the same answer (no user enumeration)', async () => {
  const m = await main()
  const a = await fails(login(m.node, 'reader', 'Wrong-Password-123456'), Code.Unauthenticated)
  const b = await fails(login(m.node, 'nobody-here', 'Wrong-Password-123456'), Code.Unauthenticated)
  eq(a.rawMessage, b.rawMessage, 'message')
})

scenario('C', 'an impossible username gets the same generic answer', async () => {
  const m = await main()
  await fails(login(m.node, 'bad name!!', 'Wrong-Password-123456'), Code.Unauthenticated, 'invalid username or password')
})

scenario('C', 'usernames are case-insensitive at sign-in', async () => {
  const m = await main()
  eq((await login(m.node, 'READER', STRONG)).username, 'reader', 'username')
})

scenario('C', 'the account locks after the threshold of failures, even for the right password', async () => {
  const v = await lockVault()
  await createUser(v, 'locky')
  for (let i = 0; i < 3; i++) await fails(attempt(v.node.url, 'locky', 'Wrong-Password-123456'), i < 2 ? Code.Unauthenticated : Code.PermissionDenied)
  await fails(attempt(v.node.url, 'locky', STRONG), Code.PermissionDenied, 'locked')
})

scenario('C', 'an administrator can unlock a locked account', async () => {
  const v = await lockVault()
  await createUser(v, 'unlocky')
  for (let i = 0; i < 3; i++) await attempt(v.node.url, 'unlocky', 'Wrong-Password-123456').catch(() => {})
  await fails(attempt(v.node.url, 'unlocky', STRONG), Code.PermissionDenied)
  const u = await web(v.node, v.root).auth.unlockUser({ username: 'unlocky' })
  eq([u.lockedUntil, u.failedAttempts], [undefined, 0], 'unlocked')
  ok((await attempt(v.node.url, 'unlocky', STRONG)).token, 'signed in')
})

scenario('C', 'a successful sign-in resets the failure counter', async () => {
  const v = await lockVault()
  await createUser(v, 'resetty')
  for (let i = 0; i < 2; i++) await attempt(v.node.url, 'resetty', 'Wrong-Password-123456').catch(() => {})
  await attempt(v.node.url, 'resetty', STRONG)
  for (let i = 0; i < 2; i++) await attempt(v.node.url, 'resetty', 'Wrong-Password-123456').catch(() => {})
  ok((await attempt(v.node.url, 'resetty', STRONG)).token, 'not locked')
})

scenario('C', 'a disabled user cannot sign in (generic message)', async () => {
  const m = await main()
  const name = uniq('dis')
  await createUser(m, name)
  await web(m.node, m.root).auth.upsertUser({ username: name, disabled: true })
  await fails(login(m.node, name, STRONG), Code.Unauthenticated, 'invalid username or password')
})

scenario('C', "disabling a user ends their live sessions immediately", async () => {
  const m = await main()
  const name = uniq('dis2')
  const tok = await createUser(m, name)
  await web(m.node, tok).auth.lookupSelf({})
  await web(m.node, m.root).auth.upsertUser({ username: name, disabled: true })
  await fails(web(m.node, tok).auth.lookupSelf({}), Code.Unauthenticated)
})

scenario('C', "deleting a user ends their live sessions immediately", async () => {
  const m = await main()
  const name = uniq('del')
  const tok = await createUser(m, name)
  await web(m.node, m.root).auth.deleteUser({ username: name })
  await fails(web(m.node, tok).kv.list({ folder: '' }), Code.Unauthenticated)
})

scenario('C', 'a new user must change the admin-set password at first sign-in', async () => {
  const m = await main()
  const name = uniq('first')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG })
  const r = await login(m.node, name, STRONG)
  eq([r.mustChangePassword, r.policies], [true, ['change-password']], 'restricted session')
})

scenario('C', 'a change-password-only session cannot read secrets', async () => {
  const m = await main()
  const name = uniq('first2')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG, policies: ['admin'] })
  const r = await login(m.node, name, STRONG)
  await fails(web(m.node, r.token).kv.list({ folder: '' }), Code.PermissionDenied, 'password change required')
})

scenario('C', 'a change-password-only session can look itself up', async () => {
  const m = await main()
  const name = uniq('first3')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG })
  const r = await login(m.node, name, STRONG)
  eq((await web(m.node, r.token).auth.lookupSelf({})).policies, ['change-password'], 'policies')
})

scenario('C', 'change password with the wrong current password is refused', async () => {
  const m = await main()
  await fails(web(m.node, m.reader).auth.changePassword({ oldPassword: 'Not-The-Password-1!', newPassword: STRONG2 }), Code.InvalidArgument, 'current password is wrong')
})

scenario('C', 'the new password must differ from the current one', async () => {
  const m = await main()
  await fails(web(m.node, m.reader).auth.changePassword({ oldPassword: STRONG, newPassword: STRONG }), Code.InvalidArgument, 'differ')
})

scenario('C', 'the new password must meet the policy', async () => {
  const m = await main()
  await fails(web(m.node, m.reader).auth.changePassword({ oldPassword: STRONG, newPassword: 'short1!' }), Code.InvalidArgument, 'at least 15')
})

scenario('C', 'changing the password ends the session; the new password grants full access', async () => {
  const m = await main()
  const name = uniq('chg')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG, policies: ['read-only'] })
  const r = await login(m.node, name, STRONG)
  await web(m.node, r.token).auth.changePassword({ oldPassword: STRONG, newPassword: STRONG2 })
  await fails(web(m.node, r.token).auth.lookupSelf({}), Code.Unauthenticated)
  const again = await login(m.node, name, STRONG2)
  eq([again.mustChangePassword, again.policies], [false, ['read-only']], 'session')
})

/** cn-mlps keeps the last 5 passwords (us-nist keeps none, per SP 800-63B). */
const cnVault = fixture(() => freshVault({ LOGIN_POLICY: 'cn-mlps', LOGIN_BANNER_REQUIRE_ACK: 'true' }))

scenario('C', 'cn-mlps: a recently used password cannot be reused', async () => {
  const m = await cnVault()
  const name = uniq('hist')
  const tok = await createUser(m, name)
  await web(m.node, tok).auth.changePassword({ oldPassword: STRONG, newPassword: STRONG2 })
  const t2 = (await login(m.node, name, STRONG2)).token
  await fails(web(m.node, t2).auth.changePassword({ oldPassword: STRONG2, newPassword: STRONG }), Code.InvalidArgument, 'reuse')
})

scenario('C', 'the root token has no password to change', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.changePassword({ oldPassword: 'x', newPassword: STRONG2 }), Code.InvalidArgument, 'root token')
})

scenario('C', 'lookup-self on the root token: root policy, no user, no expiry', async () => {
  const m = await main()
  const me = await web(m.node, m.root).auth.lookupSelf({})
  eq([me.policies, me.username, me.expiresAt], [['root'], undefined, undefined], 'root')
})

scenario('C', 'the root token is not renewable; a session is', async () => {
  const m = await main()
  eq((await web(m.node, m.root).auth.renewSelf({})).renewable, false, 'root renewable')
  const before = (await web(m.node, m.reader).auth.lookupSelf({})).expiresAt!
  await sleep(1100)
  const r = await web(m.node, m.reader).auth.renewSelf({})
  ok(r.renewable && new Date(r.expiresAt!) > new Date(before), `renewed ${before} → ${r.expiresAt}`)
})

scenario('C', 'revoke-self ends the session', async () => {
  const m = await main()
  const t = (await login(m.node, 'reader2', STRONG)).token
  await web(m.node, t).auth.revokeSelf({})
  await fails(web(m.node, t).auth.lookupSelf({}), Code.Unauthenticated)
})

scenario('C', 'tokens work as `authorization: Bearer` too', async () => {
  const m = await main()
  const { createClient } = await import('@connectrpc/connect')
  const { createGrpcWebTransport } = await import('@connectrpc/connect-web')
  const { AuthService } = await import('../../src/gen/timika/v1/auth_pb')
  const t = createGrpcWebTransport({ baseUrl: m.node.url, interceptors: [(n) => (req) => { req.header.set('authorization', `Bearer ${m.root}`); return n(req) }] })
  eq((await createClient(AuthService, t).lookupSelf({})).policies, ['root'], 'policies')
})

scenario('C', 'a made-up token is UNAUTHENTICATED', async () => {
  const m = await main()
  await fails(web(m.node, 'tmk.made-up-token').kv.list({ folder: '' }), Code.Unauthenticated)
})

scenario('C', 'sign-in attempts are rate limited per client address', async () => {
  const v = await freshVault({ LOGIN_RATE_LIMIT: '3' })
  for (let i = 0; i < 3; i++) await attempt(v.node.url, 'nobody', 'Wrong-Password-123456').catch(() => {})
  await fails(attempt(v.node.url, 'nobody', 'Wrong-Password-123456'), Code.ResourceExhausted, 'too many')
})

scenario('C', 'the second sign-in reports when and from where the previous one was', async () => {
  const m = await main()
  const name = uniq('last')
  await createUser(m, name)
  const r = await login(m.node, name, STRONG)
  ok(r.lastLogin?.time && r.lastLogin.ip === '127.0.0.1', `lastLogin ${JSON.stringify(r.lastLogin)}`)
})
