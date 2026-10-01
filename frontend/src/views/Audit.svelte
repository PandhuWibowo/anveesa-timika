<script lang="ts">
  // Audit trail: who did what, to what, from where, and whether it worked.
  // `#/audit/<user>` opens it filtered to one person.
  import { onMount } from 'svelte'
  import { Search, ShieldCheck, ShieldAlert, Download, Loader2, Radio, ChevronDown, ChevronRight, X } from '@lucide/svelte'
  import { audit, errMsg } from '../lib/api'
  import { router, navigate } from '../lib/router.svelte'
  import { describe, outcome, type Category } from '../lib/auditText'
  import type { AuditEvent } from '../gen/timika/v1/audit_pb'

  const CATS: { id: '' | Category; label: string }[] = [
    { id: '', label: 'All' }, { id: 'signin', label: 'Sign-in' }, { id: 'secrets', label: 'Secrets' }, { id: 'servers', label: 'Servers' },
    { id: 'sessions', label: 'Sessions' }, { id: 'files', label: 'Files' }, { id: 'automation', label: 'Automation' }, { id: 'users', label: 'Users' }, { id: 'system', label: 'System' },
  ]

  let events = $state<AuditEvent[]>([])
  let next = $state<bigint | undefined>(undefined)
  let stored = $state(true)
  let instance = $state('')
  let loading = $state(true)
  let more = $state(false)
  let error = $state('')
  let q = $state('')
  let category = $state<'' | Category>('')
  let outcomeF = $state('')
  let routine = $state(false)
  let live = $state(true)
  let open = $state<bigint | null>(null)
  let verify = $state<{ ok: boolean; text: string } | null>(null)
  let verifying = $state(false)

  // #/audit/<user> or #/audit/server/<id>
  const parts = $derived(router.path.split('/').map(decodeURIComponent))
  const server = $derived(parts[2] === 'server' ? parts[3] ?? '' : '')
  const user = $derived(parts[2] && parts[2] !== 'server' ? parts[2] : '')

  const req = (before?: bigint) => ({ limit: 100, beforeSeq: before, user, server, query: q, category, outcome: outcomeF, includeRoutine: routine })

  async function load() {
    try {
      const r = await audit.listEvents(req())
      events = r.events
      next = r.nextBeforeSeq
      stored = r.stored
      instance = r.instance
      error = ''
    } catch (e) { error = errMsg(e) } finally { loading = false }
  }
  async function loadMore() {
    if (!next) return
    more = true
    try {
      const r = await audit.listEvents(req(next))
      events = [...events, ...r.events]
      next = r.nextBeforeSeq
    } catch (e) { error = errMsg(e) } finally { more = false }
  }

  // Reload when a filter changes (search debounced).
  let timer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    void [user, server, category, outcomeF, routine]
    load()
  })
  function onSearch() {
    clearTimeout(timer)
    timer = setTimeout(load, 250)
  }

  // Live: newest page every 10s while visible, unless you're paging back.
  onMount(() => {
    const t = setInterval(() => { if (live && !document.hidden && events.length <= 100) load() }, 10000)
    return () => clearInterval(t)
  })

  async function runVerify() {
    verifying = true
    try {
      const v = await audit.verify({})
      verify = v.ok
        ? { ok: true, text: `Hash chain intact — ${v.entries} entries, nothing edited, removed or reordered.` }
        : { ok: false, text: `${v.problems.length} problem(s): ${v.problems.slice(0, 3).join(' · ')}` }
    } catch (e) { verify = { ok: false, text: errMsg(e) } } finally { verifying = false }
  }

  function exportCsv() {
    const esc = (s: unknown) => `"${String(s ?? '').replace(/"/g, '""')}"`
    const rows = [['time', 'user', 'action', 'target', 'result', 'from', 'error', 'duration_ms', 'request_id', 'instance']]
    for (const e of events) rows.push([e.time, e.user ?? '', describe(e).label, e.target ?? '', outcome(e), e.remoteAddr ?? '', e.error ?? '', String(e.durationMs), e.id, e.instance])
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([rows.map((r) => r.map(esc).join(',')).join('\n')], { type: 'text/csv' }))
    a.download = `timika-audit-${new Date().toISOString().slice(0, 19)}.csv`
    a.click()
    URL.revokeObjectURL(a.href)
  }

  const when = (t: string) => {
    const d = new Date(t)
    const s = (Date.now() - d.getTime()) / 1000
    if (s < 60) return 'just now'
    if (s < 3600) return `${Math.floor(s / 60)} min ago`
    return d.toDateString() === new Date().toDateString() ? d.toLocaleTimeString() : d.toLocaleString()
  }
  const catOf = (e: AuditEvent) => describe(e).category
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Access</div>
          <h1 class="page-title">Audit trail</h1>
          <p class="page-subtitle">Every sign-in, secret access, server change and terminal session — tamper-evident, never containing secret values.</p>
        </div>
        <div class="page-hero__actions aud-actions">
          <button class="base-btn base-btn--ghost base-btn--sm" onclick={runVerify} disabled={verifying}>
            {#if verifying}<Loader2 size={13} class="spin" />{:else}<ShieldCheck size={13} />{/if} Verify integrity
          </button>
          <button class="base-btn base-btn--ghost base-btn--sm" onclick={exportCsv} disabled={!events.length}><Download size={13} /> Export CSV</button>
        </div>
      </section>

      {#if verify}
        <div class="notice notice--{verify.ok ? 'success' : 'error'}">
          {#if verify.ok}<ShieldCheck size={14} />{:else}<ShieldAlert size={14} />{/if} {verify.text}
          <button class="icon-btn" title="Dismiss" onclick={() => (verify = null)}><X size={13} /></button>
        </div>
      {/if}
      {#if !stored}
        <div class="notice notice--warning">This instance writes no local audit file (AUDIT_FILE is off) — its trail is only in syslog.</div>
      {/if}

      <section class="page-card">
        <div class="aud-filters">
          <div class="aud-search"><Search size={14} /><input bind:value={q} oninput={onSearch} placeholder="Search action, target, IP, error…" /></div>
          {#if user}
            <span class="badge badge--info aud-user">user: {user} <button title="Clear" onclick={() => navigate('/audit')}><X size={11} /></button></span>
          {/if}
          {#if server}
            <span class="badge badge--info aud-user">server: {server} <button title="Clear" onclick={() => navigate('/audit')}><X size={11} /></button></span>
          {/if}
          <div class="seg">
            {#each CATS as c (c.id)}<button class:is-on={category === c.id} onclick={() => (category = c.id)}>{c.label}</button>{/each}
          </div>
          <select class="base-select" bind:value={outcomeF}>
            <option value="">Any result</option><option value="ok">Succeeded</option><option value="error">Failed</option>
          </select>
          <label class="aud-toggle" title="Session checks, list views and polling"><input type="checkbox" bind:checked={routine} /> routine reads</label>
          <label class="aud-toggle" class:aud-toggle--on={live} title="Refresh every 10 s"><input type="checkbox" bind:checked={live} /> <Radio size={12} /> live</label>
        </div>
        {#if error}<div class="notice notice--error">{error}</div>{/if}
        {#if loading}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <div class="data-table-wrap">
            <table class="data-table aud">
              <thead><tr><th></th><th>When</th><th>Who</th><th>What</th><th>Target</th><th>Result</th><th>From</th></tr></thead>
              <tbody>
                {#each events as e (`${e.instance}-${e.seq}`)}
                  <tr class="aud__row" class:aud__row--bad={!e.ok} onclick={() => (open = open === e.seq ? null : e.seq)}>
                    <td class="aud__chev">{#if open === e.seq}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}</td>
                    <td title={new Date(e.time).toLocaleString()} class="aud__when">{when(e.time)}</td>
                    <td>{#if e.user}<button class="aud__link" onclick={(ev) => { ev.stopPropagation(); navigate(`/audit/${e.user}`) }}>{e.user}</button>{:else}<span class="muted">—</span>{/if}</td>
                    <td><span class="aud__cat aud__cat--{catOf(e)}"></span>{describe(e).label}</td>
                    <td class="mono aud__target">{e.target ?? ''}</td>
                    <td>{#if e.ok}<span class="badge badge--success">ok</span>{:else}<span class="badge badge--danger" title={e.error}>{outcome(e)}</span>{/if}</td>
                    <td class="mono muted">{e.remoteAddr ?? ''}</td>
                  </tr>
                  {#if open === e.seq}
                    <tr class="aud__detail"><td></td><td colspan="6">
                      <div class="aud__grid">
                        <span>Action</span><span class="mono">{e.action}</span>
                        {#if e.error}<span>Error</span><span>{e.error}</span>{/if}
                        <span>Roles</span><span>{e.policies.join(', ') || '—'}</span>
                        {#if e.kind === 'call'}<span>Duration</span><span>{e.durationMs} ms</span>{/if}
                        <span>Time</span><span>{new Date(e.time).toLocaleString()} <span class="muted">({e.time})</span></span>
                        <span>Request id</span><span class="mono">{e.id}</span>
                        <span>Instance · seq</span><span class="mono">{e.instance} · #{e.seq}</span>
                        {#if e.details}
                          <span>Session</span><span class="mono aud__json">{JSON.stringify(e.details)}</span>
                        {/if}
                      </div>
                    </td></tr>
                  {/if}
                {:else}
                  <tr><td colspan="7"><div class="empty-state">No events match.</div></td></tr>
                {/each}
              </tbody>
            </table>
          </div>
          <div class="aud-foot">
            <span class="muted">{events.length} events · from instance <span class="mono">{instance}</span>{routine ? '' : ' · routine reads hidden'}</span>
            {#if next}<button class="base-btn base-btn--ghost base-btn--sm" onclick={loadMore} disabled={more}>{#if more}<Loader2 size={13} class="spin" />{/if} Load older</button>{/if}
          </div>
        {/if}
      </section>
    </div>
  </div>
</div>

<style>
  .aud-actions { display: flex; gap: 6px; }
  .aud-filters { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; padding: 12px 14px; border-bottom: 1px solid var(--border); }
  .aud-search { flex: 1; min-width: 200px; display: flex; align-items: center; gap: 8px; color: var(--text-muted); }
  .aud-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 13px; }
  .aud-user { display: inline-flex; align-items: center; gap: 4px; }
  .aud-user button { display: flex; background: none; border: 0; color: inherit; cursor: pointer; padding: 0; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .seg button { padding: 5px 9px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .aud-toggle { display: flex; align-items: center; gap: 5px; font-size: 12px; color: var(--text-muted); cursor: pointer; white-space: nowrap; }
  .aud-toggle--on { color: var(--brand); }
  .aud__row { cursor: pointer; }
  .aud__row--bad td { background: var(--danger-bg); }
  .aud__chev { width: 18px; color: var(--text-muted); }
  .aud__when { white-space: nowrap; }
  .aud__target { font-size: 12px; color: var(--text-secondary); max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .aud__link { background: none; border: 0; padding: 0; color: var(--text-primary); font-weight: 600; cursor: pointer; }
  .aud__link:hover { color: var(--brand); text-decoration: underline; }
  .aud__cat { display: inline-block; width: 6px; height: 6px; border-radius: 50%; margin-right: 8px; vertical-align: middle; background: var(--text-muted); }
  .aud__cat--signin { background: var(--info); }
  .aud__cat--secrets { background: var(--warning); }
  .aud__cat--servers { background: var(--brand); }
  .aud__cat--sessions { background: #c79bf2; }
  .aud__cat--files { background: var(--info); }
  .aud__cat--automation { background: var(--success); }
  .aud__cat--users { background: var(--danger); }
  .aud__detail td { background: var(--bg-body); }
  .aud__grid { display: grid; grid-template-columns: 120px 1fr; gap: 6px 14px; font-size: 12.5px; padding: 4px 0; }
  .aud__grid > span:nth-child(odd) { color: var(--text-muted); }
  .aud__json { word-break: break-all; font-size: 11.5px; }
  .aud-foot { display: flex; justify-content: space-between; align-items: center; padding: 10px 14px; font-size: 12px; }
</style>
