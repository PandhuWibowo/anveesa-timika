<script lang="ts">
  // The last lines of a container's output (docker logs), refreshed on demand or live.
  import { onMount, tick } from 'svelte'
  import { fade, scale } from 'svelte/transition'
  import { X, Loader2, RefreshCw, Download } from '@lucide/svelte'
  import { monitor, errMsg } from '../lib/api'

  let { asset, name, system, onclose }: { asset: string; name: string; system: string; onclose: () => void } = $props()

  let text = $state('')
  let truncated = $state(false)
  let tail = $state(200)
  let live = $state(false)
  let busy = $state(true)
  let error = $state('')
  let el = $state<HTMLElement>()

  async function load() {
    busy = true
    try {
      const r = await monitor.containerLogs({ asset, name, tail })
      const atEnd = !el || el.scrollHeight - el.scrollTop - el.clientHeight < 60
      text = r.text
      truncated = r.truncated
      error = ''
      if (atEnd) { await tick(); el?.scrollTo({ top: el.scrollHeight }) }
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }

  onMount(() => {
    load()
    const t = setInterval(() => { if (live && !busy && !document.hidden) load() }, 3000)
    return () => clearInterval(t)
  })

  function download() {
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([text], { type: 'text/plain' }))
    a.download = `${system}-${name}.log`
    a.click()
    URL.revokeObjectURL(a.href)
  }
  const lines = $derived(text.replace(/\n$/, '').split('\n'))
  const cls = (l: string) => (/\b(error|fatal|panic|exception|emerg|crit)\b/i.test(l) ? 'bad' : /\bwarn(ing)?\b/i.test(l) ? 'warn' : '')
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="md-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="md" role="dialog" aria-modal="true" transition:scale={{ duration: 140, start: 0.97 }}>
    <header class="md__head">
      <div>
        <div class="page-kicker">Container logs · {system}</div>
        <div class="md__title mono">{name}</div>
      </div>
      <div class="md__tools">
        <select class="base-select" bind:value={tail} onchange={load} title="Lines from the end">
          <option value={100}>100 lines</option><option value={200}>200 lines</option><option value={500}>500 lines</option><option value={2000}>2000 lines</option>
        </select>
        <label class="live" class:is-on={live}><input type="checkbox" bind:checked={live} /> live</label>
        <button class="icon-btn" title="Refresh" onclick={load}>{#if busy}<Loader2 size={14} class="spin" />{:else}<RefreshCw size={14} />{/if}</button>
        <button class="icon-btn" title="Download" disabled={!text} onclick={download}><Download size={14} /></button>
        <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
      </div>
    </header>
    {#if error}<div class="notice notice--error md__err">{error}</div>{/if}
    <div class="log" bind:this={el}>
      {#if truncated}<div class="ln warn">… output cut at 512 KB — choose fewer lines</div>{/if}
      {#if !text && !busy && !error}<div class="ln muted">No output.</div>{/if}
      {#each lines as l, i (i)}<div class="ln {cls(l)}">{l || ' '}</div>{/each}
    </div>
  </div>
</div>

<style>
  .md-mask { position: fixed; inset: 0; z-index: 950; display: flex; align-items: center; justify-content: center; background: rgba(0, 0, 0, 0.5); backdrop-filter: blur(3px); }
  .md { width: min(1100px, calc(100vw - 48px)); height: min(760px, calc(100vh - 64px)); display: flex; flex-direction: column; background: var(--bg-surface); border: 1px solid var(--border); border-radius: 14px; box-shadow: var(--shadow-lg); overflow: hidden; }
  .md__head { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; gap: 12px; }
  .md__title { font-size: 15px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .md__tools { display: flex; align-items: center; gap: 8px; }
  .md__err { margin: 0 18px 10px; }
  .live { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--text-muted); cursor: pointer; }
  .live.is-on { color: var(--success); font-weight: 600; }
  .log { flex: 1; overflow: auto; background: #0d0f12; color: #d4d4d8; font-family: var(--mono); font-size: 12px; line-height: 1.55; padding: 10px 0; }
  .ln { padding: 0 16px; white-space: pre-wrap; word-break: break-word; }
  .ln.bad { color: #f87171; }
  .ln.warn { color: #fbbf24; }
  .ln.muted { color: #71717a; }
</style>
