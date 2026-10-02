// Monitoring: metrics over SSH (the SSH target answers the metrics script from
// `.timika-metrics`), rates between readings, history, alerts, access.
import { chmodSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { uniq } from '../fixtures'
import { freshVault, unseal, web, createUser, startSsh, ok, eq, fails, waitFor, sleep, auditEntries, WORK, Code, type SshTarget } from '../lib'

type M = { now: number; busy?: number; total?: number; memAvail?: number; rx?: number; tx?: number; rootUsed?: number; extra?: string }

/** What a Linux server would answer, with the counters we choose. */
function metrics(t: SshTarget, m: M) {
  const busy = m.busy ?? 1000
  const total = m.total ?? 10000
  const text = [
    `now=${m.now}`, 'os=Ubuntu 24.04.1 LTS', 'kernel=6.8.0-45-generic', 'cpus=4', 'model=AMD EPYC 7B13', `uptime=${m.now - 1_700_000_000}`,
    `stat=cpu  ${busy} 0 0 ${total - busy} 0 0 0 0 0 0`, 'load=0.50 0.40 0.30',
    'mem.MemTotal=8000000', `mem.MemAvailable=${m.memAvail ?? 6000000}`, 'mem.SwapTotal=0', 'mem.SwapFree=0',
    `net=${m.rx ?? 0} ${m.tx ?? 0}`, 'io=1000 2000', 'temp=48000',
    `disk=/dev/vda1|/|100000000|${m.rootUsed ?? 40000000}`, 'disk=tmpfs|/run|800000|100', 'disk=/dev/vdb|/data|200000000|20000000',
    m.extra ?? '', 'end=1', '',
  ].join('\n')
  writeFileSync(join(t.files, '.timika-metrics'), text)
}

const mon = fixture(async () => {
  const v = await freshVault({ MONITOR_INTERVAL_SECS: '1' })
  const admin = await createUser(v, 'mon-admin', ['admin'])
  return { v, node: v.node, admin, c: web(v.node, admin) }
})

/** A fresh SSH target registered as a server (each scenario has its own metrics file). */
async function server(tags: string[] = []) {
  const x = await mon()
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('mon-host') })
  const a = await x.c.bastion.createAsset({ name: uniq('box'), host: '127.0.0.1', port: t.port, tags, accounts: [{ username: 'ops', password: 'ops-pass' }], test: true })
  return { ...x, t, id: a.asset!.id, name: a.asset!.name }
}

const sys = async (c: Awaited<ReturnType<typeof mon>>['c'], id: string) => (await c.monitor.listSystems({})).systems.find((s) => s.asset === id)

scenario('M', 'monitoring a server reads its numbers over SSH; rates (CPU, network) come from two readings', async () => {
  const s = await server(['web'])
  const now = Math.floor(Date.now() / 1000)
  metrics(s.t, { now, busy: 1000, total: 10000, rx: 1_000_000, tx: 500_000 })
  ok((await s.c.monitor.listSystems({})).available.some((a) => a.asset === s.id), 'offered to admins')
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: true })
  const first = await waitFor('first reading', async () => { const r = await sys(s.c, s.id); return r?.status === 'up' && r.latest && r })
  const l = first.latest!
  eq([first.account, l.os, l.cpus, l.cpuModel], ['ops', 'Ubuntu 24.04.1 LTS', 4, 'AMD EPYC 7B13'], 'system info')
  eq([Math.round(l.mem!), Math.round(l.disk!), l.swap, l.load1, l.temp], [25, 40, undefined, 0.5, 48], 'memory, root disk, no swap, load, temperature')
  eq([l.memTotal, l.diskTotal], [8000000n * 1024n, 100000000n * 1024n], 'sizes in bytes')
  // One minute later on the server: 50% busy, 6 MB in, 3 MB out.
  metrics(s.t, { now: now + 60, busy: 1500, total: 11000, rx: 7_000_000, tx: 3_500_000 })
  const second = await waitFor('rates', async () => { const r = await sys(s.c, s.id); return r?.latest?.cpu !== undefined && r })
  eq([Math.round(second.latest!.cpu!), second.latest!.rx, second.latest!.tx], [50, 100000, 50000], 'cpu %, bytes per second')
  ok(second.spark.length >= 1, 'sparkline')
  ok(!(await s.c.monitor.listSystems({})).available.some((a) => a.asset === s.id), 'no longer offered')
})

scenario('M', 'history is kept and returned oldest first for a range; disks, containers and failed services are shown', async () => {
  const s = await server()
  const now = Math.floor(Date.now() / 1000)
  const extra = 'ctr=web|running\nctr=old-job|exited\ncst=web|12.50%|128MiB / 1.9GiB\nunit=nginx.service'
  metrics(s.t, { now, extra })
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: true })
  for (let i = 1; i <= 4; i++) { await sleep(1100); metrics(s.t, { now: now + i * 60, busy: 1000 + i * 100, total: 10000 + i * 1000, extra }) }
  const d = await waitFor('history', async () => { const r = await s.c.monitor.getSystem({ asset: s.id, range: '1h' }); return r.times.length >= 4 && r })
  ok(d.times.every((t, i) => i === 0 || t > d.times[i - 1]), 'oldest first')
  eq(d.series.map((x) => x.metric), ['cpu', 'mem', 'swap', 'disk', 'rx', 'tx', 'load', 'temp', 'io_read', 'io_write'], 'series')
  ok(d.series.every((x) => x.values.length === d.times.length), 'one value per time')
  ok(d.series.find((x) => x.metric === 'mem')!.values.every((v) => Math.round(v) === 25), 'memory series')
  ok(d.series.find((x) => x.metric === 'swap')!.values.every((v) => Number.isNaN(v)), 'no swap → no readings')
  eq(d.disks.map((k) => k.mount), ['/', '/data'], 'real disks only')
  eq(d.containers.map((c) => [c.name, c.state, c.cpu]), [['web', 'running', 12.5], ['old-job', 'exited', undefined]], 'containers')
  eq([d.failedUnitNames, d.system?.containersRunning, d.system?.failedUnits], [['nginx.service'], 1, 1], 'failed services')
  eq([d.step, d.defaultRules, d.rules.map((r) => r.metric)], [1, true, ['status', 'cpu', 'mem', 'disk']], 'step and default rules')
  eq((await s.c.monitor.getSystem({ asset: s.id, range: '24h' })).step, 600, '10-minute averages for a day')
  await fails(s.c.monitor.getSystem({ asset: s.id, range: '2h' }), Code.InvalidArgument, 'range')
})

scenario('M', 'a server that is not Linux, or stops answering, shows as down with the reason', async () => {
  const s = await server()
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: true })
  const bad = await waitFor('down', async () => { const r = await sys(s.c, s.id); return r?.status === 'down' && r })
  ok(bad.error?.includes('only Linux servers'), `error: ${bad.error}`)
  metrics(s.t, { now: Math.floor(Date.now() / 1000) })
  await waitFor('up', async () => (await sys(s.c, s.id))?.status === 'up')
  await s.t.stop()
  const down = await waitFor('down again', async () => { const r = await sys(s.c, s.id); return r?.status === 'down' && r }, 30000)
  ok(/connect|closed|did not/.test(down.error ?? ''), `error: ${down.error}`)
  await s.t.start()
  await waitFor('back up', async () => (await sys(s.c, s.id))?.status === 'up', 30000)
})

scenario('M', 'alerts: a server’s own rule fires once when the average crosses the threshold, and is announced', async () => {
  const s = await server()
  const got: any[] = []
  const srv = Bun.serve({ port: 0, async fetch(req) { got.push(await req.json().catch(() => null)); return new Response('ok') } })
  const saved = await s.c.monitor.saveSettings({ defaults: [{ metric: 'status', threshold: 0, minutes: 2 }], notifiers: [{ kind: 'webhook', url: `http://127.0.0.1:${srv.port}/secret-hook-path` }] })
  ok(!JSON.stringify(saved).includes('secret-hook-path') && saved.notifiers[0].label.startsWith('127.0.0.1:'), 'only a label comes back')
  eq((await s.c.monitor.testNotifier({ notifier: { id: saved.notifiers[0].id, kind: 'webhook' } })).ok, true, 'test with the stored target')
  ok(got.some((g) => g?.text?.includes('test notification')), 'test arrived')
  const now = Math.floor(Date.now() / 1000)
  metrics(s.t, { now, memAvail: 400000 }) // 95% used
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: true })
  await fails(s.c.monitor.setSystemAlerts({ asset: s.id, rules: [{ metric: 'mem', threshold: 150, minutes: 1 }] }), Code.InvalidArgument, '1–100 ％')
  await fails(s.c.monitor.setSystemAlerts({ asset: s.id, rules: [{ metric: 'gpu', threshold: 5, minutes: 1 }] }), Code.InvalidArgument, 'unknown')
  await s.c.monitor.setSystemAlerts({ asset: s.id, rules: [{ metric: 'mem', threshold: 80, minutes: 1 }] })
  const d = await s.c.monitor.getSystem({ asset: s.id })
  eq([d.defaultRules, d.rules.map((r) => [r.metric, r.threshold, r.minutes])], [false, [['mem', 80, 1]]], 'own rules')
  const fired = await waitFor('the alert', async () => { const r = await sys(s.c, s.id); return r?.firing.length ? r : undefined }, 80000)
  eq(fired.firing.map((f) => [f.metric, Math.round(f.value!)]), [['mem', 95]], 'firing')
  const msg = await waitFor('notification', async () => got.find((g) => g?.text?.startsWith('⚠')), 10000)
  ok(msg.text.includes(`${s.name}: Memory 95% — above 80% for 1 min`) && msg.event === 'monitor', `text: ${msg.text}`)
  await sleep(2500)
  eq(got.filter((g) => g?.text?.startsWith('⚠')).length, 1, 'announced once')
  await s.c.monitor.setSystemAlerts({ asset: s.id, useDefaults: true })
  eq((await s.c.monitor.getSystem({ asset: s.id })).defaultRules, true, 'back to the defaults')
  await waitFor('rule gone → not firing', async () => (await sys(s.c, s.id))?.firing.length === 0)
  srv.stop(true)
}, { timeout: 150000 })

scenario('M', 'people see only the servers they may use; only admins choose what is monitored and how', async () => {
  const s = await server()
  const other = await server()
  metrics(s.t, { now: Math.floor(Date.now() / 1000) })
  await s.c.monitor.setMonitoring({ assets: [s.id, other.id], enabled: true })
  const name = uniq('mon-user')
  const u = web(s.node, await createUser(s.v, name, ['ssh']))
  eq((await u.monitor.listSystems({})).systems.length, 0, 'no grant → nothing')
  await fails(u.monitor.getSystem({ asset: s.id }), Code.PermissionDenied)
  await s.c.bastion.createGrant({ asset: s.id, subjectType: 'user', subject: name })
  const seen = await u.monitor.listSystems({})
  eq([seen.systems.map((y) => y.asset), seen.available.length], [[s.id], 0], 'granted server only, nothing offered')
  await u.monitor.getSystem({ asset: s.id })
  await fails(u.monitor.getSystem({ asset: other.id }), Code.PermissionDenied)
  await fails(u.monitor.setMonitoring({ assets: [s.id], enabled: false }), Code.PermissionDenied)
  await fails(u.monitor.setSystemAlerts({ asset: s.id, useDefaults: true }), Code.PermissionDenied)
  await fails(u.monitor.getSettings({}), Code.PermissionDenied)
  await fails(u.monitor.saveSettings({ defaults: [] }), Code.PermissionDenied)
  await fails(s.c.monitor.setMonitoring({ assets: [s.id], enabled: true, account: 'nobody' }), Code.InvalidArgument, 'no account')
  await fails(s.c.monitor.getSystem({ asset: 'nope' }), Code.NotFound)
})

scenario('M', 'stopping monitoring deletes the history; deleting the server stops monitoring it', async () => {
  const s = await server()
  const gone = await server()
  const now = Math.floor(Date.now() / 1000)
  metrics(s.t, { now }); metrics(gone.t, { now })
  await s.c.monitor.setMonitoring({ assets: [s.id, gone.id], enabled: true })
  await waitFor('both up', async () => (await sys(s.c, s.id))?.status === 'up' && (await sys(s.c, gone.id))?.status === 'up')
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: false })
  await fails(s.c.monitor.getSystem({ asset: s.id }), Code.NotFound, 'not monitored')
  ok((await s.c.monitor.listSystems({})).available.some((a) => a.asset === s.id), 'offered again')
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: true })
  eq((await s.c.monitor.getSystem({ asset: s.id })).times.length <= 1, true, 'history started over')
  await s.c.bastion.deleteAsset({ id: gone.id })
  await sleep(2500)
  ok(!(await s.c.monitor.listSystems({})).systems.some((y) => y.asset === gone.id), 'gone with its server')
  const raw = JSON.stringify(auditEntries(s.node))
  ok(raw.includes('monitoring on for') && raw.includes('monitoring off for'), 'changes are audited')
  rmSync(join(s.t.files, '.timika-metrics'), { force: true })
})

scenario('M', 'restarting timika costs one reading, not the rates: the first reading afterwards continues from the stored counters', async () => {
  const v = await freshVault({ MONITOR_INTERVAL_SECS: '1' })
  const c = web(v.node, v.root)
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('mon-restart') })
  const a = await c.bastion.createAsset({ name: 'restart-box', host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: true })
  const id = a.asset!.id
  const now = Math.floor(Date.now() / 1000)
  metrics(t, { now, busy: 1000, total: 10000, rx: 0 })
  await c.monitor.setMonitoring({ assets: [id], enabled: true })
  await waitFor('first reading', async () => (await sys(c, id))?.status === 'up')
  await v.node.stop()
  // While timika is down the server keeps counting: one minute, 25% busy, 3 MB in.
  metrics(t, { now: now + 60, busy: 1250, total: 11000, rx: 3_000_000 })
  await v.node.start()
  await unseal(v)
  const after = await waitFor('reading after the restart', async () => { const r = await sys(c, id); return r?.latest && new Date(r.latest.time).getTime() > Date.now() - 3000 && r.status === 'up' && r }, 30000)
  // The file doesn't change any more: only the first reading can have a rate.
  const d = await waitFor('its sample', async () => { const x = await c.monitor.getSystem({ asset: id }); return x.series[0].values.some((val) => !Number.isNaN(val)) && x }, 15000)
  const cpu = d.series.find((x) => x.metric === 'cpu')!.values.filter((val) => !Number.isNaN(val))
  const rx = d.series.find((x) => x.metric === 'rx')!.values.filter((val) => !Number.isNaN(val))
  eq([cpu.map(Math.round), rx], [[25], [50000]], 'rates across the restart')
  ok(after.status === 'up', 'up')
  await t.stop()
  await v.node.stop()
})

scenario('M', 'containers: image, status, ports, health and network rates; history per container; logs and start / stop / restart for admins only', async () => {
  const x = await mon()
  // A stand-in `docker` on the server: says what it was asked.
  const bin = join(WORK, uniq('fakedocker'))
  mkdirSync(bin, { recursive: true })
  writeFileSync(join(bin, 'docker'), '#!/bin/sh\nif [ "$1" = restart ] && [ "$2" = db ]; then echo "Error response from daemon: cannot restart"; exit 1; fi\necho "docker $*"\n')
  chmodSync(join(bin, 'docker'), 0o755)
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('mon-docker'), env: { PATH: `${bin}:${process.env.PATH}` } })
  const a = await x.c.bastion.createAsset({ name: uniq('dock'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: true })
  const id = a.asset!.id
  const now = Math.floor(Date.now() / 1000)
  const ctr = (rx: number, cpu: string) => [
    'ctr=web|running|nginx:1.27|Up 3 hours (healthy)|0.0.0.0:8080->80/tcp, [::]:8080->80/tcp',
    'ctr=db|running|postgres:16|Up 2 days|5432/tcp',
    'ctr=migrate|exited|acme/app:1.8|Exited (0) 2 days ago|',
    `cst=web|${cpu}%|128MiB / 2GiB|${rx}kB / 100kB`, 'cst=db|3.00%|512MiB / 2GiB|0B / 0B',
  ].join('\n')
  metrics(t, { now, extra: ctr(1000, '10.00') })
  await x.c.monitor.setMonitoring({ assets: [id], enabled: true })
  await waitFor('first reading', async () => (await sys(x.c, id))?.containersTotal === 3)
  metrics(t, { now: now + 60, busy: 1100, total: 11000, extra: ctr(1600, '20.00') })
  const row = await waitFor('container rates', async () => (await x.c.monitor.listContainers({})).containers.find((r) => r.asset === id && r.container?.name === 'web' && r.container.rx !== undefined))
  const w = row.container!
  eq([row.system, w.image, w.status, w.health, w.state], [a.asset!.name, 'nginx:1.27', 'Up 3 hours (healthy)', 'healthy', 'running'], 'container facts')
  eq([w.ports, w.cpu, w.mem, w.memLimit, w.rx, w.tx], ['0.0.0.0:8080->80/tcp, [::]:8080->80/tcp', 20, 128n * 1024n * 1024n, 2n * 1024n * 1024n * 1024n, 10000, 0], 'ports, usage, network rate (600 kB in a minute)')
  const d = await x.c.monitor.getSystem({ asset: id })
  eq(d.containerSeries.map((s) => s.name), ['db', 'web'], 'history for running containers only')
  const web1 = d.containerSeries.find((s) => s.name === 'web')!
  ok(web1.cpu.length === d.times.length && web1.cpu.filter((v) => !Number.isNaN(v)).some((v) => v === 20), `cpu series aligned with the system's times: ${web1.cpu}`)
  ok(web1.mem.some((v) => v === 128 * 1024 * 1024) && web1.rx.some((v) => v === 10000), 'memory and network series')

  // Logs and actions.
  const logs = await x.c.monitor.containerLogs({ asset: id, name: 'web', tail: 50 })
  eq(logs.text.trim(), 'docker logs --tail 50 --timestamps web', 'docker logs, as asked')
  eq((await x.c.monitor.containerLogs({ asset: id, name: 'web', tail: 999999 })).text.trim(), 'docker logs --tail 2000 --timestamps web', 'tail is capped')
  const done = await x.c.monitor.containerAction({ asset: id, name: 'web', action: 'restart' })
  eq([done.ok, done.output], [true, 'docker restart web'], 'restart')
  const refused = await x.c.monitor.containerAction({ asset: id, name: 'db', action: 'restart' })
  eq([refused.ok, refused.output], [false, 'Error response from daemon: cannot restart'], 'docker’s refusal is shown')
  await fails(x.c.monitor.containerAction({ asset: id, name: 'web', action: 'rm' }), Code.InvalidArgument, 'started, stopped or restarted')
  await fails(x.c.monitor.containerAction({ asset: id, name: 'nope', action: 'stop' }), Code.NotFound)
  await fails(x.c.monitor.containerLogs({ asset: id, name: "web'; touch pwned; '" }), Code.NotFound)
  await fails(x.c.monitor.containerLogs({ asset: id, name: '-f' }), Code.NotFound)
  // Someone with access to the server sees its containers, but not logs or actions.
  const name = uniq('dock-user')
  const u = web(x.node, await createUser(x.v, name, ['ssh']))
  eq((await u.monitor.listContainers({})).containers.length, 0, 'no grant → nothing')
  await x.c.bastion.createGrant({ asset: id, subjectType: 'user', subject: name })
  eq((await u.monitor.listContainers({})).containers.filter((r) => r.asset === id).length, 3, 'granted → listed')
  await fails(u.monitor.containerLogs({ asset: id, name: 'web' }), Code.PermissionDenied)
  await fails(u.monitor.containerAction({ asset: id, name: 'web', action: 'stop' }), Code.PermissionDenied)
  const raw = JSON.stringify(auditEntries(x.node))
  ok(raw.includes(`restart container web on ${id}`) && raw.includes(`logs of container web on ${id}`), 'audited')
  await t.stop()
})

scenario('M', 'container usage: disk I/O rate, processes and size on disk per container; its own history for a range; only for people with access', async () => {
  const s = await server()
  const now = Math.floor(Date.now() / 1000)
  const ctr = (cpu: string, rd: number, pids: number) => [
    'ctr=api|running|acme/api:2.1|Up 3 hours|8080/tcp', 'ctr=job|exited|acme/job:1|Exited (0) 1 hour ago|',
    `cst=api|${cpu}%|256MiB / 1GiB|1MB / 1MB|${rd}MB / 6MB|${pids}`,
    'csz=api|2.5MB (virtual 190MB)', 'csz=job|0B (virtual 50MB)',
  ].join('\n')
  metrics(s.t, { now, extra: ctr('10.00', 12, 7) })
  await s.c.monitor.setMonitoring({ assets: [s.id], enabled: true })
  await waitFor('first reading', async () => (await sys(s.c, s.id))?.containersTotal === 2)
  // A minute later: 6 MB more read, nothing written, 9 processes.
  metrics(s.t, { now: now + 60, busy: 1100, total: 11000, extra: ctr('30.00', 18, 9) })
  const find = async (n: string) => (await s.c.monitor.listContainers({})).containers.find((r) => r.asset === s.id && r.container?.name === n)?.container
  const api = await waitFor('disk I/O rate', async () => { const c = await find('api'); return c?.ioRead !== undefined && c })
  eq([api.cpu, api.ioRead, api.ioWrite, api.pids], [30, 100000, 0, 9], 'cpu, disk read / write per second, processes')
  eq([api.sizeRw, api.sizeTotal, api.mem, api.memLimit], [2_500_000n, 190_000_000n, 256n * 1024n * 1024n, 1024n * 1024n * 1024n], 'written on top of the image, with the image, memory of its limit')
  const job = (await find('job'))!
  eq([job.state, job.sizeTotal, job.cpu, job.pids], ['exited', 50_000_000n, undefined, undefined], 'a stopped container still has a size, but no usage')

  const u = await s.c.monitor.getContainerUsage({ asset: s.id, name: 'api', range: '1h' })
  const real = (v: number[]) => v.filter((x) => !Number.isNaN(x))
  eq([u.system, u.container?.name, u.step], [s.name, 'api', 1], 'the container and its server')
  ok(u.times.length >= 2 && [...u.times].every((t, i, a) => !i || a[i - 1] < t), `oldest first: ${u.times}`)
  const se = u.series!
  ok([se.cpu, se.mem, se.rx, se.tx, se.ioRead, se.ioWrite, se.pids].every((v) => v.length === u.times.length), 'every series lines up with the times')
  ok(real(se.cpu).includes(10) && real(se.cpu).includes(30), `cpu history: ${se.cpu}`)
  ok(real(se.ioRead).includes(100000) && real(se.pids).includes(7) && real(se.pids).includes(9) && real(se.mem).every((v) => v === 256 * 1024 * 1024), 'disk I/O, processes and memory history')
  ok((await s.c.monitor.getContainerUsage({ asset: s.id, name: 'api', range: '24h' })).step === 600, 'longer ranges use averages')
  eq((await s.c.monitor.getContainerUsage({ asset: s.id, name: 'job', range: '1h' })).times.length, 0, 'a stopped container: known, no history')
  await fails(s.c.monitor.getContainerUsage({ asset: s.id, name: 'nope', range: '1h' }), Code.NotFound)
  await fails(s.c.monitor.getContainerUsage({ asset: s.id, name: 'api', range: '2y' }), Code.InvalidArgument)

  const name = uniq('usage-user')
  const p = web(s.node, await createUser(s.v, name, ['ssh']))
  await fails(p.monitor.getContainerUsage({ asset: s.id, name: 'api', range: '1h' }), Code.PermissionDenied)
  await s.c.bastion.createGrant({ asset: s.id, subjectType: 'user', subject: name })
  eq((await p.monitor.getContainerUsage({ asset: s.id, name: 'api', range: '1h' })).container?.pids, 9, 'granted → usage is visible')
  await s.t.stop()
})
