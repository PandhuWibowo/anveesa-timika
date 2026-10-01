<script lang="ts">
  import type { Component } from 'svelte'
  import { Search, Lock, SunMoon, CornerDownLeft, SquareTerminal, UserRound } from '@lucide/svelte'
  import { allNav } from '../lib/nav'
  import { isAdmin, canReadVault } from '../lib/session.svelte'
  import { bastion } from '../lib/api'
  import { openTerminal } from '../lib/terminals.svelte'
  import { navigate } from '../lib/router.svelte'
  import { toggleTheme } from '../lib/ui.svelte'

  let { open = $bindable(false), onseal }: { open: boolean; onseal: () => void } = $props()

  type Row = { group: string; label: string; sub: string; ico: Component<{ size?: number }>; run: () => void }

  // "Connect to account@server" for every server you may use (loaded on open).
  let connectRows = $state<Row[]>([])
  async function loadServers() {
    try {
      const r = await bastion.listAssets({})
      connectRows = r.assets.flatMap((a) =>
        a.allowedAccounts.map((acc) => ({
          group: 'Connect',
          label: `${acc}@${a.name}`,
          sub: `${a.host}:${a.port}${a.tags.length ? ' · ' + a.tags.join(', ') : ''}`,
          ico: SquareTerminal,
          run: () => openTerminal(a.id, a.name, acc),
        })),
      )
    } catch { connectRows = [] }
  }

  const baseRows: Row[] = [
    ...allNav.filter((n) => (!n.admin || isAdmin()) && (!n.vault || canReadVault())).map((n) => ({ group: 'Go to', label: n.label, sub: n.hint, ico: n.ico, run: () => navigate(n.to) })),
    ...(isAdmin() ? [{ group: 'Actions', label: 'Seal vault', sub: 'drop in-memory keys now', ico: Lock, run: () => onseal() }] : []),
    { group: 'Go to', label: 'My account', sub: 'password and two-factor authentication', ico: UserRound, run: () => navigate('/account') },
    { group: 'Actions', label: 'Toggle theme', sub: 'dark / light', ico: SunMoon, run: () => toggleTheme() },
  ]

  let q = $state('')
  let active = $state(0)
  let input = $state<HTMLInputElement>()

  const results = $derived.by(() => {
    const needle = q.trim().toLowerCase()
    const rows = [...baseRows, ...connectRows]
    return needle ? rows.filter((r) => `${r.group} ${r.label} ${r.sub}`.toLowerCase().includes(needle)) : rows
  })

  $effect(() => {
    if (open) {
      loadServers()
      q = ''
      active = 0
      queueMicrotask(() => input?.focus())
    }
  })

  function pick(r: Row | undefined) {
    if (!r) return
    open = false
    r.run()
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') open = false
    else if (e.key === 'ArrowDown') { e.preventDefault(); active = Math.min(active + 1, results.length - 1) }
    else if (e.key === 'ArrowUp') { e.preventDefault(); active = Math.max(active - 1, 0) }
    else if (e.key === 'Enter') { e.preventDefault(); pick(results[active]) }
  }
</script>

{#if open}
  <div class="cmdk-mask" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) open = false }}>
    <div class="cmdk" role="dialog" aria-modal="true">
      <div class="cmdk__input-row">
        <Search size={15} class="cmdk__icon" />
        <input class="cmdk__input" bind:this={input} bind:value={q} placeholder="Jump to a page, connect to a server, or run an action…"
               oninput={() => (active = 0)} onkeydown={onKeydown} />
        <kbd class="cmdk__kbd">esc</kbd>
      </div>
      <div class="cmdk__results">
        {#each results as r, i (r.label)}
          {#if i === 0 || results[i - 1].group !== r.group}
            <div class="cmdk__group-label">{r.group}</div>
          {/if}
          <button class="cmdk__row" class:is-active={i === active} onmouseenter={() => (active = i)} onclick={() => pick(r)}>
            <span class="cmdk__row-icon"><r.ico size={15} /></span>
            <span class="cmdk__row-text">
              <span class="cmdk__row-label">{r.label}</span>
              <span class="cmdk__row-sub">{r.sub}</span>
            </span>
            {#if i === active}<CornerDownLeft size={13} class="cmdk__row-go" />{/if}
          </button>
        {:else}
          <div class="cmdk__hint">No matches.</div>
        {/each}
      </div>
      <div class="cmdk__footer"><span>↑↓ navigate</span><span>↵ open</span><span>esc close</span></div>
    </div>
  </div>
{/if}

<style>
  .cmdk-mask {
    position: fixed; inset: 0; background: rgba(0, 0, 0, 0.5); backdrop-filter: blur(2px);
    display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 300;
  }
  .cmdk {
    width: min(560px, 92vw); max-height: 70vh; display: flex; flex-direction: column;
    background: var(--bg-elevated); border: 1px solid var(--border-2); border-radius: var(--r-lg);
    box-shadow: var(--shadow-lg); overflow: hidden;
  }
  .cmdk__input-row { display: flex; align-items: center; gap: 10px; padding: 12px 16px; border-bottom: 1px solid var(--border); }
  :global(.cmdk__icon) { color: var(--text-muted); }
  .cmdk__input { flex: 1; background: none; border: none; outline: none; color: var(--text-primary); font-size: 14px; font-family: inherit; }
  .cmdk__input::placeholder { color: var(--text-muted); }
  .cmdk__kbd { font-family: var(--mono); font-size: 10.5px; color: var(--text-muted); border: 1px solid var(--border-2); border-radius: 4px; padding: 1px 5px; background: var(--bg-body); }
  .cmdk__results { flex: 1; overflow-y: auto; padding: 6px; min-height: 80px; }
  .cmdk__group-label { padding: 8px 10px 4px; font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.6px; color: var(--text-muted); }
  .cmdk__row {
    display: flex; align-items: center; gap: 10px; width: 100%; padding: 8px 10px; border: none; background: none;
    border-radius: var(--r-sm); cursor: pointer; text-align: left; color: var(--text-secondary); font-size: 13px; font-family: inherit;
  }
  .cmdk__row.is-active { background: var(--bg-hover); color: var(--text-primary); outline: 1px solid var(--brand-ring); }
  .cmdk__row-icon { width: 20px; display: inline-flex; align-items: center; justify-content: center; opacity: 0.9; }
  .cmdk__row-text { display: flex; flex-direction: column; min-width: 0; flex: 1; }
  .cmdk__row-label { font-weight: 500; }
  .cmdk__row-sub { font-size: 11px; color: var(--text-muted); font-family: var(--mono); }
  :global(.cmdk__row-go) { color: var(--text-muted); }
  .cmdk__hint { padding: 8px 12px; font-size: 12px; color: var(--text-muted); }
  .cmdk__footer { display: flex; gap: 16px; padding: 8px 14px; border-top: 1px solid var(--border); font-size: 11px; color: var(--text-muted); }
</style>
