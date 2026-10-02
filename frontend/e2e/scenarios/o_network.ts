// Network tools: checks run from a server over SSH. The SSH target runs the
// real script with `sh`; ping, dig, curl … are stand-ins on its PATH.
import { chmodSync, existsSync, mkdirSync, symlinkSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { uniq } from '../fixtures'
import { freshVault, web, createUser, startSsh, ok, eq, fails, auditEntries, WORK, Code } from '../lib'

const openssl = (d: Date) => d.toUTCString().replace(/^\w+, (\d+) (\w+) (\d+) (.*) GMT$/, '$2 $1 $4 $3 GMT')

/** Stand-ins for the programs a Linux server would have. */
function tools() {
  const bin = join(WORK, uniq('nettools'))
  mkdirSync(bin, { recursive: true })
  const put = (name: string, body: string) => { writeFileSync(join(bin, name), `#!/bin/sh\n${body}\n`); chmodSync(join(bin, name), 0o755) }
  put('ping', `case "$5" in
  down.internal) echo "4 packets transmitted, 0 received, 100% packet loss, time 3000ms"; exit 1;;
  nowhere.internal) echo "ping: nowhere.internal: Name or service not known"; exit 2;;
esac
echo "PING $5 (10.0.0.5) 56(84) bytes of data."; echo "args: $*"
echo "4 packets transmitted, 4 received, 0% packet loss, time 3004ms"; echo "rtt min/avg/max/mdev = 0.310/0.402/0.520/0.080 ms"`)
  // "timeout 3 bash -c '…' host port" (port check) · "timeout 10 openssl …" (certificate)
  put('timeout', `if [ "$2" = openssl ]; then shift; exec "$@"; fi
case "$6" in 22|443) exit 0;; 81) echo "bash: connect: Connection refused" >&2; exit 1;; *) exit 124;; esac`)
  put('dig', `echo "; args: $*"
case "$*" in *nx.example*) echo ";; ->>HEADER<<- opcode: QUERY, status: NXDOMAIN, id: 7"; exit 0;; esac
echo ";; ->>HEADER<<- opcode: QUERY, status: NOERROR, id: 7"
printf 'example.com.\\t300\\tIN\\tMX\\t10 mx1.example.com.\\nexample.com.\\t300\\tIN\\tMX\\t20 mx2.example.com.\\n'
echo ";; Query time: 12 msec"; echo ";; SERVER: 10.0.0.2#53(10.0.0.2) (UDP)"`)
  put('traceroute', `echo "traceroute to $8 (10.0.2.5), 20 hops max"; echo " 1  10.0.0.1  0.412 ms"; echo " 2  *"; echo " 3  10.0.2.5  1.200 ms"`)
  put('curl', `for a; do u=$a; done
case "$u" in *down*) echo "curl: (7) Failed to connect to down.internal port 443: Connection refused"; printf 'code=000\\ntotal=0.001\\n'; exit 7;; esac
printf 'code=200\\nip=10.0.0.5\\ndns=0.010\\nconnect=0.030\\ntls=0.090\\nfirst=0.150\\ntotal=0.200\\nredirects=1\\nurl=%s\\nsize=512\\n' "$u"`)
  put('openssl', `if [ "$1" = s_client ]; then echo "connect: args: $*"; echo "New, TLSv1.3, Cipher is TLS_AES_256_GCM_SHA384"; echo "    Verify return code: 0 (ok)"; exit 0; fi
cat >/dev/null
echo "subject=CN = example.com"; echo "issuer=C = US, O = Let's Encrypt, CN = R11"
echo "notBefore=${openssl(new Date(Date.now() - 30 * 864e5))}"; echo "notAfter=${openssl(new Date(Date.now() + 10 * 864e5))}"
echo "X509v3 Subject Alternative Name: "; echo "    DNS:example.com, DNS:www.example.com"`)
  put('ss', `echo "Netid State Recv-Q Send-Q Local Address:Port Peer Address:Port Process"
echo 'tcp   LISTEN 0  511  0.0.0.0:80      0.0.0.0:*  users:(("nginx",pid=812,fd=6))'
echo 'tcp   LISTEN 0  128  127.0.0.1:5432  0.0.0.0:*'`)
  return bin
}

const net = fixture(async () => {
  const v = await freshVault()
  const admin = await createUser(v, 'net-admin', ['admin'])
  const c = web(v.node, admin)
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('net-host'), env: { PATH: `${tools()}:/bin:/usr/bin` } })
  const a = await c.bastion.createAsset({ name: uniq('edge'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }, { username: 'root', password: 'ops-pass' }], test: false })
  return { v, node: v.node, c, t, id: a.asset!.id, name: a.asset!.name }
})

const facts = (r: { facts: { label: string; value: string }[] }) => Object.fromEntries(r.facts.map((f) => [f.label, f.value]))

scenario('O', 'ping from a server: replies, loss and latency; a host that is down or unknown is said plainly', async () => {
  const x = await net()
  const r = await x.c.net.run({ tool: 'ping', asset: x.id, target: 'db.internal' })
  eq([r.ok, r.level, r.verdict, r.tool, r.source, r.account], [true, 'ok', 'db.internal answers, 0.4 ms on average', 'ping', x.name, 'ops'], 'verdict, from the first account')
  eq(facts(r), { Replies: '4 of 4', 'Packet loss': '0%', Fastest: '0.3 ms', Average: '0.4 ms', Slowest: '0.5 ms' }, 'numbers')
  ok(r.output.includes('args: -c 4 -W 2 db.internal') && !r.output.includes('#tool'), `bounded, and the output as it came: ${r.output}`)
  const down = await x.c.net.run({ tool: 'ping', asset: x.id, target: 'down.internal' })
  eq([down.ok, down.level, down.verdict], [false, 'bad', 'down.internal did not answer'], 'no reply')
  eq((await x.c.net.run({ tool: 'ping', asset: x.id, target: 'nowhere.internal' })).verdict, 'ping: nowhere.internal: Name or service not known', 'the tool’s own words')
})

scenario('O', 'port check: open, refused and no answer are told apart, several ports at once', async () => {
  const x = await net()
  const r = await x.c.net.run({ tool: 'port', asset: x.id, target: 'db.internal', ports: [22, 81, 5432] })
  eq(r.ports.map((p) => [p.port, p.state]), [[22, 'open'], [81, 'closed'], [5432, 'timeout']], 'states')
  eq([r.ok, r.level, r.verdict, r.tool], [true, 'warn', '1 of 3 ports open on db.internal', 'bash'], 'summary')
  ok(r.ports[1].detail.includes('Connection refused'), 'why')
  const one = await x.c.net.run({ tool: 'port', asset: x.id, target: 'db.internal', ports: [5432] })
  eq([one.ok, one.verdict], [false, 'db.internal:5432 did not answer — a firewall drops it, or the host is down'], 'one port, explained')
  await fails(x.c.net.run({ tool: 'port', asset: x.id, target: 'db.internal' }), Code.InvalidArgument, '1 to 20 ports')
  await fails(x.c.net.run({ tool: 'port', asset: x.id, target: 'db.internal', ports: [70000] }), Code.InvalidArgument, 'not a port')
})

scenario('O', 'DNS lookup: records with TTL, the resolver asked, and a name that does not exist', async () => {
  const x = await net()
  const r = await x.c.net.run({ tool: 'dns', asset: x.id, target: 'example.com', record: 'mx', resolver: '10.0.0.2' })
  eq(r.records.map((d) => [d.name, d.ttl, d.type, d.value]), [['example.com', 300, 'MX', '10 mx1.example.com.'], ['example.com', 300, 'MX', '20 mx2.example.com.']], 'records')
  eq([r.ok, r.verdict, r.tool], [true, 'example.com → 10 mx1.example.com. and 1 more', 'dig'], 'verdict')
  eq(facts(r), { 'Answered in': '12 ms', Resolver: '10.0.0.2#53' }, 'who answered')
  ok(r.output.includes('@10.0.0.2 example.com MX'), `the resolver and type reach dig: ${r.output}`)
  const nx = await x.c.net.run({ tool: 'dns', asset: x.id, target: 'nx.example' })
  eq([nx.ok, nx.verdict], [false, 'nx.example does not exist (NXDOMAIN)'], 'NXDOMAIN')
  await fails(x.c.net.run({ tool: 'dns', asset: x.id, target: 'example.com', record: 'ANY' }), Code.InvalidArgument, 'unknown record type')
})

scenario('O', 'trace route, HTTP timing and TLS certificate: hops, where the time goes, days until expiry', async () => {
  const x = await net()
  const tr = await x.c.net.run({ tool: 'trace', asset: x.id, target: 'db.internal' })
  eq(tr.hops.map((h) => [h.n, h.addr, h.ms]), [[1, '10.0.0.1', 0.412], [2, '', undefined], [3, '10.0.2.5', 1.2]], 'hops; a silent one stays in the list')
  eq([tr.ok, tr.verdict], [true, 'db.internal reached in 3 hops, 1.2 ms'], 'reached')

  const h = await x.c.net.run({ tool: 'http', asset: x.id, target: 'api.internal/health?token=s3cret' })
  eq([h.ok, h.verdict, h.tool], [true, 'HTTP 200 in 200 ms', 'curl'], 'https:// assumed')
  eq(facts(h), { Status: '200', 'Answered by': '10.0.0.5', DNS: '10 ms', Connect: '20 ms', TLS: '60 ms', 'First byte': '60 ms', Total: '200 ms', Redirects: '1 → https://api.internal/health' }, 'timing split')
  ok(!JSON.stringify(h).includes('s3cret'), 'the query string is not echoed back')
  const dead = await x.c.net.run({ tool: 'http', asset: x.id, target: 'https://down.internal/' })
  eq([dead.ok, dead.verdict], [false, 'Failed to connect to down.internal port 443: Connection refused'], 'curl’s reason')
  await fails(x.c.net.run({ tool: 'http', asset: x.id, target: 'file:///etc/passwd' }), Code.InvalidArgument, 'only http')
  await fails(x.c.net.run({ tool: 'http', asset: x.id, target: 'https://user:pw@x.internal/' }), Code.InvalidArgument)

  const t = await x.c.net.run({ tool: 'tls', asset: x.id, target: 'example.com' })
  const f = facts(t)
  eq([t.ok, t.level, f['Issued to'], f['Issued by'], f.Names, f.Protocol, f['Trusted by this server']], [true, 'warn', 'example.com', 'R11', 'example.com, www.example.com', 'TLSv1.3 · TLS_AES_256_GCM_SHA384', 'yes'], 'certificate facts')
  ok(/^valid, but expires in (9|10) days$/.test(t.verdict) && /· (9|10) days left$/.test(f['Valid until']), `expiry warning: ${t.verdict}`)
  ok(t.output.includes('args: s_client -connect example.com:443 -servername example.com'), `port 443 and SNI by default: ${t.output}`)
  const ip = await x.c.net.run({ tool: 'tls', asset: x.id, target: '10.0.0.5', ports: [8443] })
  ok(ip.output.includes('args: s_client -connect 10.0.0.5:8443') && !ip.output.includes('-servername'), 'no SNI for an address')
})

scenario('O', 'listening ports: what the server listens on, the process when visible; a missing program is named', async () => {
  const x = await net()
  const r = await x.c.net.run({ tool: 'listen', asset: x.id })
  eq(r.listeners.map((l) => [l.proto, l.addr, l.port, l.process, l.pid]), [['tcp', '0.0.0.0', 80, 'nginx', 812], ['tcp', '127.0.0.1', 5432, '', undefined]], 'listeners, by port')
  eq([r.ok, r.verdict, r.tool], [true, '2 listening ports, 1 reachable from other machines', 'ss'], 'summary')
  // A server with nothing installed but a shell.
  const bare = join(WORK, uniq('bare'))
  mkdirSync(bare, { recursive: true })
  symlinkSync(existsSync('/bin/sh') ? '/bin/sh' : '/usr/bin/sh', join(bare, 'sh'))
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('bare-host'), env: { PATH: bare } })
  const a = await x.c.bastion.createAsset({ name: uniq('bare'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  for (const [tool, what] of [['ping', 'ping'], ['dns', 'dig, host, nslookup or getent'], ['trace', 'traceroute, tracepath or mtr'], ['http', 'curl or wget'], ['tls', 'openssl'], ['listen', 'ss or netstat']]) {
    const m = await x.c.net.run({ tool, asset: a.asset!.id, target: 'example.com' })
    eq([m.ok, m.verdict], [false, `${what} is not installed on this server`], tool)
  }
  await t.stop()
})

scenario('O', 'no free-form commands, only on servers you may use and as accounts you were given; every run is audited without secrets', async () => {
  const x = await net()
  for (const target of ['x; touch pwned', '$(id)', '`id`', '-c 1000 x', "x' y", 'a|b', '']) {
    await fails(x.c.net.run({ tool: 'ping', asset: x.id, target }), Code.InvalidArgument, 'hostname or an IP')
  }
  await fails(x.c.net.run({ tool: 'sh', asset: x.id, target: 'x' }), Code.InvalidArgument, 'unknown tool')
  await fails(x.c.net.run({ tool: 'dns', asset: x.id, target: 'example.com', resolver: '-f /etc/passwd' }), Code.InvalidArgument, 'resolver')
  await fails(x.c.net.run({ tool: 'ping', asset: 'no-such-server', target: 'x' }), Code.NotFound)
  ok(!existsSync(join(x.t.files, 'pwned')), 'nothing ran')

  const name = uniq('net-user')
  const u = web(x.node, await createUser(x.v, name, ['ssh']))
  await fails(u.net.run({ tool: 'ping', asset: x.id, target: 'db.internal' }), Code.PermissionDenied, 'access')
  await x.c.bastion.createGrant({ asset: x.id, subjectType: 'user', subject: name, accounts: ['ops'] })
  eq((await u.net.run({ tool: 'ping', asset: x.id, target: 'db.internal' })).account, 'ops', 'granted → runs as the granted account')
  await fails(u.net.run({ tool: 'ping', asset: x.id, target: 'db.internal', account: 'root' }), Code.PermissionDenied)
  await fails(web(x.node, await createUser(x.v, uniq('net-reader'), ['read-only'])).net.run({ tool: 'ping', asset: x.id, target: 'db.internal' }), Code.PermissionDenied)

  await x.c.net.run({ tool: 'http', asset: x.id, target: 'https://api.internal/login?password=hunter2' })
  const raw = JSON.stringify(auditEntries(x.node))
  ok(raw.includes(`ping db.internal from ops@${x.id}`) && raw.includes(`http check https://api.internal/login from ops@${x.id}`), 'who checked what, from where')
  ok(!raw.includes('hunter2'), 'no query string in the audit log')
})
