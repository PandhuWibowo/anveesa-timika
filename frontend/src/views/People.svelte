<script lang="ts">
  // People: who can sign in, their role, which servers they can reach.
  import { onMount } from 'svelte'
  import { UserPlus, Search, Loader2, ScrollText } from '@lucide/svelte'
  import { authApi, bastion, errMsg } from '../lib/api'
  import { ROLES, roleOf, roleLabel, roleBadge } from '../lib/roles'
  import { navigate } from '../lib/router.svelte'
  import type { User } from '../gen/timika/v1/auth_pb'
  import type { Grant } from '../gen/timika/v1/bastion_pb'
  import UserDrawer from '../components/UserDrawer.svelte'

  let users = $state<User[]>([])
  let grants = $state<Grant[]>([])
  let assetCount = $state(0)
  let loading = $state(true)
  let error = $state('')
  let q = $state('')
  let roleFilter = $state('')
  let drawer = $state<{ open: boolean; user: User | null }>({ open: false, user: null })

  async function load() {
    try {
      const [u, g, a] = await Promise.all([authApi.listUsers({}), bastion.listAllGrants({}), bastion.listAssets({})])
      users = u.users.sort((x, y) => x.username.localeCompare(y.username))
      grants = g.grants
      assetCount = a.assets.length
      error = ''
    } catch (e) { error = errMsg(e) } finally { loading = false }
  }
  onMount(load)

  const shown = $derived(users.filter((u) =>
    (!roleFilter || roleOf(u.policies) === roleFilter) &&
    (!q || u.username.includes(q.trim().toLowerCase()))))

  /** Servers a user can reach: direct grants + their role's grants (admins: all). */
  function servers(u: User): number | 'all' {
    const role = roleOf(u.policies)
    if (role === 'admin') return 'all'
    const live = grants.filter((g) => !g.expired && ((g.subjectType === 'user' && g.subject === u.username) || (g.subjectType === 'role' && g.subject === role)))
    return new Set(live.map((g) => g.asset)).size
  }
  const locked = (u: User) => !!u.lockedUntil && new Date(u.lockedUntil) > new Date()
  const ago = (t?: string) => {
    if (!t) return 'never'
    const s = (Date.now() - new Date(t).getTime()) / 1000
    if (s < 60) return 'just now'
    if (s < 3600) return `${Math.floor(s / 60)} min ago`
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`
    return `${Math.floor(s / 86400)} d ago`
  }
  const count = (r: string) => users.filter((u) => roleOf(u.policies) === r).length
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Access</div>
          <h1 class="page-title">People</h1>
          <p class="page-subtitle">Who can sign in, what they can do, and which servers they reach. Add someone in one step — with a generated password and their servers.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{users.length}</span><span class="page-metric__label">People</span></div>
          <div class="page-metric"><span class="page-metric__value">{count('ssh')}</span><span class="page-metric__label">SSH only</span></div>
          <div class="page-metric"><span class="page-metric__value">{users.filter((u) => u.mfaEnabled).length}/{users.length}</span><span class="page-metric__label">With 2FA</span></div>
          <div class="page-metric"><span class="page-metric__value">{users.filter(locked).length}</span><span class="page-metric__label">Locked</span></div>
        </div>
      </section>

      <div class="ppl-bar">
        <div class="ppl-search"><Search size={15} /><input bind:value={q} placeholder="Search people…" /></div>
        <div class="seg">
          <button class:is-on={roleFilter === ''} onclick={() => (roleFilter = '')}>All</button>
          {#each ROLES as r (r.id)}<button class:is-on={roleFilter === r.id} onclick={() => (roleFilter = r.id)}>{r.label}</button>{/each}
        </div>
        <button class="base-btn base-btn--primary" onclick={() => (drawer = { open: true, user: null })}><UserPlus size={14} /> Add user</button>
      </div>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      <section class="page-card">
        {#if loading}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          <div class="data-table-wrap">
            <table class="data-table ppl">
              <thead><tr><th>Person</th><th>Role</th><th>Status</th><th>2FA</th><th>Servers</th><th>Last sign-in</th><th></th></tr></thead>
              <tbody>
                {#each shown as u (u.username)}
                  {@const n = servers(u)}
                  <tr class="ppl__row" onclick={() => (drawer = { open: true, user: u })}>
                    <td><div class="ppl__who"><span class="avatar">{u.username[0].toUpperCase()}</span><span class="strong">{u.username}</span></div></td>
                    <td><span class="badge badge--{roleBadge(u.policies)}">{roleLabel(u.policies)}</span></td>
                    <td>
                      {#if u.disabled}<span class="badge badge--danger">disabled</span>
                      {:else if locked(u)}<span class="badge badge--warning">locked</span>
                      {:else if u.mustChangePassword}<span class="badge badge--info">pending first sign-in</span>
                      {:else}<span class="badge badge--success">active</span>{/if}
                    </td>
                    <td>
                      {#if u.mfaEnabled}<span class="badge badge--success" title="Two-factor authentication is on">on</span>
                      {:else if u.mfaRequired}<span class="badge badge--danger" title="Required — set up at next sign-in">required</span>
                      {:else}<span class="muted">off</span>{/if}
                    </td>
                    <td>{n === 'all' ? `all ${assetCount}` : n}</td>
                    <td title={u.lastLogin ? `${new Date(u.lastLogin.time).toLocaleString()} from ${u.lastLogin.ip ?? '?'}` : ''}>
                      {ago(u.lastLogin?.time)}{#if u.lastFailedLogin && (!u.lastLogin || u.lastFailedLogin.time > u.lastLogin.time)} <span class="badge badge--warning" title="failed attempt {new Date(u.lastFailedLogin.time).toLocaleString()} from {u.lastFailedLogin.ip ?? '?'}">failed attempt</span>{/if}
                    </td>
                    <td class="ppl__actions">
                      <button class="icon-btn" title="Activity in the audit trail" onclick={(e) => { e.stopPropagation(); navigate(`/audit/${u.username}`) }}><ScrollText size={14} /></button>
                    </td>
                  </tr>
                {:else}
                  <tr><td colspan="7"><div class="empty-state">{users.length ? 'Nobody matches.' : 'No users yet — add the first one.'}</div></td></tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    </div>
  </div>
</div>

{#if drawer.open}
  <UserDrawer user={drawer.user} onclose={() => (drawer = { open: false, user: null })} onsaved={load} />
{/if}

<style>
  .ppl-bar { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .ppl-search { flex: 1; min-width: 200px; display: flex; align-items: center; gap: 8px; padding: 0 12px; height: 34px; background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); color: var(--text-muted); }
  .ppl-search:focus-within { border-color: var(--brand-ring); box-shadow: 0 0 0 3px var(--brand-dim); }
  .ppl-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 6px 10px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .ppl__row { cursor: pointer; }
  .ppl__who { display: flex; align-items: center; gap: 10px; }
  .avatar { width: 24px; height: 24px; border-radius: 50%; display: grid; place-items: center; font-size: 12px; font-weight: 700; background: var(--brand-soft); color: var(--brand); }
  .ppl__actions { width: 40px; text-align: right; }
</style>
