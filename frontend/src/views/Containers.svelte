<script lang="ts">
  // Every Docker container on every monitored server.
  import { Container as Box, Search, Loader2, ScrollText, ArrowDown, ArrowUp } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { monitor, errMsg } from '../lib/api'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { pct, rate, size } from '../lib/monitor'
  import type { ContainerRow } from '../gen/timika/v1/monitor_pb'
  import ContainerLogs from '../components/ContainerLogs.svelte'

  const list = createQuery(() => ({ queryKey: keys.containers, queryFn: () => monitor.listContainers({}), refetchInterval: 10000 }))
  const rows = $derived<ContainerRow[]>(list.data?.containers ?? [])
  const error = $derived(list.error && !list.data ? errMsg(list.error) : '')
  let q = $state('')
  let only = $state('')
  let logs = $state<ContainerRow | null>(null)
  const canManage = isAdmin()

  const shown = $derived(rows.filter((r) => {
    const c = r.container!
    return (!only || (only === 'stopped' ? c.state !== 'running' : only === 'unhealthy' ? c.health === 'unhealthy' : c.state === only)) &&
      (!q || `${c.name} ${c.image} ${r.system} ${c.ports}`.toLowerCase().includes(q.trim().toLowerCase()))
  }).sort((a, b) => a.system.localeCompare(b.system) || Number(b.container!.state === 'running') - Number(a.container!.state === 'running') || a.container!.name.localeCompare(b.container!.name)))
  const count = (f: (r: ContainerRow) => boolean) => rows.filter(f).length
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Monitoring</div>
          <h1 class="page-title">Containers</h1>
          <p class="page-subtitle">Every Docker container on your monitored servers. Open a server for history charts{canManage ? ', logs and start / stop / restart' : ''}.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{rows.length}</span><span class="page-metric__label">Containers</span></div>
          <div class="page-metric"><span class="page-metric__value ct-ok">{count((r) => r.container!.state === 'running')}</span><span class="page-metric__label">Running</span></div>
          <div class="page-metric"><span class="page-metric__value">{count((r) => r.container!.state !== 'running')}</span><span class="page-metric__label">Stopped</span></div>
          <div class="page-metric"><span class="page-metric__value" class:ct-bad={count((r) => r.container!.health === 'unhealthy') > 0}>{count((r) => r.container!.health === 'unhealthy')}</span><span class="page-metric__label">Unhealthy</span></div>
        </div>
      </section>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      <div class="ct-bar">
        <div class="ct-search"><Search size={15} /><input bind:value={q} placeholder="Filter by name, image, server, port…" /></div>
        <div class="seg">
          {#each [['', 'All'], ['running', 'Running'], ['stopped', 'Stopped'], ['unhealthy', 'Unhealthy']] as [id, label] (id)}<button class:is-on={only === id} onclick={() => (only = id)}>{label}</button>{/each}
        </div>
      </div>

      <section class="page-card">
        {#if list.isPending}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <div class="data-table-wrap">
            <table class="data-table">
              <thead><tr><th>Container</th><th>Server</th><th>Image</th><th>Status</th><th>CPU</th><th>Memory</th><th>Network</th>{#if canManage}<th></th>{/if}</tr></thead>
              <tbody>
                {#each shown as r (r.asset + '/' + r.container!.name)}
                  {@const c = r.container!}
                  <tr class="ct-row" class:ct--off={c.state !== 'running'} onclick={() => navigate(`/monitoring/${r.asset}`)}>
                    <td><span class="ct-name"><i class="ct-dot ct-dot--{c.state}"></i><span class="mono strong">{c.name}</span>{#if c.health}<span class="badge badge--{c.health === 'healthy' ? 'success' : c.health === 'unhealthy' ? 'danger' : 'warning'}">{c.health}</span>{/if}</span></td>
                    <td>{r.system}</td>
                    <td class="mono ct-img" title={c.image}>{c.image || '—'}</td>
                    <td class="ct-status">{c.status || c.state}</td>
                    <td class="mono">{c.cpu !== undefined ? pct(c.cpu) : '—'}</td>
                    <td class="mono">{Number(c.mem) ? size(c.mem) : '—'}</td>
                    <td class="mono ct-net">{#if c.rx !== undefined}<ArrowDown size={11} /> {rate(c.rx)} <ArrowUp size={11} /> {rate(c.tx)}{:else}—{/if}</td>
                    {#if canManage}<td class="ct-act"><button class="icon-btn" title="Logs" onclick={(e) => { e.stopPropagation(); logs = r }}><ScrollText size={14} /></button></td>{/if}
                  </tr>
                {:else}
                  <tr><td colspan={canManage ? 8 : 7}><div class="empty-state ct-empty">
                    <Box size={26} />
                    {#if rows.length}Nothing matches.{:else}No containers found. Monitored servers with Docker show theirs here — the monitoring account must be allowed to run <code>docker</code>.{/if}
                  </div></td></tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    </div>
  </div>
</div>

{#if logs}<ContainerLogs asset={logs.asset} system={logs.system} name={logs.container!.name} onclose={() => (logs = null)} />{/if}

<style>
  .ct-ok { color: var(--success); }
  .ct-bad { color: var(--danger); }
  .ct-bar { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .ct-search { flex: 1; min-width: 220px; display: flex; align-items: center; gap: 8px; padding: 0 12px; height: 34px; background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); color: var(--text-muted); }
  .ct-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 6px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .ct-row { cursor: pointer; }
  .ct--off { opacity: 0.7; }
  .ct-name { display: inline-flex; align-items: center; gap: 8px; }
  .ct-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-muted); flex: 0 0 auto; }
  .ct-dot--running { background: var(--success); }
  .ct-dot--restarting, .ct-dot--paused { background: var(--warning); }
  .ct-dot--dead { background: var(--danger); }
  .ct-img { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
  .ct-status { font-size: 12px; color: var(--text-secondary); white-space: nowrap; }
  .ct-net { font-size: 11.5px; white-space: nowrap; }
  .ct-net :global(svg) { vertical-align: -2px; }
  .ct-act { text-align: right; }
  .ct-empty { gap: 8px; padding: 30px 20px; }
</style>
