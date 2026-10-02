<script lang="ts">
  // A server's Docker containers: history per container (CPU, memory,
  // network), the list, and — for admins — logs and start / stop / restart.
  import { Container as Box, ScrollText, RotateCw, Square, Play, Loader2, ArrowDown, ArrowUp } from '@lucide/svelte'
  import { monitor, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { pct, rate, size } from '../lib/monitor'
  import type { Container, ContainerSeries } from '../gen/timika/v1/monitor_pb'
  import Chart from './Chart.svelte'
  import ContainerLogs from './ContainerLogs.svelte'

  let { asset, system, containers, series, times, step, canManage, onchanged }: {
    asset: string; system: string; containers: Container[]; series: ContainerSeries[]; times: number[]; step: number; canManage: boolean; onchanged: () => void
  } = $props()

  const COLORS = ['#2f9e8f', '#3b82f6', '#f59e0b', '#c084fc', '#ef4444', '#14b8a6', '#eab308', '#ec4899', '#64748b', '#84cc16']
  const MAX_LINES = 10

  let hidden = $state<string[]>([])
  let logs = $state<string | null>(null)
  let acting = $state('')
  let note = $state<{ ok: boolean; text: string } | null>(null)

  const mean = (v: number[]) => { const x = v.filter((n) => !Number.isNaN(n)); return x.length ? x.reduce((a, b) => a + b, 0) / x.length : 0 }
  // The busiest containers get a line each (by memory, then CPU).
  const ranked = $derived([...series].sort((a, b) => mean(b.mem) - mean(a.mem) || mean(b.cpu) - mean(a.cpu)).slice(0, MAX_LINES))
  const color = (name: string) => COLORS[Math.max(0, ranked.findIndex((s) => s.name === name)) % COLORS.length]
  const shown = $derived(ranked.filter((s) => !hidden.includes(s.name)))
  const lines = (pick: (s: ContainerSeries) => number[]) => shown.map((s) => ({ label: s.name, values: pick(s), color: color(s.name) }))
  const toggle = (n: string) => (hidden = hidden.includes(n) ? hidden.filter((x) => x !== n) : [...hidden, n])
  const sum = (a: number[], b: number[]) => a.map((v, i) => (Number.isNaN(v) && Number.isNaN(b[i]) ? NaN : (Number.isNaN(v) ? 0 : v) + (Number.isNaN(b[i]) ? 0 : b[i])))

  async function act(c: Container, action: 'start' | 'stop' | 'restart') {
    if (action !== 'start') {
      const ok = await confirm({ title: `${action === 'stop' ? 'Stop' : 'Restart'} ${c.name}?`, message: `On ${system}. ${action === 'stop' ? 'It stays stopped until someone starts it.' : 'It will be unavailable for a moment.'}`, confirmText: action === 'stop' ? 'Stop' : 'Restart', variant: action === 'stop' ? 'danger' : 'warning' })
      if (!ok) return
    }
    acting = `${c.name}:${action}`
    note = null
    try {
      const r = await monitor.containerAction({ asset, name: c.name, action })
      note = { ok: r.ok, text: r.ok ? `${c.name}: ${action === 'stop' ? 'stopped' : action === 'start' ? 'started' : 'restarted'} — the list updates with the next reading.` : `${c.name}: ${r.output || 'docker refused'}` }
      onchanged()
    } catch (e) { note = { ok: false, text: errMsg(e) } } finally { acting = '' }
  }
  const running = $derived(containers.filter((c) => c.state === 'running').length)
  const ports = (p: string) => [...new Set(p.split(',').map((x) => x.trim().replace(/^(0\.0\.0\.0|\[::\]|:::):/, '')).filter(Boolean))].join(', ')
</script>

<section class="page-card cp">
  <div class="page-card__head cp__head">
    <div class="page-card__title"><Box size={14} /> Containers <span class="muted cp__sub">{running} running of {containers.length}</span></div>
  </div>

  {#if ranked.length}
    <div class="cp__legend">
      {#each ranked as s (s.name)}
        <button class="cp__chip" class:is-off={hidden.includes(s.name)} onclick={() => toggle(s.name)} title="Show / hide in the charts"><i style="background:{color(s.name)}"></i>{s.name}</button>
      {/each}
      {#if series.length > MAX_LINES}<span class="muted cp__more">+{series.length - MAX_LINES} more in the table</span>{/if}
    </div>
    <div class="cp__charts">
      <div class="cp__chart"><div class="cp__t">CPU</div><Chart {times} {step} height={130} format={(v) => `${v.toFixed(v < 10 ? 1 : 0)}%`} series={lines((s) => s.cpu)} /></div>
      <div class="cp__chart"><div class="cp__t">Memory</div><Chart {times} {step} height={130} format={(v) => size(Math.round(v))} series={lines((s) => s.mem)} /></div>
      <div class="cp__chart"><div class="cp__t">Network (in + out)</div><Chart {times} {step} height={130} format={rate} series={lines((s) => sum(s.rx, s.tx))} /></div>
    </div>
  {/if}

  {#if note}<div class="notice notice--{note.ok ? 'success' : 'error'} cp__note">{note.text}</div>{/if}

  <div class="data-table-wrap">
    <table class="data-table">
      <thead><tr><th>Name</th><th>Image</th><th>Status</th><th>Ports</th><th>CPU</th><th>Memory</th><th>Network</th>{#if canManage}<th></th>{/if}</tr></thead>
      <tbody>
        {#each containers as c (c.name)}
          <tr class:cp--off={c.state !== 'running'}>
            <td><span class="cp__name"><i class="cp__dot cp__dot--{c.state}"></i><span class="mono strong">{c.name}</span>{#if c.health}<span class="badge badge--{c.health === 'healthy' ? 'success' : c.health === 'unhealthy' ? 'danger' : 'warning'}">{c.health}</span>{/if}</span></td>
            <td class="mono cp__img" title={c.image}>{c.image || '—'}</td>
            <td class="cp__status">{c.status || c.state}</td>
            <td class="mono cp__ports" title={c.ports}>{ports(c.ports) || '—'}</td>
            <td class="mono">{c.cpu !== undefined ? pct(c.cpu) : '—'}</td>
            <td class="mono">{Number(c.mem) ? size(c.mem) : '—'}{#if Number(c.memLimit) && Number(c.mem)}<span class="muted"> / {size(c.memLimit)}</span>{/if}</td>
            <td class="mono cp__net">{#if c.rx !== undefined}<ArrowDown size={11} /> {rate(c.rx)} <ArrowUp size={11} /> {rate(c.tx)}{:else}—{/if}</td>
            {#if canManage}
              <td class="cp__act">
                <button class="icon-btn" title="Logs" onclick={() => (logs = c.name)}><ScrollText size={14} /></button>
                {#if c.state === 'running'}
                  <button class="icon-btn" title="Restart" disabled={!!acting} onclick={() => act(c, 'restart')}>{#if acting === `${c.name}:restart`}<Loader2 size={14} class="spin" />{:else}<RotateCw size={14} />{/if}</button>
                  <button class="icon-btn" title="Stop" disabled={!!acting} onclick={() => act(c, 'stop')}>{#if acting === `${c.name}:stop`}<Loader2 size={14} class="spin" />{:else}<Square size={13} />{/if}</button>
                {:else}
                  <button class="icon-btn" title="Start" disabled={!!acting} onclick={() => act(c, 'start')}>{#if acting === `${c.name}:start`}<Loader2 size={14} class="spin" />{:else}<Play size={14} />{/if}</button>
                {/if}
              </td>
            {/if}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</section>

{#if logs}<ContainerLogs {asset} {system} name={logs} onclose={() => (logs = null)} />{/if}

<style>
  .cp__head { display: flex; align-items: center; justify-content: space-between; }
  .cp__sub { font-size: 11.5px; font-weight: 400; margin-left: 6px; }
  .cp__legend { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 16px 8px; align-items: center; }
  .cp__chip { display: inline-flex; align-items: center; gap: 6px; padding: 3px 10px; border-radius: 12px; border: 1px solid var(--border); background: var(--bg-surface); color: var(--text-secondary); font-size: 12px; font-family: var(--mono); cursor: pointer; }
  .cp__chip i { width: 9px; height: 9px; border-radius: 3px; }
  .cp__chip.is-off { opacity: 0.45; text-decoration: line-through; }
  .cp__more { font-size: 11.5px; }
  .cp__charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(330px, 1fr)); gap: 12px; padding: 0 16px 12px; }
  .cp__chart { min-width: 0; }
  .cp__t { font-size: 12px; font-weight: 700; color: var(--text-primary); margin-bottom: 2px; }
  .cp__note { margin: 0 16px 10px; }
  .cp__name { display: inline-flex; align-items: center; gap: 8px; }
  .cp__dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-muted); flex: 0 0 auto; }
  .cp__dot--running { background: var(--success); }
  .cp__dot--restarting, .cp__dot--paused { background: var(--warning); }
  .cp__dot--dead { background: var(--danger); }
  .cp--off { opacity: 0.7; }
  .cp__img, .cp__ports { max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
  .cp__status { font-size: 12px; color: var(--text-secondary); white-space: nowrap; }
  .cp__net { font-size: 11.5px; white-space: nowrap; }
  .cp__net :global(svg), .page-card__title :global(svg) { vertical-align: -2px; }
  .cp__act { text-align: right; white-space: nowrap; }
</style>
