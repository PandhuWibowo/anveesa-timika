<script lang="ts">
  // One server's story: who connected and when (sessions, replayable), every
  // audited action about it (config, access, tests, refused connections) and
  // who can reach it now.
  import { onMount } from 'svelte'
  import { ArrowLeft, SquareTerminal, Pencil, ShieldCheck, ShieldAlert, ChevronDown, Loader2, ExternalLink, UserRound, Users, Activity, Search, Play, RefreshCw } from '@lucide/svelte'
  import { bastion, audit, errMsg, isNetworkError } from '../lib/api'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { openTerminal, loadXterm } from '../lib/terminals.svelte'
  import { describe, outcome } from '../lib/auditText'
  import type { Asset, Grant, Session, Command } from '../gen/timika/v1/bastion_pb'
  import type { AuditEvent } from '../gen/timika/v1/audit_pb'
  import SessionsTable from '../components/SessionsTable.svelte'
  import ServerDrawer from '../components/ServerDrawer.svelte'
  import Replay from '../components/Replay.svelte'
  import FilesBrowser from '../components/FilesBrowser.svelte'

  let { id, tab: initialTab = '', account: initialAccount = '' }: { id: string; tab?: string; account?: string } = $props()

  let asset = $state<Asset | null>(null)
  let sessions = $state<Session[]>([])
  let events = $state<AuditEvent[]>([])
  let grants = $state<Grant[]>([])
  let loading = $state(true)
  let error = $state('')
  type Tab = 'commands' | 'files' | 'sessions' | 'activity' | 'access'
  // svelte-ignore state_referenced_locally
  let tab = $state<Tab>((['commands', 'files', 'sessions', 'activity', 'access'] as Tab[]).includes(initialTab as Tab) ? (initialTab as Tab) : 'commands')
  let commands = $state<Command[]>([])
  let cmdQuery = $state('')
  let riskyOnly = $state(false)
  let cmdTruncated = $state(false)
  let replay = $state<{ session: Session; at: number } | null>(null)
  let failedOnly = $state(false)
  let picker = $state(false)
  let editing = $state(false)
  const admin = isAdmin()

  let offline = $state(false)
  let updated = $state<Date | null>(null)
  let refreshing = $state(false)

  async function load() {
    refreshing = true
    try {
      const [a, s] = await Promise.all([bastion.listAssets({}), bastion.listSessions({})])
      asset = a.assets.find((x) => x.id === id) ?? null
      sessions = s.sessions.filter((x) => x.asset === id)
      await loadCommands()
      if (admin) {
        const [ev, g] = await Promise.all([
          audit.listEvents({ server: id, limit: 200, outcome: failedOnly ? 'error' : '' }),
          bastion.listGrants({ id }),
        ])
        events = ev.events
        grants = g.grants
      }
      error = asset ? '' : `No server "${id}" — it was deleted, or you have no access to it.`
      offline = false
      updated = new Date()
    } catch (e) {
      // A blip (server restarting, network) isn't an error to keep on screen:
      // say so quietly and keep the last data until the next refresh works.
      if (isNetworkError(e)) offline = true
      else error = errMsg(e)
    } finally { loading = false; refreshing = false }
  }
  onMount(() => {
    load()
    loadXterm()
    const t = setInterval(() => { if (!document.hidden) load() }, 5000)
    // Coming back to the tab: refresh right away.
    const onVisible = () => { if (!document.hidden) load() }
    document.addEventListener('visibilitychange', onVisible)
    return () => { clearInterval(t); document.removeEventListener('visibilitychange', onVisible) }
  })

  async function loadCommands() {
    const r = await bastion.listCommands({ asset: id, query: cmdQuery, riskyOnly, limit: 500 })
    commands = r.commands.sort((a, b) => b.time.localeCompare(a.time))
    cmdTruncated = r.truncated
  }
  let cmdTimer: ReturnType<typeof setTimeout> | undefined
  function onCmdSearch() {
    clearTimeout(cmdTimer)
    cmdTimer = setTimeout(() => loadCommands().catch((e) => (error = errMsg(e))), 250)
  }
  function replayAt(c: Command) {
    const s = sessions.find((x) => x.id === c.session)
    if (s) replay = { session: s, at: c.offset }
  }
  const riskCount = $derived(commands.filter((c) => c.risk === 'high').length)

  const live = $derived(sessions.filter((s) => s.status === 'live').length)
  const people = $derived(new Set(sessions.map((s) => s.user)).size)
  const lastAccess = $derived(sessions[0]?.startedAt)
  const failures = $derived(events.filter((e) => !e.ok).length)

  function connect(account?: string) {
    if (!asset) return
    const acc = account ?? (asset.allowedAccounts.length === 1 ? asset.allowedAccounts[0] : null)
    if (!acc) { picker = !picker; return }
    picker = false
    openTerminal(asset.id, asset.name, acc)
  }

  const ago = (t?: string) => {
    if (!t) return 'never'
    const s = (Date.now() - new Date(t).getTime()) / 1000
    if (s < 60) return 'just now'
    if (s < 3600) return `${Math.floor(s / 60)} min ago`
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`
    return `${Math.floor(s / 86400)} d ago`
  }
  const expiry = (g: Grant) => (g.expiresAt ? (g.expired ? 'expired' : `until ${new Date(g.expiresAt).toLocaleString()}`) : 'permanent')

  // Group the activity timeline by day.
  const days = $derived.by(() => {
    const out: { day: string; items: AuditEvent[] }[] = []
    for (const e of events) {
      const day = new Date(e.time).toDateString()
      if (out.at(-1)?.day !== day) out.push({ day, items: [] })
      out.at(-1)!.items.push(e)
    }
    return out
  })
  const dayLabel = (d: string) => (d === new Date().toDateString() ? 'Today' : d === new Date(Date.now() - 864e5).toDateString() ? 'Yesterday' : d)
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <button class="base-btn base-btn--ghost base-btn--sm sd-back" onclick={() => navigate('/servers')}><ArrowLeft size={13} /> Servers</button>

      {#if loading}
        <div class="empty-state"><Loader2 size={18} class="spin" /></div>
      {:else if !asset}
        <div class="notice notice--error">{error}</div>
      {:else}
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">Server</div>
            <h1 class="page-title">{asset.name}</h1>
            <p class="page-subtitle">
              <span class="mono">{asset.host}:{asset.port}</span>
              {#if asset.hostKey}<span class="badge badge--default" title={asset.hostKey}><ShieldCheck size={11} /> key pinned</span>
              {:else}<span class="badge badge--warning"><ShieldAlert size={11} /> key not verified</span>{/if}
              {#each asset.tags as t (t)}<span class="badge badge--info">{t}</span>{/each}
              {#if asset.description}<br />{asset.description}{/if}
            </p>
          </div>
          <div class="page-hero__actions sd-actions">
            {#if asset.allowedAccounts.length}
              <div class="sd-connect">
                <button class="base-btn base-btn--primary" onclick={() => connect()}>
                  <SquareTerminal size={14} /> {asset.allowedAccounts.length === 1 ? `Connect as ${asset.allowedAccounts[0]}` : 'Connect'}
                  {#if asset.allowedAccounts.length > 1}<ChevronDown size={12} />{/if}
                </button>
                {#if picker}
                  <div class="sd-picker">
                    {#each asset.allowedAccounts as acc (acc)}<button class="mono" onclick={() => connect(acc)}>{acc}</button>{/each}
                  </div>
                {/if}
              </div>
            {/if}
            {#if admin}<button class="base-btn base-btn--ghost" onclick={() => (editing = true)}><Pencil size={14} /> Edit</button>{/if}
          </div>
        </section>

        <div class="sd-metrics">
          <div class="sd-metric"><span>{sessions.length}</span><small>Sessions</small></div>
          <div class="sd-metric"><span class:sd-live={live > 0}>{live}</span><small>Live now</small></div>
          <div class="sd-metric"><span>{people}</span><small>People connected</small></div>
          <div class="sd-metric"><span>{ago(lastAccess)}</span><small>Last access</small></div>
          {#if admin}<div class="sd-metric"><span class:sd-bad={failures > 0}>{failures}</span><small>Failed actions</small></div>{/if}
        </div>

        <section class="page-card">
          <div class="page-tabs sd-tabs">
            <button class="page-tab" class:is-active={tab === 'commands'} onclick={() => (tab = 'commands')}>Commands <span class="sd-count">{commands.length}{cmdTruncated ? '+' : ''}</span></button>
            {#if asset.allowedAccounts.length}
              <button class="page-tab" class:is-active={tab === 'files'} onclick={() => (tab = 'files')}>Files</button>
            {/if}
            <button class="page-tab" class:is-active={tab === 'sessions'} onclick={() => (tab = 'sessions')}>Sessions <span class="sd-count">{sessions.length}</span></button>
            {#if admin}
              <button class="page-tab" class:is-active={tab === 'activity'} onclick={() => (tab = 'activity')}>Activity <span class="sd-count">{events.length}</span></button>
              <button class="page-tab" class:is-active={tab === 'access'} onclick={() => (tab = 'access')}>Who has access <span class="sd-count">{grants.length}</span></button>
            {/if}
          </div>

          {#if offline}<div class="notice notice--warning sd-offline"><Loader2 size={13} class="spin" /> Reconnecting to the server… showing what was loaded {updated ? updated.toLocaleTimeString() : 'earlier'}.</div>{/if}
          {#if error}<div class="notice notice--error">{error}</div>{/if}

          {#if tab === 'commands'}
            <div class="sd-bar">
              <div class="sd-search"><Search size={14} /><input bind:value={cmdQuery} oninput={onCmdSearch} placeholder="Search commands…" /></div>
              <label class="sd-toggle"><input type="checkbox" bind:checked={riskyOnly} onchange={() => loadCommands()} /> risky only {#if riskCount}<span class="badge badge--danger">{riskCount} high</span>{/if}</label>
              <button class="icon-btn sd-refresh" title={updated ? `Updated ${updated.toLocaleTimeString()} — refreshes every 5 s` : 'Refresh'} onclick={load}>
                <RefreshCw size={13} class={refreshing ? 'spin' : ''} />
              </button>
            </div>
            <div class="data-table-wrap">
              <table class="data-table">
                <thead><tr><th>When</th><th>User</th><th>Account</th><th>Command</th><th></th></tr></thead>
                <tbody>
                  {#each commands as c, i (i)}
                    <tr class:sd-cmd--high={c.risk === 'high'}>
                      <td class="sd-when" title={new Date(c.time).toLocaleString()}>{new Date(c.time).toLocaleString()}</td>
                      <td class="strong">{c.user}</td>
                      <td class="mono">{c.account}</td>
                      <td class="mono sd-cmd">
                        {#if c.risk}<span class="badge badge--{c.risk === 'high' ? 'danger' : 'warning'}">{c.risk}</span>{/if}
                        <span title={c.source === 'screen' ? 'as shown after tab completion / history recall' : 'as typed'}>{c.command}</span>
                      </td>
                      <td class="sd-cmd-act"><button class="icon-btn" title="Replay from here" onclick={() => replayAt(c)}><Play size={13} /></button></td>
                    </tr>
                  {:else}
                    <tr><td colspan="5"><div class="empty-state">{cmdQuery || riskyOnly ? 'No commands match.' : 'No commands yet — they appear here as people work in a terminal on this server.'}</div></td></tr>
                  {/each}
                </tbody>
              </table>
            </div>
            {#if cmdTruncated}<div class="muted sd-note">Showing the newest 500 — search to narrow down.</div>{/if}

          {:else if tab === 'files'}
            <FilesBrowser {asset} {initialAccount} />

          {:else if tab === 'sessions'}
            <SessionsTable sessions={sessions} showServer={false} onchanged={load} empty={admin ? 'Nobody has connected to this server yet.' : 'You have no sessions on this server yet.'} />

          {:else if tab === 'activity'}
            <div class="sd-bar">
              <label class="sd-toggle"><input type="checkbox" bind:checked={failedOnly} onchange={load} /> failures only</label>
              <button class="base-btn base-btn--ghost base-btn--xs" onclick={() => navigate(`/audit/server/${id}`)}><ExternalLink size={12} /> Open in Audit trail</button>
            </div>
            <div class="tl">
              {#each days as d (d.day)}
                <div class="tl__day">{dayLabel(d.day)}</div>
                {#each d.items as e (e.seq)}
                  {@const lbl = describe(e)}
                  <div class="tl__item" class:tl__item--bad={!e.ok}>
                    <span class="tl__time" title={new Date(e.time).toLocaleString()}>{new Date(e.time).toLocaleTimeString()}</span>
                    <span class="tl__dot tl__dot--{lbl.category}" class:tl__dot--bad={!e.ok}></span>
                    <div class="tl__text">
                      <div>
                        <b>{e.user ?? 'someone'}</b> · {lbl.label}
                        {#if e.target}<span class="mono tl__target">{e.target}</span>{/if}
                        {#if !e.ok}<span class="badge badge--danger">{outcome(e)}</span>{/if}
                      </div>
                      <div class="muted tl__sub">{e.remoteAddr ?? ''}{!e.ok && e.error ? ` · ${e.error}` : ''}</div>
                    </div>
                  </div>
                {/each}
              {:else}
                <div class="empty-state"><Activity size={20} /> No activity recorded for this server on this instance.</div>
              {/each}
            </div>

          {:else}
            <p class="muted sd-note">Administrators can always connect. Others reach this server through these grants.</p>
            <div class="sd-grants">
              {#each grants as g (g.id)}
                <div class="sd-grant" class:sd-grant--expired={g.expired}>
                  {#if g.subjectType === 'user'}<UserRound size={14} />{:else}<Users size={14} />{/if}
                  <div class="sd-grant__text">
                    <div><b>{g.subject}</b> <span class="muted">{g.subjectType === 'role' ? 'everyone with this role' : 'user'}</span></div>
                    <div class="muted sd-grant__sub">{g.accounts.length ? g.accounts.join(', ') : 'all accounts'} · {expiry(g)} · granted by {g.createdBy} {ago(g.createdAt)}</div>
                  </div>
                  {#if g.subjectType === 'user'}<button class="icon-btn" title="Their activity" onclick={() => navigate(`/audit/${g.subject}`)}><ExternalLink size={13} /></button>{/if}
                </div>
              {:else}
                <div class="empty-state">Only administrators can connect.</div>
              {/each}
            </div>
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (editing = true)}>Manage access</button>
          {/if}
        </section>
      {/if}
    </div>
  </div>
</div>

{#if replay}
  <Replay session={replay.session} startAt={replay.at} onclose={() => (replay = null)} />
{/if}

{#if editing && asset}
  <ServerDrawer asset={asset} onclose={() => (editing = false)} onsaved={load} />
{/if}

<style>
  .sd-back { align-self: flex-start; }
  .sd-actions { display: flex; gap: 8px; align-items: flex-start; }
  .sd-connect { position: relative; }
  .sd-picker { position: absolute; top: calc(100% + 6px); right: 0; z-index: 20; min-width: 160px; padding: 4px; background: var(--bg-elevated); border: 1px solid var(--border); border-radius: var(--r); box-shadow: var(--shadow-md); }
  .sd-picker button { display: block; width: 100%; text-align: left; padding: 7px 10px; border-radius: var(--r-sm); background: none; border: 0; color: var(--text-primary); cursor: pointer; font-size: 12.5px; }
  .sd-picker button:hover { background: var(--brand-dim); color: var(--brand); }
  .page-subtitle :global(.badge) { margin-left: 6px; }
  .sd-metrics { display: grid; grid-template-columns: repeat(auto-fit, minmax(130px, 1fr)); gap: 8px; }
  .sd-metric { padding: 8px 12px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); display: flex; flex-direction: column; gap: 2px; }
  .sd-metric span { font-size: 16px; font-weight: 700; color: var(--text-primary); }
  .sd-metric small { font-size: 10px; color: var(--text-muted); text-transform: uppercase; letter-spacing: .04em; }
  .sd-live { color: var(--success) !important; }
  .sd-bad { color: var(--danger) !important; }
  .sd-tabs { padding: 8px 12px 0; }
  .sd-count { font-size: 10.5px; padding: 1px 6px; border-radius: 10px; background: var(--bg-elevated); margin-left: 4px; }
  .sd-bar { display: flex; justify-content: space-between; align-items: center; padding: 8px 12px; }
  .sd-search { flex: 1; display: flex; align-items: center; gap: 8px; color: var(--text-muted); margin-right: 12px; }
  .sd-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 13px; }
  .sd-when { white-space: nowrap; font-size: 12px; color: var(--text-muted); }
  .sd-cmd { word-break: break-all; color: var(--text-primary); }
  .sd-cmd :global(.badge) { margin-right: 6px; }
  .sd-cmd--high td { background: var(--danger-bg); }
  .sd-cmd-act { width: 40px; text-align: right; }
  .sd-offline { display: flex; align-items: center; gap: 8px; margin: 8px 12px 0; }
  .sd-refresh { margin-left: 6px; }
  .sd-toggle { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--text-muted); cursor: pointer; }
  .tl { padding: 0 14px 14px; }
  .tl__day { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: .05em; color: var(--text-muted); padding: 14px 0 6px; }
  .tl__item { display: grid; grid-template-columns: 80px 14px 1fr; gap: 10px; align-items: start; padding: 7px 8px; border-radius: var(--r-sm); font-size: 13px; }
  .tl__item:hover { background: var(--bg-hover); }
  .tl__item--bad { background: var(--danger-bg); }
  .tl__time { font-family: var(--mono); font-size: 11.5px; color: var(--text-muted); padding-top: 1px; }
  .tl__dot { width: 8px; height: 8px; border-radius: 50%; margin-top: 5px; background: var(--text-muted); }
  .tl__dot--servers { background: var(--brand); }
  .tl__dot--sessions { background: #c79bf2; }
  .tl__dot--bad { background: var(--danger); }
  .tl__text { min-width: 0; }
  .tl__target { font-size: 11.5px; color: var(--text-secondary); margin-left: 4px; }
  .tl__text :global(.badge) { margin-left: 6px; }
  .tl__sub { font-size: 11.5px; }
  .sd-note { padding: 10px 14px 0; margin: 0; font-size: 12.5px; }
  .sd-grants { display: flex; flex-direction: column; gap: 6px; padding: 10px 14px; }
  .sd-grant { display: flex; align-items: center; gap: 10px; padding: 9px 10px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); color: var(--brand); }
  .sd-grant--expired { opacity: 0.55; }
  .sd-grant__text { flex: 1; min-width: 0; font-size: 13px; color: var(--text-primary); }
  .sd-grant__sub { font-size: 11.5px; }
  .page-card > .base-btn { margin: 0 14px 14px; }
</style>
