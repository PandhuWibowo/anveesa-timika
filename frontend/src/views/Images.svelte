<script lang="ts">
  // Container images (Docker or Podman) on every monitored server: size, what uses them, clean up.
  import { Layers, Loader2, Trash2, Search, Eraser, RefreshCw } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { containerApi, monitor, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { size } from '../lib/monitor'
  import type { Image } from '../gen/timika/v1/containers_pb'

  type Row = { asset: string; system: string; image: Image }
  // One request per server; a server without Docker just has none.
  const list = createQuery(() => ({
    queryKey: ['docker', 'images-all'],
    queryFn: async () => {
      const systems = (await monitor.listSystems({})).systems.filter((s) => s.status === 'up')
      const per = await Promise.all(systems.map(async (s) => {
        try { return { system: s.name, asset: s.asset, images: (await containerApi.listImages({ asset: s.asset })).images, error: '' } }
        catch (e) { return { system: s.name, asset: s.asset, images: [] as Image[], error: errMsg(e) } }
      }))
      return per
    },
    staleTime: 15_000,
  }))
  const rows = $derived<Row[]>((list.data ?? []).flatMap((p) => p.images.map((image) => ({ asset: p.asset, system: p.system, image }))))
  const servers = $derived(list.data ?? [])
  let q = $state('')
  let only = $state('')
  let busy = $state('')
  let note = $state<{ ok: boolean; text: string } | null>(null)

  const shown = $derived(rows.filter((r) =>
    (!only || (only === 'unused' ? r.image.usedBy.length === 0 : r.image.dangling)) &&
    (!q || `${r.image.repository}:${r.image.tag} ${r.image.id} ${r.system}`.toLowerCase().includes(q.trim().toLowerCase()))))
  const total = $derived(rows.reduce((n, r) => n + Number(r.image.size), 0))
  const label = (i: Image) => (i.dangling ? '<untagged>' : `${i.repository}:${i.tag}`)

  async function remove(r: Row) {
    const ok = await confirm({ title: `Remove ${label(r.image)}?`, message: `From ${r.system} (${size(r.image.size)}). ${r.image.usedBy.length ? `It will be refused while ${r.image.usedBy.join(', ')} use${r.image.usedBy.length === 1 ? 's' : ''} it.` : 'No container uses it.'}`, confirmText: 'Remove', variant: 'danger' })
    if (!ok) return
    busy = r.asset + r.image.id
    try {
      const res = await containerApi.removeImage({ asset: r.asset, image: r.image.dangling ? r.image.id : label(r.image), runtime: r.image.runtime })
      note = { ok: res.ok, text: res.ok ? `Removed ${label(r.image)} from ${r.system}.` : res.output }
      list.refetch()
    } catch (e) { note = { ok: false, text: errMsg(e) } } finally { busy = '' }
  }
  async function prune(asset: string, system: string) {
    const ok = await confirm({ title: `Remove untagged images on ${system}?`, message: 'Removes dangling images (left over from builds and pulls) that no container uses.', confirmText: 'Clean up', variant: 'warning' })
    if (!ok) return
    busy = 'prune' + asset
    try {
      const res = await containerApi.pruneImages({ asset })
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
          <h1 class="page-title">Images</h1>
          <p class="page-subtitle">Images on your monitored servers: how big they are, which containers use them, and what can go.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{rows.length}</span><span class="page-metric__label">Images</span></div>
          <div class="page-metric"><span class="page-metric__value">{size(total)}</span><span class="page-metric__label">On disk</span></div>
          <div class="page-metric"><span class="page-metric__value">{rows.filter((r) => !r.image.usedBy.length).length}</span><span class="page-metric__label">Unused</span></div>
        </div>
      </section>

      {#if list.error}<div class="notice notice--error">{errMsg(list.error)}</div>{/if}
      {#if note}<div class="notice notice--{note.ok ? 'success' : 'error'} dk-note">{note.text}</div>{/if}
      {#each servers.filter((s) => s.error) as s (s.asset)}<div class="notice notice--warning">{s.system}: {s.error}</div>{/each}

      <div class="dk-bar">
        <div class="dk-search"><Search size={15} /><input bind:value={q} placeholder="Filter by name, tag, id, server…" /></div>
        <div class="seg">{#each [['', 'All'], ['unused', 'Unused'], ['dangling', 'Untagged']] as [id, l] (id)}<button class:is-on={only === id} onclick={() => (only = id)}>{l}</button>{/each}</div>
        {#each servers.filter((s) => s.images.some((i) => i.dangling)) as s (s.asset)}
          <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!busy} onclick={() => prune(s.asset, s.system)}>{#if busy === 'prune' + s.asset}<Loader2 size={13} class="spin" />{:else}<Eraser size={13} />{/if} Clean up {s.system}</button>
        {/each}
        <button class="icon-btn" title="Refresh" onclick={() => list.refetch()}>{#if list.isFetching}<Loader2 size={14} class="spin" />{:else}<RefreshCw size={14} />{/if}</button>
      </div>

      <section class="page-card">
        {#if list.isPending}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <div class="data-table-wrap">
            <table class="data-table">
              <thead><tr><th>Image</th><th>Server</th><th>Id</th><th class="dk-num">Size</th><th>Created</th><th>Used by</th><th></th></tr></thead>
              <tbody>
                {#each shown as r (r.asset + r.image.id + r.image.tag)}
                  <tr>
                    <td><span class="dk-name"><Layers size={14} /><span class="mono" class:strong={!r.image.dangling} class:muted={r.image.dangling}>{label(r.image)}</span></span></td>
                    <td>{r.system} <span class="badge badge--default dk-rt">{r.image.runtime}</span></td>
                    <td class="mono muted">{r.image.id}</td>
                    <td class="mono dk-num">{size(r.image.size)}</td>
                    <td class="muted">{r.image.created}</td>
                    <td>{#each r.image.usedBy as c (c)}<span class="badge badge--success dk-use">{c}</span>{:else}<span class="muted">unused</span>{/each}</td>
                    <td class="dk-act"><button class="icon-btn" title="Remove" disabled={!!busy} onclick={() => remove(r)}>{#if busy === r.asset + r.image.id}<Loader2 size={14} class="spin" />{:else}<Trash2 size={14} />{/if}</button></td>
                  </tr>
                {:else}
                  <tr><td colspan="7"><div class="empty-state">{rows.length ? 'Nothing matches.' : 'No images found on your monitored servers.'}</div></td></tr>
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
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 6px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .dk-name { display: inline-flex; align-items: center; gap: 8px; }
  .dk-name :global(svg) { color: var(--text-muted); flex: 0 0 auto; }
  .dk-num { text-align: right; }
  .dk-use { margin-right: 4px; }
  .dk-act { text-align: right; }
  .dk-rt { margin-left: 4px; font-size: 10px; }
  .dk-note { white-space: pre-wrap; }
</style>
