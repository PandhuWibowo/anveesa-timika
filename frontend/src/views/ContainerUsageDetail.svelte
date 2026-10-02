<script lang="ts">
  // One container over time: CPU, memory, network, disk I/O and processes.
  import { ArrowLeft, Loader2, Cpu, MemoryStick, HardDrive, Network, ListTree, Activity, Container as Box } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { monitor, errMsg } from '../lib/api'
  import { navigate } from '../lib/router.svelte'
  import { pct, rate, size, level, RANGES, type Range } from '../lib/monitor'
  import type { ContainerUsage } from '../gen/timika/v1/monitor_pb'
  import Chart from '../components/Chart.svelte'

  let { asset, name }: { asset: string; name: string } = $props()

  let range = $state<Range>('1h')
  const usage = createQuery(() => ({ queryKey: keys.containerUsage(asset, name, range), queryFn: () => monitor.getContainerUsage({ asset, name, range }), refetchInterval: 10000, placeholderData: (prev: ContainerUsage | undefined) => prev }))
  const d = $derived<ContainerUsage | null>(usage.data ?? null)
  const error = $derived(usage.error && !usage.data ? errMsg(usage.error) : '')
  const c = $derived(d?.container)
  const s = $derived(d?.series)
  const times = $derived(d?.times.map(Number) ?? [])
  const running = $derived(c?.state === 'running')
  const memPct = $derived(c && Number(c.memLimit) ? (Number(c.mem) / Number(c.memLimit)) * 100 : undefined)
  const has = (v?: number[]) => !!v?.some((x) => !Number.isNaN(x))
  const C = { a: 'var(--brand)', b: 'var(--info)', c: 'var(--warning)', d: '#c79bf2', e: 'var(--danger)' }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <div><button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate('/usage')}><ArrowLeft size={13} /> Containers</button></div>
      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if !d}
        {#if !error}<div class="empty-state"><Loader2 size={20} class="spin" /></div>{/if}
      {:else}
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">Container usage · {d.system}{c?.runtime ? ` · ${c.runtime}` : ''}</div>
            <h1 class="page-title ud-title">
              <span class="ud-dot" class:ud-dot--on={running}></span><span class="mono">{name}</span>
              <span class="badge badge--{running ? 'success' : 'default'}">{c ? c.status || c.state : 'gone'}</span>
              {#if c?.health}<span class="badge badge--{c.health === 'healthy' ? 'success' : c.health === 'unhealthy' ? 'danger' : 'warning'}">{c.health}</span>{/if}
            </h1>
            {#if c?.image}<div class="ud-meta mono">{c.image}</div>{/if}
            {#if !c}<div class="ud-meta">This container is no longer on the server; its history is kept for a while.</div>{/if}
          </div>
          <div class="page-hero__actions ud-actions">
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(`/monitoring/${asset}`)}><Activity size={13} /> Server</button>
            {#if c}<button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(`/containers/${asset}/${name}`)}><Box size={13} /> Manage</button>{/if}
          </div>
        </section>

        {#if c}
          <div class="ud-tiles">
            <div class="ud-tile"><div class="ud-tile__l"><Cpu size={13} /> CPU</div><div class="ud-tile__v ud-lv--{level(c.cpu)}">{pct(c.cpu)}</div><div class="ud-tile__s">100% = one core</div></div>
            <div class="ud-tile"><div class="ud-tile__l"><MemoryStick size={13} /> Memory</div><div class="ud-tile__v ud-lv--{level(memPct)}">{Number(c.mem) ? size(c.mem) : '—'}</div><div class="ud-tile__s">{memPct !== undefined ? `${pct(memPct)} of ${size(c.memLimit)}` : 'no limit reported'}</div></div>
            <div class="ud-tile"><div class="ud-tile__l"><HardDrive size={13} /> Disk</div><div class="ud-tile__v">{Number(c.sizeTotal) ? size(c.sizeRw) : '—'}</div><div class="ud-tile__s">{Number(c.sizeTotal) ? `written · ${size(c.sizeTotal)} with the image` : 'size not reported'}</div></div>
            <div class="ud-tile"><div class="ud-tile__l"><Network size={13} /> Network</div><div class="ud-tile__v ud-tile__v--sm">↓ {rate(c.rx)}</div><div class="ud-tile__s">↑ {rate(c.tx)}</div></div>
            <div class="ud-tile"><div class="ud-tile__l"><HardDrive size={13} /> Disk I/O</div><div class="ud-tile__v ud-tile__v--sm">R {rate(c.ioRead)}</div><div class="ud-tile__s">W {rate(c.ioWrite)}</div></div>
            <div class="ud-tile"><div class="ud-tile__l"><ListTree size={13} /> Processes</div><div class="ud-tile__v">{c.pids ?? '—'}</div><div class="ud-tile__s">inside the container</div></div>
          </div>
        {/if}

        <div class="ud-range">
          <div class="seg">{#each RANGES as r (r)}<button class:is-on={range === r} onclick={() => (range = r)}>{r}</button>{/each}</div>
          <span class="muted">{d.step < 600 ? `every ${d.step} s` : d.step === 600 ? '10-minute averages' : 'hourly averages'}</span>
        </div>

        {#if !times.length}
          <div class="empty-state">No readings in this range yet.</div>
        {:else if s}
          <div class="ud-charts">
            <section class="page-card ud-chart"><div class="ud-chart__t"><Cpu size={13} /> CPU</div><Chart {times} step={d.step} format={(v) => `${v.toFixed(v < 10 ? 1 : 0)}%`} series={[{ label: 'CPU', values: s.cpu, color: C.a }]} /></section>
            <section class="page-card ud-chart"><div class="ud-chart__t"><MemoryStick size={13} /> Memory</div><Chart {times} step={d.step} format={(v) => size(Math.round(v))} series={[{ label: 'Memory', values: s.mem, color: C.b }]} /></section>
            <section class="page-card ud-chart"><div class="ud-chart__t"><Network size={13} /> Network</div><Chart {times} step={d.step} format={rate} series={[{ label: 'In', values: s.rx, color: C.a }, { label: 'Out', values: s.tx, color: C.c }]} /></section>
            {#if has(s.ioRead) || has(s.ioWrite)}<section class="page-card ud-chart"><div class="ud-chart__t"><HardDrive size={13} /> Disk I/O</div><Chart {times} step={d.step} format={rate} series={[{ label: 'Read', values: s.ioRead, color: C.b }, { label: 'Write', values: s.ioWrite, color: C.e }]} /></section>{/if}
            {#if has(s.pids)}<section class="page-card ud-chart"><div class="ud-chart__t"><ListTree size={13} /> Processes</div><Chart {times} step={d.step} format={(v) => v.toFixed(0)} series={[{ label: 'Processes', values: s.pids, color: C.d }]} /></section>{/if}
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>

<style>
  .ud-title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .ud-title .badge { font-size: 11px; }
  .ud-title .mono { font-size: inherit; }
  .ud-dot { width: 11px; height: 11px; border-radius: 50%; background: var(--text-muted); }
  .ud-dot--on { background: var(--success); box-shadow: 0 0 0 4px var(--success-bg); }
  .ud-meta { font-size: 12px; color: var(--text-secondary); margin-top: 6px; }
  .ud-actions { flex: 0 0 auto; flex-wrap: nowrap; }
  .ud-tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 10px; }
  .ud-tile { padding: 12px 14px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); }
  .ud-tile__l { display: flex; align-items: center; gap: 6px; font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); }
  .ud-tile__v { font-size: 22px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .ud-tile__v--sm { font-size: 16px; line-height: 33px; }
  .ud-tile__s { font-size: 11.5px; color: var(--text-muted); }
  .ud-lv--warn { color: var(--warning); }
  .ud-lv--bad { color: var(--danger); }
  .ud-range { display: flex; align-items: center; gap: 12px; font-size: 12px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 4px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .ud-charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 12px; }
  .ud-chart { padding: 12px 12px 8px; }
  .ud-chart__t { display: flex; align-items: center; gap: 6px; font-size: 12.5px; font-weight: 700; color: var(--text-primary); margin-bottom: 4px; }
  .ud-chart__t :global(svg) { color: var(--text-muted); }
</style>
