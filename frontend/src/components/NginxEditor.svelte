<script lang="ts">
  // One nginx config file: edit → "Test & apply" (nginx -t, then reload; a
  // rejected change is put back on the server), with the file's earlier versions.
  // Without `path` it starts a new site from a template.
  import { onMount, tick } from 'svelte'
  import { fade, scale } from 'svelte/transition'
  import { X, Loader2, Check, History, Trash2, RotateCcw, CircleAlert } from '@lucide/svelte'
  import { nginxApi, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { KINDS, template, newPath, errorLine, applyText, base, type Kind } from '../lib/nginx'
  import type { NginxVersion } from '../gen/timika/v1/nginx_pb'

  type Note = { ok: boolean; text: string; detail: string }
  let { asset, container, system, path = '', line = 0, sitesDir, mainConf, onclose, ondone }: {
    asset: string; container: string; system: string; path?: string; line?: number; sitesDir: string; mainConf: string
    onclose: () => void; ondone: (n: Note) => void
  } = $props()

  // svelte-ignore state_referenced_locally
  const creating = !path
  let text = $state('')
  let original = $state('')
  let rev = $state('')
  let loading = $state(!creating)
  let busy = $state('')
  let error = $state('')
  let rejected = $state('')
  let conflict = $state(false)
  let versions = $state<NginxVersion[] | null>(null)
  let count = $state(0)
  let showing = $state('')
  let ta = $state<HTMLTextAreaElement>()
  let gutter = $state<HTMLElement>()

  // New site.
  let kind = $state<Kind>('proxy')
  let domain = $state('')
  let target = $state('')
  let customPath = $state('')
  let edited = $state(false)
  const K = $derived(KINDS.find((k) => k.id === kind)!)
  const file = $derived(creating ? customPath || newPath(sitesDir, domain) : path)
  $effect(() => { if (creating && !edited) text = template(kind, domain, target) })

  const dirty = $derived(text !== original)
  const lines = $derived(text.split('\n').length)
  const badLine = $derived(rejected ? errorLine(rejected, file) : 0)
  const LH = 19

  async function load() {
    loading = true
    conflict = false
    try {
      const f = await nginxApi.readFile({ asset, container, path })
      text = original = f.content
      rev = f.rev
      count = f.versions
      error = ''
      showing = ''
      await tick()
      if (line > 1) jump(line)
    } catch (e) { error = errMsg(e) } finally { loading = false }
  }
  onMount(() => { if (!creating) load() })

  function jump(n: number) {
    if (!ta) return
    const all = text.split('\n')
    const start = all.slice(0, n - 1).reduce((s, l) => s + l.length + 1, 0)
    ta.focus()
    ta.setSelectionRange(start, start + (all[n - 1]?.length ?? 0))
    ta.scrollTop = Math.max(0, (n - 6) * LH)
  }

  async function apply() {
    if (busy || !file) return
    busy = 'apply'
    error = rejected = ''
    try {
      const r = await nginxApi.saveFile({ asset, container, path: file, content: text, rev: creating ? '' : rev, enable: true, note: showing ? `restored the version of ${showing}` : '' })
      if (!r.ok) {
        rejected = r.testOutput
        await tick()
        const n = errorLine(r.testOutput, file)
        if (n) jump(n)
        return
      }
      ondone(applyText(r, base(file)))
    } catch (e) {
      error = errMsg(e)
      conflict = /changed on the server/.test(error)
    } finally { busy = '' }
  }

  async function remove() {
    if (!(await confirm({ title: `Delete ${base(path)}?`, message: 'The file is removed, nginx is tested and reloaded. Its last content stays in the history.', confirmText: 'Delete', variant: 'danger' }))) return
    busy = 'delete'
    error = rejected = ''
    try {
      const r = await nginxApi.deleteFile({ asset, container, path })
      if (!r.ok) { rejected = r.testOutput; return }
      ondone({ ...applyText(r, base(path)), text: `${base(path)} deleted${r.reloaded ? ', nginx reloaded' : ''}.` })
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  async function toggleHistory() {
    if (versions) return (versions = null)
    try { versions = (await nginxApi.listVersions({ asset, container, path })).versions } catch (e) { error = errMsg(e) }
  }
  async function show(v: NginxVersion) {
    try {
      text = (await nginxApi.getVersion({ asset, container, path, id: v.id })).content
      showing = new Date(v.at).toLocaleString()
      rejected = ''
    } catch (e) { error = errMsg(e) }
  }

  async function close() {
    if (dirty && !creating && !(await confirm({ title: 'Discard your changes?', message: `${base(path)} has changes that were not applied.`, confirmText: 'Discard', variant: 'warning' }))) return
    onclose()
  }
  function onkey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); apply() }
    else if (e.key === 'Tab' && ta && !e.shiftKey) {
      e.preventDefault()
      const { selectionStart: s, selectionEnd: en } = ta
      text = text.slice(0, s) + '    ' + text.slice(en)
      edited = true
      tick().then(() => ta?.setSelectionRange(s + 4, s + 4))
    }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && close()} />

<div class="md-mask" role="presentation" transition:fade={{ duration: 120 }}>
  <div class="md" role="dialog" aria-modal="true" transition:scale={{ duration: 140, start: 0.97 }}>
    <header class="md__head">
      <div class="md__id">
        <div class="page-kicker">{creating ? 'New site' : 'Config file'} · {system}{container ? ` · ${container}` : ''}</div>
        <div class="md__title mono">{file || 'type a domain…'}</div>
      </div>
      <div class="md__tools">
        {#if !creating}
          <button class="base-btn base-btn--ghost base-btn--sm" class:is-on={!!versions} onclick={toggleHistory} disabled={!count} title={count ? 'Earlier versions of this file' : 'No earlier version yet — one is kept each time you apply'}><History size={13} /> History{count ? ` ${count}` : ''}</button>
          {#if path !== mainConf}<button class="icon-btn" title="Delete this file" disabled={!!busy} onclick={remove}>{#if busy === 'delete'}<Loader2 size={14} class="spin" />{:else}<Trash2 size={14} />{/if}</button>{/if}
        {/if}
        <button class="base-btn base-btn--primary base-btn--sm" disabled={!!busy || loading || !file || (!creating && !dirty) || !text.trim()} onclick={apply} title="Write the file, run nginx -t, reload (⌘S). If nginx rejects it, the old file is put back.">
          {#if busy === 'apply'}<Loader2 size={13} class="spin" />{:else}<Check size={13} />{/if} Test &amp; apply
        </button>
        <button class="icon-btn" title="Close (esc)" onclick={close}><X size={16} /></button>
      </div>
    </header>

    {#if creating}
      <div class="new">
        <div class="seg">{#each KINDS as k (k.id)}<button class:is-on={kind === k.id} onclick={() => { kind = k.id; edited = false }}>{k.label}</button>{/each}</div>
        <!-- svelte-ignore a11y_autofocus -->
        <label><span>Domain</span><input class="base-input mono" bind:value={domain} placeholder="app.example.com" spellcheck="false" autocapitalize="off" autofocus /></label>
        {#if K.field}<label class="grow"><span>{K.field}</span><input class="base-input mono" bind:value={target} placeholder={K.placeholder} spellcheck="false" /></label>{/if}
        <label class="grow"><span>File</span><input class="base-input mono" value={file} oninput={(e) => (customPath = e.currentTarget.value)} placeholder="{sitesDir}/…" spellcheck="false" /></label>
      </div>
    {/if}

    {#if error}
      <div class="notice notice--error md__note">{error} {#if conflict}<button class="linkish" onclick={load}>Reload the file</button>{/if}</div>
    {/if}
    {#if rejected}
      <div class="rej">
        <div class="rej__t"><CircleAlert size={14} /> nginx rejected this, so nothing changed on the server. {#if badLine}<button class="linkish" onclick={() => jump(badLine)}>Go to line {badLine}</button>{/if}</div>
        <pre class="mono">{rejected}</pre>
      </div>
    {/if}
    {#if showing}
      <div class="notice notice--info md__note">This is the version of {showing}. <b>Test &amp; apply</b> makes it live. <button class="linkish" onclick={() => { text = original; showing = '' }}><RotateCcw size={11} /> Back to the current file</button></div>
    {/if}

    <div class="body">
      {#if loading}
        <div class="empty-state"><Loader2 size={18} class="spin" /></div>
      {:else}
        <div class="ed">
          <div class="gut mono" bind:this={gutter} aria-hidden="true">
            {#each { length: lines } as _, i (i)}<div class:bad={badLine === i + 1}>{i + 1}</div>{/each}
          </div>
          <textarea class="mono" bind:this={ta} bind:value={text} oninput={() => (edited = true)} onkeydown={onkey} onscroll={() => { if (gutter && ta) gutter.scrollTop = ta.scrollTop }} spellcheck="false" autocapitalize="off" autocomplete="off" wrap="off"></textarea>
        </div>
        {#if versions}
          <aside class="hist">
            <div class="hist__t">Earlier versions</div>
            {#each versions as v (v.id)}
              <button class="hist__v" onclick={() => show(v)}>
                <span class="strong">{new Date(v.at).toLocaleString()}</span>
                <span class="muted">{v.by || 'not through timika'}{v.note ? ` · ${v.note}` : ''}</span>
              </button>
            {:else}<div class="muted hist__none">None yet.</div>{/each}
          </aside>
        {/if}
      {/if}
    </div>
    <footer class="foot">
      <span>{lines} lines{dirty && !creating ? ' · changed' : ''}</span>
      <span>Applying runs <span class="mono">nginx -t</span> first; a rejected change is undone on the server.{container ? ' Files inside a container last only as long as the container, unless they are on a volume.' : ''}</span>
    </footer>
  </div>
</div>

<style>
  .md-mask { position: fixed; inset: 0; z-index: 900; display: flex; align-items: center; justify-content: center; background: rgba(0, 0, 0, 0.5); backdrop-filter: blur(3px); }
  .md { width: min(1180px, calc(100vw - 48px)); height: min(820px, calc(100vh - 48px)); display: flex; flex-direction: column; background: var(--bg-surface); border: 1px solid var(--border); border-radius: 14px; box-shadow: var(--shadow-lg); overflow: hidden; }
  .md__head { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; gap: 12px; border-bottom: 1px solid var(--border); }
  .md__id { min-width: 0; }
  .md__title { font-size: 14px; font-weight: 700; color: var(--text-primary); margin-top: 2px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .md__tools { display: flex; align-items: center; gap: 8px; flex: 0 0 auto; }
  .md__tools .is-on { color: var(--brand); }
  .md__note { margin: 10px 18px 0; }
  .new { display: flex; gap: 10px; align-items: flex-end; flex-wrap: wrap; padding: 12px 18px; border-bottom: 1px solid var(--border); }
  .new label { display: flex; flex-direction: column; gap: 4px; font-size: 11.5px; color: var(--text-muted); flex: 0 1 220px; }
  .new .grow { flex: 1 1 220px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .seg button { padding: 6px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; white-space: nowrap; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .rej { margin: 10px 18px 0; padding: 10px 12px; border-radius: var(--r); border: 1px solid var(--danger); background: var(--danger-bg); }
  .rej__t { display: flex; align-items: center; gap: 6px; font-size: 12.5px; font-weight: 600; color: var(--danger); }
  .rej pre { margin: 6px 0 0; font-size: 11.5px; white-space: pre-wrap; word-break: break-word; color: var(--text-primary); max-height: 110px; overflow: auto; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; font-weight: 600; }
  .linkish :global(svg) { vertical-align: -1px; }
  .body { flex: 1; display: flex; min-height: 0; margin-top: 10px; border-top: 1px solid var(--border); }
  .ed { flex: 1; display: flex; min-width: 0; background: var(--bg-body); }
  .gut { flex: 0 0 auto; min-width: 44px; padding: 10px 8px 10px 10px; text-align: right; color: var(--text-muted); font-size: 12px; line-height: 19px; overflow: hidden; user-select: none; border-right: 1px solid var(--border); opacity: 0.75; }
  .gut .bad { color: #fff; background: var(--danger); border-radius: 3px; opacity: 1; }
  textarea { flex: 1; min-width: 0; resize: none; border: 0; outline: 0; background: none; color: var(--text-primary); font-size: 12.5px; line-height: 19px; padding: 10px 14px; tab-size: 4; white-space: pre; overflow: auto; }
  .hist { flex: 0 0 250px; border-left: 1px solid var(--border); overflow: auto; padding: 10px; display: flex; flex-direction: column; gap: 4px; }
  .hist__t { font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); padding: 2px 6px 6px; }
  .hist__v { display: flex; flex-direction: column; gap: 1px; padding: 7px 8px; border: 0; border-radius: var(--r); background: none; text-align: left; font: inherit; font-size: 12px; color: var(--text-secondary); cursor: pointer; }
  .hist__v:hover { background: var(--bg-hover); }
  .hist__v .muted { font-size: 11px; }
  .hist__none { font-size: 12px; padding: 6px; }
  .foot { display: flex; justify-content: space-between; gap: 16px; padding: 8px 18px; font-size: 11.5px; color: var(--text-muted); border-top: 1px solid var(--border); }
</style>
