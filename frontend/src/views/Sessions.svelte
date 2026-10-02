<script lang="ts">
  // Recorded terminal sessions: who, where, when, how long — replay or end them.
  import { onMount } from 'svelte'
  import { Search, Loader2 } from '@lucide/svelte'
  import { bastion, errMsg } from '../lib/api'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { isAdmin } from '../lib/session.svelte'
  import type { Session } from '../gen/timika/v1/bastion_pb'
  import SessionsTable from '../components/SessionsTable.svelte'
  import { loadXterm } from '../lib/terminals.svelte'

  const list = createQuery(() => ({ queryKey: keys.sessions, queryFn: () => bastion.listSessions({}), refetchInterval: 5000 }))
  const sessions = $derived<Session[]>(list.data?.sessions ?? [])
  const loading = $derived(list.isPending)
  const error = $derived(list.error && !list.data ? errMsg(list.error) : '')
  const load = () => list.refetch()
  let q = $state('')
  let status = $state('')

  onMount(() => {
    loadXterm() // prefetch for the player
  })

  const shown = $derived.by(() => {
    const n = q.trim().toLowerCase()
    return sessions.filter((s) =>
      (!status || s.status === status) &&
      (!n || [s.user, s.assetName, s.host, s.account, s.clientIp ?? '', s.id].join(' ').toLowerCase().includes(n)))
  })
  const live = $derived(sessions.filter((s) => s.status === 'live').length)

</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Access</div>
          <h1 class="page-title">Sessions</h1>
          <p class="page-subtitle">{isAdmin() ? 'Every terminal session, recorded.' : 'Your terminal sessions, recorded.'} Replay what happened, or end a live session.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{live}</span><span class="page-metric__label">Live</span></div>
          <div class="page-metric"><span class="page-metric__value">{sessions.length}</span><span class="page-metric__label">Recorded</span></div>
        </div>
      </section>

      <section class="page-card">
        <div class="page-card__head ses-head">
          <div class="ses-search"><Search size={14} /><input bind:value={q} placeholder="Filter by user, server, account, IP…" /></div>
          <select class="base-select" bind:value={status}>
            <option value="">All statuses</option>
            {#each ['live', 'closed', 'killed', 'failed', 'lost'] as s (s)}<option value={s}>{s}</option>{/each}
          </select>
        </div>
        {#if error}<div class="notice notice--error">{error}</div>{/if}
        {#if loading}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <SessionsTable sessions={shown} onchanged={load} empty={sessions.length ? 'No sessions match.' : 'No sessions yet — connect to a server to start one.'} />
        {/if}
      </section>
    </div>
  </div>
</div>

<style>
  .ses-head { gap: 10px; }
  .ses-search { flex: 1; display: flex; align-items: center; gap: 8px; color: var(--text-muted); }
  .ses-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 13px; }
</style>
