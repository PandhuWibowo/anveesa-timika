<script lang="ts">
  // Container volumes (Docker or Podman) on every monitored server: size, what uses them, clean up.
  import { Database, Loader2, Trash2, Search, Eraser, RefreshCw } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { containerApi, monitor, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { navigate } from '../lib/router.svelte'
  import { size } from '../lib/monitor'
  import type { Volume } from '../gen/timika/v1/containers_pb'

  type Row = { asset: string; system: string; volume: Volume }
  const list = createQuery(() => ({
    queryKey: ['docker', 'volumes-all'],
    queryFn: async () => {
      const systems = (await monitor.listSystems({})).systems.filter((s) => s.status === 'up')
      return Promise.all(systems.map(async (s) => {
        try { return { system: s.name, asset: s.asset, volumes: (await containerApi.listVolumes({ asset: s.asset })).volumes, error: '' } }
        catch (e) { return { system: s.name, asset: s.asset, volumes: [] as Volume[], error: errMsg(e) } }
      }))
    },
    staleTime: 15_000,
  }))
  const servers = $derived(list.data ?? [])
  const rows = $derived<Row[]>(servers.flatMap((p) => p.volumes.map((volume) => ({ asset: p.asset, system: p.system, volume }))))
  let q = $state('')
  let unused = $state(false)
  let busy = $state('')
  let note = $state<{ ok: boolean; text: string } | null>(null)

  const shown = $derived(rows.filter((r) => (!unused || !r.volume.usedBy.length) && (!q || `${r.volume.name} ${r.system} ${r.volume.usedBy.join(' ')}`.toLowerCase().includes(q.trim().toLowerCase()))))
  const total = $derived(rows.reduce((n, r) => n + Number(r.volume.size ?? 0), 0))
  const short = (n: string) => (/^[0-9a-f]{64}$/.test(n) ? `${n.slice(0, 12)}… (anonymous)` : n)

  async function remove(r: Row) {
    const ok = await confirm({ title: `Remove volume ${short(r.volume.name)}?`, message: `From ${r.system}. Its data is deleted for good.${r.volume.usedBy.length ? ` It will be refused while ${r.volume.usedBy.join(', ')} use${r.volume.usedBy.length === 1 ? 's' : ''} it.` : ''}`, confirmText: 'Remove', variant: 'danger' })
    if (!ok) return
    busy = r.asset + r.volume.name
    try {
      const res = await containerApi.removeVolume({ asset: r.asset, name: r.volume.name, runtime: r.volume.runtime })
      note = { ok: res.ok, text: res.ok ? `Removed ${short(r.volume.name)} from ${r.system}.` : res.output }
      list.refetch()
    } catch (e) { note = { ok: false, text: errMsg(e) } } finally { busy = '' }
  }
  async function prune(asset: string, system: string) {
    const ok = await confirm({ title: `Remove unused volumes on ${system}?`, message: 'Deletes volumes no container uses — and the data in them. This cannot be undone.', confirmText: 'Remove unused', variant: 'danger' })
    if (!ok) return
    busy = 'prune' + asset
    try {
      const res = await containerApi.pruneVolumes({ asset })
      note = { ok: res.ok, text: `${system}: ${res.output.split('\n').pop() || 'done'}` }
      list.refetch()
    } catch (e) { note = { ok: false, text: errMsg(e) } } finally { busy = '' }
  }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Containers</div>
          <h1 class="page-title">Volumes</h1>
          <p class="page-subtitle">Where containers keep data that must survive them: how big each volume is and which containers mount it. To look inside one, open a container that mounts it → Files.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{rows.length}</span><span class="page-metric__label">Volumes</span></div>
          <div class="page-metric"><span class="page-metric__value">{size(total)}</span><span class="page-metric__label">On disk</span></div>
          <div class="page-metric"><span class="page-metric__value">{rows.filter((r) => !r.volume.usedBy.length).length}</span><span class="page-metric__label">Unused</span></div>
        </div>
      </section>

      {#if list.error}<div class="notice notice--error">{errMsg(list.error)}</div>{/if}
      {#if note}<div class="notice notice--{note.ok ? 'success' : 'error'} dk-note">{note.text}</div>{/if}
      {#each servers.filter((s) => s.error) as s (s.asset)}<div class="notice notice--warning">{s.system}: {s.error}</div>{/each}

      <div class="dk-bar">
        <div class="dk-search"><Search size={15} /><input bind:value={q} placeholder="Filter by name, server, container…" /></div>
        <label class="dk-check"><input type="checkbox" bind:checked={unused} /> unused only</label>
        {#each servers.filter((s) => s.volumes.some((v) => !v.usedBy.length)) as s (s.asset)}
          <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!busy} onclick={() => prune(s.asset, s.system)}>{#if busy === 'prune' + s.asset}<Loader2 size={13} class="spin" />{:else}<Eraser size={13} />{/if} Remove unused on {s.system}</button>
        {/each}
        <button class="icon-btn" title="Refresh" onclick={() => list.refetch()}>{#if list.isFetching}<Loader2 size={14} class="spin" />{:else}<RefreshCw size={14} />{/if}</button>
      </div>

      <section class="page-card">
        {#if list.isPending}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <div class="data-table-wrap">
            <table class="data-table">
              <thead><tr><th>Volume</th><th>Server</th><th>Driver</th><th class="dk-num">Size</th><th>Used by</th><th>On the server</th><th></th></tr></thead>
              <tbody>
                {#each shown as r (r.asset + r.volume.name)}
                  <tr>
                    <td><span class="dk-name"><Database size={14} /><span class="mono strong" title={r.volume.name}>{short(r.volume.name)}</span></span></td>
                    <td>{r.system} <span class="badge badge--default dk-rt">{r.volume.runtime}</span></td>
                    <td class="muted">{r.volume.driver}</td>
                    <td class="mono dk-num">{r.volume.size !== undefined ? size(r.volume.size) : '—'}</td>
                    <td>{#each r.volume.usedBy as c (c)}<button class="badge badge--success dk-use dk-link" onclick={() => navigate(`/containers/${r.asset}/${c}`)}>{c}</button>{:else}<span class="muted">unused</span>{/each}</td>
                    <td class="mono muted dk-path" title={r.volume.mountpoint}>{r.volume.mountpoint}</td>
                    <td class="dk-act"><button class="icon-btn" title="Remove" disabled={!!busy} onclick={() => remove(r)}>{#if busy === r.asset + r.volume.name}<Loader2 size={14} class="spin" />{:else}<Trash2 size={14} />{/if}</button></td>
                  </tr>
                {:else}
                  <tr><td colspan="7"><div class="empty-state">{rows.length ? 'Nothing matches.' : 'No volumes on your monitored servers.'}</div></td></tr>
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
  .dk-bar { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .dk-search { flex: 1; min-width: 220px; display: flex; align-items: center; gap: 8px; padding: 0 12px; height: 34px; background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); color: var(--text-muted); }
  .dk-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .dk-check { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--text-secondary); cursor: pointer; }
  .dk-name { display: inline-flex; align-items: center; gap: 8px; }
  .dk-name :global(svg) { color: var(--text-muted); flex: 0 0 auto; }
  .dk-num { text-align: right; }
  .dk-use { margin-right: 4px; }
  .dk-link { border: 0; cursor: pointer; font-family: inherit; }
  .dk-path { max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11.5px; }
  .dk-act { text-align: right; }
  .dk-rt { margin-left: 4px; font-size: 10px; }
  .dk-note { white-space: pre-wrap; }
</style>
