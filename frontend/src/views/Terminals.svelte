<script lang="ts">
  // Terminal workspace: one tab per session. Always mounted (hidden when another
  // page is shown) so connections survive navigation.
  import { Plus, X, RotateCw, SquareTerminal, Server, Maximize2, Minimize2, FolderOpen } from '@lucide/svelte'
  import { terminals, mountTerminal, closeTab, focusTab, reconnect, type Tab } from '../lib/terminals.svelte'
  import { navigate } from '../lib/router.svelte'

  let { visible }: { visible: boolean } = $props()

  const active = $derived(terminals.tabs.find((t) => t.id === terminals.active) ?? null)

  // Coming back to this page: re-fit and focus the active terminal.
  $effect(() => {
    if (visible && terminals.active) focusTab(terminals.active)
  })

  // ── full screen: just the terminal (tabs + panes) ──
  let page = $state<HTMLDivElement>()
  let full = $state(false)
  /** Fallback when the Fullscreen API isn't available: fill the window. */
  let focusMode = $state(false)

  type KeyboardLock = { lock?: (keys?: string[]) => Promise<void>; unlock?: () => void }
  const keyboard = () => (navigator as Navigator & { keyboard?: KeyboardLock }).keyboard

  async function toggleFull() {
    if (full || focusMode) {
      if (document.fullscreenElement) await document.exitFullscreen().catch(() => {})
      focusMode = false
      return
    }
    try {
      await page!.requestFullscreen({ navigationUI: 'hide' })
      // Let Esc reach the shell (vim!) — hold Esc to leave (Chromium browsers).
      await keyboard()?.lock?.(['Escape']).catch(() => {})
    } catch {
      focusMode = true
    }
  }
  function onFullscreenChange() {
    full = document.fullscreenElement === page
    if (!full) keyboard()?.unlock?.()
    if (terminals.active) focusTab(terminals.active)
  }

  const dot = (t: Tab) => (t.status === 'live' ? 'ok' : t.status === 'connecting' ? 'warn' : t.status === 'error' ? 'err' : 'off')

  function onKeydown(e: KeyboardEvent) {
    if (!visible || !terminals.tabs.length) return
    // Ctrl+Shift+F toggles full screen; Esc leaves the window-filling fallback.
    if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'f') {
      e.preventDefault()
      toggleFull()
      return
    }
    if (focusMode && e.key === 'Escape' && e.shiftKey) focusMode = false
    // Ctrl+Shift+[ / ] switch tabs (plain keys belong to the remote shell).
    if (e.ctrlKey && e.shiftKey && (e.key === '{' || e.key === '}' || e.key === '[' || e.key === ']')) {
      e.preventDefault()
      const i = terminals.tabs.findIndex((t) => t.id === terminals.active)
      const n = terminals.tabs.length
      const next = e.key === '{' || e.key === '[' ? (i - 1 + n) % n : (i + 1) % n
      focusTab(terminals.tabs[next].id)
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />
<svelte:document onfullscreenchange={onFullscreenChange} />

<div class="term-page" class:term-page--hidden={!visible} class:term-page--full={full || focusMode} bind:this={page}>
  {#if terminals.tabs.length}
    <div class="term-tabs" role="tablist">
      {#each terminals.tabs as t (t.id)}
        <div class="term-tab" class:is-active={t.id === terminals.active} role="tab" tabindex="0" aria-selected={t.id === terminals.active}
             onclick={() => focusTab(t.id)} onkeydown={(e) => e.key === 'Enter' && focusTab(t.id)}
             onauxclick={(e) => { if (e.button === 1) closeTab(t.id) }}>
          <span class="term-dot term-dot--{dot(t)}"></span>
          <span class="mono term-tab__label">{t.container ? `${t.container} · ${t.assetName}` : `${t.account}@${t.assetName}`}</span>
          <button class="term-tab__close" title="Close (ends the session)" onclick={(e) => { e.stopPropagation(); closeTab(t.id) }}><X size={12} /></button>
        </div>
      {/each}
      <button class="icon-btn term-tabs__new" title="Connect to another server" onclick={() => navigate('/servers')}><Plus size={15} /></button>
      <div class="term-tabs__status">
        {#if active}
          <span class="muted">{active.message}</span>
          {#if active.status === 'closed' || active.status === 'error'}
            <button class="base-btn base-btn--ghost base-btn--xs" onclick={() => reconnect(active.id)}><RotateCw size={12} /> Reconnect</button>
          {/if}
        {/if}
        {#if active}
          <button class="icon-btn" title="Files on {active.assetName} as {active.account}" onclick={async () => { if (document.fullscreenElement) await document.exitFullscreen().catch(() => {}); focusMode = false; navigate(`/servers/${active.assetId}/files/${active.account}`) }}><FolderOpen size={15} /></button>
        {/if}
        <button class="icon-btn term-full" title={full || focusMode ? 'Exit full screen (Ctrl+Shift+F)' : 'Full screen (Ctrl+Shift+F)'} onclick={toggleFull}>
          {#if full || focusMode}<Minimize2 size={15} />{:else}<Maximize2 size={15} />{/if}
        </button>
      </div>
    </div>
    <div class="term-panes">
      {#each terminals.tabs as t (t.id)}
        <div class="term-pane" class:is-active={t.id === terminals.active} use:mountTerminal={t.id}></div>
      {/each}
    </div>
  {:else}
    <div class="term-empty">
      <SquareTerminal size={34} />
      <div class="term-empty__title">No open terminals</div>
      <div class="muted">Pick a server and connect — sessions open here as tabs, and keep running while you browse.</div>
      <button class="base-btn base-btn--primary" onclick={() => navigate('/servers')}><Server size={14} /> Choose a server</button>
    </div>
  {/if}
</div>

<style>
  .term-page { flex: 1; min-height: 0; display: flex; flex-direction: column; padding: 10px 14px 14px; gap: 0; }
  .term-page--hidden { display: none; }
  /* Full screen: only the terminal, edge to edge. */
  .term-page--full { position: fixed; inset: 0; z-index: 1000; padding: 8px 10px 10px; background: #0e0f11; }
  .term-page--full .term-panes { border-radius: var(--r); border-color: transparent; }
  .term-page--full .term-tabs { padding-bottom: 6px; }
  .term-page:fullscreen { background: #0e0f11; }
  .term-full { margin-left: 4px; }
  .term-tabs { display: flex; align-items: center; gap: 4px; padding: 0 0 8px; overflow-x: auto; }
  .term-tab {
    display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px; border-radius: var(--r);
    background: var(--bg-surface); border: 1px solid var(--border); cursor: pointer; white-space: nowrap;
    color: var(--text-secondary); font-size: 12.5px; transition: background var(--dur) var(--ease), border-color var(--dur) var(--ease);
  }
  .term-tab:hover { background: var(--bg-elevated); }
  .term-tab.is-active { border-color: var(--brand-ring); color: var(--text-primary); background: var(--bg-elevated); }
  .term-tab__label { max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
  .term-tab__close { display: flex; padding: 2px; border-radius: var(--r-xs); color: var(--text-muted); background: none; border: 0; cursor: pointer; }
  .term-tab__close:hover { color: var(--danger); background: var(--danger-bg); }
  .term-tabs__new { flex: 0 0 auto; }
  .term-tabs__status { margin-left: auto; display: flex; align-items: center; gap: 10px; font-size: 12px; white-space: nowrap; padding-left: 12px; }
  .term-dot { width: 7px; height: 7px; border-radius: 50%; flex: 0 0 auto; }
  .term-dot--ok { background: var(--success); box-shadow: 0 0 0 3px var(--success-bg); }
  .term-dot--warn { background: var(--warning); animation: pulse 1s infinite alternate; }
  .term-dot--err { background: var(--danger); }
  .term-dot--off { background: var(--text-muted); }
  @keyframes pulse { to { opacity: 0.35; } }
  .term-panes { position: relative; flex: 1; min-height: 0; border-radius: var(--r-lg); overflow: hidden; background: #0e0f11; border: 1px solid var(--border); }
  .term-pane { position: absolute; inset: 10px 4px 10px 12px; visibility: hidden; }
  .term-pane.is-active { visibility: visible; }
  .term-empty { margin: auto; display: flex; flex-direction: column; align-items: center; gap: 10px; text-align: center; color: var(--text-muted); max-width: 380px; }
  .term-empty__title { font-size: 16px; font-weight: 700; color: var(--text-primary); }
</style>
