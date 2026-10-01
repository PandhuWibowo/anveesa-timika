<script lang="ts">
  // Browse a repository at its pulled commit and read files. Reads timika's
  // clone: nothing is sent to the runner.
  import { onMount } from 'svelte'
  import { Folder, FileText, FileSymlink, Package, ChevronRight, Home, Loader2, Copy, Check, Download, WrapText, FileQuestion } from '@lucide/svelte'
  import { automation, errMsg } from '../lib/api'
  import { navigate } from '../lib/router.svelte'
  import { highlight, bytes, short } from '../lib/automation'
  import type { RepoFilesResponse, RepoFile } from '../gen/timika/v1/automation_pb'

  let { repo, path = '' }: { repo: string; path?: string } = $props()

  const SHOWN = 4000
  let listing = $state<RepoFilesResponse | null>(null)
  let file = $state<RepoFile | null>(null)
  let loading = $state(true)
  let error = $state('')
  let wrap = $state(false)
  let copied = $state(false)
  let seq = 0

  const crumbs = $derived(path ? path.split('/') : [])
  const go = (p: string) => navigate(`/automation/${repo}/files${p ? '/' + p : ''}`)

  async function load(p: string) {
    const n = ++seq
    loading = true
    error = ''
    try {
      const l = await automation.listRepoFiles({ repo, path: p })
      if (n !== seq) return
      listing = l
      file = l.kind === 'file' ? await automation.readRepoFile({ repo, path: p }) : null
    } catch (e) {
      if (n === seq) { error = errMsg(e); listing = null; file = null }
    } finally { if (n === seq) loading = false }
  }

  $effect(() => { load(path) })

  const lines = $derived(file && !file.binary && !file.tooLarge ? file.content.replace(/\n$/, '').split('\n') : [])
  const shown = $derived(lines.slice(0, SHOWN))

  function copy() {
    if (!file) return
    navigator.clipboard.writeText(file.content)
    copied = true
    setTimeout(() => (copied = false), 1400)
  }

  function download() {
    if (!file) return
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([file.content], { type: 'text/plain' }))
    a.download = file.path.split('/').pop() ?? 'file'
    a.click()
    URL.revokeObjectURL(a.href)
  }

  const ico = (k: string) => (k === 'dir' ? Folder : k === 'link' ? FileSymlink : k === 'submodule' ? Package : FileText)
</script>

<div class="rf">
  <div class="rf-bar">
    <nav class="rf-crumbs">
      <button class="rf-crumb" onclick={() => go('')} title="Repository root"><Home size={13} /></button>
      {#each crumbs as c, i (i)}
        <ChevronRight size={12} class="rf-sep" />
        <button class="rf-crumb mono" class:is-last={i === crumbs.length - 1} onclick={() => go(crumbs.slice(0, i + 1).join('/'))}>{c}</button>
      {/each}
    </nav>
    {#if listing}<span class="muted rf-at">at <span class="mono">{short(listing.sha)}</span></span>{/if}
  </div>

  {#if error}
    <div class="notice notice--error rf-err">{error}</div>
  {:else if loading && !listing}
    <div class="empty-state"><Loader2 size={18} class="spin" /></div>
  {:else if file}
    <div class="rf-head">
      <span class="mono strong">{file.path.split('/').pop()}</span>
      <span class="muted">{bytes(Number(file.size))}{#if lines.length} · {lines.length} line{lines.length === 1 ? '' : 's'}{/if}</span>
      <span class="rf-spacer"></span>
      {#if lines.length}
        <button class="base-btn base-btn--ghost base-btn--xs" class:rf-on={wrap} onclick={() => (wrap = !wrap)} title="Wrap long lines"><WrapText size={12} /> Wrap</button>
        <button class="base-btn base-btn--ghost base-btn--xs" onclick={copy}>{#if copied}<Check size={12} /> Copied{:else}<Copy size={12} /> Copy{/if}</button>
        <button class="base-btn base-btn--ghost base-btn--xs" onclick={download}><Download size={12} /> Download</button>
      {/if}
    </div>
    {#if file.binary}
      <div class="empty-state"><FileQuestion size={22} /> Binary file ({bytes(Number(file.size))}) — not shown.</div>
    {:else if file.tooLarge}
      <div class="empty-state"><FileQuestion size={22} /> {bytes(Number(file.size))} is over the 1 MB viewer limit. Open it in Git.</div>
    {:else if !lines.length}
      <div class="empty-state">Empty file.</div>
    {:else}
      <div class="rf-code" class:rf-code--wrap={wrap}>
        {#each shown as l, i (i)}
          <div class="rf-line"><span class="rf-no">{i + 1}</span><span class="rf-text">{#each highlight(l) as tk, j (j)}<span class={tk.c}>{tk.t}</span>{/each}{#if !l}&nbsp;{/if}</span></div>
        {/each}
        {#if lines.length > SHOWN}<div class="rf-more">… {lines.length - SHOWN} more lines — Download shows everything</div>{/if}
      </div>
    {/if}
  {:else if listing}
    <div class="data-table-wrap">
      <table class="data-table rf-table">
        <tbody>
          {#each listing.entries as e (e.name)}
            {@const I = ico(e.kind)}
            <tr class:rf-row={e.kind !== 'submodule'} onclick={() => e.kind !== 'submodule' && go(path ? `${path}/${e.name}` : e.name)}>
              <td class="rf-name"><I size={14} class={e.kind === 'dir' ? 'rf-dir' : 'rf-file'} /> <span class="mono" class:strong={e.kind === 'dir'}>{e.name}</span>{#if e.kind === 'link'} <span class="muted">symlink</span>{:else if e.kind === 'submodule'} <span class="muted">submodule</span>{/if}</td>
              <td class="muted rf-size">{e.kind === 'file' ? bytes(Number(e.size)) : ''}</td>
            </tr>
          {:else}
            <tr><td colspan="2"><div class="empty-state">Empty folder.</div></td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .rf-bar { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 16px; border-bottom: 1px solid var(--border); }
  .rf-crumbs { display: flex; align-items: center; gap: 2px; flex-wrap: wrap; min-width: 0; font-size: 12.5px; }
  .rf-crumbs :global(.rf-sep) { color: var(--text-muted); }
  .rf-crumb { border: 0; background: none; padding: 3px 6px; border-radius: var(--r-sm); color: var(--text-secondary); cursor: pointer; display: inline-flex; align-items: center; font-size: 12.5px; }
  .rf-crumb:hover { background: var(--bg-hover); color: var(--brand); }
  .rf-crumb.is-last { color: var(--text-primary); font-weight: 700; }
  .rf-at { font-size: 11.5px; flex: 0 0 auto; }
  .rf-err { margin: 12px 16px; }
  .rf-table td { padding-top: 7px; padding-bottom: 7px; }
  .rf-row { cursor: pointer; }
  .rf-name { display: flex; align-items: center; gap: 8px; }
  .rf-name :global(.rf-dir) { color: var(--brand); }
  .rf-name :global(.rf-file) { color: var(--text-muted); }
  .rf-size { text-align: right; width: 120px; font-size: 12px; }
  .rf-head { display: flex; align-items: center; gap: 12px; padding: 8px 16px; border-bottom: 1px solid var(--border); font-size: 12.5px; }
  .rf-spacer { flex: 1; }
  .rf-on { background: var(--brand-dim) !important; color: var(--brand) !important; }
  .rf-code { overflow: auto; max-height: calc(100vh - 360px); min-height: 200px; padding: 8px 0; background: var(--bg-body); font-family: var(--mono); font-size: 12px; line-height: 1.55; }
  .rf-line { display: flex; }
  .rf-line:hover { background: var(--bg-hover); }
  .rf-no { flex: 0 0 52px; padding-right: 12px; text-align: right; color: var(--text-muted); user-select: none; opacity: .7; }
  .rf-text { flex: 1; white-space: pre; color: var(--text-primary); padding-right: 16px; }
  .rf-code--wrap .rf-text { white-space: pre-wrap; word-break: break-word; }
  .rf-text .com { color: var(--text-muted); font-style: italic; }
  .rf-text .str { color: var(--success); }
  .rf-text .kw { color: var(--brand); font-weight: 600; }
  .rf-text .num { color: var(--warning); }
  .rf-more { padding: 8px 16px 8px 64px; color: var(--text-muted); font-size: 12px; }
</style>
