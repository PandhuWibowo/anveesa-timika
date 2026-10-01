import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { main, ssh, uniq, addServer } from '../fixtures'
import { freshVault, web, createUser, startSsh, ok, eq, fails, waitFor, Code, type Node } from '../lib'

/** A server on the main vault plus a fresh folder on the SSH target for this scenario. */
async function setup() {
  const { asset, m, t } = await addServer()
  const dir = `/${uniq('d')}`
  mkdirSync(join(t.files, dir), { recursive: true })
  return { asset, m, t, dir, files: web(m.node, m.root).bastion }
}

async function upload(node: Node, token: string | null, q: { asset: string; account: string; path: string }, body: BodyInit) {
  const qs = new URLSearchParams(q).toString()
  return fetch(`${node.url}/v1/bastion/files/upload?${qs}`, { method: 'PUT', headers: token ? { 'x-timika-token': token } : {}, body })
}

const sha = (b: Uint8Array | Buffer) => createHash('sha256').update(b).digest('hex')

scenario('J', 'browsing starts in the home folder and lists files and folders, folders first', async () => {
  const { asset, t, dir, files } = await setup()
  mkdirSync(join(t.files, dir, 'zeta-dir'))
  writeFileSync(join(t.files, dir, 'alpha.txt'), 'hello')
  const home = await files.listFiles({ asset: asset.id, account: t.user, path: '' })
  eq(home.path, '/', 'home path')
  const r = await files.listFiles({ asset: asset.id, account: t.user, path: dir })
  eq(r.entries.map((e) => [e.name, e.kind]), [['zeta-dir', 'dir'], ['alpha.txt', 'file']], 'entries')
  const f = r.entries[1]
  ok(f.size === 5n && f.mode.startsWith('-rw') && f.modified, `file ${f.size} ${f.mode} ${f.modified}`)
})

scenario('J', 'an upload streams onto the server', async () => {
  const { asset, m, t, dir } = await setup()
  const r = await upload(m.node, m.root, { asset: asset.id, account: t.user, path: `${dir}/up.txt` }, 'uploaded through the bastion')
  eq(r.status, 200, 'status')
  eq(readFileSync(join(t.files, dir, 'up.txt'), 'utf8'), 'uploaded through the bastion', 'content on disk')
})

scenario('J', 'a download link streams the file with its name', async () => {
  const { asset, m, t, dir, files } = await setup()
  writeFileSync(join(t.files, dir, 'report 2026 ü.csv'), 'a,b\n1,2\n')
  const link = await files.downloadLink({ asset: asset.id, account: t.user, path: `${dir}/report 2026 ü.csv` })
  eq([link.name, link.size], ['report 2026 ü.csv', 8n], 'link')
  const r = await fetch(`${m.node.url}${link.url}`)
  eq(r.status, 200, 'status')
  eq(await r.text(), 'a,b\n1,2\n', 'content')
  ok((r.headers.get('content-disposition') ?? '').includes("filename*=UTF-8''report%202026%20%C3%BC.csv"), `disposition ${r.headers.get('content-disposition')}`)
})

scenario('J', 'a tampered or made-up download link is refused', async () => {
  const { asset, m, t, dir, files } = await setup()
  writeFileSync(join(t.files, dir, 'secret.txt'), 'x')
  const link = await files.downloadLink({ asset: asset.id, account: t.user, path: `${dir}/secret.txt` })
  const ticket = new URL(link.url, m.node.url).searchParams.get('t')!
  const [body, sig] = ticket.split('.')
  const forged = JSON.parse(Buffer.from(body, 'base64url').toString())
  forged.path = '/etc/shadow'
  const bad = `${Buffer.from(JSON.stringify(forged)).toString('base64url')}.${sig}`
  eq((await fetch(`${m.node.url}/v1/bastion/files/download?t=${bad}`)).status, 401, 'forged')
  eq((await fetch(`${m.node.url}/v1/bastion/files/download?t=nonsense`)).status, 401, 'garbage')
})

scenario('J', 'a 12 MiB file goes up and comes back byte for byte', async () => {
  const { asset, m, t, dir, files } = await setup()
  const data = crypto.getRandomValues(new Uint8Array(12 * 1024 * 1024).fill(0).map((_, i) => i % 251))
  const r = await upload(m.node, m.root, { asset: asset.id, account: t.user, path: `${dir}/big.bin` }, data)
  eq(r.status, 200, `upload: ${await r.clone().text()}`)
  eq(sha(readFileSync(join(t.files, dir, 'big.bin'))), sha(data), 'on disk')
  const link = await files.downloadLink({ asset: asset.id, account: t.user, path: `${dir}/big.bin` })
  const back = new Uint8Array(await (await fetch(`${m.node.url}${link.url}`)).arrayBuffer())
  eq(sha(back), sha(data), 'downloaded')
})

scenario('J', 'new folder, rename and recursive delete', async () => {
  const { asset, t, dir, files } = await setup()
  const a = { asset: asset.id, account: t.user }
  await files.makeDir({ ...a, path: `${dir}/new` })
  writeFileSync(join(t.files, dir, 'new', 'f.txt'), 'x')
  mkdirSync(join(t.files, dir, 'new', 'deeper'))
  writeFileSync(join(t.files, dir, 'new', 'deeper', 'g.txt'), 'y')
  await files.renameFile({ ...a, from: `${dir}/new`, to: `${dir}/renamed` })
  ok(existsSync(join(t.files, dir, 'renamed', 'deeper', 'g.txt')), 'renamed')
  const r = await files.deleteFiles({ ...a, paths: [`${dir}/renamed`] })
  eq(r.deleted, 4, 'deleted (2 files + 2 folders)')
  ok(!existsSync(join(t.files, dir, 'renamed')), 'gone')
})

scenario('J', 'renaming onto an existing name is refused; deleting / is refused', async () => {
  const { asset, t, dir, files } = await setup()
  const a = { asset: asset.id, account: t.user }
  writeFileSync(join(t.files, dir, 'a.txt'), 'a')
  writeFileSync(join(t.files, dir, 'b.txt'), 'b')
  await fails(files.renameFile({ ...a, from: `${dir}/a.txt`, to: `${dir}/b.txt` }), Code.Aborted, 'already exists')
  eq(readFileSync(join(t.files, dir, 'b.txt'), 'utf8'), 'b', 'not overwritten')
  await fails(files.deleteFiles({ ...a, paths: ['/'] }), Code.InvalidArgument, 'refusing')
})

scenario('J', 'missing files are NOT_FOUND', async () => {
  const { asset, t, dir, files } = await setup()
  await fails(files.listFiles({ asset: asset.id, account: t.user, path: `${dir}/nope` }), Code.NotFound)
  await fails(files.downloadLink({ asset: asset.id, account: t.user, path: `${dir}/nope.txt` }), Code.NotFound)
})

scenario('J', 'folders cannot be downloaded as a file', async () => {
  const { asset, t, dir, files } = await setup()
  await fails(files.downloadLink({ asset: asset.id, account: t.user, path: dir }), Code.InvalidArgument, 'folders')
})

scenario('J', 'without access to the account there are no files', async () => {
  const { asset, m, t } = await setup()
  const tok = await createUser(m, uniq('nofiles'), ['ssh'])
  await fails(web(m.node, tok).bastion.listFiles({ asset: asset.id, account: t.user, path: '' }), Code.PermissionDenied)
  eq((await upload(m.node, tok, { asset: asset.id, account: t.user, path: '/x.txt' }, 'x')).status, 403, 'upload')
  eq((await upload(m.node, null, { asset: asset.id, account: t.user, path: '/x.txt' }, 'x')).status, 401, 'no token')
})

scenario('J', 'a grant to one account gives files as that account only', async () => {
  const { asset, m, t } = await setup()
  const b = web(m.node, m.root).bastion
  await b.updateAsset({ id: asset.id, asset: { name: asset.name, host: asset.host, port: asset.port, accounts: [{ username: t.user }, { username: 'other', password: 'x' }] } })
  const name = uniq('acc-only')
  const tok = await createUser(m, name, ['ssh'])
  await b.createGrant({ asset: asset.id, subjectType: 'user', subject: name, accounts: [t.user] })
  ok((await web(m.node, tok).bastion.listFiles({ asset: asset.id, account: t.user, path: '' })).path, 'own account works')
  await fails(web(m.node, tok).bastion.listFiles({ asset: asset.id, account: 'other', path: '' }), Code.PermissionDenied)
})

scenario('J', "file operations appear in the server's activity", async () => {
  const { asset, m, t, dir, files } = await setup()
  const a = { asset: asset.id, account: t.user }
  await upload(m.node, m.root, { ...a, path: `${dir}/audited.txt` }, 'abc')
  const link = await files.downloadLink({ ...a, path: `${dir}/audited.txt` })
  await (await fetch(`${m.node.url}${link.url}`)).text()
  await files.deleteFiles({ ...a, paths: [`${dir}/audited.txt`] })
  const evs = await waitFor('file events', async () => {
    const e = (await web(m.node, m.root).audit.listEvents({ server: asset.id, category: 'files' })).events
    return e.length >= 3 && e
  })
  const targets = evs.map((e) => `${e.action} ${e.target}`)
  ok(targets.some((x) => x.startsWith('PUT /v1/bastion/files/upload') && x.includes(`${dir}/audited.txt (upload 3 bytes)`)), targets.join(' | '))
  ok(targets.some((x) => x.startsWith('GET /v1/bastion/files/download') && x.includes('(download)')), 'download audited')
  ok(evs.find((e) => e.action === 'GET /v1/bastion/files/download')?.user === 'root', 'download attributed to the person')
  ok(targets.some((x) => x.startsWith('BastionService/DeleteFiles')), 'delete audited')
})

const smallLimit = fixture(async () => {
  const v = await freshVault({ FILES_MAX_UPLOAD_MB: '1' })
  const t = await startSsh({ hostKeyName: `lim_${Date.now()}` })
  const r = await web(v.node, v.root).bastion.createAsset({ name: 'lim', host: '127.0.0.1', port: t.port, accounts: [{ username: t.user, password: t.password }] })
  return { v, t, asset: r.asset! }
})

scenario('J', 'uploads over the size limit are refused and leave nothing behind', async () => {
  const { v, t, asset } = await smallLimit()
  const r = await upload(v.node, v.root, { asset: asset.id, account: t.user, path: '/too-big.bin' }, new Uint8Array(2 * 1024 * 1024))
  eq(r.status, 400, 'status')
  ok((await r.text()).includes('upload limit'), 'message')
  ok(!existsSync(join(t.files, 'too-big.bin')), 'partial file removed')
})

scenario('J', 'browsing reuses one connection (fast after the first call)', async () => {
  const { asset, t, dir, files } = await setup()
  await files.listFiles({ asset: asset.id, account: t.user, path: dir })
  const t0 = performance.now()
  for (let i = 0; i < 10; i++) await files.listFiles({ asset: asset.id, account: t.user, path: dir })
  const avg = (performance.now() - t0) / 10
  ok(avg < 150, `average listing ${avg.toFixed(0)} ms`)
})

void ssh
void main

// ─── archives ────────────────────────────────────────────────────────────────

/** Folder `site/` (two files, one nested) and a file `notes.txt` in a fresh dir. */
async function tree() {
  const s = await setup()
  mkdirSync(join(s.t.files, s.dir, 'site', 'css'), { recursive: true })
  writeFileSync(join(s.t.files, s.dir, 'site', 'index.html'), '<h1>hi</h1>')
  writeFileSync(join(s.t.files, s.dir, 'site', 'css', 'app.css'), 'body{}')
  writeFileSync(join(s.t.files, s.dir, 'notes.txt'), 'remember')
  return { ...s, a: { asset: s.asset.id, account: s.t.user } }
}

/** List an archive's entries with the system tools. */
function entries(file: string): string[] {
  const zip = file.endsWith('.zip')
  const r = Bun.spawnSync(zip ? ['unzip', '-Z1', file] : ['tar', '-tf', file])
  if (r.exitCode !== 0) throw new Error(`cannot read ${file}: ${r.stderr}`)
  return r.stdout.toString().split('\n').filter(Boolean).map((l) => l.replace(/^\.\//, '').replace(/\/$/, '')).filter((l) => l && l !== '.').sort()
}

const ALL = ['notes.txt', 'site', 'site/css', 'site/css/app.css', 'site/index.html']

scenario('J', 'compress a folder and a file into a .tar.gz on the server', async () => {
  const { files, a, t, dir } = await tree()
  const r = await files.compress({ ...a, dir, names: ['site', 'notes.txt'], format: 'tar.gz', archive: 'release' })
  eq(r.path, `${dir}/release.tar.gz`, 'path (extension added)')
  ok(r.size > 0n, 'size')
  eq(entries(join(t.files, dir, 'release.tar.gz')), ALL, 'entries')
})

scenario('J', 'compress as .zip, .tar.xz and .tar', async () => {
  const { files, a, t, dir } = await tree()
  for (const [format, file] of [['zip', 'site.zip'], ['tar.xz', 'site.tar.xz'], ['tar', 'site.tar']]) {
    const r = await files.compress({ ...a, dir, names: ['site'], format, archive: '' })
    eq(r.path, `${dir}/${file}`, `${format} default name`)
    eq(entries(join(t.files, dir, file)), ['site', 'site/css', 'site/css/app.css', 'site/index.html'], `${format} entries`)
  }
})

scenario('J', 'compressing onto an existing archive name is refused', async () => {
  const { files, a, dir } = await tree()
  await files.compress({ ...a, dir, names: ['notes.txt'], format: 'zip', archive: 'n.zip' })
  await fails(files.compress({ ...a, dir, names: ['site'], format: 'zip', archive: 'n.zip' }), Code.Aborted, 'already exists')
})

scenario('J', 'extracting makes a new folder named after the archive, then "name (2)"', async () => {
  const { files, a, t, dir } = await tree()
  await files.compress({ ...a, dir, names: ['site', 'notes.txt'], format: 'tar.gz', archive: 'bundle' })
  const first = await files.extract({ ...a, path: `${dir}/bundle.tar.gz` })
  eq(first.dir, `${dir}/bundle`, 'first')
  eq(readFileSync(join(t.files, dir, 'bundle', 'site', 'css', 'app.css'), 'utf8'), 'body{}', 'content')
  eq((await files.extract({ ...a, path: `${dir}/bundle.tar.gz` })).dir, `${dir}/bundle (2)`, 'second')
})

scenario('J', 'zip archives extract too', async () => {
  const { files, a, t, dir } = await tree()
  await files.compress({ ...a, dir, names: ['site'], format: 'zip', archive: 'z' })
  const r = await files.extract({ ...a, path: `${dir}/z.zip` })
  eq(readFileSync(join(t.files, dir, 'z', 'site', 'index.html'), 'utf8'), '<h1>hi</h1>', `content in ${r.dir}`)
})

scenario('J', 'a folder downloads as one streamed .tar.gz (nothing left on the server)', async () => {
  const { files, a, m, t, dir } = await tree()
  const link = await files.archiveLink({ ...a, dir, names: ['site'], format: 'tar.gz' })
  eq(link.name, 'site.tar.gz', 'name')
  const r = await fetch(`${m.node.url}${link.url}`)
  eq([r.status, r.headers.get('content-type')], [200, 'application/gzip'], 'response')
  const local = join(t.files, '..', `dl-${Date.now()}.tar.gz`)
  writeFileSync(local, new Uint8Array(await r.arrayBuffer()))
  eq(entries(local), ['site', 'site/css', 'site/css/app.css', 'site/index.html'], 'entries')
  ok(!readdirNames(join(t.files, dir)).some((n) => n.endsWith('.tar.gz')), 'no archive left on the server')
})

scenario('J', 'a selection downloads as one .zip', async () => {
  const { files, a, m, t, dir } = await tree()
  const link = await files.archiveLink({ ...a, dir, names: ['site', 'notes.txt'], format: 'zip' })
  ok(link.name.endsWith('.zip'), link.name)
  const local = join(t.files, '..', `dl-${Date.now()}.zip`)
  writeFileSync(local, new Uint8Array(await (await fetch(`${m.node.url}${link.url}`)).arrayBuffer()))
  eq(entries(local), ALL, 'entries')
})

scenario('J', 'odd file names (-rf, quotes, spaces) are archived as names, never as options or code', async () => {
  const { files, a, t, dir } = await setup().then((s) => ({ ...s, a: { asset: s.asset.id, account: s.t.user } }))
  for (const n of ['-rf', "it's here", 'a b.txt']) writeFileSync(join(t.files, dir, n), n)
  await files.compress({ ...a, dir, names: ['-rf', "it's here", 'a b.txt'], format: 'tar', archive: 'odd' })
  eq(entries(join(t.files, dir, 'odd.tar')), ['-rf', 'a b.txt', "it's here"], 'entries')
})

scenario('J', 'archives refuse names outside the folder and non-archives', async () => {
  const { files, a, dir } = await tree()
  await fails(files.compress({ ...a, dir, names: ['../etc'], format: 'zip', archive: 'x' }), Code.InvalidArgument, 'invalid name')
  await fails(files.compress({ ...a, dir, names: ['site'], format: 'rar', archive: 'x' }), Code.InvalidArgument, 'unsupported format')
  await fails(files.extract({ ...a, path: `${dir}/notes.txt` }), Code.InvalidArgument, "isn't a zip or tar")
  await fails(files.archiveLink({ ...a, dir, names: ['a/b'], format: 'zip' }), Code.InvalidArgument)
})

scenario('J', 'compressing and archive downloads are in the audit trail', async () => {
  const { files, a, m, asset, dir } = await tree()
  await files.compress({ ...a, dir, names: ['site'], format: 'tar.gz', archive: 'aud' })
  const link = await files.archiveLink({ ...a, dir, names: ['notes.txt'], format: 'zip' })
  await (await fetch(`${m.node.url}${link.url}`)).arrayBuffer()
  const evs = await waitFor('archive events', async () => {
    const e = (await web(m.node, m.root).audit.listEvents({ server: asset.id, category: 'files' })).events
    return e.some((x) => x.action === 'BastionService/Compress') && e.some((x) => (x.target ?? '').includes('as zip')) && e
  })
  ok(evs.find((x) => x.action === 'BastionService/Compress')?.target?.includes('compress site → aud.tar.gz'), 'compress target')
})

function readdirNames(p: string) {
  return require('node:fs').readdirSync(p) as string[]
}
