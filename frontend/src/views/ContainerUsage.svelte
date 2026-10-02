<script lang="ts">
  // What every container uses right now: CPU, memory, disk, network, disk I/O
  // and processes, across all monitored servers. Click a column to sort.
  import { Gauge, Search, Loader2, ArrowDown, ArrowUp, ChevronDown, ChevronUp } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { monitor, errMsg } from '../lib/api'
  import { navigate } from '../lib/router.svelte'
  import { pct, rate, size, level } from '../lib/monitor'
  import type { ContainerRow } from '../gen/timika/v1/monitor_pb'

  const list = createQuery(() => ({ queryKey: keys.containers, queryFn: () => monitor.listContainers({}), refetchInterval: 10000 }))
  const rows = $derived<ContainerRow[]>(list.data?.containers ?? [])
  const error = $derived(list.error && !list.data ? errMsg(list.error) : '')
  let q = $state('')
  let server = $state('')
  let all = $state(false)
  let sort = $state('cpu')
  let asc = $state(false)

  const memPct = (r: ContainerRow) => (Number(r.container!.memLimit) ? (Number(r.container!.mem) / Number(r.container!.memLimit)) * 100 : undefined)
  const COLS: { id: string; label: string; of: (r: ContainerRow) => number | string | undefined }[] = [
    { id: 'name', label: 'Container', of: (r) => r.container!.name },
    { id: 'server', label: 'Server', of: (r) => r.system },
    { id: 'cpu', label: 'CPU', of: (r) => r.container!.cpu },
    { id: 'mem', label: 'Memory', of: (r) => Number(r.container!.mem) || undefined },
    { id: 'disk', label: 'Disk', of: (r) => Number(r.container!.sizeRw) || undefined },
    { id: 'net', label: 'Network', of: (r) => (r.container!.rx === undefined ? undefined : r.container!.rx + (r.container!.tx ?? 0)) },
    { id: 'io', label: 'Disk I/O', of: (r) => (r.container!.ioRead === undefined ? undefined : r.container!.ioRead + (r.container!.ioWrite ?? 0)) },
    { id: 'pids', label: 'Processes', of: (r) => r.container!.pids },
  ]
  function by(id: string) {
    if (sort === id) asc = !asc
    else { sort = id; asc = id === 'name' || id === 'server' }
  }

  const servers = $derived([...new Set(rows.map((r) => r.system))].sort())
  const running = $derived(rows.filter((r) => r.container!.state === 'running'))
  const shown = $derived.by(() => {
    const of = COLS.find((c) => c.id === sort)!.of
    const text = q.trim().toLowerCase()
    return rows
      .filter((r) => (all || r.container!.state === 'running') && (!server || r.system === server) && (!text || `${r.container!.name} ${r.container!.image} ${r.system}`.toLowerCase().includes(text)))
      .sort((a, b) => {
        const x = of(a), y = of(b)
        // Containers without a number always go last.
        if (x === undefined || y === undefined) return Number(x === undefined) - Number(y === undefined) || a.container!.name.localeCompare(b.container!.name)
        const d = typeof x === 'string' ? x.localeCompare(y as string) : x - (y as number)
        return (asc ? d : -d) || a.container!.name.localeCompare(b.container!.name)
      })
  })
  const sum = (f: (r: ContainerRow) => number | undefined) => running.reduce((n, r) => n + (f(r) ?? 0), 0)
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Monitoring</div>
          <h1 class="page-title">Containers</h1>
          <p class="page-subtitle">What every container uses — CPU, memory, disk, network, disk I/O and processes — on all your monitored servers, Docker or Podman. Open one for its history.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{running.length}<span class="cu-of"> / {rows.length}</span></span><span class="page-metric__label">Running</span></div>
          <div class="page-metric"><span class="page-metric__value">{pct(sum((r) => r.container!.cpu))}</span><span class="page-metric__label">CPU (of one core)</span></div>
          <div class="page-metric"><span class="page-metric__value">{size(sum((r) => Number(r.container!.mem)))}</span><span class="page-metric__label">Memory</span></div>
          <div class="page-metric"><span class="page-metric__value">{size(rows.reduce((n, r) => n + Number(r.container!.sizeRw), 0))}</span><span class="page-metric__label">Disk written</span></div>
        </div>
      </section>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      <div class="cu-bar">
        <div class="cu-search"><Search size={15} /><input bind:value={q} placeholder="Filter by name, image, server…" /></div>
        {#if servers.length > 1}
          <select class="base-input cu-select" bind:value={server} aria-label="Server">
            <option value="">All servers</option>
            {#each servers as s (s)}<option value={s}>{s}</option>{/each}
          </select>
        {/if}
        <div class="seg">
          <button class:is-on={!all} onclick={() => (all = false)}>Running</button>
          <button class:is-on={all} onclick={() => (all = true)}>All</button>
        </div>
      </div>

      <section class="page-card">
        {#if list.isPending}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <div class="data-table-wrap">
            <table class="data-table">
              <thead><tr>
                {#each COLS as c (c.id)}
                  <th><button class="cu-th" class:is-on={sort === c.id} onclick={() => by(c.id)}>{c.label}{#if sort === c.id}{#if asc}<ChevronUp size={12} />{:else}<ChevronDown size={12} />{/if}{/if}</button></th>
                {/each}
              </tr></thead>
              <tbody>
                {#each shown as r (r.asset + '/' + r.container!.name)}
                  {@const c = r.container!}
                  {@const mp = memPct(r)}
                  <tr class="cu-row" class:cu--off={c.state !== 'running'} onclick={() => navigate(`/usage/${r.asset}/${c.name}`)}>
                    <td><span class="cu-name"><i class="cu-dot cu-dot--{c.state}"></i><span class="mono strong">{c.name}</span>{#if c.health === 'unhealthy'}<span class="badge badge--danger">unhealthy</span>{/if}</span><div class="cu-sub mono" title={c.image}>{c.image}</div></td>
                    <td>{r.system}<div class="cu-sub">{c.runtime} · {c.status || c.state}</div></td>
                    <td class="cu-num">
                      {#if c.cpu !== undefined}<span class="mono cu-lv--{level(c.cpu)}">{pct(c.cpu)}</span><div class="cu-meter"><i class="cu-fill cu-fill--{level(c.cpu)}" style="width:{Math.min(c.cpu, 100)}%"></i></div>{:else}<span class="muted">—</span>{/if}
                    </td>
                    <td class="cu-num">
                      {#if Number(c.mem)}<span class="mono">{size(c.mem)}</span>{#if mp !== undefined}<span class="cu-sub cu-inline"> of {size(c.memLimit)} · {pct(mp)}</span><div class="cu-meter"><i class="cu-fill cu-fill--{level(mp)}" style="width:{Math.min(mp, 100)}%"></i></div>{/if}{:else}<span class="muted">—</span>{/if}
                    </td>
                    <td class="mono" title="Written on top of the image · with the image">{#if Number(c.sizeTotal)}{size(c.sizeRw)}<div class="cu-sub">image {size(Number(c.sizeTotal) - Number(c.sizeRw))}</div>{:else}<span class="muted">—</span>{/if}</td>
                    <td class="mono cu-rate">{#if c.rx !== undefined}<ArrowDown size={11} /> {rate(c.rx)}<br /><ArrowUp size={11} /> {rate(c.tx)}{:else}<span class="muted">—</span>{/if}</td>
                    <td class="mono cu-rate">{#if c.ioRead !== undefined}R {rate(c.ioRead)}<br />W {rate(c.ioWrite)}{:else}<span class="muted">—</span>{/if}</td>
                    <td class="mono">{c.pids ?? '—'}</td>
                  </tr>
                {:else}
                  <tr><td colspan={COLS.length}><div class="empty-state cu-empty">
                    <Gauge size={26} />
                    {#if rows.length}Nothing matches.{:else}No containers found. Turn on monitoring for a server that runs Docker or Podman under <button class="linkish" onclick={() => navigate('/monitoring')}>Systems</button>; the monitoring account must be allowed to run <code>docker</code> / <code>podman</code>.{/if}
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

<style>
  .cu-of { font-size: 13px; font-weight: 400; color: var(--text-muted); }
  .cu-bar { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .cu-search { flex: 1; min-width: 220px; display: flex; align-items: center; gap: 8px; padding: 0 12px; height: 34px; background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); color: var(--text-muted); }
  .cu-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .cu-select { width: auto; height: 34px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 6px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .cu-th { display: inline-flex; align-items: center; gap: 3px; border: 0; background: none; padding: 0; font: inherit; color: inherit; text-transform: inherit; letter-spacing: inherit; cursor: pointer; }
  .cu-th.is-on { color: var(--brand); }
  .cu-row { cursor: pointer; }
  .cu-row td { vertical-align: top; }
  .cu--off { opacity: 0.65; }
  .cu-name { display: inline-flex; align-items: center; gap: 8px; }
  .cu-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-muted); flex: 0 0 auto; }
  .cu-dot--running { background: var(--success); }
  .cu-dot--restarting, .cu-dot--paused { background: var(--warning); }
  .cu-dot--dead { background: var(--danger); }
  .cu-sub { font-size: 11px; color: var(--text-muted); max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cu-inline { display: inline; }
  .cu-num { min-width: 120px; }
  .cu-meter { height: 4px; border-radius: 2px; background: var(--bg-elevated); overflow: hidden; margin-top: 4px; max-width: 150px; }
  .cu-fill { display: block; height: 100%; background: var(--success); }
  .cu-fill--warn { background: var(--warning); }
  .cu-fill--bad { background: var(--danger); }
  .cu-lv--warn { color: var(--warning); }
  .cu-lv--bad { color: var(--danger); }
  .cu-rate { font-size: 11.5px; white-space: nowrap; }
  .cu-rate :global(svg) { vertical-align: -2px; }
  .cu-empty { gap: 8px; padding: 30px 20px; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; }
</style>
