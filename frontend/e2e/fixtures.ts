import { fixture } from './suite'
import { freshVault, createUser, startSsh, web, type Vault } from './lib'

export type Main = Vault & { admin: string; reader: string; reader2: string }

/** One unsealed vault shared by most scenarios, with an admin and two read-only users. */
export const main = fixture<Main>(async () => {
  const v = await freshVault()
  const admin = await createUser(v, 'ops-admin', ['admin'])
  const reader = await createUser(v, 'reader', ['read-only'])
  const reader2 = await createUser(v, 'reader2', ['read-only'])
  return { ...v, admin, reader, reader2 }
})

export const ssh = fixture(() => startSsh())

let n = 0
/** A unique path / name per call so scenarios never collide on the shared vault. */
export const uniq = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${++n}`

/** A server registered on the main vault, pointing at the SSH target. */
export async function addServer(name = uniq('srv'), opts: { test?: boolean } = {}) {
  const m = await main()
  const t = await ssh()
  const r = await web(m.node, m.root).bastion.createAsset({
    name,
    host: '127.0.0.1',
    port: t.port,
    accounts: [{ username: t.user, password: t.password }],
    test: opts.test ?? true,
  })
  return { m, t, asset: r.asset!, test: r.test }
}
