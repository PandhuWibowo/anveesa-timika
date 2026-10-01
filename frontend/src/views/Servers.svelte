<script lang="ts">
  // Servers you can reach, one click to a terminal. Admins add / edit them in a
  // drawer ("Test & save" logs in once and pins the host key).
  import { onMount } from 'svelte'
  import { Plus, Search, Server, ShieldCheck, ShieldAlert, Pencil, Trash2, Loader2, SquareTerminal, Activity, ChevronDown, History, FolderOpen } from '@lucide/svelte'
  import { bastion, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { openTerminal, terminals, loadXterm } from '../lib/terminals.svelte'
  import { navigate } from '../lib/router.svelte'
  import type { Asset } from '../gen/timika/v1/bastion_pb'
  import ServerDrawer from '../components/ServerDrawer.svelte'

  let assets = $state<Asset[]>([])
  let canManage = $state(false)
  let loading = $state(true)
  let error = $state('')
  let q = $state('')
  let searchEl = $state<HTMLInputElement>()
  let drawer = $state<{ open: boolean; asset: Asset | null }>({ open: false, asset: null })
  let testing = $state<Record<string, string>>({})
  let picker = $state<string | null>(null)

  async function load() {
    try {
      const r = await bastion.listAssets({})
      assets = r.assets
      canManage = r.canManage
      error = ''
    } catch (e) { error = errMsg(e) } finally { loading = false }
  }
  onMount(() => {
    load()
    loadXterm() // prefetch, so Connect is instant
    searchEl?.focus()
  })

  const shown = $derived.by(() => {
    const n = q.trim().toLowerCase()
    if (!n) return assets
    return assets.filter((a) => [a.name, a.host, a.description, ...a.tags, ...a.allowedAccounts].join(' ').toLowerCase().includes(n))
  })
  const openCount = (a: Asset) => terminals.tabs.filter((t) => t.assetId === a.id && (t.status === 'live' || t.status === 'connecting')).length

  function connect(a: Asset, account?: string) {
    const acc = account ?? (a.allowedAccounts.length === 1 ? a.allowedAccounts[0] : null)
    if (!acc) { picker = picker === a.id ? null : a.id; return }
    picker = null
    openTerminal(a.id, a.name, acc)
  }

  function onSearchKey(e: KeyboardEvent) {
    // Enter on a single match connects straight away.
    if (e.key === 'Enter' && shown.length === 1) connect(shown[0])
    if (e.key === 'Escape') q = ''
  }

  async function test(a: Asset) {
    testing[a.id] = 'Testing…'
    try {
      const r = await bastion.testAsset({ id: a.id })
      testing[a.id] = r.ok ? `✓ ${r.account} logged in (${r.connectMs} ms)` : `✗ ${r.error}`
      if (r.ok) load()
    } catch (e) { testing[a.id] = `✗ ${errMsg(e)}` }
  }

  async function remove(a: Asset) {
    const ok = await confirm({
      title: `Delete ${a.name}?`,
      message: 'Its stored credentials and every access grant are deleted. Past session recordings are kept.',
      subject: `${a.host}:${a.port}`,
      confirmText: 'Delete server',
      variant: 'danger',
    })
    if (!ok) return
    try { await bastion.deleteAsset({ id: a.id }); await load() } catch (e) { error = errMsg(e) }
  }
</script>

<svelte:window onkeydown={(e) => { if (e.key === '/' && document.activeElement?.tagName !== 'INPUT' && document.activeElement?.tagName !== 'TEXTAREA') { e.preventDefault(); searchEl?.focus() } }} />

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Access</div>
          <h1 class="page-title">Servers</h1>
          <p class="page-subtitle">Connect in one click — no keys on your laptop. Credentials stay in the vault, every session is recorded.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{assets.length}</span><span class="page-metric__label">Servers</span></div>
          <div class="page-metric"><span class="page-metric__value">{terminals.tabs.filter((t) => t.status === 'live').length}</span><span class="page-metric__label">Open now</span></div>
        </div>
      </section>

      <div class="srv-bar">
        <div class="srv-search">
          <Search size={15} />
          <input bind:this={searchEl} bind:value={q} onkeydown={onSearchKey} placeholder="Search servers, hosts, tags…   ( / )" />
          {#if q && shown.length === 1}<kbd class="cmdk-kbd">↵ connect</kbd>{/if}
        </div>
        {#if canManage}
          <button class="base-btn base-btn--primary" onclick={() => (drawer = { open: true, asset: null })}><Plus size={14} /> Add server</button>
        {/if}
      </div>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if loading}
        <div class="empty-state"><Loader2 size={20} class="spin" /></div>
      {:else if !assets.length}
        <section class="page-card">
          <div class="empty-state srv-empty">
            <Server size={30} />
            {#if canManage}
              <div class="srv-empty__title">Add your first server</div>
              <div>Host, an account and its password or key. We log in once to check it and pin the host key.</div>
              <button class="base-btn base-btn--primary" onclick={() => (drawer = { open: true, asset: null })}><Plus size={14} /> Add server</button>
            {:else}
              <div class="srv-empty__title">No servers shared with you yet</div>
              <div>An administrator grants access per server.</div>
            {/if}
          </div>
        </section>
      {:else}
        <div class="srv-grid">
          {#each shown as a (a.id)}
            <article class="srv-card">
              <div class="srv-card__top">
                <div class="srv-card__icon"><Server size={17} /></div>
                <div class="srv-card__id">
                  <button class="srv-card__name" title="Sessions & activity" onclick={() => navigate(`/servers/${a.id}`)}>{a.name}</button>
                  <div class="mono srv-card__host">{a.host}:{a.port}</div>
                </div>
                {#if openCount(a)}<span class="badge badge--success"><Activity size={11} /> {openCount(a)} open</span>{/if}
              </div>
              {#if a.description}<p class="srv-card__desc">{a.description}</p>{/if}
              <div class="srv-card__meta">
                {#if a.hostKey}
                  <span class="badge badge--default" title="Pinned host key {a.hostKey}"><ShieldCheck size={11} /> key pinned</span>
                {:else}
                  <span class="badge badge--warning" title="The host key is trusted on the first connection"><ShieldAlert size={11} /> key not verified</span>
                {/if}
                {#each a.tags as t (t)}<span class="badge badge--info">{t}</span>{/each}
              </div>
              {#if testing[a.id]}<div class="srv-card__test" class:srv-card__test--bad={testing[a.id].startsWith('✗')}>{testing[a.id]}</div>{/if}

              <div class="srv-card__actions">
                {#if a.allowedAccounts.length}
                  <div class="srv-connect">
                    <button class="base-btn base-btn--primary base-btn--sm" onclick={() => connect(a)}>
                      <SquareTerminal size={13} />
                      {a.allowedAccounts.length === 1 ? `Connect as ${a.allowedAccounts[0]}` : 'Connect'}
                      {#if a.allowedAccounts.length > 1}<ChevronDown size={12} />{/if}
                    </button>
                    {#if picker === a.id}
                      <div class="srv-picker" role="menu">
                        {#each a.allowedAccounts as acc (acc)}
                          <button class="srv-picker__row mono" role="menuitem" onclick={() => connect(a, acc)}>{acc}</button>
                        {/each}
                      </div>
                    {/if}
                  </div>
                {/if}
                <div class="srv-card__admin">
                  <button class="icon-btn" title="Sessions & activity" onclick={() => navigate(`/servers/${a.id}`)}><History size={14} /></button>
                  {#if a.allowedAccounts.length}<button class="icon-btn" title="Files (SFTP)" onclick={() => navigate(`/servers/${a.id}/files`)}><FolderOpen size={14} /></button>{/if}
                {#if canManage}
                    <button class="icon-btn" title="Test login" onclick={() => test(a)}>
                      {#if testing[a.id] === 'Testing…'}<Loader2 size={14} class="spin" />{:else}<Activity size={14} />{/if}
                    </button>
                    <button class="icon-btn" title="Edit, access & host key" onclick={() => (drawer = { open: true, asset: a })}><Pencil size={14} /></button>
                    <button class="icon-btn" title="Delete" onclick={() => remove(a)}><Trash2 size={14} /></button>
                {/if}
                </div>
              </div>
            </article>
          {:else}
            <div class="empty-state">No servers match “{q}”.</div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
</div>

{#if drawer.open}
  <ServerDrawer asset={drawer.asset} onclose={() => (drawer = { open: false, asset: null })} onsaved={load} />
{/if}

<style>
  .srv-bar { display: flex; gap: 10px; align-items: center; }
  .srv-search {
    flex: 1; display: flex; align-items: center; gap: 8px; padding: 0 12px; height: 34px;
    background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); color: var(--text-muted);
  }
  .srv-search:focus-within { border-color: var(--brand-ring); box-shadow: 0 0 0 3px var(--brand-dim); }
  .srv-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .srv-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 10px; }
  .srv-card {
    display: flex; flex-direction: column; gap: 8px; padding: 12px; border-radius: var(--r-lg);
    background: var(--bg-surface); border: 1px solid var(--border);
    transition: border-color var(--dur) var(--ease), transform var(--dur) var(--ease);
  }
  .srv-card:hover { border-color: var(--brand-ring); }
  .srv-card__top { display: flex; align-items: center; gap: 12px; }
  .srv-card__icon { width: 30px; height: 30px; border-radius: var(--r); display: grid; place-items: center; background: var(--brand-soft); color: var(--brand); flex: 0 0 auto; }
  .srv-card__id { min-width: 0; flex: 1; }
  .srv-card__name { display: block; max-width: 100%; padding: 0; border: 0; background: none; font: inherit; text-align: left; cursor: pointer; font-weight: 700; color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .srv-card__name:hover { color: var(--brand); text-decoration: underline; }
  .srv-card__host { font-size: 12px; color: var(--text-muted); }
  .srv-card__desc { font-size: 12.5px; color: var(--text-secondary); line-height: 1.5; margin: 0; }
  .srv-card__meta { display: flex; gap: 6px; flex-wrap: wrap; }
  .srv-card__test { font-size: 12px; color: var(--success); word-break: break-word; }
  .srv-card__test--bad { color: var(--danger); }
  .srv-card__actions { display: flex; align-items: center; justify-content: space-between; margin-top: auto; padding-top: 4px; }
  .srv-card__admin { display: flex; gap: 2px; margin-left: auto; }
  .srv-connect { position: relative; }
  .srv-picker {
    position: absolute; top: calc(100% + 6px); left: 0; z-index: 20; min-width: 160px; padding: 4px;
    background: var(--bg-elevated); border: 1px solid var(--border); border-radius: var(--r); box-shadow: var(--shadow-md);
  }
  .srv-picker__row { display: block; width: 100%; text-align: left; padding: 7px 10px; border-radius: var(--r-sm); background: none; border: 0; color: var(--text-primary); cursor: pointer; font-size: 12.5px; }
  .srv-picker__row:hover { background: var(--brand-dim); color: var(--brand); }
  .srv-empty { padding: 48px 20px; gap: 10px; }
  .srv-empty__title { font-size: 16px; font-weight: 700; color: var(--text-primary); }
</style>
