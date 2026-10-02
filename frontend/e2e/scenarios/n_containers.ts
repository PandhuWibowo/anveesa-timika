// Container management (Docker or Podman): container details, files in a container (browse, upload,
// download, folders), images, volumes — all `docker` over SSH. The SSH target
// gets a stand-in `docker` that maps a container's paths onto a folder, so
// the real commands (stat, mkdir, rm, cat, tar) run.
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { uniq } from '../fixtures'
import { freshVault, web, createUser, startSsh, ok, eq, fails, waitFor, wsRefusal, auditEntries, WORK, Code } from '../lib'

const FAKE = `#!/bin/sh
ROOT="$FAKE_DOCKER_ROOT"
cmd=$1; shift
case "$cmd" in
  exec)
    while [ "\${1#-}" != "$1" ]; do shift; done
    name=$1; shift
    [ "$name" = noshell ] && { echo 'OCI runtime exec failed: exec: "sh": executable file not found in $PATH: unknown'; exit 126; }
    [ "$name" = stopped ] && { echo "Error response from daemon: container 1a2b is not running"; exit 1; }
    [ -d "$ROOT/$name" ] || { echo "Error response from daemon: No such container: $name"; exit 1; }
    n=$#
    while [ $n -gt 0 ]; do a=$1; shift; case "$a" in /*) a="$ROOT/$name$a" ;; esac; set -- "$@" "$a"; n=$((n - 1)); done
    PATH="$ROOT/shim:$PATH" exec "$@" ;;
  cp)
    n=\${1%%:*}; p=\${1#*:}
    exec tar -cf - -C "$ROOT/$n$(dirname "$p")" "$(basename "$p")" ;;
  inspect)
    if [ "$1" = --format ]; then cat "$ROOT/voluse.txt"; elif [ "$1" = web ]; then cat "$ROOT/inspect.json"; else echo "Error: No such object: $1"; exit 1; fi ;;
  images) cat "$ROOT/images.txt" ;;
  ps) if [ "$1" = -aq ]; then echo c1; else cat "$ROOT/uses.txt"; fi ;;
  system) cat "$ROOT/sizes.txt" ;;
  volume)
    case "$1" in
      ls) cat "$ROOT/vols.txt" ;;
      rm) if [ "$2" = pgdata ]; then echo "Error response from daemon: remove pgdata: volume is in use - [c1]"; exit 1; else echo "$2"; fi ;;
      prune) echo "Deleted Volumes:"; echo cache; echo "Total reclaimed space: 12MB" ;;
    esac ;;
  rmi) echo "Untagged: $1" ;;
  image) echo "Total reclaimed space: 64.5MB" ;;
  *) echo "docker $cmd $*" ;;
esac
`
// BSD stat (macOS) has no -c: translate the one format timika uses.
const STAT = `#!/bin/sh
if /usr/bin/stat -c %s / >/dev/null 2>&1; then exec /usr/bin/stat "$@"; fi
d=\${2%%|*}; shift 2; [ "$1" = -- ] && shift
exec /usr/bin/stat -f "$d|%HT|%z|%m|%Lp|%N" "$@"
`
const INSPECT = JSON.stringify([{
  Id: '0123456789abcdef0123456789abcdef', Name: '/web', Created: '2026-09-30T08:00:00Z',
  State: { Status: 'running', StartedAt: '2026-10-01T09:00:00Z', ExitCode: 0, Health: { Status: 'healthy' } },
  Config: { Image: 'nginx:1.27', Entrypoint: ['/docker-entrypoint.sh'], Cmd: ['nginx', '-g', 'daemon off;'], WorkingDir: '/app', User: 'www-data',
    Env: ['PATH=/usr/bin', 'DB_PASSWORD=s3cret-db', 'EMPTY='], Labels: { 'com.docker.compose.project': 'shop', 'com.docker.compose.service': 'web' } },
  HostConfig: { RestartPolicy: { Name: 'unless-stopped' } },
  Mounts: [{ Type: 'volume', Name: 'webdata', Source: '/var/lib/docker/volumes/webdata/_data', Destination: '/data', RW: true }, { Type: 'bind', Source: '/etc/app', Destination: '/config', RW: false }],
  NetworkSettings: { Networks: { shop_default: { IPAddress: '172.18.0.3' } }, Ports: { '80/tcp': [{ HostIp: '0.0.0.0', HostPort: '8080' }], '443/tcp': null } },
}])

const dock = fixture(async () => {
  const v = await freshVault({ MONITOR_INTERVAL_SECS: '1' })
  const admin = await createUser(v, 'dock-admin', ['admin'])
  const c = web(v.node, admin)
  const root = join(WORK, uniq('droot'))
  const bin = join(root, 'bin')
  for (const d of [bin, join(root, 'shim'), join(root, 'web/usr/share/nginx/html/assets'), join(root, 'web/etc/nginx')]) mkdirSync(d, { recursive: true })
  writeFileSync(join(bin, 'docker'), FAKE); chmodSync(join(bin, 'docker'), 0o755)
  writeFileSync(join(root, 'shim/stat'), STAT); chmodSync(join(root, 'shim/stat'), 0o755)
  writeFileSync(join(root, 'inspect.json'), INSPECT)
  writeFileSync(join(root, 'web/usr/share/nginx/html/index.html'), '<h1>hi</h1>\n')
  writeFileSync(join(root, 'web/usr/share/nginx/html/assets/app.js'), 'console.log(1)\n')
  writeFileSync(join(root, 'web/usr/share/nginx/html/.htaccess'), 'deny\n')
  writeFileSync(join(root, 'web/etc/nginx/nginx.conf'), 'worker_processes 1;\n')
  writeFileSync(join(root, 'images.txt'), 'img=nginx|1.27|sha256:aaaaaaaaaaaa1111|187MB|3 weeks ago\nimg=<none>|<none>|sha256:cccccccccccc3333|64.5MB|5 days ago\n')
  writeFileSync(join(root, 'uses.txt'), 'use=web|nginx:1.27\n')
  writeFileSync(join(root, 'vols.txt'), 'vol=pgdata|local|/var/lib/docker/volumes/pgdata/_data\nvol=cache|local|/var/lib/docker/volumes/cache/_data\n')
  writeFileSync(join(root, 'voluse.txt'), 'use=/db|pgdata,\n')
  writeFileSync(join(root, 'sizes.txt'), 'siz=pgdata|1.5GB\nsiz=cache|12MB\n')
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('dock-host'), env: { PATH: `${bin}:${process.env.PATH}`, FAKE_DOCKER_ROOT: root } })
  const a = await c.bastion.createAsset({ name: 'dock-1', host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: true })
  const id = a.asset!.id
  await c.monitor.setMonitoring({ assets: [id], enabled: true })
  const other = await c.bastion.createAsset({ name: 'not-monitored', host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  return { v, node: v.node, admin, c, t, id, root, other: other.asset!.id }
})

const put = (node: { url: string }, token: string | null, q: Record<string, string>, body: BodyInit) =>
  fetch(`${node.url}/v1/containers/upload?${new URLSearchParams(q)}`, { method: 'PUT', headers: token ? { 'x-timika-token': token } : {}, body })

scenario('N', 'a container’s details: image, command, environment, mounts, networks, published ports, compose', async () => {
  const x = await dock()
  const d = await x.c.containers.inspectContainer({ asset: x.id, name: 'web' })
  eq([d.id, d.name, d.image, d.command, d.state, d.health], ['0123456789ab', 'web', 'nginx:1.27', '/docker-entrypoint.sh nginx -g daemon off;', 'running', 'healthy'], 'about')
  eq([d.restartPolicy, d.workingDir, d.user, d.composeProject, d.composeService], ['unless-stopped', '/app', 'www-data', 'shop', 'web'], 'config')
  eq(d.env.map((e) => [e.name, e.value]), [['PATH', '/usr/bin'], ['DB_PASSWORD', 's3cret-db'], ['EMPTY', '']], 'environment')
  eq(d.mounts.map((m) => [m.kind, m.source, m.destination, m.readOnly]), [['volume', 'webdata', '/data', false], ['bind', '/etc/app', '/config', true]], 'mounts')
  eq([d.networks.map((n) => [n.name, n.ip]), d.ports], [[['shop_default', '172.18.0.3']], ['0.0.0.0:8080 → 80/tcp', '443/tcp (not published)']], 'networks and ports')
  await fails(x.c.containers.inspectContainer({ asset: x.id, name: 'nope' }), Code.InvalidArgument, 'No such object')
  await fails(x.c.containers.inspectContainer({ asset: x.id, name: 'web; id' }), Code.InvalidArgument, 'invalid container name')
  await fails(x.c.containers.inspectContainer({ asset: x.other, name: 'web' }), Code.NotFound, 'not monitored')
})

scenario('N', 'files in a container: browse folders, hidden files, sizes and modes; missing folders and shell-less images are explained', async () => {
  const x = await dock()
  const html = await x.c.containers.listFiles({ asset: x.id, name: 'web', path: '/usr/share/nginx/html' })
  eq(html.entries.map((e) => [e.name, e.kind]), [['assets', 'dir'], ['.htaccess', 'file'], ['index.html', 'file']], 'folders first, hidden files included')
  const index = html.entries.find((e) => e.name === 'index.html')!
  ok(index.size === 12n && index.mode === 'rw-r--r--' && Number(index.modified) > 1_700_000_000, `size, mode, time: ${index.size} ${index.mode} ${index.modified}`)
  eq((await x.c.containers.listFiles({ asset: x.id, name: 'web', path: '/usr/share/../share/nginx/html/assets/' })).path, '/usr/share/nginx/html/assets', 'path normalized')
  eq((await x.c.containers.listFiles({ asset: x.id, name: 'web', path: '' })).entries.map((e) => e.name), ['etc', 'usr'], 'root')
  await fails(x.c.containers.listFiles({ asset: x.id, name: 'web', path: '/nope' }), Code.NotFound, 'folder')
  await fails(x.c.containers.listFiles({ asset: x.id, name: 'web', path: 'relative' }), Code.InvalidArgument, 'starts with /')
  await fails(x.c.containers.listFiles({ asset: x.id, name: 'noshell', path: '/' }), Code.InvalidArgument, 'has no shell')
  await fails(x.c.containers.listFiles({ asset: x.id, name: 'stopped', path: '/' }), Code.InvalidArgument, 'not running')
  await fails(x.c.containers.listFiles({ asset: x.id, name: 'ghost', path: '/' }), Code.InvalidArgument, 'no longer exists')
})

scenario('N', 'transfer: upload a file and a folder into a container, download a file byte for byte and a folder as a tar', async () => {
  const x = await dock()
  const bytes = new Uint8Array(300_000).map((_, i) => (i * 31) % 251)
  const base = { asset: x.id, container: 'web' }
  const up = await put(x.node, x.admin, { ...base, path: '/usr/share/nginx/html/logo.bin' }, bytes)
  eq([up.status, (await up.json()).size], [200, 300000], 'uploaded')
  ok(Buffer.from(readFileSync(join(x.root, 'web/usr/share/nginx/html/logo.bin'))).equals(Buffer.from(bytes)), 'the container has the same bytes')
  // A folder upload: files with relative paths; folders are created.
  for (const [rel, text] of [['site/css/a.css', 'a{}'], ['site/it\'s here.txt', 'quote']]) eq((await put(x.node, x.admin, { ...base, path: `/srv/${rel}` }, text)).status, 200, rel)
  eq((await x.c.containers.listFiles({ asset: x.id, name: 'web', path: '/srv/site' })).entries.map((e) => e.name), ['css', "it's here.txt"], 'folder arrived')

  const link = await x.c.containers.downloadLink({ asset: x.id, name: 'web', path: '/usr/share/nginx/html/logo.bin' })
  eq(link.name, 'logo.bin', 'a file keeps its name')
  const got = await fetch(`${x.node.url}${link.url}`)
  eq([got.status, got.headers.get('content-disposition')?.includes('logo.bin')], [200, true], 'download')
  ok(Buffer.from(await got.arrayBuffer()).equals(Buffer.from(bytes)), 'byte for byte')

  const dir = await x.c.containers.downloadLink({ asset: x.id, name: 'web', path: '/usr/share/nginx/html' })
  eq(dir.name, 'html.tar', 'a folder is a tar')
  const tar = Buffer.from(await (await fetch(`${x.node.url}${dir.url}`)).arrayBuffer())
  const tarFile = join(WORK, uniq('dl') + '.tar')
  writeFileSync(tarFile, tar)
  const listed = Bun.spawnSync(['tar', '-tf', tarFile]).stdout.toString()
  ok(listed.includes('html/index.html') && listed.includes('html/assets/app.js') && listed.includes('html/logo.bin'), `tar contents: ${listed}`)

  await fails(x.c.containers.downloadLink({ asset: x.id, name: 'web', path: '/nope.txt' }), Code.NotFound)
  eq((await fetch(`${x.node.url}${link.url}x`)).status, 401, 'tampered link')
  eq((await put(x.node, null, { ...base, path: '/x' }, 'a')).status, 401, 'no token')
  eq((await put(x.node, x.admin, { ...base, path: '/' }, 'a')).status, 400, 'a folder is not a file name')
  eq((await put(x.node, x.admin, { asset: x.id, container: 'web; id', path: '/x' }, 'a')).status, 400, 'bad container name')
})

scenario('N', 'new folder and delete inside a container; / can’t be deleted', async () => {
  const x = await dock()
  await x.c.containers.makeDir({ asset: x.id, name: 'web', path: '/tmp/a/b' })
  ok(existsSync(join(x.root, 'web/tmp/a/b')), 'created with parents')
  writeFileSync(join(x.root, 'web/tmp/a/keep.txt'), 'k')
  await x.c.containers.deleteFiles({ asset: x.id, name: 'web', paths: ['/tmp/a/b', '/tmp/a/keep.txt'] })
  eq((await x.c.containers.listFiles({ asset: x.id, name: 'web', path: '/tmp/a' })).entries.length, 0, 'deleted')
  await fails(x.c.containers.deleteFiles({ asset: x.id, name: 'web', paths: ['/'] }), Code.InvalidArgument, 'not /')
  await fails(x.c.containers.deleteFiles({ asset: x.id, name: 'web', paths: ['/tmp/..'] }), Code.InvalidArgument, 'not /')
  await fails(x.c.containers.deleteFiles({ asset: x.id, name: 'web', paths: [] }), Code.InvalidArgument)
  ok(existsSync(join(x.root, 'web/etc/nginx/nginx.conf')), 'nothing else touched')
})

scenario('N', 'images and volumes: size, what uses them, remove and clean up — the runtime’s refusals are shown', async () => {
  const x = await dock()
  const images = (await x.c.containers.listImages({ asset: x.id })).images
  eq(images.map((i) => [i.repository, i.tag, i.id, i.size, i.usedBy, i.dangling]), [['nginx', '1.27', 'aaaaaaaaaaaa', 187_000_000n, ['web'], false], ['<none>', '<none>', 'cccccccccccc', 64_500_000n, [], true]], 'images')
  const rm = await x.c.containers.removeImage({ asset: x.id, image: 'nginx:1.27' })
  eq([rm.ok, rm.output], [true, 'Untagged: nginx:1.27'], 'remove image')
  ok((await x.c.containers.pruneImages({ asset: x.id })).output.includes('reclaimed space: 64.5MB'), 'prune images')
  await fails(x.c.containers.removeImage({ asset: x.id, image: 'nginx; id' }), Code.InvalidArgument, 'invalid image')
  const vols = (await x.c.containers.listVolumes({ asset: x.id })).volumes
  eq(vols.map((v) => [v.name, v.driver, v.size, v.usedBy]), [['cache', 'local', 12_000_000n, []], ['pgdata', 'local', 1_500_000_000n, ['db']]], 'volumes')
  const busy = await x.c.containers.removeVolume({ asset: x.id, name: 'pgdata' })
  ok(!busy.ok && busy.output.includes('volume is in use'), 'in use: refused, with docker’s reason')
  eq((await x.c.containers.removeVolume({ asset: x.id, name: 'cache' })).ok, true, 'unused: removed')
  ok((await x.c.containers.pruneVolumes({ asset: x.id })).output.endsWith('Total reclaimed space: 12MB'), 'prune volumes')
})

scenario('N', 'Container management is for administrators: readers and people with access to the server are refused everywhere, also for a shell', async () => {
  const x = await dock()
  const name = uniq('dock-user')
  const token = await createUser(x.v, name, ['ssh'])
  await x.c.bastion.createGrant({ asset: x.id, subjectType: 'user', subject: name })
  const u = web(x.node, token).containers
  const ref = { asset: x.id, name: 'web' }
  await fails(u.inspectContainer(ref), Code.PermissionDenied)
  await fails(u.listFiles({ ...ref, path: '/' }), Code.PermissionDenied)
  await fails(u.makeDir({ ...ref, path: '/x' }), Code.PermissionDenied)
  await fails(u.deleteFiles({ ...ref, paths: ['/x'] }), Code.PermissionDenied)
  await fails(u.downloadLink({ ...ref, path: '/etc/nginx/nginx.conf' }), Code.PermissionDenied)
  await fails(u.listImages({ asset: x.id }), Code.PermissionDenied)
  await fails(u.removeImage({ asset: x.id, image: 'nginx' }), Code.PermissionDenied)
  await fails(u.listVolumes({ asset: x.id }), Code.PermissionDenied)
  await fails(u.pruneVolumes({ asset: x.id }), Code.PermissionDenied)
  eq((await put(x.node, token, { asset: x.id, container: 'web', path: '/x' }, 'a')).status, 403, 'upload')
  // They may open a terminal on the server (granted) — but not inside a container.
  eq((await wsRefusal(x.node, token, { asset: x.id, account: 'ops', container: 'web' })).status, 403, 'container shell')
  eq((await wsRefusal(x.node, x.admin, { asset: x.id, account: 'ops', container: 'web; id' })).status, 400, 'bad container name')
  const raw = JSON.stringify(auditEntries(x.node))
  ok(raw.includes('(upload') && raw.includes('(download)') && raw.includes('→ container web') && !raw.includes('s3cret-db'), 'transfers and shells are audited; no content')
})

scenario('N', 'Podman: a server that runs podman is driven with podman — files, logs, images, start / stop — and says so', async () => {
  const x = await dock()
  // The same stand-in, as `podman`, with its own containers.
  const root = join(x.root, 'podman-root')
  const bin = join(root, 'bin')
  for (const d of [bin, join(root, 'pod-web/srv/www')]) mkdirSync(d, { recursive: true })
  writeFileSync(join(bin, 'podman'), FAKE.replace('ROOT="$FAKE_DOCKER_ROOT"', `ROOT="${root}"`).replace('PATH="$ROOT/shim:$PATH"', `PATH="${x.root}/shim:$PATH"`).replace('echo "docker $cmd $*"', 'echo "podman $cmd $*"'))
  chmodSync(join(bin, 'podman'), 0o755)
  writeFileSync(join(root, 'pod-web/srv/www/index.html'), 'from podman\n')
  writeFileSync(join(root, 'images.txt'), 'img=docker.io/library/nginx|latest|sha256:dddddddddddd4444|192 MB|2 weeks ago\n')
  writeFileSync(join(root, 'uses.txt'), 'use=pod-web|docker.io/library/nginx:latest\n')
  writeFileSync(join(root, 'vols.txt'), 'vol=podvol|local|/home/ops/.local/share/containers/storage/volumes/podvol/_data\n')
  writeFileSync(join(root, 'voluse.txt'), 'use=pod-web|podvol,\n')
  writeFileSync(join(root, 'sizes.txt'), '')
  // Only podman is on this server's PATH (no `docker`).
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('podman-host'), env: { PATH: `${bin}:/usr/bin:/bin` } })
  const a = await x.c.bastion.createAsset({ name: uniq('podbox'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: true })
  const id = a.asset!.id
  const now = Math.floor(Date.now() / 1000)
  writeFileSync(join(t.files, '.timika-metrics'), [`now=${now}`, 'os=Fedora Linux 42', 'kernel=6.14', 'cpus=2', 'model=x', 'uptime=1000', 'stat=cpu  1 0 0 9 0 0 0 0', 'load=0 0 0', 'mem.MemTotal=1000', 'mem.MemAvailable=500',
    'rt=podman', 'ctr=pod-web|running|docker.io/library/nginx:latest|Up 2 hours|0.0.0.0:8081->80/tcp', 'cst=pod-web|1.50%|20.5MB / 2GB|-- / --', 'end=1', ''].join('\n'))
  await x.c.monitor.setMonitoring({ assets: [id], enabled: true })
  const row = await waitFor('the podman container', async () => (await x.c.monitor.listContainers({})).containers.find((r) => r.asset === id))
  eq([row.container?.name, row.container?.runtime, row.container?.image, row.container?.mem], ['pod-web', 'podman', 'docker.io/library/nginx:latest', 20_500_000n], 'seen as a podman container')
  eq((await x.c.containers.listFiles({ asset: id, name: 'pod-web', path: '/srv/www' })).entries.map((e) => e.name), ['index.html'], 'files through podman exec')
  const link = await x.c.containers.downloadLink({ asset: id, name: 'pod-web', path: '/srv/www/index.html' })
  eq(await (await fetch(`${x.node.url}${link.url}`)).text(), 'from podman\n', 'download through podman')
  eq((await x.c.monitor.containerLogs({ asset: id, name: 'pod-web', tail: 10 })).text.trim(), 'podman logs --tail 10 --timestamps pod-web', 'logs through podman')
  eq((await x.c.monitor.containerAction({ asset: id, name: 'pod-web', action: 'restart' })).output, 'podman restart pod-web', 'restart through podman')
  const images = (await x.c.containers.listImages({ asset: id })).images
  eq(images.map((i) => [i.repository, i.runtime, i.size, i.usedBy]), [['docker.io/library/nginx', 'podman', 192_000_000n, ['pod-web']]], 'podman images ("192 MB" with a space)')
  const vols = (await x.c.containers.listVolumes({ asset: id })).volumes
  eq(vols.map((v) => [v.name, v.runtime, v.usedBy]), [['podvol', 'podman', ['pod-web']]], 'podman volumes')
  eq((await x.c.containers.removeImage({ asset: id, image: 'docker.io/library/nginx:latest', runtime: 'podman' })).output, 'Untagged: docker.io/library/nginx:latest', 'remove through podman')
  await t.stop()
})
