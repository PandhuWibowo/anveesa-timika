<script lang="ts">
  // SFTP file browser for one server: browse, upload (drag & drop), download,
  // new folder, rename, delete — as one of the accounts you may use.
  import { onMount } from 'svelte'
  import { Folder, File as FileIcon, FileSymlink, ArrowUp, RefreshCw, FolderPlus, Upload, Download, Pencil, Trash2, Eye, EyeOff, Search, X, Loader2, Home, ChevronRight, CircleCheck, CircleAlert, FileArchive, PackageOpen, FolderDown, ChevronDown } from '@lucide/svelte'
  import { bastion, errMsg } from '../lib/api'
  import { session } from '../lib/session.svelte'
  import { confirm } from '../lib/ui.svelte'
  import type { Asset, FileEntry } from '../gen/timika/v1/bastion_pb'

  let { asset, initialAccount = '' }: { asset: Asset; initialAccount?: string } = $props()

  // svelte-ignore state_referenced_locally
  let account = $state(asset.allowedAccounts.includes(initialAccount) ? initialAccount : asset.allowedAccounts[0] ?? '')
  let path = $state('')
  let entries = $state<FileEntry[]>([])
  let loading = $state(false)
  let error = $state('')
  let showHidden = $state(false)
  let filter = $state('')
  let selected = $state<Set<string>>(new Set())
  let editingPath = $state(false)
  let pathInput = $state('')
  let renaming = $state<{ name: string; value: string } | null>(null)
  let newFolder = $state<string | null>(null)
  let dragging = $state(0)
  let fileInput = $state<HTMLInputElement>()

  const target = () => ({ asset: asset.id, account })
  const join = (dir: string, name: string) => (dir.endsWith('/') ? dir + name : `${dir}/${name}`)
  const parent = (p: string) => (p === '/' ? '/' : p.replace(/\/[^/]+\/?$/, '') || '/')

  async function open(p: string) {
    loading = true
    error = ''
    try {
      const r = await bastion.listFiles({ ...target(), path: p })
      path = r.path
      entries = r.entries
      selected = new Set()
      renaming = null
      newFolder = null
    } catch (e) { error = errMsg(e) } finally { loading = false }
  }
  onMount(() => { if (account) open('') })

  function switchAccount(a: string) {
    account = a
    path = ''
    open('')
  }

  const shown = $derived(entries.filter((e) => (showHidden || !e.name.startsWith('.')) && (!filter || e.name.toLowerCase().includes(filter.toLowerCase()))))
  const crumbs = $derived.by(() => {
    const parts = path.split('/').filter(Boolean)
    return parts.map((name, i) => ({ name, path: '/' + parts.slice(0, i + 1).join('/') }))
  })
  const hiddenCount = $derived(entries.filter((e) => e.name.startsWith('.')).length)

  function size(n: bigint) {
    const b = Number(n)
    if (b < 1024) return `${b} B`
    const u = ['KB', 'MB', 'GB', 'TB']
    let v = b / 1024, i = 0
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++ }
    return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`
  }
  function when(t?: string) {
    if (!t) return ''
    const d = new Date(t)
    const s = (Date.now() - d.getTime()) / 1000
    if (s < 60) return 'just now'
    if (s < 3600) return `${Math.floor(s / 60)} min ago`
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`
    return d.toLocaleDateString(undefined, { year: d.getFullYear() === new Date().getFullYear() ? undefined : 'numeric', month: 'short', day: 'numeric' })
  }

  function activate(e: FileEntry) {
    if (e.kind === 'dir' || e.kind === 'link') open(join(path, e.name)).then(() => { if (error && e.kind === 'link') { error = ''; download(e) } })
    else download(e)
  }

  async function download(e: FileEntry) {
    try {
      const link = await bastion.downloadLink({ ...target(), path: join(path, e.name) })
      // The browser streams it straight to disk (one-minute signed link).
      const a = document.createElement('a')
      a.href = link.url
      a.download = link.name
      document.body.appendChild(a)
      a.click()
      a.remove()
    } catch (err) { error = errMsg(err) }
  }

  async function downloadSelected() {
    for (const e of entries.filter((x) => selected.has(x.name) && x.kind !== 'dir')) {
      await download(e)
      await new Promise((r) => setTimeout(r, 300))
    }
  }

  async function remove(names: string[]) {
    const dirs = entries.filter((e) => names.includes(e.name) && e.kind === 'dir').length
    const ok = await confirm({
      title: names.length === 1 ? `Delete ${names[0]}?` : `Delete ${names.length} items?`,
      message: dirs ? 'Folders are deleted with everything inside them. This can’t be undone.' : 'This can’t be undone.',
      subject: `${account}@${asset.name}:${names.length === 1 ? join(path, names[0]) : path}`,
      confirmText: 'Delete',
      variant: 'danger',
    })
    if (!ok) return
    try {
      await bastion.deleteFiles({ ...target(), paths: names.map((n) => join(path, n)) })
      await open(path)
    } catch (e) { error = errMsg(e) }
  }

  async function commitRename() {
    if (!renaming) return
    const { name, value } = renaming
    renaming = null
    if (!value.trim() || value === name) return
    try {
      await bastion.renameFile({ ...target(), from: join(path, name), to: join(path, value.trim()) })
      await open(path)
    } catch (e) { error = errMsg(e) }
  }

  async function createFolder() {
    const name = newFolder?.trim()
    newFolder = null
    if (!name) return
    try {
      await bastion.makeDir({ ...target(), path: join(path, name) })
      await open(path)
    } catch (e) { error = errMsg(e) }
  }

  function goTo() {
    editingPath = false
    if (pathInput.trim() && pathInput.trim() !== path) open(pathInput.trim())
  }

  // ── archives ──
  type Fmt = 'zip' | 'tar.gz' | 'tar.xz' | 'tar'
  const FORMATS: { id: Fmt; label: string; hint: string }[] = [
    { id: 'zip', label: '.zip', hint: 'universal' },
    { id: 'tar.gz', label: '.tar.gz', hint: 'Linux standard' },
    { id: 'tar.xz', label: '.tar.xz', hint: 'smallest, slower' },
    { id: 'tar', label: '.tar', hint: 'no compression' },
  ]
  const ARCHIVE = /\.(zip|tar|tar\.gz|tgz|tar\.xz|txz|tar\.bz2|tbz2)$/i
  const isArchive = (e: FileEntry) => e.kind === 'file' && ARCHIVE.test(e.name)
  const base = () => path.split('/').filter(Boolean).pop() ?? 'files'

  let compressing = $state<{ names: string[]; format: Fmt; name: string; busy: boolean; error: string } | null>(null)
  let dlMenu = $state(false)
  let notice = $state('')
  let highlight = $state('')
  let working = $state('')

  function flash(msg: string, name = '') {
    notice = msg
    highlight = name
    setTimeout(() => { if (notice === msg) notice = '' }, 5000)
    setTimeout(() => { if (highlight === name) highlight = '' }, 3000)
  }

  function startCompress(names: string[]) {
    compressing = { names, format: 'zip', name: names.length === 1 ? names[0] : base(), busy: false, error: '' }
  }

  async function doCompress() {
    if (!compressing) return
    compressing.busy = true
    compressing.error = ''
    try {
      const r = await bastion.compress({ ...target(), dir: path, names: compressing.names, format: compressing.format, archive: compressing.name.trim() })
      const name = r.path.split('/').pop() ?? ''
      compressing = null
      selected = new Set()
      await open(path)
      flash(`Created ${name} (${size(r.size)})`, name)
    } catch (e) {
      // Shown in the dialog, where the user is looking.
      if (compressing) { compressing.error = errMsg(e); compressing.busy = false }
    }
  }

  async function extract(e: FileEntry) {
    working = e.name
    error = ''
    try {
      const r = await bastion.extract({ ...target(), path: join(path, e.name) })
      const name = r.dir.split('/').pop() ?? ''
      await open(path)
      flash(`Extracted ${e.name} into ${name}/`, name)
    } catch (err) { error = errMsg(err) } finally { working = '' }
  }

  /** Stream a folder / selection as one archive (made on the fly, nothing left on the server). */
  async function downloadArchive(names: string[], format: Fmt = 'zip') {
    dlMenu = false
    try {
      const link = await bastion.archiveLink({ ...target(), dir: path, names, format })
      const a = document.createElement('a')
      a.href = link.url
      a.download = link.name
      document.body.appendChild(a)
      a.click()
      a.remove()
      flash(`Downloading ${link.name}…`)
    } catch (e) { error = errMsg(e) }
  }

  // ── uploads ──
  type Up = { id: number; name: string; size: number; sent: number; state: 'up' | 'done' | 'error'; error?: string; xhr: XMLHttpRequest }
  let uploads = $state<Up[]>([])
  let upSeq = 0

  async function uploadFiles(list: FileList | File[]) {
    const files = [...list]
    if (!files.length) return
    const clash = files.filter((f) => entries.some((e) => e.name === f.name))
    if (clash.length) {
      const ok = await confirm({
        title: clash.length === 1 ? `Replace ${clash[0].name}?` : `Replace ${clash.length} files?`,
        message: 'Files with the same name in this folder are overwritten.',
        confirmText: 'Replace',
        variant: 'warning',
      })
      if (!ok) return
    }
    const dir = path
    await Promise.all(files.map((f) => uploadOne(f, dir)))
    if (path === dir) open(path)
  }

  function uploadOne(f: File, dir: string) {
    return new Promise<void>((resolve) => {
      const xhr = new XMLHttpRequest()
      const up: Up = { id: ++upSeq, name: f.name, size: f.size, sent: 0, state: 'up', xhr }
      uploads.push(up)
      const u = uploads[uploads.length - 1]
      const q = new URLSearchParams({ ...target(), path: join(dir, f.name) })
      xhr.open('PUT', `/v1/bastion/files/upload?${q}`)
      if (session.token) xhr.setRequestHeader('x-timika-token', session.token)
      xhr.upload.onprogress = (e) => { u.sent = e.loaded }
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) { u.state = 'done'; u.sent = u.size; setTimeout(() => { uploads = uploads.filter((x) => x.id !== u.id) }, 4000) }
        else { u.state = 'error'; u.error = (() => { try { return JSON.parse(xhr.responseText).error } catch { return `HTTP ${xhr.status}` } })() }
        resolve()
      }
      xhr.onerror = () => { u.state = 'error'; u.error = 'connection failed'; resolve() }
      xhr.onabort = () => { u.state = 'error'; u.error = 'cancelled'; resolve() }
      xhr.send(f)
    })
  }

  function onDrop(e: DragEvent) {
    e.preventDefault()
    dragging = 0
    if (e.dataTransfer?.files.length) uploadFiles(e.dataTransfer.files)
  }

  const toggle = (name: string) => {
    const s = new Set(selected)
    s.has(name) ? s.delete(name) : s.add(name)
    selected = s
  }
  const allSelected = $derived(shown.length > 0 && shown.every((e) => selected.has(e.name)))
  const selectAll = () => (selected = allSelected ? new Set() : new Set(shown.map((e) => e.name)))
</script>

{#if !account}
  <div class="empty-state">You don't have an account on this server to browse files with.</div>
{:else}
<div class="fb" role="region" aria-label="Files"
     ondragenter={(e) => { if (e.dataTransfer?.types.includes('Files')) { e.preventDefault(); dragging++ } }}
     ondragover={(e) => { if (e.dataTransfer?.types.includes('Files')) e.preventDefault() }}
     ondragleave={() => (dragging = Math.max(0, dragging - 1))}
     ondrop={onDrop}>

  <!-- toolbar -->
  <div class="fb__bar">
    {#if asset.allowedAccounts.length > 1}
      <select class="base-select fb__acc mono" value={account} onchange={(e) => switchAccount((e.target as HTMLSelectElement).value)} title="Browse as">
        {#each asset.allowedAccounts as a (a)}<option value={a}>{a}</option>{/each}
      </select>
    {:else}
      <span class="fb__acc-one mono">{account}</span>
    {/if}
    <button class="icon-btn" title="Up one folder" disabled={path === '/'} onclick={() => open(parent(path))}><ArrowUp size={15} /></button>
    <button class="icon-btn" title="Home folder" onclick={() => open('')}><Home size={15} /></button>
    {#if editingPath}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="base-input mono fb__path-input" bind:value={pathInput} autofocus onblur={goTo}
             onkeydown={(e) => { if (e.key === 'Enter') goTo(); if (e.key === 'Escape') editingPath = false }} />
    {:else}
      <div class="fb__crumbs" role="button" tabindex="0" title="Click to type a path"
           onclick={(e) => { if (e.target === e.currentTarget) { pathInput = path; editingPath = true } }}
           onkeydown={(e) => { if (e.key === 'Enter') { pathInput = path; editingPath = true } }}>
        <button class="fb__crumb" onclick={() => open('/')}>/</button>
        {#each crumbs as c, i (c.path)}
          {#if i > 0}<ChevronRight size={12} />{/if}
          <button class="fb__crumb" class:is-last={i === crumbs.length - 1} onclick={() => open(c.path)}>{c.name}</button>
        {/each}
      </div>
    {/if}
    <button class="icon-btn" title="Refresh" onclick={() => open(path)}>{#if loading}<Loader2 size={15} class="spin" />{:else}<RefreshCw size={15} />{/if}</button>
  </div>

  <div class="fb__bar fb__bar--2">
    <div class="fb__filter"><Search size={13} /><input bind:value={filter} placeholder="Filter this folder…" /></div>
    <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (showHidden = !showHidden)} title="Dotfiles">
      {#if showHidden}<EyeOff size={13} /> Hide hidden{:else}<Eye size={13} /> Show hidden{#if hiddenCount}&nbsp;({hiddenCount}){/if}{/if}
    </button>
    <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (newFolder = '')}><FolderPlus size={13} /> New folder</button>
    <button class="base-btn base-btn--primary base-btn--sm" onclick={() => fileInput?.click()}><Upload size={13} /> Upload</button>
    <input type="file" multiple hidden bind:this={fileInput} onchange={(e) => { const t = e.target as HTMLInputElement; if (t.files) uploadFiles(t.files); t.value = '' }} />
  </div>

  {#if selected.size}
    <div class="fb__sel">
      <span>{selected.size} selected</span>
      <button class="base-btn base-btn--ghost base-btn--xs" onclick={() => startCompress([...selected])}><FileArchive size={12} /> Compress…</button>
      <div class="fb__menu-wrap">
        <button class="base-btn base-btn--ghost base-btn--xs" onclick={() => (dlMenu = !dlMenu)}><Download size={12} /> Download <ChevronDown size={11} /></button>
        {#if dlMenu}
          <div class="fb__menu" role="menu">
            <button role="menuitem" onclick={() => downloadArchive([...selected], 'zip')}><FileArchive size={13} /> As one .zip</button>
            <button role="menuitem" onclick={() => downloadArchive([...selected], 'tar.gz')}><FileArchive size={13} /> As one .tar.gz</button>
            {#if entries.some((e) => selected.has(e.name) && e.kind !== 'dir')}
              <button role="menuitem" onclick={() => { dlMenu = false; downloadSelected() }}><Download size={13} /> Files one by one</button>
            {/if}
          </div>
        {/if}
      </div>
      <button class="base-btn base-btn--danger base-btn--xs" onclick={() => remove([...selected])}><Trash2 size={12} /> Delete</button>
      <button class="icon-btn" title="Clear selection" onclick={() => (selected = new Set())}><X size={13} /></button>
    </div>
  {/if}

  {#if notice}<div class="notice notice--success fb__err">{notice} <button class="icon-btn" onclick={() => (notice = '')}><X size={12} /></button></div>{/if}
  {#if error}<div class="notice notice--error fb__err">{error} <button class="icon-btn" onclick={() => (error = '')}><X size={12} /></button></div>{/if}

  <!-- listing -->
  <div class="fb__list">
    <div class="fb__row fb__row--head">
      <input type="checkbox" checked={allSelected} onchange={selectAll} aria-label="Select all" />
      <span>Name</span><span class="fb__num">Size</span><span>Modified</span><span>Mode</span><span></span>
    </div>
    {#if newFolder !== null}
      <div class="fb__row">
        <span></span>
        <span class="fb__name"><Folder size={15} class="fb__ico fb__ico--dir" />
          <!-- svelte-ignore a11y_autofocus -->
          <input class="base-input fb__inline" bind:value={newFolder} placeholder="Folder name" autofocus
                 onkeydown={(e) => { if (e.key === 'Enter') createFolder(); if (e.key === 'Escape') newFolder = null }} onblur={createFolder} />
        </span>
      </div>
    {/if}
    {#each shown as e (e.name)}
      <div class="fb__row" class:is-sel={selected.has(e.name)} class:is-new={highlight === e.name} role="row" tabindex="-1" ondblclick={() => activate(e)}>
        <input type="checkbox" checked={selected.has(e.name)} onchange={() => toggle(e.name)} aria-label="Select {e.name}" />
        <span class="fb__name">
          {#if e.kind === 'dir'}<Folder size={15} class="fb__ico fb__ico--dir" />{:else if e.kind === 'link'}<FileSymlink size={15} class="fb__ico" />{:else}<FileIcon size={15} class="fb__ico" />{/if}
          {#if renaming?.name === e.name}
            <!-- svelte-ignore a11y_autofocus -->
            <input class="base-input fb__inline" bind:value={renaming.value} autofocus
                   onkeydown={(ev) => { if (ev.key === 'Enter') commitRename(); if (ev.key === 'Escape') renaming = null }} onblur={commitRename} />
          {:else}
            <button class="fb__link" onclick={() => activate(e)} title={e.kind === 'dir' ? 'Open' : 'Download'}>{e.name}</button>
          {/if}
        </span>
        <span class="fb__num muted">{e.kind === 'dir' ? '—' : size(e.size)}</span>
        <span class="muted" title={e.modified ? new Date(e.modified).toLocaleString() : ''}>{when(e.modified)}</span>
        <span class="mono muted fb__mode">{e.mode}</span>
        <span class="fb__acts">
          {#if working === e.name}<Loader2 size={13} class="spin" />{/if}
          {#if isArchive(e)}<button class="icon-btn" title="Extract into a new folder" disabled={!!working} onclick={() => extract(e)}><PackageOpen size={13} /></button>{/if}
          {#if e.kind === 'dir'}
            <button class="icon-btn" title="Download folder as .zip" onclick={() => downloadArchive([e.name], 'zip')}><FolderDown size={13} /></button>
          {:else}
            <button class="icon-btn" title="Download" onclick={() => download(e)}><Download size={13} /></button>
          {/if}
          <button class="icon-btn" title="Compress…" onclick={() => startCompress([e.name])}><FileArchive size={13} /></button>
          <button class="icon-btn" title="Rename" onclick={() => (renaming = { name: e.name, value: e.name })}><Pencil size={13} /></button>
          <button class="icon-btn" title="Delete" onclick={() => remove([e.name])}><Trash2 size={13} /></button>
        </span>
      </div>
    {:else}
      {#if !loading}
        <div class="empty-state fb__empty">
          {filter ? 'Nothing matches.' : entries.length ? 'Only hidden files here.' : 'This folder is empty — drop files here to upload.'}
        </div>
      {/if}
    {/each}
  </div>

  {#if dragging}
    <div class="fb__drop"><Upload size={26} /><div>Drop to upload to <span class="mono">{path}</span></div></div>
  {/if}

  {#if compressing}
    <div class="fb__dlg-mask" role="presentation" onclick={(ev) => { if (ev.target === ev.currentTarget && !compressing?.busy) compressing = null }}>
      <div class="fb__dlg" role="dialog" aria-modal="true">
        <div class="fb__dlg-title"><FileArchive size={16} /> Compress {compressing.names.length === 1 ? compressing.names[0] : `${compressing.names.length} items`}</div>
        <div class="fb__fmts">
          {#each FORMATS as f (f.id)}
            <button class="fb__fmt" class:is-on={compressing.format === f.id} disabled={compressing.busy} onclick={() => (compressing!.format = f.id)}>
              <b>{f.label}</b><span>{f.hint}</span>
            </button>
          {/each}
        </div>
        <label class="fb__dlg-name">
          <span>Name</span>
          <div class="fb__dlg-input">
            <!-- svelte-ignore a11y_autofocus -->
            <input class="mono" bind:value={compressing.name} disabled={compressing.busy} autofocus
                   onkeydown={(ev) => { if (ev.key === 'Enter') doCompress(); if (ev.key === 'Escape' && !compressing?.busy) compressing = null }} />
            <span class="mono muted">{FORMATS.find((f) => f.id === compressing!.format)?.label}</span>
          </div>
        </label>
        {#if compressing.error}<div class="notice notice--error">{compressing.error}</div>{/if}
        <p class="muted fb__dlg-hint">Made on the server in <span class="mono">{path}</span> with its own {compressing.format === 'zip' ? 'zip' : 'tar'} — nothing is downloaded.</p>
        <div class="fb__dlg-actions">
          <button class="base-btn base-btn--ghost" disabled={compressing.busy} onclick={() => (compressing = null)}>Cancel</button>
          <button class="base-btn base-btn--primary" disabled={compressing.busy || !compressing.name.trim()} onclick={doCompress}>
            {#if compressing.busy}<Loader2 size={14} class="spin" /> Compressing…{:else}Compress{/if}
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if uploads.length}
    <div class="fb__ups">
      {#each uploads as u (u.id)}
        <div class="fb__up">
          <span class="fb__up-ico">
            {#if u.state === 'done'}<CircleCheck size={14} />{:else if u.state === 'error'}<CircleAlert size={14} />{:else}<Loader2 size={14} class="spin" />{/if}
          </span>
          <div class="fb__up-main">
            <div class="fb__up-name"><span class="mono">{u.name}</span><span class="muted">{u.state === 'error' ? u.error : `${size(BigInt(u.sent))} / ${size(BigInt(u.size))}`}</span></div>
            <div class="fb__up-bar"><div class="fb__up-fill" class:is-err={u.state === 'error'} style="width: {u.size ? Math.round((u.sent / u.size) * 100) : 100}%"></div></div>
          </div>
          {#if u.state === 'up'}<button class="icon-btn" title="Cancel" onclick={() => u.xhr.abort()}><X size={12} /></button>
          {:else}<button class="icon-btn" title="Dismiss" onclick={() => (uploads = uploads.filter((x) => x.id !== u.id))}><X size={12} /></button>{/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
{/if}

<style>
  .fb { position: relative; display: flex; flex-direction: column; min-height: 360px; }
  :global([data-theme='dark']) .fb { color-scheme: dark; }
  .fb input[type='checkbox'] { width: 14px; height: 14px; accent-color: var(--brand); cursor: pointer; }
  .fb__bar { display: flex; align-items: center; gap: 6px; padding: 10px 14px; border-bottom: 1px solid var(--border); }
  .fb__bar--2 { gap: 8px; }
  .fb__acc { width: auto; min-width: 110px; }
  .fb__acc-one { padding: 4px 10px; border-radius: var(--r); background: var(--brand-dim); color: var(--brand); font-size: 12px; }
  .fb__crumbs { flex: 1; min-width: 0; display: flex; align-items: center; gap: 2px; padding: 5px 8px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); cursor: text; overflow-x: auto; white-space: nowrap; color: var(--text-muted); }
  .fb__crumb { padding: 1px 5px; border: 0; border-radius: var(--r-xs); background: none; color: var(--text-secondary); font-family: var(--mono); font-size: 12.5px; cursor: pointer; }
  .fb__crumb:hover { background: var(--bg-elevated); color: var(--text-primary); }
  .fb__crumb.is-last { color: var(--text-primary); font-weight: 600; }
  .fb__path-input { flex: 1; font-size: 12.5px; }
  .fb__filter { flex: 1; display: flex; align-items: center; gap: 6px; color: var(--text-muted); }
  .fb__filter input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .fb__sel { display: flex; align-items: center; gap: 8px; padding: 6px 14px; background: var(--brand-dim); font-size: 12.5px; color: var(--brand); }
  .fb__err { margin: 8px 14px 0; display: flex; justify-content: space-between; align-items: center; }
  .fb__list { display: flex; flex-direction: column; padding: 4px 6px 10px; }
  .fb__row { display: grid; grid-template-columns: 24px minmax(0, 1fr) 84px 130px 104px 150px; align-items: center; gap: 8px 16px; padding: 4px 8px; border-radius: var(--r-sm); font-size: 13px; }
  .fb__row:not(.fb__row--head):hover { background: var(--bg-hover); }
  .fb__row.is-sel { background: var(--brand-dim); }
  .fb__row--head { font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); padding-top: 8px; padding-bottom: 6px; }
  .fb__num { text-align: right; }
  .fb__name { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .fb__name :global(.fb__ico) { flex: 0 0 auto; color: var(--text-muted); }
  .fb__name :global(.fb__ico--dir) { color: var(--brand); }
  .fb__link { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 0; border: 0; background: none; color: var(--text-primary); font: inherit; cursor: pointer; text-align: left; }
  .fb__link:hover { color: var(--brand); text-decoration: underline; }
  .fb__inline { height: 26px; padding: 0 8px; font-size: 13px; }
  .fb__mode { font-size: 11.5px; }
  .fb__acts { display: flex; justify-content: flex-end; gap: 0; opacity: 0; transition: opacity var(--dur) var(--ease); }
  .fb__row:hover .fb__acts, .fb__row.is-sel .fb__acts { opacity: 1; }
  .fb__empty { padding: 40px 0; }
  .fb__row.is-new { animation: fbnew 3s ease-out; }
  @keyframes fbnew { 0%, 40% { background: var(--brand-soft); } 100% { background: transparent; } }
  .fb__menu-wrap { position: relative; }
  .fb__menu { position: absolute; top: calc(100% + 4px); left: 0; z-index: 20; min-width: 180px; padding: 4px; background: var(--bg-elevated); border: 1px solid var(--border); border-radius: var(--r); box-shadow: var(--shadow-md); }
  .fb__menu button { display: flex; align-items: center; gap: 8px; width: 100%; padding: 7px 10px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-primary); font-size: 12.5px; cursor: pointer; text-align: left; }
  .fb__menu button:hover { background: var(--brand-dim); color: var(--brand); }
  .fb__dlg-mask { position: fixed; inset: 0; z-index: 960; background: rgba(0, 0, 0, 0.5); display: grid; place-items: center; padding: 20px; }
  .fb__dlg { width: min(440px, 100%); display: flex; flex-direction: column; gap: 14px; padding: 20px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); box-shadow: var(--shadow-lg); }
  .fb__dlg-title { display: flex; align-items: center; gap: 8px; font-weight: 700; color: var(--text-primary); word-break: break-all; }
  .fb__fmts { display: grid; grid-template-columns: repeat(4, 1fr); gap: 6px; }
  .fb__fmt { display: flex; flex-direction: column; gap: 2px; padding: 9px 8px; border-radius: var(--r); border: 1px solid var(--border); background: var(--bg-body); color: var(--text-secondary); cursor: pointer; text-align: left; }
  .fb__fmt b { font-family: var(--mono); font-size: 13px; color: var(--text-primary); }
  .fb__fmt span { font-size: 10.5px; color: var(--text-muted); }
  .fb__fmt.is-on { border-color: var(--brand-ring); background: var(--brand-dim); }
  .fb__fmt.is-on b { color: var(--brand); }
  .fb__dlg-name { display: flex; flex-direction: column; gap: 5px; }
  .fb__dlg-name > span { font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: .03em; }
  .fb__dlg-input { display: flex; align-items: center; gap: 2px; padding: 0 10px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .fb__dlg-input:focus-within { border-color: var(--brand-ring); }
  .fb__dlg-input input { flex: 1; min-width: 0; height: 34px; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 13px; }
  .fb__dlg-hint { margin: 0; font-size: 12px; }
  .fb__dlg-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .fb__drop { position: absolute; inset: 0; z-index: 5; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; border: 2px dashed var(--brand); border-radius: var(--r-lg); background: rgba(14, 15, 17, 0.75); color: var(--brand); font-size: 14px; pointer-events: none; }
  .fb__ups { position: fixed; right: 20px; bottom: 20px; z-index: 950; width: 340px; display: flex; flex-direction: column; gap: 6px; padding: 10px; border-radius: var(--r-lg); background: var(--bg-elevated); border: 1px solid var(--border); box-shadow: var(--shadow-lg); }
  .fb__up { display: flex; align-items: center; gap: 8px; }
  .fb__up-ico { color: var(--brand); display: flex; }
  .fb__up-main { flex: 1; min-width: 0; }
  .fb__up-name { display: flex; justify-content: space-between; gap: 8px; font-size: 12px; }
  .fb__up-name .mono { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fb__up-bar { height: 4px; border-radius: 2px; background: var(--bg-body); overflow: hidden; margin-top: 4px; }
  .fb__up-fill { height: 100%; background: var(--brand); transition: width 120ms linear; }
  .fb__up-fill.is-err { background: var(--danger); }
  @media (max-width: 760px) { .fb__row { grid-template-columns: 24px minmax(0, 1fr) 70px 92px; } .fb__row > :nth-child(4), .fb__row > :nth-child(5) { display: none; } }
</style>
