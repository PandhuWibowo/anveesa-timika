<script lang="ts">
  // One system: live numbers, history charts, disks, containers, failed
  // services and its alert rules.
  import { ArrowLeft, Loader2, SquareTerminal, BellRing, Pencil, CircleAlert, Trash2, Cpu, MemoryStick, HardDrive, Network, Gauge, Thermometer, Container as Box, Check } from '@lucide/svelte'
  import { monitor, errMsg } from '../lib/api'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys, queryClient } from '../lib/query'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { confirm } from '../lib/ui.svelte'
  import { pct, rate, size, uptime, level, since, RANGES, RULES, ruleLabel, type Range, type RuleRow } from '../lib/monitor'
  import type { SystemDetail } from '../gen/timika/v1/monitor_pb'
  import Chart from '../components/Chart.svelte'
  import RulesEditor from '../components/RulesEditor.svelte'
  import ContainerPanel from '../components/ContainerPanel.svelte'

  let { id }: { id: string } = $props()

  let range = $state<Range>('1h')
  // One cache entry per range: switching back shows it at once.
  const detail = createQuery(() => ({ queryKey: keys.system(id, range), queryFn: () => monitor.getSystem({ asset: id, range }), refetchInterval: 5000, placeholderData: (prev: SystemDetail | undefined) => prev }))
  const d = $derived<SystemDetail | null>(detail.data ?? null)
  let actionError = $state('')
  const error = $derived(actionError || (detail.error && !detail.data ? errMsg(detail.error) : ''))
  const load = () => detail.refetch()
  let editing = $state(false)
  let rules = $state<RuleRow[]>([])
  let busy = $state(false)
  const canManage = isAdmin()


  const s = $derived(d?.system)
  const l = $derived(d?.system?.latest)
  const times = $derived(d?.times.map(Number) ?? [])
  const col = (m: string) => d?.series.find((x) => x.metric === m)?.values ?? []
  const has = (m: string) => col(m).some((v) => !Number.isNaN(v))
  const C = { a: 'var(--brand)', b: 'var(--info)', c: 'var(--warning)', d: '#c79bf2', e: 'var(--danger)' }

  function startEdit() {
    rules = d?.rules.map((r) => ({ metric: r.metric, threshold: r.threshold, minutes: r.minutes })) ?? []
    editing = true
  }
  async function saveRules(useDefaults: boolean) {
    busy = true
    try {
      await monitor.setSystemAlerts({ asset: id, useDefaults, rules: useDefaults ? [] : rules })
      editing = false
      await load()
    } catch (e) { actionError = errMsg(e) } finally { busy = false }
  }
  async function stop() {
    const ok = await confirm({ title: `Stop monitoring ${s?.name}?`, message: 'Its history is deleted. The server itself is not touched.', confirmText: 'Stop monitoring', variant: 'danger' })
    if (!ok) return
    try { await monitor.setMonitoring({ assets: [id], enabled: false }); queryClient.invalidateQueries({ queryKey: keys.systems }); navigate('/monitoring') } catch (e) { actionError = errMsg(e) }
  }
  const ruleText = (r: { metric: string; threshold: number; minutes: number }) => {
    const def = RULES.find((x) => x.id === r.metric)
    return r.metric === 'status' ? `Down for ${r.minutes} min` : `${def?.label ?? r.metric} ${r.threshold / (def?.scale ?? 1)}${def?.unit === '%' ? '%' : def?.unit ? ' ' + def.unit : ''} for ${r.minutes} min`
  }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <div><button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate('/monitoring')}><ArrowLeft size={13} /> Systems</button></div>
      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if !d || !s}
        {#if !error}<div class="empty-state"><Loader2 size={20} class="spin" /></div>{/if}
      {:else}
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">System</div>
            <h1 class="page-title ms-title">
              <span class="ms-dot ms-dot--{s.status}"></span>{s.name}
              <span class="badge badge--{s.status === 'up' ? 'success' : s.status === 'down' ? 'danger' : 'default'}">{s.status}{s.since && s.status !== 'pending' ? ` · ${since(s.since)}` : ''}</span>
              {#each s.firing as f (f.metric)}<span class="badge badge--warning"><BellRing size={11} /> {ruleLabel(f.metric)}</span>{/each}
            </h1>
            <div class="ms-meta">
              <span class="mono">{s.account}@{s.host}</span>
              {#if l}
                {#if l.os}<span>{l.os}</span>{/if}
                {#if l.kernel}<span class="mono">{l.kernel}</span>{/if}
                {#if l.cpus}<span>{l.cpus} × {l.cpuModel || 'CPU'}</span>{/if}
                <span>up {uptime(l.uptime)}</span>
              {/if}
            </div>
            {#if s.status === 'down'}<div class="ms-err"><CircleAlert size={13} /> {s.error}</div>
            {:else if s.lastError && s.lastErrorAt && Date.now() - new Date(s.lastErrorAt).getTime() < 86400_000}
              <div class="ms-miss" title="A missed reading shows as a gap in the charts"><CircleAlert size={12} /> Last missed reading {new Date(s.lastErrorAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })} — {s.lastError}</div>
            {/if}
          </div>
          <div class="page-hero__actions ms-actions">
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(`/servers/${id}`)}><SquareTerminal size={13} /> Server</button>
            {#if canManage}<button class="base-btn base-btn--ghost base-btn--sm" onclick={stop}><Trash2 size={13} /> Stop</button>{/if}
          </div>
        </section>

        {#if l}
          <div class="ms-tiles">
            <div class="ms-tile"><div class="ms-tile__l"><Cpu size={13} /> CPU</div><div class="ms-tile__v ms-lv--{level(l.cpu)}">{pct(l.cpu)}</div><div class="ms-tile__s">load {l.load1?.toFixed(2) ?? '—'} · {l.load5?.toFixed(2) ?? '—'} · {l.load15?.toFixed(2) ?? '—'}</div></div>
            <div class="ms-tile"><div class="ms-tile__l"><MemoryStick size={13} /> Memory</div><div class="ms-tile__v ms-lv--{level(l.mem)}">{pct(l.mem)}</div><div class="ms-tile__s">{size(l.memUsed)} of {size(l.memTotal)}{l.swap !== undefined ? ` · swap ${pct(l.swap)}` : ''}</div></div>
            <div class="ms-tile"><div class="ms-tile__l"><HardDrive size={13} /> Disk</div><div class="ms-tile__v ms-lv--{level(l.disk)}">{pct(l.disk)}</div><div class="ms-tile__s">{size(l.diskUsed)} of {size(l.diskTotal)}</div></div>
            <div class="ms-tile"><div class="ms-tile__l"><Network size={13} /> Network</div><div class="ms-tile__v ms-tile__v--sm">↓ {rate(l.rx)}</div><div class="ms-tile__s">↑ {rate(l.tx)}</div></div>
            {#if l.temp !== undefined}<div class="ms-tile"><div class="ms-tile__l"><Thermometer size={13} /> Temperature</div><div class="ms-tile__v">{l.temp.toFixed(0)}°C</div><div class="ms-tile__s">hottest sensor</div></div>{/if}
          </div>
        {/if}

        <div class="ms-range">
          <div class="seg">{#each RANGES as r (r)}<button class:is-on={range === r} onclick={() => (range = r)}>{r}</button>{/each}</div>
          <span class="muted">{d.step < 600 ? `every ${d.step} s` : d.step === 600 ? '10-minute averages' : 'hourly averages'}</span>
        </div>

        <div class="ms-charts">
          <section class="page-card ms-chart"><div class="ms-chart__t"><Cpu size={13} /> CPU</div><Chart {times} step={d.step} max={100} format={(v) => `${v.toFixed(0)}%`} series={[{ label: 'CPU', values: col('cpu'), color: C.a }]} /></section>
          <section class="page-card ms-chart"><div class="ms-chart__t"><MemoryStick size={13} /> Memory{has('swap') ? ' & swap' : ''}</div><Chart {times} step={d.step} max={100} format={(v) => `${v.toFixed(0)}%`} series={[{ label: 'Memory', values: col('mem'), color: C.b }, ...(has('swap') ? [{ label: 'Swap', values: col('swap'), color: C.d }] : [])]} /></section>
          <section class="page-card ms-chart"><div class="ms-chart__t"><Network size={13} /> Network</div><Chart {times} step={d.step} format={rate} series={[{ label: 'In', values: col('rx'), color: C.a }, { label: 'Out', values: col('tx'), color: C.c }]} /></section>
          <section class="page-card ms-chart"><div class="ms-chart__t"><HardDrive size={13} /> Disk I/O</div><Chart {times} step={d.step} format={rate} series={[{ label: 'Read', values: col('io_read'), color: C.b }, { label: 'Write', values: col('io_write'), color: C.e }]} /></section>
          <section class="page-card ms-chart"><div class="ms-chart__t"><Gauge size={13} /> Load (1 min)</div><Chart {times} step={d.step} format={(v) => v.toFixed(2)} series={[{ label: 'Load', values: col('load'), color: C.c }]} /></section>
          <section class="page-card ms-chart"><div class="ms-chart__t"><HardDrive size={13} /> Disk usage</div><Chart {times} step={d.step} max={100} format={(v) => `${v.toFixed(0)}%`} series={[{ label: 'Disk', values: col('disk'), color: C.d }]} /></section>
          {#if has('temp')}<section class="page-card ms-chart"><div class="ms-chart__t"><Thermometer size={13} /> Temperature</div><Chart {times} step={d.step} format={(v) => `${v.toFixed(0)}°C`} series={[{ label: 'Hottest', values: col('temp'), color: C.e }]} /></section>{/if}
        </div>

        <div class="ms-lower">
          <section class="page-card">
            <div class="page-card__head"><div class="page-card__title">Disks</div></div>
            <div class="ms-list">
              {#each d.disks as k (k.device)}
                {@const p = Number(k.total) ? (Number(k.used) / Number(k.total)) * 100 : 0}
                <div class="ms-disk">
                  <div class="ms-disk__top"><span class="mono strong">{k.mount}</span><span class="muted mono">{k.device}</span><span class="ms-disk__n">{size(k.used)} / {size(k.total)} · {pct(p)}</span></div>
                  <div class="ms-meter"><i class="ms-fill ms-fill--{level(p)}" style="width:{p}%"></i></div>
                </div>
              {:else}<div class="empty-state">No disks reported.</div>{/each}
            </div>
          </section>

          <section class="page-card">
            <div class="page-card__head">
              <div class="page-card__title">Alerts {#if d.defaultRules}<span class="muted ms-sub">default rules</span>{:else}<span class="badge badge--info">own rules</span>{/if}</div>
              {#if canManage && !editing}<button class="base-btn base-btn--ghost base-btn--xs" onclick={startEdit}><Pencil size={12} /> Edit</button>{/if}
            </div>
            <div class="ms-list">
              {#if editing}
                <RulesEditor bind:value={rules} />
                <div class="ms-edit">
                  <button class="base-btn base-btn--ghost base-btn--sm" disabled={busy} onclick={() => saveRules(true)}>Use the defaults</button>
                  <span class="ms-spacer"></span>
                  <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (editing = false)}>Cancel</button>
                  <button class="base-btn base-btn--primary base-btn--sm" disabled={busy} onclick={() => saveRules(false)}>{#if busy}<Loader2 size={13} class="spin" />{/if} Save</button>
                </div>
              {:else}
                {#each d.rules as r (r.metric)}
                  {@const f = s.firing.find((x) => x.metric === r.metric)}
                  <div class="ms-rule" class:ms-rule--on={!!f || (r.metric === 'status' && s.status === 'down')}>
                    {#if f || (r.metric === 'status' && s.status === 'down')}<BellRing size={13} />{:else}<Check size={13} />{/if}
                    <span>{ruleText(r)}</span>
                    {#if f}<span class="ms-rule__since">firing {since(f.since)}</span>{/if}
                  </div>
                {:else}<div class="empty-state">No alert rules for this server.</div>{/each}
              {/if}
            </div>
          </section>

          {#if d.containers.length}
            <div class="ms-wide"><ContainerPanel asset={id} system={s.name} containers={d.containers} series={d.containerSeries} {times} step={d.step} {canManage} onchanged={load} /></div>
          {/if}

          {#if d.failedUnitNames.length}
            <section class="page-card ms-wide">
              <div class="page-card__head"><div class="page-card__title ms-bad"><CircleAlert size={14} /> Failed services</div></div>
              <div class="ms-units">{#each d.failedUnitNames as u (u)}<span class="badge badge--danger mono">{u}</span>{/each}</div>
            </section>
          {/if}
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .ms-title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .ms-title .badge { font-size: 11px; }
  .ms-title :global(svg) { vertical-align: -1px; }
  .ms-dot { width: 11px; height: 11px; border-radius: 50%; background: var(--text-muted); }
  .ms-dot--up { background: var(--success); box-shadow: 0 0 0 4px var(--success-bg); }
  .ms-dot--down { background: var(--danger); box-shadow: 0 0 0 4px var(--danger-bg); }
  .ms-meta { display: flex; gap: 14px; flex-wrap: wrap; font-size: 12px; color: var(--text-secondary); margin-top: 6px; }
  .ms-err { color: var(--danger); font-size: 12.5px; margin-top: 8px; }
  .ms-err :global(svg), .ms-miss :global(svg) { vertical-align: -2px; }
  .ms-miss { color: var(--text-muted); font-size: 12px; margin-top: 8px; }
  .ms-actions { flex: 0 0 auto; flex-wrap: nowrap; }
  .ms-tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 10px; }
  .ms-tile { padding: 12px 14px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); }
  .ms-tile__l { display: flex; align-items: center; gap: 6px; font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); }
  .ms-tile__v { font-size: 22px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .ms-tile__v--sm { font-size: 16px; line-height: 33px; }
  .ms-tile__s { font-size: 11.5px; color: var(--text-muted); }
  .ms-lv--warn { color: var(--warning); }
  .ms-lv--bad { color: var(--danger); }
  .ms-range { display: flex; align-items: center; gap: 12px; font-size: 12px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 4px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .ms-charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 12px; }
  .ms-chart { padding: 12px 12px 8px; }
  .ms-chart__t { display: flex; align-items: center; gap: 6px; font-size: 12.5px; font-weight: 700; color: var(--text-primary); margin-bottom: 4px; }
  .ms-chart__t :global(svg) { color: var(--text-muted); }
  .ms-lower { display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 12px; align-items: start; }
  .ms-wide { grid-column: 1 / -1; }
  .ms-list { padding: 4px 16px 14px; display: flex; flex-direction: column; gap: 10px; }
  .ms-disk__top { display: flex; align-items: baseline; gap: 8px; font-size: 12.5px; margin-bottom: 4px; }
  .ms-disk__n { margin-left: auto; font-size: 11.5px; color: var(--text-secondary); font-family: var(--mono); }
  .ms-meter { height: 6px; border-radius: 3px; background: var(--bg-elevated); overflow: hidden; }
  .ms-fill { display: block; height: 100%; background: var(--success); }
  .ms-fill--warn { background: var(--warning); }
  .ms-fill--bad { background: var(--danger); }
  .ms-sub { font-size: 11.5px; font-weight: 400; margin-left: 6px; }
  .ms-rule { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-secondary); }
  .ms-rule :global(svg) { color: var(--success); flex: 0 0 auto; }
  .ms-rule--on { color: var(--warning); font-weight: 600; }
  .ms-rule--on :global(svg) { color: var(--warning); }
  .ms-rule__since { margin-left: auto; font-size: 11.5px; font-weight: 400; }
  .ms-edit { display: flex; gap: 8px; align-items: center; margin-top: 4px; }
  .ms-spacer { flex: 1; }
  .ms-units { display: flex; gap: 6px; flex-wrap: wrap; padding: 4px 16px 14px; }
  .ms-bad { color: var(--danger); }
  .page-card__head { display: flex; align-items: center; justify-content: space-between; }
  .page-card__title :global(svg) { vertical-align: -2px; }
</style>
