<script lang="ts">
  // Files inside a container: browse, download a file or a whole folder (.tar),
  // upload files or a folder, new folder, delete. All through `docker` / `podman`
  // on the server — nothing is installed in the container.
  import { Folder, FileText, FileSymlink, Home, ChevronRight, Loader2, Download, Upload, FolderUp, FolderPlus, Trash2, RefreshCw, X, Check } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { containerApi, errMsg } from '../lib/api'
  import { session } from '../lib/session.svelte'
  import { confirm } from '../lib/ui.svelte'
  import { navigate } from '../lib/router.svelte'
  import { size } from '../lib/monitor'
  import type { ContainerFile } from '../gen/timika/v1/containers_pb'

  let { asset, name, path = '/' }: { asset: string; name: string; path?: string } = $props()

  const list = createQuery(() => ({ queryKey: keys.containerFiles(asset, name, path), queryFn: () => containerApi.listFiles({ asset, name, path }), retry: false }))
  const entries = $derived<ContainerFile[]>(list.data?.entries ?? [])
  let actionError = $state('')
  const error = $derived(actionError || (list.error ? errMsg(list.error) : ''))
  let picked = $state<string[]>([])
  let busy = $state('')
  let newFolder = $state<string | null>(null)
  let dragging = $state(0)
  let fileInput = $state<HTMLInputElement>()
  let dirInput = $state<HTMLInputElement>()

  type Up = { id: number; name: string; size: number; sent: number; state: 'up' | 'done' | 'error'; error?: string }
  let ups = $state<Up[]>([])
  let upSeq = 0

  const crumbs = $derived(path.split('/').filter(Boolean))
  const join = (dir: string, n: string) => (dir.endsWith('/') ? dir + n : `${dir}/${n}`)
  const go = (p: string) => { picked = []; actionError = ''; navigate(`/containers/${asset}/${name}/files${p === '/' ? '' : p}`) }
  $effect(() => { path; picked = [] })

  async function download(p: string) {
    busy = `dl:${p}`
    try {
      const link = await containerApi.downloadLink({ asset, name, path: p })
      const a = document.createElement('a')
      a.href = link.url
      a.download = link.name
      document.body.appendChild(a)
      a.click()
      a.remove()
      actionError = ''
    } catch (e) { actionError = errMsg(e) } finally { busy = '' }
  }

  async function mkdir() {
    const n = (newFolder ?? '').trim()
    if (!n) return
    busy = 'mkdir'
    try { await containerApi.makeDir({ asset, name, path: join(path, n) }); newFolder = null; actionError = ''; list.refetch() } catch (e) { actionError = errMsg(e) } finally { busy = '' }
  }

  async function remove() {
    const ok = await confirm({ title: `Delete ${picked.length === 1 ? picked[0] : `${picked.length} items`}?`, message: `Inside container ${name}. Folders are deleted with everything in them. This cannot be undone.`, confirmText: 'Delete', variant: 'danger' })
    if (!ok) return
    busy = 'rm'
    try { await containerApi.deleteFiles({ asset, name, paths: picked.map((n) => join(path, n)) }); picked = []; actionError = ''; list.refetch() } catch (e) { actionError = errMsg(e) } finally { busy = '' }
  }

  /** PUT each file (a folder upload keeps its relative paths). */
  function upload(files: FileList | File[]) {
    for (const f of Array.from(files)) {
      const rel = (f as File & { webkitRelativePath?: string }).webkitRelativePath || f.name
      const u: Up = { id: ++upSeq, name: rel, size: f.size, sent: 0, state: 'up' }
      ups.push(u)
      const row = ups[ups.length - 1]
      const xhr = new XMLHttpRequest()
      const q = new URLSearchParams({ asset, container: name, path: join(path, rel) })
      xhr.open('PUT', `/v1/containers/upload?${q}`)
      if (session.token) xhr.setRequestHeader('x-timika-token', session.token)
      xhr.upload.onprogress = (e) => { row.sent = e.loaded }
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) { row.state = 'done'; row.sent = f.size; list.refetch() }
        else { row.state = 'error'; try { row.error = JSON.parse(xhr.responseText).error } catch { row.error = `HTTP ${xhr.status}` } }
      }
      xhr.onerror = () => { row.state = 'error'; row.error = "can't reach the server" }
      xhr.send(f)
    }
  }

  const ico = (k: string) => (k === 'dir' ? Folder : k === 'link' ? FileSymlink : FileText)
  const when = (t: bigint) => (Number(t) ? new Date(Number(t) * 1000).toLocaleString([], { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '')
  const allPicked = $derived(entries.length > 0 && picked.length === entries.length)
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="cf" class:cf--drag={dragging > 0}
     ondragenter={(e) => { if (e.dataTransfer?.types.includes('Files')) { e.preventDefault(); dragging++ } }}
     ondragover={(e) => { if (e.dataTransfer?.types.includes('Files')) e.preventDefault() }}
     ondragleave={() => (dragging = Math.max(0, dragging - 1))}
     ondrop={(e) => { e.preventDefault(); dragging = 0; if (e.dataTransfer?.files.length) upload(e.dataTransfer.files) }}>
  <div class="cf-bar">
    <nav class="cf-crumbs">
      <button class="cf-crumb" title="/" onclick={() => go('/')}><Home size={13} /></button>
      {#each crumbs as c, i (i)}
        <ChevronRight size={12} class="cf-sep" />
        <button class="cf-crumb mono" class:is-last={i === crumbs.length - 1} onclick={() => go('/' + crumbs.slice(0, i + 1).join('/'))}>{c}</button>
      {/each}
    </nav>
    <div class="cf-tools">
      {#if picked.length}
        <button class="base-btn base-btn--danger base-btn--sm" disabled={!!busy} onclick={remove}><Trash2 size={13} /> Delete {picked.length}</button>
      {/if}
      <button class="base-btn base-btn--ghost base-btn--sm" title="Download this folder as .tar" disabled={!!busy} onclick={() => download(path)}>{#if busy === `dl:${path}`}<Loader2 size={13} class="spin" />{:else}<Download size={13} />{/if} Folder</button>
      <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (newFolder = '')}><FolderPlus size={13} /> New folder</button>
      <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => dirInput?.click()} title="Upload a folder with everything in it"><FolderUp size={13} /> Upload folder</button>
      <button class="base-btn base-btn--primary base-btn--sm" onclick={() => fileInput?.click()}><Upload size={13} /> Upload</button>
      <button class="icon-btn" title="Refresh" onclick={() => list.refetch()}>{#if list.isFetching}<Loader2 size={14} class="spin" />{:else}<RefreshCw size={14} />{/if}</button>
      <input bind:this={fileInput} type="file" multiple hidden onchange={(e) => { if (e.currentTarget.files) upload(e.currentTarget.files); e.currentTarget.value = '' }} />
      <input bind:this={dirInput} type="file" webkitdirectory hidden onchange={(e) => { if (e.currentTarget.files) upload(e.currentTarget.files); e.currentTarget.value = '' }} />
    </div>
  </div>

  {#if newFolder !== null}
    <form class="cf-new" onsubmit={(e) => { e.preventDefault(); mkdir() }}>
      <FolderPlus size={14} />
      <!-- svelte-ignore a11y_autofocus -->
      <input class="base-input mono" bind:value={newFolder} placeholder="folder name" autofocus />
      <button class="base-btn base-btn--primary base-btn--sm" disabled={!newFolder.trim() || busy === 'mkdir'}>Create</button>
      <button type="button" class="base-btn base-btn--ghost base-btn--sm" onclick={() => (newFolder = null)}>Cancel</button>
    </form>
  {/if}

  {#if ups.length}
    <div class="cf-ups">
      {#each ups as u (u.id)}
        <div class="cf-up" class:cf-up--err={u.state === 'error'}>
          <span class="mono cf-up__name">{u.name}</span>
          {#if u.state === 'up'}<div class="cf-up__bar"><i style="width:{u.size ? (u.sent / u.size) * 100 : 0}%"></i></div><span class="muted">{size(u.sent)} / {size(u.size)}</span>
          {:else if u.state === 'done'}<span class="cf-ok"><Check size={12} /> {size(u.size)}</span>
          {:else}<span class="cf-err">{u.error}</span>{/if}
        </div>
      {/each}
      {#if ups.every((u) => u.state !== 'up')}<button class="icon-btn cf-ups__x" title="Clear" onclick={() => (ups = [])}><X size={13} /></button>{/if}
    </div>
  {/if}

  {#if error}<div class="notice notice--error cf-msg">{error}</div>{/if}

  {#if list.isPending}
    <div class="empty-state"><Loader2 size={18} class="spin" /></div>
  {:else if !list.error}
    <div class="data-table-wrap">
      <table class="data-table">
        <thead><tr>
          <th class="cf-check"><input type="checkbox" checked={allPicked} onchange={() => (picked = allPicked ? [] : entries.map((e) => e.name))} /></th>
          <th>Name</th><th class="cf-num">Size</th><th>Modified</th><th>Mode</th><th></th>
        </tr></thead>
        <tbody>
          {#each entries as e (e.name)}
            {@const I = ico(e.kind)}
            <tr class:cf-row={e.kind === 'dir'}>
              <td class="cf-check"><input type="checkbox" value={e.name} bind:group={picked} /></td>
              <td>
                {#if e.kind === 'dir'}
                  <button class="cf-name" onclick={() => go(join(path, e.name))}><I size={14} class="cf-dir" /> <span class="mono strong">{e.name}</span></button>
                {:else}
                  <span class="cf-name"><I size={14} class="cf-file" /> <span class="mono">{e.name}</span>{#if e.kind === 'link'}<span class="muted">link</span>{/if}</span>
                {/if}
              </td>
              <td class="cf-num mono muted">{e.kind === 'file' ? size(e.size) : ''}</td>
              <td class="muted cf-when">{when(e.modified)}</td>
              <td class="mono muted cf-mode">{e.mode}</td>
              <td class="cf-act"><button class="icon-btn" title={e.kind === 'dir' ? 'Download as .tar' : 'Download'} disabled={!!busy} onclick={() => download(join(path, e.name))}>{#if busy === `dl:${join(path, e.name)}`}<Loader2 size={14} class="spin" />{:else}<Download size={14} />{/if}</button></td>
            </tr>
          {:else}
            <tr><td colspan="6"><div class="empty-state">Empty folder — drop files here to upload.</div></td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
  {#if dragging > 0}<div class="cf-drop"><Upload size={22} /> Drop to upload into <span class="mono">{path}</span></div>{/if}
</div>

<style>
  .cf { position: relative; }
  .cf-bar { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 16px; border-bottom: 1px solid var(--border); flex-wrap: wrap; }
  .cf-crumbs { display: flex; align-items: center; gap: 2px; flex-wrap: wrap; min-width: 0; }
  .cf-crumbs :global(.cf-sep) { color: var(--text-muted); }
  .cf-crumb { border: 0; background: none; padding: 3px 6px; border-radius: var(--r-sm); color: var(--text-secondary); cursor: pointer; display: inline-flex; align-items: center; font-size: 12.5px; }
  .cf-crumb:hover { background: var(--bg-hover); color: var(--brand); }
  .cf-crumb.is-last { color: var(--text-primary); font-weight: 700; }
  .cf-tools { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .cf-new { display: flex; align-items: center; gap: 8px; padding: 8px 16px; border-bottom: 1px solid var(--border); color: var(--text-muted); }
  .cf-new input { flex: 1; max-width: 320px; }
  .cf-ups { position: relative; display: flex; flex-direction: column; gap: 4px; padding: 8px 40px 8px 16px; border-bottom: 1px solid var(--border); max-height: 150px; overflow-y: auto; }
  .cf-up { display: flex; align-items: center; gap: 10px; font-size: 12px; }
  .cf-up__name { flex: 0 1 46%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cf-up__bar { flex: 1; height: 5px; border-radius: 3px; background: var(--bg-elevated); overflow: hidden; }
  .cf-up__bar i { display: block; height: 100%; background: var(--brand); }
  .cf-ok { color: var(--success); display: inline-flex; gap: 4px; align-items: center; }
  .cf-err { color: var(--danger); }
  .cf-ups__x { position: absolute; right: 12px; top: 6px; }
  .cf-msg { margin: 10px 16px; }
  .cf-check { width: 34px; }
  .cf-row { cursor: default; }
  .cf-name { display: inline-flex; align-items: center; gap: 8px; border: 0; background: none; padding: 0; color: inherit; font: inherit; }
  button.cf-name { cursor: pointer; }
  button.cf-name:hover .strong { color: var(--brand); }
  .cf-name :global(.cf-dir) { color: var(--brand); }
  .cf-name :global(.cf-file) { color: var(--text-muted); }
  .cf-num { text-align: right; width: 110px; font-size: 12px; }
  .cf-when { font-size: 12px; white-space: nowrap; }
  .cf-mode { font-size: 11.5px; }
  .cf-act { text-align: right; width: 44px; }
  .cf-drop { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; gap: 10px; background: color-mix(in srgb, var(--brand) 14%, var(--bg-surface)); border: 2px dashed var(--brand); border-radius: var(--r-lg); color: var(--brand); font-weight: 600; pointer-events: none; }
</style>
