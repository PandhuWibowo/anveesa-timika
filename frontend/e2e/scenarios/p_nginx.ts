// Nginx: found by monitoring (on the server, or in a container), sites and
// files read from `nginx -T`, a safe apply (write → nginx -t → reload, undone
// when rejected), history, switches, certificates, logs. The SSH target runs
// the real scripts with `sh`; `nginx`, `sudo`, `docker` are stand-ins.
import { chmodSync, existsSync, lstatSync, mkdirSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { uniq } from '../fixtures'
import { freshVault, web, createUser, startSsh, ok, eq, fails, waitFor, auditEntries, WORK, Code } from '../lib'

// Accepts a configuration unless a loaded file contains the word BROKEN.
const NGINX = `#!/bin/sh
R=$NGX_ROOT; D=$R/etc/nginx; C=$D/nginx.conf
files() { echo "$C"; for f in "$D"/conf.d/*.conf "$D"/sites-enabled/*; do [ -e "$f" ] && echo "$f"; done; }
t() {
  for f in $(files); do
    n=$(grep -n BROKEN "$f" | head -n 1 | cut -d: -f1)
    if [ -n "$n" ]; then echo "nginx: [emerg] unknown directive \\"BROKEN\\" in $f:$n" >&2; echo "nginx: configuration file $C test failed" >&2; return 1; fi
  done
  echo "nginx: the configuration file $C syntax is ok" >&2; echo "nginx: configuration file $C test is successful" >&2
}
case "$1" in
  -v) echo "nginx version: nginx/1.27.2" >&2 ;;
  -V) echo "nginx version: nginx/1.27.2" >&2; echo "configure arguments: --prefix=$R/usr --conf-path=$C --pid-path=$R/run/nginx.pid" >&2 ;;
  -t) t ;;
  -T) t || exit 1; for f in $(files); do echo "# configuration file $f:"; cat "$f"; done ;;
  -s) if [ -f "$R/stopped" ]; then echo 'nginx: [error] invalid PID number "" in "/run/nginx.pid"' >&2; exit 1; fi; echo reload >> "$R/reloads" ;;
esac
`

const ngx = fixture(async () => {
  const v = await freshVault({ MONITOR_INTERVAL_SECS: '1' })
  const admin = await createUser(v, 'ngx-admin', ['admin'])
  const c = web(v.node, admin)
  const root = join(WORK, uniq('ngx'))
  const bin = join(root, 'bin')
  const D = join(root, 'etc/nginx')
  for (const d of [bin, join(D, 'conf.d'), join(D, 'sites-available'), join(D, 'sites-enabled'), join(D, 'certs'), join(root, 'usr/logs')]) mkdirSync(d, { recursive: true })
  const put = (name: string, body: string) => { writeFileSync(join(bin, name), body); chmodSync(join(bin, name), 0o755) }
  put('nginx', NGINX)
  put('sudo', '#!/bin/sh\n[ "$1" = -n ] && shift\nexec "$@"\n')
  put('pgrep', '#!/bin/sh\n[ ! -f "$NGX_ROOT/stopped" ]\n')
  // `docker exec [-i] <name> <command…>` runs the command here, and says it was asked.
  put('docker', '#!/bin/sh\n[ "$1" = exec ] || exit 1; shift; [ "$1" = -i ] && shift; echo "$1" >> "$NGX_ROOT/docker-calls"; shift; exec "$@"\n')
  writeFileSync(join(D, 'nginx.conf'), `user nginx;\nerror_log logs/error.log;\nevents { worker_connections 1024; }\nhttp {\n    upstream app { server 10.0.0.5:3000; }\n    include ${D}/conf.d/*.conf;\n    include ${D}/sites-enabled/*;\n}\n`)
  writeFileSync(join(D, 'conf.d/api.conf'), `server {\n    listen 443 ssl;\n    server_name api.example.com;\n    ssl_certificate certs/api.pem;\n    access_log logs/api.log;\n    location / { proxy_pass http://app; }\n}\n`)
  writeFileSync(join(D, 'sites-available/shop'), `server {\n    listen 80;\n    server_name shop.example.com www.shop.example.com;\n    root /srv/shop;\n}\n`)
  symlinkSync('../sites-available/shop', join(D, 'sites-enabled/shop'))
  writeFileSync(join(D, 'sites-available/old'), `server { listen 80; server_name old.example.com; return 301 https://shop.example.com$request_uri; }\n`)
  writeFileSync(join(root, 'usr/logs/api.log'), Array.from({ length: 30 }, (_, i) => `10.0.0.${i} - GET /v1/${i} 200`).join('\n') + '\n')
  Bun.spawnSync(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '10', '-subj', '/CN=api.example.com', '-keyout', join(D, 'certs/api.key'), '-out', join(D, 'certs/api.pem')], { stderr: 'ignore' })

  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('ngx-host'), env: { PATH: `${bin}:${process.env.PATH}`, NGX_ROOT: root } })
  const a = await c.bastion.createAsset({ name: uniq('edge'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  const id = a.asset!.id
  const now = Math.floor(Date.now() / 1000)
  writeFileSync(join(t.files, '.timika-metrics'), [`now=${now}`, 'os=Debian 12', 'kernel=6.1', 'cpus=2', 'model=x', 'uptime=1000', 'stat=cpu  1 0 0 9 0 0 0 0', 'load=0 0 0', 'mem.MemTotal=1000', 'mem.MemAvailable=500',
    'rt=docker', 'ctr=web|running|nginx:1.27-alpine|Up 2 hours|', 'ctr=db|running|postgres:16|Up 2 hours|', 'ctr=old-proxy|exited|openresty/openresty:1.25|Exited (0) 3 days ago|', 'ngx=1.27.2|1', 'end=1', ''].join('\n'))
  await c.monitor.setMonitoring({ assets: [id], enabled: true })
  await waitFor('first reading', async () => (await c.monitor.listSystems({})).systems.find((s) => s.asset === id)?.status === 'up')
  const reloads = () => (existsSync(join(root, 'reloads')) ? readFileSync(join(root, 'reloads'), 'utf8').split('\n').filter(Boolean).length : 0)
  return { v, node: v.node, c, t, id, name: a.asset!.name, root, D, reloads, host: { asset: id, container: '' } }
})

scenario('P', 'nginx is found by itself: on the server and in containers whose image is nginx; admins only', async () => {
  const x = await ngx()
  const r = await x.c.nginx.listInstances({})
  eq(r.instances.filter((i) => i.asset === x.id).map((i) => [i.system, i.container, i.runtime, i.version, i.running]), [[x.name, '', '', '1.27.2', true], [x.name, 'web', 'docker', '', true], [x.name, 'old-proxy', 'docker', '', false]], 'the server’s own, and the nginx containers (not postgres)')
  eq(r.monitored, 1, 'monitored servers')
  const name = uniq('ngx-user')
  const u = web(x.node, await createUser(x.v, name, ['ssh']))
  await x.c.bastion.createGrant({ asset: x.id, subjectType: 'user', subject: name })
  await fails(u.nginx.listInstances({}), Code.PermissionDenied)
  await fails(u.nginx.getInstance(x.host), Code.PermissionDenied)
  await fails(u.nginx.readFile({ ...x.host, path: `${x.D}/nginx.conf` }), Code.PermissionDenied)
  await fails(u.nginx.saveFile({ ...x.host, path: `${x.D}/conf.d/x.conf`, content: 'server {}' }), Code.PermissionDenied)
  await fails(u.nginx.reload(x.host), Code.PermissionDenied)
  await fails(u.nginx.readLog({ ...x.host, path: `${x.root}/usr/logs/api.log` }), Code.PermissionDenied)
  // A server that is not monitored has no nginx page.
  const other = await x.c.bastion.createAsset({ name: uniq('plain'), host: '127.0.0.1', port: x.t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  await fails(x.c.nginx.getInstance({ asset: other.asset!.id, container: '' }), Code.NotFound, 'not monitored')
})

scenario('P', 'one nginx at a glance: sites (proxy, static, redirect; on and off), files, where new sites go, certificates with days left, logs', async () => {
  const x = await ngx()
  const d = await x.c.nginx.getInstance(x.host)
  eq([d.system, d.version, d.running, d.testOk, d.confPath, d.sitesDir], [x.name, '1.27.2', true, true, `${x.D}/nginx.conf`, `${x.D}/sites-available`], 'state')
  eq(d.sites.map((s) => [s.names.join(' '), s.listens.join(), s.tls, s.kind, s.targets.join(), s.file.replace(x.D, ''), s.line, s.enabled]), [
    ['api.example.com', '443 ssl', true, 'proxy', 'http://app', '/conf.d/api.conf', 1, true],
    ['shop.example.com www.shop.example.com', '80', false, 'static', '/srv/shop', '/sites-available/shop', 1, true],
    ['old.example.com', '80', false, 'redirect', 'https://shop.example.com$request_uri', '/sites-available/old', 1, false],
  ], 'sites: a linked site is edited where the file is; a site that is off is still listed')
  const f = (p: string) => { const y = d.files.find((z) => z.path === x.D + p)!; return [y.loaded, y.switchable, y.enabled] }
  eq([f('/nginx.conf'), f('/conf.d/api.conf'), f('/sites-available/shop'), f('/sites-available/old')], [[true, false, true], [true, true, true], [true, true, true], [false, true, false]], 'files')
  ok(!d.files.some((y) => y.path.includes('sites-enabled')), 'links are not listed as files')
  ok(!d.files.some((y) => y.path.includes('/certs/')), 'certificates and keys are not config files')
  await fails(x.c.nginx.readFile({ ...x.host, path: `${x.D}/certs/api.key` }), Code.InvalidArgument, 'keys are not opened')
  writeFileSync(`${x.D}/conf.d/leak.txt`, readFileSync(`${x.D}/certs/api.key`, 'utf8'))
  await fails(x.c.nginx.readFile({ ...x.host, path: `${x.D}/conf.d/leak.txt` }), Code.InvalidArgument, 'private key')
  const cert = d.certificates[0]
  eq([d.certificates.length, cert.path, cert.subject, cert.usedBy], [1, `${x.D}/certs/api.pem`, 'api.example.com', ['api.example.com']], 'the certificate, resolved against the config folder')
  ok(cert.daysLeft === 9 || cert.daysLeft === 10, `days left: ${cert.daysLeft} ${cert.error}`)
  eq(d.logs, [`${x.root}/usr/logs/error.log`, `${x.root}/usr/logs/api.log`], 'log files, resolved against nginx’s prefix')
  const log = await x.c.nginx.readLog({ ...x.host, path: `${x.root}/usr/logs/api.log`, tail: 5 })
  eq(log.text.trim().split('\n'), [25, 26, 27, 28, 29].map((i) => `10.0.0.${i} - GET /v1/${i} 200`), 'the last lines')
  await fails(x.c.nginx.readLog({ ...x.host, path: '/etc/passwd' }), Code.InvalidArgument, 'not one of nginx')
  await fails(x.c.nginx.readLog({ ...x.host, path: `${x.root}/usr/logs/none.log` }), Code.NotFound)
})

scenario('P', 'safe apply: a good change is tested, reloaded and kept in the history; a rejected one is undone on the server and changes nothing', async () => {
  const x = await ngx()
  const path = `${x.D}/conf.d/api.conf`
  const file = await x.c.nginx.readFile({ ...x.host, path })
  ok(file.content.includes('proxy_pass http://app;') && file.rev.length === 16 && file.versions === 0, 'the file, its revision, no history yet')
  const before = x.reloads()
  const good = file.content.replace('http://app', 'http://app/v2')
  const r = await x.c.nginx.saveFile({ ...x.host, path, content: good.replace(/\n/g, '\r\n'), rev: file.rev, note: 'v2' })
  eq([r.ok, r.reloaded, r.path, r.rev !== file.rev], [true, true, path, true], 'applied')
  ok(r.testOutput.includes('test is successful'), r.testOutput)
  eq([readFileSync(path, 'utf8'), x.reloads()], [good, before + 1], 'on disk with unix line ends, reloaded once')
  const hist = (await x.c.nginx.listVersions({ ...x.host, path })).versions
  eq(hist.map((h) => [h.by, h.note]), [['ngx-admin', 'v2'], ['', 'as found on the server']], 'history, newest first: mine, and what was there')

  const bad = await x.c.nginx.saveFile({ ...x.host, path, content: good.replace('listen 443 ssl;', 'listen 443 ssl;\n    BROKEN;'), rev: r.rev })
  eq([bad.ok, bad.reloaded], [false, false], 'rejected')
  ok(bad.testOutput.includes(`unknown directive "BROKEN" in ${path}:3`), `nginx’s words, with the line: ${bad.testOutput}`)
  eq([readFileSync(path, 'utf8'), x.reloads(), (await x.c.nginx.listVersions({ ...x.host, path })).versions.length], [good, before + 1, 2], 'the file is as before, no reload, no new version')
  ok((await x.c.nginx.getInstance(x.host)).testOk, 'the running configuration is still fine')

  // Someone changed the file on the server meanwhile.
  writeFileSync(path, good + '# edited over ssh\n')
  await fails(x.c.nginx.saveFile({ ...x.host, path, content: good, rev: r.rev }), Code.Aborted, 'changed on the server')
  // Bring the first version back.
  const cur = await x.c.nginx.readFile({ ...x.host, path })
  const first = await x.c.nginx.getVersion({ ...x.host, path, id: hist[1].id })
  eq(first.content, file.content, 'the earlier version')
  ok((await x.c.nginx.saveFile({ ...x.host, path, content: first.content, rev: cur.rev })).ok, 'restored')
  eq(readFileSync(path, 'utf8'), file.content, 'back to the start')
  eq((await x.c.nginx.listVersions({ ...x.host, path })).versions.map((h) => h.note), ['', 'as found on the server', 'v2', 'as found on the server'], 'the outside edit is in the history too')
  await fails(x.c.nginx.saveFile({ ...x.host, path, content: 'x'.repeat(300 * 1024), rev: cur.rev }), Code.InvalidArgument, 'larger than')
})

scenario('P', 'a new site, switching sites on and off (sites-enabled link, or .disabled), and deleting — each tested, each undone when rejected', async () => {
  const x = await ngx()
  const site = `${x.D}/sites-available/blog`
  const link = `${x.D}/sites-enabled/blog`
  const conf = 'server {\n    listen 80;\n    server_name blog.example.com;\n    root /srv/blog;\n}\n'
  const broken = await x.c.nginx.saveFile({ ...x.host, path: site, content: conf + 'BROKEN;\n', enable: true })
  eq([broken.ok, existsSync(site), existsSync(link)], [false, false, false], 'a rejected new site leaves nothing behind')
  const made = await x.c.nginx.saveFile({ ...x.host, path: site, content: conf, enable: true })
  eq([made.ok, made.reloaded, readFileSync(site, 'utf8'), lstatSync(link).isSymbolicLink()], [true, true, conf, true], 'created and switched on')
  await fails(x.c.nginx.saveFile({ ...x.host, path: site, content: conf }), Code.InvalidArgument, 'already exists')
  const blog = async () => (await x.c.nginx.getInstance(x.host)).sites.find((s) => s.names[0] === 'blog.example.com')
  eq([(await blog())?.enabled, (await blog())?.file], [true, site], 'listed')

  const off = await x.c.nginx.setEnabled({ ...x.host, path: site, enabled: false })
  eq([off.ok, off.path, existsSync(link), existsSync(site), (await blog())?.enabled], [true, site, false, true, false], 'off: the link is gone, the file stays')
  ok((await x.c.nginx.setEnabled({ ...x.host, path: site, enabled: true })).ok && lstatSync(link).isSymbolicLink(), 'on again')

  // conf.d: a file is switched off by its name.
  const api = `${x.D}/conf.d/api.conf`
  const renamed = await x.c.nginx.setEnabled({ ...x.host, path: api, enabled: false })
  eq([renamed.ok, renamed.path, existsSync(api), existsSync(api + '.disabled')], [true, api + '.disabled', false, true], 'renamed to .disabled')
  const d = await x.c.nginx.getInstance(x.host)
  eq([d.sites.find((s) => s.names[0] === 'api.example.com')?.enabled, d.files.find((f) => f.path === api + '.disabled')?.switchable], [false, true], 'still listed, as off')
  await fails(x.c.nginx.setEnabled({ ...x.host, path: api + '.disabled', enabled: false }), Code.InvalidArgument, 'already off')
  // Switching on something nginx rejects is undone.
  writeFileSync(api + '.disabled', readFileSync(api + '.disabled', 'utf8') + 'BROKEN;\n')
  const no = await x.c.nginx.setEnabled({ ...x.host, path: api + '.disabled', enabled: true })
  eq([no.ok, no.path, existsSync(api), existsSync(api + '.disabled')], [false, api + '.disabled', false, true], 'rejected → still off')
  writeFileSync(api + '.disabled', readFileSync(api + '.disabled', 'utf8').replace('BROKEN;\n', ''))
  ok((await x.c.nginx.setEnabled({ ...x.host, path: api + '.disabled', enabled: true })).ok && existsSync(api), 'on again')

  const gone = await x.c.nginx.deleteFile({ ...x.host, path: site })
  eq([gone.ok, existsSync(site), existsSync(link)], [true, false, false], 'deleted, with its link')
  const kept = (await x.c.nginx.listVersions({ ...x.host, path: site })).versions
  eq([kept.length, (await x.c.nginx.getVersion({ ...x.host, path: site, id: kept[0].id })).content], [1, conf], 'a deleted file can be brought back from its history')
  await fails(x.c.nginx.deleteFile({ ...x.host, path: `${x.D}/nginx.conf` }), Code.InvalidArgument, 'main configuration')
  await fails(x.c.nginx.setEnabled({ ...x.host, path: `${x.D}/nginx.conf`, enabled: false }), Code.InvalidArgument, 'main configuration')
  await fails(x.c.nginx.deleteFile({ ...x.host, path: site }), Code.NotFound)
})

scenario('P', 'only nginx’s own folder, quoted names, nginx in a container goes through the runtime, a stopped nginx is said, everything audited', async () => {
  const x = await ngx()
  for (const path of ['/etc/passwd', `${x.root}/usr/logs/api.log`, `${x.D}/../../secret`, 'nginx.conf', `${x.D}/conf.d/`, '']) {
    await fails(x.c.nginx.readFile({ ...x.host, path }), Code.InvalidArgument)
    await fails(x.c.nginx.saveFile({ ...x.host, path, content: 'server {}' }), Code.InvalidArgument)
  }
  await fails(x.c.nginx.deleteFile({ ...x.host, path: '/etc/passwd' }), Code.InvalidArgument, 'outside')
  const odd = `${x.D}/conf.d/it's; touch pwned $(id).conf`
  ok((await x.c.nginx.saveFile({ ...x.host, path: odd, content: '# odd name\n' })).ok && readFileSync(odd, 'utf8') === '# odd name\n', 'a hostile file name is just a name')
  ok(!existsSync(join(x.t.files, 'pwned')) && !existsSync(join(x.D, 'pwned')), 'nothing ran')
  ok((await x.c.nginx.deleteFile({ ...x.host, path: odd })).ok, 'and can be deleted')

  // The same nginx, addressed as the container `web`.
  const inC = { asset: x.id, container: 'web' }
  const d = await x.c.nginx.getInstance(inC)
  eq([d.version, d.runtime, d.sites.length >= 3, d.certificates[0]?.subject], ['1.27.2', 'docker', true, 'api.example.com'], 'read through `docker exec`')
  const p = `${x.D}/conf.d/in-container.conf`
  ok((await x.c.nginx.saveFile({ ...inC, path: p, content: '# from the container\n' })).ok && existsSync(p), 'written inside it')
  ok(readFileSync(join(x.root, 'docker-calls'), 'utf8').split('\n').filter((l) => l === 'web').length >= 3, 'every command went to the container')
  eq((await x.c.nginx.listVersions({ ...inC, path: p })).versions.length, 1, 'its own history')
  eq((await x.c.nginx.listVersions({ ...x.host, path: p })).versions.length, 0, 'not mixed with the server’s')
  await x.c.nginx.deleteFile({ ...inC, path: p })
  await fails(x.c.nginx.getInstance({ asset: x.id, container: 'nope' }), Code.NotFound)
  await fails(x.c.nginx.getInstance({ asset: x.id, container: 'web; id' }), Code.InvalidArgument)

  // The map knows what nginx forwards to, from its configuration, without a connection ever being seen.
  const m = await waitFor('configured routes on the map', async () => { const r = await x.c.monitor.getMap({}); return r.edges.some((e) => e.declared && e.from === `p:${x.id}:nginx`) && r })
  const route = m.edges.find((e) => e.declared && e.from === `p:${x.id}:nginx`)!
  eq([route.to, route.port, route.observed, route.sites, route.tls], ['x:10.0.0.5', 3000, false, ['api.example.com'], true], 'site → the upstream’s server')

  // nginx stopped: the change is saved and tested, the reload says why it did not happen.
  writeFileSync(join(x.root, 'stopped'), '')
  const s = await x.c.nginx.reload(x.host)
  eq([s.ok, s.reloaded], [true, false], 'tested, not reloaded')
  ok(s.reloadOutput.includes('invalid PID'), s.reloadOutput)
  eq((await x.c.nginx.getInstance(x.host)).running, false, 'not running')
  Bun.spawnSync(['rm', join(x.root, 'stopped')])

  const raw = JSON.stringify(auditEntries(x.node))
  ok(raw.includes(`${x.D}/conf.d/in-container.conf of nginx in web on ${x.id}`) && raw.includes(`reload nginx on ${x.id}`), 'who changed what, where')
  ok(!raw.includes('# from the container'), 'no file content in the audit log')
})
