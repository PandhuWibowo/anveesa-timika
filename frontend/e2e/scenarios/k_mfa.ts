import { createHmac } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { scenario, fixture } from '../suite'
import { main, uniq } from '../fixtures'
import { freshVault, web, login, createUser, solveCaptcha, ok, eq, fails, sleep, Code, STRONG, type Vault } from '../lib'

// ── an "authenticator app" ──
function b32decode(s: string): Buffer {
  const A = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'
  let bits = 0, val = 0
  const out: number[] = []
  for (const c of s.replace(/=+$/, '').toUpperCase()) {
    val = (val << 5) | A.indexOf(c)
    bits += 5
    if (bits >= 8) { out.push((val >>> (bits - 8)) & 255); bits -= 8 }
  }
  return Buffer.from(out)
}
function totp(secret: string, offsetSteps = 0): string {
  const step = Math.floor(Date.now() / 1000 / 30) + offsetSteps
  const msg = Buffer.alloc(8)
  msg.writeBigUInt64BE(BigInt(step))
  const h = createHmac('sha1', b32decode(secret)).update(msg).digest()
  const o = h[19] & 15
  const n = ((h[o] & 0x7f) << 24) | (h[o + 1] << 16) | (h[o + 2] << 8) | h[o + 3]
  return String(n % 1_000_000).padStart(6, '0')
}

/** A user with 2FA on: returns their secret and recovery codes. */
async function enrolled(v: Vault, name = uniq('mfa'), roles = ['ssh']) {
  const tok = await createUser(v, name, roles)
  const a = web(v.node, tok).auth
  const setup = await a.beginMfaSetup({})
  const done = await a.confirmMfaSetup({ code: totp(setup.secret) })
  return { name, tok, secret: setup.secret, codes: done.recoveryCodes }
}

/** Password step; expects the 2FA challenge. */
async function passwordStep(v: Vault, name: string) {
  const r = await login(v.node, name, STRONG)
  ok(r.mfaRequired && r.mfaToken && !r.token, `password step: ${JSON.stringify({ req: r.mfaRequired, tok: !!r.token })}`)
  return r.mfaToken!
}

const lockVault = fixture(() => freshVault({ LOCKOUT_THRESHOLD: '3' }))
const allVault = fixture(() => freshVault({ MFA_REQUIRED: 'all' }))
const adminsVault = fixture(() => freshVault({ MFA_REQUIRED: 'admins' }))

scenario('K', 'setup gives a secret, an otpauth URI and a QR code; 2FA stays off until confirmed', async () => {
  const m = await main()
  const tok = await createUser(m, uniq('setup'), ['ssh'])
  const a = web(m.node, tok).auth
  eq((await a.getMfaStatus({})).enabled, false, 'off at first')
  const s = await a.beginMfaSetup({})
  ok(/^[A-Z2-7]{32}$/.test(s.secret), `secret ${s.secret}`)
  ok(s.uri.startsWith('otpauth://totp/Timika:') && s.uri.includes(`secret=${s.secret}`), s.uri)
  ok(s.qrSvg.includes('<svg'), 'qr svg')
  eq((await a.getMfaStatus({})).enabled, false, 'still off before confirming')
})

scenario('K', 'confirming needs a right code, then gives 10 one-time recovery codes', async () => {
  const m = await main()
  const tok = await createUser(m, uniq('conf'), ['ssh'])
  const a = web(m.node, tok).auth
  const s = await a.beginMfaSetup({})
  await fails(a.confirmMfaSetup({ code: '000000' }), Code.InvalidArgument, "isn't right")
  const r = await a.confirmMfaSetup({ code: totp(s.secret) })
  eq(r.recoveryCodes.length, 10, 'recovery codes')
  ok(r.recoveryCodes.every((c) => /^[a-z2-9]{5}-[a-z2-9]{5}$/.test(c)), r.recoveryCodes.join(' '))
  const st = await a.getMfaStatus({})
  eq([st.enabled, st.recoveryRemaining], [true, 10], 'status')
  await fails(a.beginMfaSetup({}), Code.Aborted, 'already on')
})

scenario('K', 'sign-in with 2FA: password, then the code, then the session', async () => {
  const m = await main()
  const u = await enrolled(m)
  const mfaToken = await passwordStep(m, u.name)
  await fails(web(m.node, mfaToken).bastion.listAssets({}), Code.Unauthenticated) // not a session
  // The setup code was used already (replay protection): the app's next code.
  const r = await web(m.node).auth.verifyMfa({ mfaToken, code: totp(u.secret, 1) })
  ok(r.token && r.policies.includes('ssh') && !r.mfaRequired, 'session')
  ok((await web(m.node, r.token).bastion.listAssets({})), 'session works')
})

scenario('K', 'a code works once (no replay)', async () => {
  const m = await main()
  const u = await enrolled(m)
  // Use the next step's code so it isn't the one setup already consumed.
  const code = totp(u.secret, 1)
  await web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code })
  await fails(web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code }), Code.Unauthenticated, "isn't right")
})

scenario('K', 'five wrong codes end the attempt; the password is needed again', async () => {
  const m = await main()
  const u = await enrolled(m)
  const mfaToken = await passwordStep(m, u.name)
  for (let i = 0; i < 4; i++) await fails(web(m.node).auth.verifyMfa({ mfaToken, code: '000000' }), Code.Unauthenticated, "isn't right")
  await fails(web(m.node).auth.verifyMfa({ mfaToken, code: '000000' }), Code.Unauthenticated, 'password again')
  await fails(web(m.node).auth.verifyMfa({ mfaToken, code: totp(u.secret, 1) }), Code.Unauthenticated, 'expired')
})

scenario('K', 'wrong codes count toward account lockout', async () => {
  const v = await lockVault()
  const u = await enrolled(v)
  const mfaToken = await passwordStep(v, u.name)
  await fails(web(v.node).auth.verifyMfa({ mfaToken, code: '000000' }), Code.Unauthenticated)
  await fails(web(v.node).auth.verifyMfa({ mfaToken, code: '000001' }), Code.Unauthenticated)
  await fails(web(v.node).auth.verifyMfa({ mfaToken, code: '000002' }), Code.PermissionDenied, 'locked')
  await fails(login(v.node, u.name, STRONG), Code.PermissionDenied, 'locked')
})

scenario('K', 'a recovery code signs in once', async () => {
  const m = await main()
  const u = await enrolled(m)
  const r = await web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code: u.codes[0].toUpperCase() })
  eq([r.usedRecoveryCode, r.recoveryCodesLeft], [true, 9], 'recovery')
  await fails(web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code: u.codes[0] }), Code.Unauthenticated)
})

scenario('K', 'a made-up 2FA token is refused', async () => {
  const m = await main()
  await fails(web(m.node).auth.verifyMfa({ mfaToken: 'nope', code: '123456' }), Code.Unauthenticated, 'expired')
})

scenario('K', 'turning 2FA off needs a valid code; then sign-in is password-only', async () => {
  const m = await main()
  const u = await enrolled(m)
  const a = web(m.node, u.tok).auth
  await fails(a.disableMfa({ code: '000000' }), Code.InvalidArgument, "isn't right")
  await a.disableMfa({ code: totp(u.secret, 1) })
  const r = await login(m.node, u.name, STRONG)
  ok(r.token && !r.mfaRequired, 'password-only again')
})

scenario('K', 'new recovery codes replace the old ones', async () => {
  const m = await main()
  const u = await enrolled(m)
  const fresh = await web(m.node, u.tok).auth.newRecoveryCodes({ code: totp(u.secret, 1) })
  eq(fresh.codes.length, 10, 'codes')
  await fails(web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code: u.codes[1] }), Code.Unauthenticated)
  ok((await web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code: fresh.codes[0] })).token, 'new code works')
})

scenario('K', 'MFA_REQUIRED=all: sign-in without 2FA must set it up first, then gets a full session', async () => {
  const v = await allVault()
  const name = uniq('forced')
  await web(v.node, v.root).auth.upsertUser({ username: name, password: STRONG, policies: ['ssh'], mustChangePassword: false })
  const r = await login(v.node, name, STRONG)
  eq([r.mustSetupMfa, r.policies], [true, ['mfa-setup']], 'setup-only session')
  await fails(web(v.node, r.token).bastion.listAssets({}), Code.PermissionDenied, 'two-factor')
  const a = web(v.node, r.token).auth
  const s = await a.beginMfaSetup({})
  const done = await a.confirmMfaSetup({ code: totp(s.secret) })
  ok(done.session?.token && done.session.policies.includes('ssh'), 'full session returned')
  ok(await web(v.node, done.session!.token).bastion.listAssets({}), 'full session works')
  await fails(web(v.node, r.token).auth.lookupSelf({}), Code.Unauthenticated) // setup token revoked
  eq((await web(v.node).auth.getLoginConfig({})).mfaRequired, 'all', 'login config')
})

scenario('K', 'MFA_REQUIRED=admins: admins must, others may; required 2FA cannot be turned off', async () => {
  const v = await adminsVault()
  const ro = uniq('ro')
  await web(v.node, v.root).auth.upsertUser({ username: ro, password: STRONG, policies: ['read-only'], mustChangePassword: false })
  ok((await login(v.node, ro, STRONG)).token && !(await login(v.node, ro, STRONG)).mustSetupMfa, 'read-only not forced')
  const ad = uniq('ad')
  await web(v.node, v.root).auth.upsertUser({ username: ad, password: STRONG, policies: ['admin'], mustChangePassword: false })
  const r = await login(v.node, ad, STRONG)
  eq(r.policies, ['mfa-setup'], 'admin forced')
  const s = await web(v.node, r.token).auth.beginMfaSetup({})
  const done = await web(v.node, r.token).auth.confirmMfaSetup({ code: totp(s.secret) })
  await fails(web(v.node, done.session!.token).auth.disableMfa({ code: totp(s.secret, 1) }), Code.PermissionDenied, 'required')
})

scenario('K', 'an admin can reset a lost 2FA; People shows who has it', async () => {
  const m = await main()
  const u = await enrolled(m)
  const listed = (await web(m.node, m.root).auth.listUsers({})).users.find((x) => x.username === u.name)!
  eq(listed.mfaEnabled, true, 'listed with 2FA')
  await fails(web(m.node, m.reader).auth.resetUserMfa({ username: u.name }), Code.PermissionDenied)
  const r = await web(m.node, m.root).auth.resetUserMfa({ username: u.name })
  eq(r.mfaEnabled, false, 'reset')
  ok((await login(m.node, u.name, STRONG)).token, 'password-only after reset')
})

scenario('K', 'the root token is unaffected and has no 2FA of its own', async () => {
  const v = await allVault()
  ok(await web(v.node, v.root).bastion.listAssets({}), 'root works under MFA_REQUIRED=all')
  await fails(web(v.node, v.root).auth.beginMfaSetup({}), Code.InvalidArgument, 'root token')
})

scenario('K', 'the 2FA secret and codes never reach the audit log', async () => {
  const m = await main()
  const u = await enrolled(m)
  const code = totp(u.secret, 1)
  await web(m.node).auth.verifyMfa({ mfaToken: await passwordStep(m, u.name), code })
  await sleep(200)
  const raw = readFileSync(m.node.auditFile, 'utf8')
  ok(!raw.includes(u.secret) && !raw.includes(u.codes[2]) && !raw.includes(code), 'secret material in audit log')
  const e = (await web(m.node, m.root).audit.listEvents({ query: u.name })).events
  ok(e.some((x) => x.action === 'AuthService/VerifyMfa' && x.ok && x.user === u.name), 'verify audited with the user')
  ok(e.some((x) => (x.target ?? '').includes('2FA turned on')), 'setup audited')
})

void solveCaptcha
