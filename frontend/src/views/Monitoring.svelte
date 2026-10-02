<script lang="ts">
  // Monitoring: every monitored server at a glance — no agent to install,
  // timika reads the numbers over SSH.
  import { Activity, Plus, Loader2, Search, BellRing, Settings2, Container as Box, ArrowDown, ArrowUp, CircleAlert, X, Thermometer } from '@lucide/svelte'
  import { monitor, errMsg } from '../lib/api'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys, queryClient } from '../lib/query'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { pct, rate, uptime, level, since, ruleLabel } from '../lib/monitor'
  import type { System, Available } from '../gen/timika/v1/monitor_pb'
  import MonitorSettings from '../components/MonitorSettings.svelte'

  const list = createQuery(() => ({ queryKey: keys.systems, queryFn: () => monitor.listSystems({}), refetchInterval: 5000 }))
  const systems = $derived<System[]>([...(list.data?.systems ?? [])].sort((a, b) => order(a) - order(b) || a.name.localeCompare(b.name)))
  const available = $derived<Available[]>(list.data?.available ?? [])
  const interval = $derived(list.data?.interval ?? 60)
  const loading = $derived(list.isPending)
  let actionError = $state('')
  // Keep showing the last numbers through a blip; say so only when there are none.
  const error = $derived(actionError || (list.error && !list.data ? errMsg(list.error) : ''))
  const load = () => list.refetch()
  let q = $state('')
  let adding = $state(false)
  let picked = $state<string[]>([])
  let busy = $state(false)
  let settings = $state(false)
  const canManage = isAdmin()

  const order = (s: System) => (s.status === 'down' ? 0 : s.firing.length ? 1 : s.status === 'pending' ? 3 : 2)

  async function add(ids: string[]) {
    busy = true
    try {
      await monitor.setMonitoring({ assets: ids, enabled: true })
      adding = false
      picked = []
      await load()
    } catch (e) { actionError = errMsg(e) } finally { busy = false }
  }

  const shown = $derived(systems.filter((s) => !q || `${s.name} ${s.host} ${s.tags.join(' ')}`.toLowerCase().includes(q.trim().toLowerCase())))
  const spark = (v: number[]) => {
    if (v.length < 2) return ''
    return v.map((y, i) => `${i ? 'L' : 'M'}${((i / (v.length - 1)) * 60).toFixed(1)},${(18 - (Math.min(y, 100) / 100) * 16).toFixed(1)}`).join('')
  }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Monitoring</div>
          <h1 class="page-title">Systems</h1>
          <p class="page-subtitle">CPU, memory, disk, network, containers and alerts for your servers. Nothing to install on them — timika reads the numbers over SSH every {interval} s.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{systems.length}</span><span class="page-metric__label">Systems</span></div>
          <div class="page-metric"><span class="page-metric__value mon-up">{systems.filter((s) => s.status === 'up').length}</span><span class="page-metric__label">Up</span></div>
          <div class="page-metric"><span class="page-metric__value" class:mon-down={systems.some((s) => s.status === 'down')}>{systems.filter((s) => s.status === 'down').length}</span><span class="page-metric__label">Down</span></div>
          <div class="page-metric"><span class="page-metric__value" class:mon-warn={systems.some((s) => s.firing.length)}>{systems.reduce((n, s) => n + s.firing.length, 0)}</span><span class="page-metric__label">Alerts</span></div>
        </div>
      </section>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if loading}
        <div class="empty-state"><Loader2 size={20} class="spin" /></div>
      {:else if !systems.length && !adding}
        <section class="page-card">
          <div class="empty-state mon-empty">
            <Activity size={30} />
            <div class="mon-empty__title">Watch your servers</div>
            <div>History, live numbers and alerts — using the SSH access timika already has. No agent, no open port.</div>
            {#if canManage && available.length}
              <button class="base-btn base-btn--primary" disabled={busy} onclick={() => add(available.map((a) => a.asset))}>{#if busy}<Loader2 size={14} class="spin" />{:else}<Plus size={14} />{/if} Monitor {available.length === 1 ? available[0].name : `all ${available.length} servers`}</button>
              {#if available.length > 1}<button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (adding = true)}>Choose servers…</button>{/if}
            {:else if canManage}
              <button class="base-btn base-btn--primary" onclick={() => navigate('/servers')}>Add a server first</button>
            {:else}
              <div class="muted">An administrator chooses which servers are monitored.</div>
            {/if}
          </div>
        </section>
      {:else}
        <div class="mon-bar">
          <div class="mon-search"><Search size={15} /><input bind:value={q} placeholder="Filter systems, hosts, tags…" /></div>
          {#if canManage}
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (settings = true)}><Settings2 size={13} /> Alerts & notifications</button>
            {#if available.length}<button class="base-btn base-btn--primary base-btn--sm" onclick={() => (adding = !adding)}><Plus size={13} /> Add servers <span class="mon-n">{available.length}</span></button>{/if}
          {/if}
        </div>

        {#if adding}
          <section class="page-card mon-add">
            <div class="mon-add__head"><b>Monitor more servers</b><button class="icon-btn" onclick={() => (adding = false)}><X size={14} /></button></div>
            <div class="mon-add__list">
              {#each available as a (a.asset)}
                <label class="chip" class:is-on={picked.includes(a.asset)}><input type="checkbox" value={a.asset} bind:group={picked} /> {a.name} <span class="muted mono">{a.host}</span></label>
              {/each}
            </div>
            <div class="mon-add__foot">
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={busy} onclick={() => add(available.map((a) => a.asset))}>Monitor all {available.length}</button>
              <button class="base-btn base-btn--primary base-btn--sm" disabled={busy || !picked.length} onclick={() => add(picked)}>{#if busy}<Loader2 size={13} class="spin" />{/if} Monitor {picked.length || ''} selected</button>
            </div>
          </section>
        {/if}

        <section class="page-card">
          <div class="data-table-wrap">
            <table class="data-table mon">
              <thead><tr><th>System</th><th>CPU</th><th>Memory</th><th>Disk</th><th>Network</th><th>Load</th><th>Up</th><th></th></tr></thead>
              <tbody>
                {#each shown as s (s.asset)}
                  {@const l = s.latest}
                  <tr class="mon__row" onclick={() => navigate(`/monitoring/${s.asset}`)}>
                    <td>
                      <div class="mon-sys">
                        <span class="mon-dot mon-dot--{s.status}" title={s.status === 'down' ? `down ${since(s.since)} — ${s.error}` : s.status}></span>
                        <div><div class="strong">{s.name}</div><div class="mono muted mon-host">{s.host}</div></div>
                      </div>
                    </td>
                    {#if s.status === 'down'}
                      <td colspan="5" class="mon-err"><CircleAlert size={13} /> down {since(s.since)} — {s.error}</td>
                    {:else if !l}
                      <td colspan="5" class="muted"><Loader2 size={12} class="spin" /> first reading…</td>
                    {:else}
                      <td>
                        <div class="mon-cell">
                          <div class="mon-meter"><i class="mon-fill mon-fill--{level(l.cpu)}" style="width:{l.cpu ?? 0}%"></i></div><span class="mono mon-val">{pct(l.cpu)}</span>
                          <svg class="mon-spark" width="60" height="20"><path d={spark(s.spark)} /></svg>
                        </div>
                      </td>
                      <td><div class="mon-cell"><div class="mon-meter"><i class="mon-fill mon-fill--{level(l.mem)}" style="width:{l.mem ?? 0}%"></i></div><span class="mono mon-val">{pct(l.mem)}</span></div></td>
                      <td><div class="mon-cell"><div class="mon-meter"><i class="mon-fill mon-fill--{level(l.disk)}" style="width:{l.disk ?? 0}%"></i></div><span class="mono mon-val">{pct(l.disk)}</span></div></td>
                      <td class="mono mon-net"><ArrowDown size={11} /> {rate(l.rx)} <ArrowUp size={11} /> {rate(l.tx)}</td>
                      <td class="mono">{l.load1?.toFixed(2) ?? '—'}{#if l.temp} <span class="muted mon-temp"><Thermometer size={11} />{l.temp.toFixed(0)}°</span>{/if}</td>
                    {/if}
                    <td class="muted">{l && s.status !== 'down' ? uptime(l.uptime) : ''}</td>
                    <td class="mon-flags">
                      {#each s.firing as f (f.metric)}<span class="badge badge--warning" title="since {new Date(f.since).toLocaleString()}"><BellRing size={11} /> {ruleLabel(f.metric)}</span>{/each}
                      {#if s.failedUnits}<span class="badge badge--danger" title="failed systemd units">{s.failedUnits} failed</span>{/if}
                      {#if s.containersTotal}<span class="badge badge--default" title="containers running / all"><Box size={11} /> {s.containersRunning}/{s.containersTotal}</span>{/if}
                    </td>
                  </tr>
                {:else}
                  <tr><td colspan="8"><div class="empty-state">No system matches “{q}”.</div></td></tr>
                {/each}
              </tbody>
            </table>
          </div>
        </section>
      {/if}
    </div>
  </div>
</div>

{#if settings}<MonitorSettings onclose={() => (settings = false)} />{/if}

<style>
  .mon-up { color: var(--success); }
  .mon-down { color: var(--danger); }
  .mon-warn { color: var(--warning); }
  .mon-bar { display: flex; gap: 10px; align-items: center; }
  .mon-search { flex: 1; display: flex; align-items: center; gap: 8px; padding: 0 12px; height: 34px; background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); color: var(--text-muted); }
  .mon-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .mon-n { font-size: 10.5px; padding: 0 6px; border-radius: 10px; background: rgba(255, 255, 255, 0.25); }
  .mon__row { cursor: pointer; }
  .mon-sys { display: flex; align-items: center; gap: 10px; }
  .mon-host { font-size: 11px; }
  .mon-dot { width: 9px; height: 9px; border-radius: 50%; flex: 0 0 auto; background: var(--text-muted); }
  .mon-dot--up { background: var(--success); box-shadow: 0 0 0 3px var(--success-bg); }
  .mon-dot--down { background: var(--danger); box-shadow: 0 0 0 3px var(--danger-bg); }
  .mon-cell { display: flex; align-items: center; gap: 8px; }
  .mon-meter { width: 74px; height: 6px; border-radius: 3px; background: var(--bg-elevated); overflow: hidden; flex: 0 0 auto; }
  .mon-fill { display: block; height: 100%; border-radius: 3px; background: var(--success); transition: width .4s var(--ease); }
  .mon-fill--warn { background: var(--warning); }
  .mon-fill--bad { background: var(--danger); }
  .mon-val { width: 40px; font-size: 12px; }
  .mon-spark path { fill: none; stroke: var(--brand); stroke-width: 1.3; }
  .mon-net { font-size: 11.5px; white-space: nowrap; }
  .mon-net :global(svg), .mon-temp :global(svg), .mon-err :global(svg) { vertical-align: -2px; }
  .mon-temp { margin-left: 6px; font-size: 11px; }
  .mon-err { color: var(--danger); font-size: 12px; }
  .mon-flags { text-align: right; white-space: nowrap; }
  .mon-flags .badge { margin-left: 4px; }
  .mon-flags :global(svg) { vertical-align: -1px; }
  .mon-empty { gap: 10px; padding: 36px 20px; }
  .mon-empty__title { font-size: 15px; font-weight: 700; color: var(--text-primary); }
  .mon-add { padding: 14px; display: flex; flex-direction: column; gap: 10px; }
  .mon-add__head { display: flex; justify-content: space-between; align-items: center; font-size: 13px; }
  .mon-add__list { display: flex; flex-wrap: wrap; gap: 6px; }
  .mon-add__foot { display: flex; justify-content: flex-end; gap: 8px; }
  .chip { display: inline-flex; align-items: center; gap: 6px; padding: 4px 10px; border-radius: 14px; border: 1px solid var(--border); cursor: pointer; font-size: 12px; }
  .chip input { display: none; }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
</style>
