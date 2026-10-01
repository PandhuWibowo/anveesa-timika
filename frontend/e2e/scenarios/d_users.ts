import { scenario } from '../suite'
import { main, uniq } from '../fixtures'
import { web, login, ok, eq, fails, Code, STRONG, STRONG2 } from '../lib'

scenario('D', 'admins list users and the assignable roles', async () => {
  const m = await main()
  const r = await web(m.node, m.admin).auth.listUsers({})
  eq(r.roles, ['admin', 'read-only', 'ssh'], 'roles')
  ok(['ops-admin', 'reader', 'reader2'].every((u) => r.users.some((x) => x.username === u)), 'users')
})

scenario('D', 'read-only users cannot list users', async () => {
  const m = await main()
  await fails(web(m.node, m.reader).auth.listUsers({}), Code.PermissionDenied)
})

scenario('D', 'an invalid username is refused', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.upsertUser({ username: 'bad name!', password: STRONG }), Code.InvalidArgument, 'username')
})

scenario('D', 'a new user needs a password', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.upsertUser({ username: uniq('nopw') }), Code.InvalidArgument, 'password is required')
})

scenario('D', 'an unknown role is refused', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.upsertUser({ username: uniq('role'), password: STRONG, policies: ['superuser'] }), Code.InvalidArgument, 'policies')
})

scenario('D', 'a weak password is refused when creating a user', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.upsertUser({ username: uniq('weak'), password: 'password' }), Code.InvalidArgument, 'password must be')
})

scenario('D', 'a password containing the username is refused', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.upsertUser({ username: 'charlie', password: 'Charlie-Long-Password-1!' }), Code.InvalidArgument)
})

scenario('D', 'get-user returns the profile and never a password hash', async () => {
  const m = await main()
  const u = await web(m.node, m.root).auth.getUser({ username: 'reader' })
  eq([u.username, u.policies, u.disabled], ['reader', ['read-only'], false], 'user')
  ok(!/argon2|\$argon/.test(JSON.stringify(u)), 'hash leaked')
})

scenario('D', 'get-user for an unknown name is NOT_FOUND', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.getUser({ username: 'ghost-user' }), Code.NotFound)
})

scenario('D', 'changing only the roles keeps the password', async () => {
  const m = await main()
  const name = uniq('roles')
  await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG, mustChangePassword: false })
  const u = await web(m.node, m.root).auth.upsertUser({ username: name, policies: ['admin'] })
  eq(u.policies, ['admin'], 'policies')
  eq((await login(m.node, name, STRONG)).policies, ['admin'], 'session policies')
})

scenario('D', 'an administrator cannot delete their own account', async () => {
  const m = await main()
  await fails(web(m.node, m.admin).auth.deleteUser({ username: 'ops-admin' }), Code.InvalidArgument, 'own account')
})

scenario('D', 'deleting an unknown user is NOT_FOUND', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.deleteUser({ username: 'ghost-user' }), Code.NotFound)
})

scenario('D', 'usernames are stored lower-case', async () => {
  const m = await main()
  const name = uniq('Mixed').replace('mixed', 'MiXeD')
  const u = await web(m.node, m.root).auth.upsertUser({ username: name, password: STRONG })
  eq(u.username, name.toLowerCase(), 'username')
})

scenario('D', 'read-only users cannot create users', async () => {
  const m = await main()
  await fails(web(m.node, m.reader).auth.upsertUser({ username: uniq('x'), password: STRONG2 }), Code.PermissionDenied)
})

scenario('D', 'unlocking an unknown user is NOT_FOUND', async () => {
  const m = await main()
  await fails(web(m.node, m.root).auth.unlockUser({ username: 'ghost-user' }), Code.NotFound)
})
