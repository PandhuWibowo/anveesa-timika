<script lang="ts">
  // Add a person in one form — name, role, password (generated or typed) and
  // which servers they may use — or manage an existing one.
  import { onMount } from 'svelte'
  import { fade, fly } from 'svelte/transition'
  import { X, Loader2, Copy, Check, RefreshCw, Eye, EyeOff, Wand2, Pencil, Server, Trash2, LockOpen, CircleCheck, CircleAlert, ExternalLink, Users } from '@lucide/svelte'
  import { authApi, bastion, audit, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { session } from '../lib/session.svelte'
  import { generatePassword, passwordProblems, type PolicyRules } from '../lib/password'
  import { ROLES, roleOf, type Role } from '../lib/roles'
  import { navigate } from '../lib/router.svelte'
  import type { User } from '../gen/timika/v1/auth_pb'
  import type { Asset, Grant } from '../gen/timika/v1/bastion_pb'
  import type { AuditEvent } from '../gen/timika/v1/audit_pb'
  import { describe } from '../lib/auditText'

  let { user, onclose, onsaved }: { user: User | null; onclose: () => void; onsaved: () => void } = $props()

  // svelte-ignore state_referenced_locally
  const existing = user
  let tab = $state<'profile' | 'password' | 'access' | 'activity'>('profile')

  // ── shared data ──
  let rules = $state<PolicyRules>({ minLength: 15, maxLength: 128, requireClasses: 0 })
  let assets = $state<Asset[]>([])
  let grants = $state<Grant[]>([])
  let loadError = $state('')

  async function loadShared() {
    try {
      const [cfg, a, g] = await Promise.all([authApi.getLoginConfig({}), bastion.listAssets({}), bastion.listAllGrants({})])
      if (cfg.policy) rules = { minLength: cfg.policy.minLength, maxLength: cfg.policy.maxLength, requireClasses: cfg.policy.requireClasses }
      assets = a.assets
      grants = g.grants
    } catch (e) { loadError = errMsg(e) }
  }
  onMount(() => {
    loadShared()
    if (!existing) password = generatePassword(Math.max(20, rules.minLength))
  })

  // ── identity & role ──
  let username = $state('')
  let role = $state<Role>(existing ? roleOf(existing.policies) : 'ssh')
  const name = $derived((existing?.username ?? username).trim().toLowerCase())
  const nameOk = $derived(/^[a-z0-9._@-]{1,64}$/.test(name))

  // ── password ──
  let pwMode = $state<'generate' | 'manual'>('generate')
  let password = $state('')
  let showPw = $state(false)
  let mustChange = $state(true)
  let copied = $state('')
  const problems = $derived(passwordProblems(password, name, rules))

  function regenerate() {
    password = generatePassword(Math.max(20, rules.minLength), name)
    showPw = true
  }
  function setMode(m: 'generate' | 'manual') {
    pwMode = m
    if (m === 'generate') regenerate()
    else { password = ''; showPw = false }
  }
  function copy(text: string, tag = text) {
    navigator.clipboard.writeText(text)
    copied = tag
    setTimeout(() => { if (copied === tag) copied = '' }, 1400)
  }

  // ── server access picker ──
  type Pick = { on: boolean; accounts: string[] }
  let picks = $state<Record<string, Pick>>({})
  let hours = $state('0')
  let srvFilter = $state('')
  const pickedIds = $derived(Object.entries(picks).filter(([, p]) => p.on).map(([id]) => id))
  const shownAssets = $derived(assets.filter((a) => !srvFilter || `${a.name} ${a.host} ${a.tags.join(' ')}`.toLowerCase().includes(srvFilter.toLowerCase())))
  const toggle = (id: string) => (picks[id] = { on: !picks[id]?.on, accounts: picks[id]?.accounts ?? [] })
  const toggleAcc = (id: string, acc: string) => {
    const p = picks[id]
    p.accounts = p.accounts.includes(acc) ? p.accounts.filter((x) => x !== acc) : [...p.accounts, acc]
  }

  async function grantPicked(subject: string) {
    for (const id of pickedIds) {
      await bastion.createGrant({ asset: id, subjectType: 'user', subject, accounts: picks[id].accounts, hours: hours === '0' ? undefined : BigInt(hours) })
    }
  }

  // ── create ──
  let busy = $state(false)
  let error = $state('')
  let created = $state<{ username: string; password: string; servers: string[]; role: Role; mustChange: boolean } | null>(null)
  const canCreate = $derived(nameOk && problems.length === 0 && !busy)

  async function create() {
    busy = true
    error = ''
    try {
      await authApi.upsertUser({ username: name, password, policies: [role], mustChangePassword: mustChange })
      if (role !== 'admin') await grantPicked(name)
      created = { username: name, password, servers: role === 'admin' ? ['every server'] : pickedIds.map((id) => assets.find((a) => a.id === id)?.name ?? id), role, mustChange }
      onsaved()
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }

  const credText = $derived(created
    ? `Anveesa Timika — your account\nSign in: ${location.origin}\nUsername: ${created.username}\nPassword: ${created.password}${created.mustChange ? '\n(You will be asked to choose a new password at first sign-in.)' : ''}`
    : '')

  // ── manage an existing user ──
  let status = $state(existing ? { disabled: existing.disabled, locked: !!existing.lockedUntil, mustChange: existing.mustChangePassword, mfa: existing.mfaEnabled, mfaRequired: existing.mfaRequired } : null)

  async function resetMfa() {
    if (!existing || !status) return
    const ok = await confirm({
      title: `Reset two-factor for ${existing.username}?`,
      message: 'Their authenticator and recovery codes stop working. Use this when they lost their phone — they set it up again at their next sign-in (required) or from My account.',
      confirmText: 'Reset 2FA',
      variant: 'warning',
    })
    if (!ok) return
    try { await authApi.resetUserMfa({ username: existing.username }); status.mfa = false; flash('Two-factor reset'); onsaved() } catch (e) { error = errMsg(e) }
  }
  let saved = $state('')

  async function saveRole(r: Role) {
    if (!existing) return
    role = r
    try { await authApi.upsertUser({ username: existing.username, policies: [r] }); flash('Role updated'); onsaved() } catch (e) { error = errMsg(e) }
  }
  async function setDisabled(d: boolean) {
    if (!existing || !status) return
    try { await authApi.upsertUser({ username: existing.username, disabled: d }); status.disabled = d; flash(d ? 'Disabled — signed out everywhere' : 'Enabled'); onsaved() } catch (e) { error = errMsg(e) }
  }
  async function unlock() {
    if (!existing || !status) return
    try { await authApi.unlockUser({ username: existing.username }); status.locked = false; flash('Unlocked'); onsaved() } catch (e) { error = errMsg(e) }
  }
  async function remove() {
    if (!existing) return
    const ok = await confirm({ title: `Delete ${existing.username}?`, message: 'Their sessions end now and their server access is removed. Recordings and the audit trail are kept.', confirmText: 'Delete user', variant: 'danger' })
    if (!ok) return
    try {
      await authApi.deleteUser({ username: existing.username })
      for (const g of grants.filter((g) => g.subjectType === 'user' && g.subject === existing.username)) await bastion.deleteGrant({ asset: g.asset, grantId: g.id })
      onsaved()
      onclose()
    } catch (e) { error = errMsg(e) }
  }
  async function setPassword() {
    if (!existing) return
    busy = true
    error = ''
    try {
      await authApi.upsertUser({ username: existing.username, password, mustChangePassword: mustChange })
      created = { username: existing.username, password, servers: [], role, mustChange }
      if (status) status.mustChange = mustChange
      onsaved()
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }
  function flash(m: string) {
    saved = m
    setTimeout(() => { if (saved === m) saved = '' }, 2200)
  }

  // access of an existing user
  const myGrants = $derived(existing ? grants.filter((g) => g.subjectType === 'user' && g.subject === existing.username) : [])
  const roleGrants = $derived(grants.filter((g) => g.subjectType === 'role' && g.subject === role))
  const assetName = (id: string) => assets.find((a) => a.id === id)?.name ?? id
  async function addAccess() {
    if (!existing) return
    busy = true
    error = ''
    try {
      await grantPicked(existing.username)
      picks = {}
      await loadShared()
      flash('Access granted')
      onsaved()
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }
  async function revoke(g: Grant) {
    try { await bastion.deleteGrant({ asset: g.asset, grantId: g.id }); await loadShared(); onsaved() } catch (e) { error = errMsg(e) }
  }

  // activity
  let events = $state<AuditEvent[] | null>(null)
  $effect(() => {
    if (tab === 'activity' && existing && events === null) {
      audit.listEvents({ user: existing.username, limit: 40 }).then((r) => (events = r.events)).catch((e) => { error = errMsg(e); events = [] })
    }
    if (tab === 'password' && existing && !password) regenerate()
  })

  const expiry = (g: Grant) => (g.expiresAt ? (g.expired ? 'expired' : `until ${new Date(g.expiresAt).toLocaleString()}`) : 'permanent')
  const isSelf = $derived(!!existing && existing.username === session.me?.username)
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="drawer-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="drawer" role="dialog" aria-modal="true" transition:fly={{ x: 40, duration: 180 }}>
    <header class="drawer__head">
      <div class="drawer__who">
        <div class="avatar">{(existing?.username ?? (username || '?'))[0].toUpperCase()}</div>
        <div>
          <div class="page-kicker">{existing ? 'Person' : 'New person'}</div>
          <div class="drawer__title">{existing ? existing.username : 'Add a user'}</div>
        </div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>

    {#if existing && !created}
      <div class="page-tabs drawer__tabs">
        <button class="page-tab" class:is-active={tab === 'profile'} onclick={() => (tab = 'profile')}>Profile</button>
        <button class="page-tab" class:is-active={tab === 'password'} onclick={() => (tab = 'password')}>Password</button>
        <button class="page-tab" class:is-active={tab === 'access'} onclick={() => (tab = 'access')}>Servers <span class="drawer__count">{myGrants.length}</span></button>
        <button class="page-tab" class:is-active={tab === 'activity'} onclick={() => (tab = 'activity')}>Activity</button>
      </div>
    {/if}

    <div class="drawer__body">
      {#if loadError}<div class="notice notice--error">{loadError}</div>{/if}

      {#if created}
        <!-- ── done: hand over the credentials ── -->
        <div class="done">
          <div class="done__icon"><CircleCheck size={28} /></div>
          <div class="done__title">{existing ? 'Password set' : `${created.username} is ready`}</div>
          <p class="muted">Share these with {created.username} over a secure channel. The password isn't shown again.</p>
          <div class="cred">
            <div class="cred__row"><span>Sign in</span><span class="mono">{location.origin}</span></div>
            <div class="cred__row"><span>Username</span><span class="mono">{created.username}</span>
              <button class="icon-btn" title="Copy" onclick={() => copy(created!.username, 'u')}>{#if copied === 'u'}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
            <div class="cred__row"><span>Password</span><span class="mono cred__pw">{created.password}</span>
              <button class="icon-btn" title="Copy" onclick={() => copy(created!.password, 'p')}>{#if copied === 'p'}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
            {#if !existing}
              <div class="cred__row"><span>Role</span><span>{ROLES.find((r) => r.id === created!.role)?.label}</span></div>
              <div class="cred__row"><span>Servers</span><span>{created.servers.length ? created.servers.join(', ') : 'none yet'}</span></div>
            {/if}
          </div>
          {#if created.mustChange}<div class="muted done__note">They'll choose their own password at first sign-in.</div>{/if}
          <div class="done__actions">
            <button class="base-btn base-btn--primary" onclick={() => copy(credText, 'all')}>
              {#if copied === 'all'}<Check size={14} /> Copied{:else}<Copy size={14} /> Copy sign-in details{/if}
            </button>
            <button class="base-btn base-btn--ghost" onclick={onclose}>Done</button>
          </div>
        </div>

      {:else if !existing || tab === 'profile'}
        {#if !existing}
          <label class="f"><span>Username</span>
            <!-- svelte-ignore a11y_autofocus -->
            <input class="base-input mono" bind:value={username} placeholder="jane.doe" autofocus autocomplete="off" />
            {#if username && !nameOk}<small class="bad">1–64 of a-z 0-9 . _ - @</small>{/if}
          </label>
        {/if}

        <div class="f-label">Role</div>
        <div class="roles">
          {#each ROLES as r (r.id)}
            <button class="role" class:is-on={role === r.id} disabled={isSelf}
                    onclick={() => (existing ? saveRole(r.id) : (role = r.id))}>
              <span class="role__ico"><r.ico size={16} /></span>
              <span class="role__label">{r.label}</span>
              <span class="role__hint">{r.hint}</span>
            </button>
          {/each}
        </div>

        {#if !existing}
          <div class="f-label">Password</div>
          <div class="seg">
            <button class:is-on={pwMode === 'generate'} onclick={() => setMode('generate')}><Wand2 size={12} /> Generate</button>
            <button class:is-on={pwMode === 'manual'} onclick={() => setMode('manual')}><Pencil size={12} /> Set manually</button>
          </div>
          {@render pwField()}

          {#if role !== 'admin'}
            <div class="f-label">Servers <span class="muted">— optional, you can add more later</span></div>
            {@render serverPicker()}
          {/if}
          {#if error}<div class="notice notice--error">{error}</div>{/if}
        {:else if status}
          <div class="f-label">Status</div>
          <div class="status">
            <div class="status__row">
              <span>{#if status.disabled}<span class="badge badge--danger">disabled</span>{:else}<span class="badge badge--success">active</span>{/if}
                {#if status.locked}<span class="badge badge--warning">locked</span>{/if}
                {#if status.mustChange}<span class="badge badge--info">must change password</span>{/if}
                {#if status.mfa}<span class="badge badge--success">2FA on</span>{:else if status.mfaRequired}<span class="badge badge--danger">2FA required</span>{:else}<span class="badge badge--default">2FA off</span>{/if}</span>
            </div>
            <div class="status__actions">
              {#if status.locked}<button class="base-btn base-btn--ghost base-btn--sm" onclick={unlock}><LockOpen size={13} /> Unlock</button>{/if}
              {#if status.mfa}<button class="base-btn base-btn--ghost base-btn--sm" onclick={resetMfa} title="Lost phone">Reset 2FA</button>{/if}
              {#if !isSelf}
                <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => setDisabled(!status!.disabled)}>{status.disabled ? 'Enable' : 'Disable'}</button>
                <button class="base-btn base-btn--danger base-btn--sm" onclick={remove}><Trash2 size={13} /> Delete</button>
              {/if}
            </div>
          </div>
          <div class="facts">
            <div><span>Created</span>{new Date(existing.createdAt).toLocaleString()}</div>
            <div><span>Password changed</span>{new Date(existing.passwordChangedAt).toLocaleString()}</div>
            <div><span>Last sign-in</span>{existing.lastLogin ? `${new Date(existing.lastLogin.time).toLocaleString()} from ${existing.lastLogin.ip ?? '?'}` : 'never'}</div>
            <div><span>Last failed</span>{existing.lastFailedLogin ? `${new Date(existing.lastFailedLogin.time).toLocaleString()} from ${existing.lastFailedLogin.ip ?? '?'}` : '—'}</div>
          </div>
          {#if saved}<div class="notice notice--success">{saved}</div>{/if}
          {#if error}<div class="notice notice--error">{error}</div>{/if}
        {/if}

      {:else if tab === 'password'}
        <p class="muted drawer__hint">A new password applies to their next sign-in; sessions already open continue until they expire.</p>
        <div class="seg">
          <button class:is-on={pwMode === 'generate'} onclick={() => setMode('generate')}><Wand2 size={12} /> Generate</button>
          <button class:is-on={pwMode === 'manual'} onclick={() => setMode('manual')}><Pencil size={12} /> Set manually</button>
        </div>
        {@render pwField()}
        {#if error}<div class="notice notice--error">{error}</div>{/if}

      {:else if tab === 'access'}
        {#if role === 'admin'}
          <div class="notice notice--info">Administrators can use every server.</div>
        {:else}
          <div class="grants">
            {#each myGrants as g (g.id)}
              <div class="grant" class:grant--expired={g.expired}>
                <Server size={14} />
                <div class="grant__text">
                  <div><b>{assetName(g.asset)}</b></div>
                  <div class="muted grant__sub">{g.accounts.length ? g.accounts.join(', ') : 'all accounts'} · {expiry(g)}</div>
                </div>
                <button class="icon-btn" title="Revoke" onclick={() => revoke(g)}><Trash2 size={14} /></button>
              </div>
            {:else}
              <div class="empty-state">No servers granted to {existing.username} directly.</div>
            {/each}
          </div>
          {#if roleGrants.length}
            <div class="muted inherited"><Users size={12} /> Also through the {ROLES.find((r) => r.id === role)?.label} role: {[...new Set(roleGrants.map((g) => assetName(g.asset)))].join(', ')}</div>
          {/if}
          <div class="f-label">Grant more</div>
          {@render serverPicker()}
          {#if saved}<div class="notice notice--success">{saved}</div>{/if}
          {#if error}<div class="notice notice--error">{error}</div>{/if}
        {/if}

      {:else}
        <div class="acts">
          {#if events === null}
            <div class="empty-state"><Loader2 size={16} class="spin" /></div>
          {:else}
            {#each events as e (e.seq)}
              {@const d = describe(e)}
              <div class="act">
                <span class="act__dot" class:act__dot--bad={!e.ok}></span>
                <div class="act__text">
                  <div>{d.label}{#if e.target} <span class="mono act__target">{e.target}</span>{/if}</div>
                  <div class="muted act__sub">{new Date(e.time).toLocaleString()}{e.remoteAddr ? ` · ${e.remoteAddr}` : ''}{!e.ok && e.error ? ` · ${e.error}` : ''}</div>
                </div>
              </div>
            {:else}
              <div class="empty-state">No activity recorded on this instance.</div>
            {/each}
          {/if}
          <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => { onclose(); navigate(`/audit/${existing.username}`) }}><ExternalLink size={13} /> Open in Audit trail</button>
        </div>
      {/if}
    </div>

    {#if !created && (!existing || tab === 'password' || (tab === 'access' && role !== 'admin'))}
      <footer class="drawer__foot">
        <button class="base-btn base-btn--ghost" onclick={onclose}>Cancel</button>
        {#if !existing}
          <button class="base-btn base-btn--primary" disabled={!canCreate} onclick={create}>
            {#if busy}<Loader2 size={14} class="spin" />{/if} Create user{pickedIds.length && role !== 'admin' ? ` + ${pickedIds.length} server${pickedIds.length > 1 ? 's' : ''}` : ''}
          </button>
        {:else if tab === 'password'}
          <button class="base-btn base-btn--primary" disabled={problems.length > 0 || busy} onclick={setPassword}>{#if busy}<Loader2 size={14} class="spin" />{/if} Set password</button>
        {:else}
          <button class="base-btn base-btn--primary" disabled={!pickedIds.length || busy} onclick={addAccess}>{#if busy}<Loader2 size={14} class="spin" />{/if} Grant access</button>
        {/if}
      </footer>
    {/if}
  </div>
</div>

{#snippet pwField()}
  <div class="pw">
    <input class="base-input mono" type={showPw || pwMode === 'generate' ? 'text' : 'password'} bind:value={password}
           readonly={pwMode === 'generate'} autocomplete="new-password" placeholder="type a password" />
    {#if pwMode === 'generate'}
      <button class="icon-btn" title="Generate another" onclick={regenerate}><RefreshCw size={14} /></button>
    {:else}
      <button class="icon-btn" title={showPw ? 'Hide' : 'Show'} onclick={() => (showPw = !showPw)}>{#if showPw}<EyeOff size={14} />{:else}<Eye size={14} />{/if}</button>
    {/if}
    <button class="icon-btn" title="Copy" onclick={() => copy(password, 'pw')}>{#if copied === 'pw'}<Check size={14} />{:else}<Copy size={14} />{/if}</button>
  </div>
  {#if pwMode === 'manual' && password}
    {#if problems.length}
      <small class="bad"><CircleAlert size={12} /> Needs {problems.join(' · ')}</small>
    {:else}
      <small class="good"><CircleCheck size={12} /> Meets the {rules.minLength}+ character policy</small>
    {/if}
  {/if}
  <label class="check"><input type="checkbox" bind:checked={mustChange} /> Ask them to choose their own password at first sign-in</label>
{/snippet}

{#snippet serverPicker()}
  {#if assets.length > 6}<input class="base-input" bind:value={srvFilter} placeholder="Filter servers…" />{/if}
  <div class="picker">
    {#each shownAssets as a (a.id)}
      {@const p = picks[a.id]}
      <div class="pick" class:is-on={p?.on}>
        <label class="pick__main">
          <input type="checkbox" checked={!!p?.on} onchange={() => toggle(a.id)} />
          <span class="pick__name">{a.name}</span>
          <span class="mono muted pick__host">{a.host}</span>
        </label>
        {#if p?.on && a.accounts.length > 1}
          <div class="pick__accs">
            {#each a.accounts as acc (acc.username)}
              <button class="chip" class:is-on={p.accounts.includes(acc.username)} onclick={() => toggleAcc(a.id, acc.username)}>{acc.username}</button>
            {/each}
            <span class="muted">{p.accounts.length ? '' : 'all accounts'}</span>
          </div>
        {/if}
      </div>
    {:else}
      <div class="empty-state">{assets.length ? 'No match.' : 'No servers yet — add them on the Servers page.'}</div>
    {/each}
  </div>
  {#if pickedIds.length}
    <label class="f f--inline"><span>Access for</span>
      <select class="base-select" bind:value={hours}>
        <option value="0">permanent</option><option value="1">1 hour</option><option value="8">8 hours</option>
        <option value="24">1 day</option><option value="168">7 days</option><option value="720">30 days</option>
      </select>
    </label>
  {/if}
{/snippet}

<style>
  .drawer-mask { position: fixed; inset: 0; z-index: 900; background: rgba(0, 0, 0, 0.45); backdrop-filter: blur(2px); display: flex; justify-content: flex-end; }
  .drawer { width: min(540px, 100vw); height: 100%; display: flex; flex-direction: column; background: var(--bg-surface); border-left: 1px solid var(--border); box-shadow: var(--shadow-lg); }
  .drawer__head { display: flex; align-items: flex-start; justify-content: space-between; padding: 20px 22px 12px; }
  .drawer__who { display: flex; gap: 12px; align-items: center; }
  .avatar { width: 40px; height: 40px; border-radius: 50%; display: grid; place-items: center; font-weight: 700; background: var(--brand-soft); color: var(--brand); }
  .drawer__title { font-size: 18px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .drawer__tabs { padding: 0 22px; }
  .drawer__count { font-size: 10.5px; padding: 1px 6px; border-radius: 10px; background: var(--bg-elevated); margin-left: 4px; }
  .drawer__body { flex: 1; overflow-y: auto; padding: 14px 22px 22px; display: flex; flex-direction: column; gap: 10px; }
  .drawer__hint { font-size: 12.5px; margin: 0; line-height: 1.5; }
  .drawer__foot { display: flex; justify-content: flex-end; gap: 8px; padding: 14px 22px; border-top: 1px solid var(--border); }
  .f { display: flex; flex-direction: column; gap: 5px; }
  .f > span, .f-label { font-size: 11px; color: var(--text-muted); letter-spacing: .03em; text-transform: uppercase; }
  .f-label { margin-top: 6px; }
  .f-label .muted { text-transform: none; letter-spacing: 0; }
  .f--inline { flex-direction: row; align-items: center; gap: 10px; }
  .bad, .good { display: flex; align-items: center; gap: 5px; font-size: 12px; }
  .bad { color: var(--danger); }
  .good { color: var(--success); }
  .roles { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; }
  .role { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; padding: 10px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); cursor: pointer; text-align: left; color: var(--text-secondary); }
  .role:disabled { cursor: not-allowed; opacity: 0.6; }
  .role.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--text-primary); }
  .role__ico { color: var(--brand); display: flex; }
  .role__label { font-weight: 700; font-size: 12.5px; }
  .role__hint { font-size: 11px; color: var(--text-muted); line-height: 1.35; }
  .seg { display: inline-flex; align-self: flex-start; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); }
  .seg button { display: flex; align-items: center; gap: 5px; padding: 5px 10px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .pw { display: flex; gap: 4px; align-items: center; }
  .pw input { flex: 1; letter-spacing: 0.02em; }
  .check { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
  .picker { display: flex; flex-direction: column; gap: 4px; max-height: 300px; overflow-y: auto; }
  .pick { border-radius: var(--r); border: 1px solid var(--border); background: var(--bg-body); }
  .pick.is-on { border-color: var(--brand-ring); }
  .pick__main { display: flex; align-items: center; gap: 10px; padding: 8px 10px; cursor: pointer; }
  .pick__name { font-weight: 600; color: var(--text-primary); }
  .pick__host { font-size: 11.5px; margin-left: auto; }
  .pick__accs { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; padding: 0 10px 8px 34px; font-size: 12px; }
  .chip { padding: 3px 9px; border-radius: 12px; border: 1px solid var(--border); background: none; color: var(--text-secondary); cursor: pointer; font-size: 12px; font-family: var(--mono); }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
  .status { display: flex; justify-content: space-between; align-items: center; gap: 10px; flex-wrap: wrap; }
  .status__row span { display: inline-flex; gap: 6px; }
  .status__actions { display: flex; gap: 6px; }
  .facts { display: grid; gap: 6px; font-size: 12.5px; margin-top: 4px; }
  .facts div { display: flex; gap: 10px; }
  .facts span { width: 130px; color: var(--text-muted); flex: 0 0 auto; }
  .grants { display: flex; flex-direction: column; gap: 6px; }
  .grant { display: flex; align-items: center; gap: 10px; padding: 9px 10px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); color: var(--brand); }
  .grant--expired { opacity: 0.55; }
  .grant__text { flex: 1; min-width: 0; font-size: 13px; color: var(--text-primary); }
  .grant__sub { font-size: 11.5px; }
  .inherited { display: flex; align-items: center; gap: 6px; font-size: 12px; }
  .acts { display: flex; flex-direction: column; gap: 2px; }
  .act { display: flex; gap: 10px; padding: 7px 0; border-bottom: 1px solid var(--border); font-size: 12.5px; }
  .act__dot { width: 7px; height: 7px; border-radius: 50%; background: var(--success); margin-top: 6px; flex: 0 0 auto; }
  .act__dot--bad { background: var(--danger); }
  .act__text { min-width: 0; }
  .act__target { font-size: 11.5px; color: var(--text-secondary); word-break: break-all; }
  .act__sub { font-size: 11.5px; }
  .done { display: flex; flex-direction: column; align-items: center; text-align: center; gap: 10px; padding-top: 10px; }
  .done__icon { color: var(--success); }
  .done__title { font-size: 17px; font-weight: 700; color: var(--text-primary); }
  .done p { margin: 0; font-size: 12.5px; }
  .done__note { font-size: 12px; }
  .done__actions { display: flex; gap: 8px; margin-top: 6px; }
  .cred { width: 100%; text-align: left; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); padding: 6px 12px; }
  .cred__row { display: flex; align-items: center; gap: 10px; padding: 6px 0; font-size: 13px; border-bottom: 1px solid var(--border); }
  .cred__row:last-child { border-bottom: 0; }
  .cred__row > span:first-child { width: 80px; color: var(--text-muted); flex: 0 0 auto; }
  .cred__row > span:nth-child(2) { flex: 1; min-width: 0; word-break: break-all; }
  .cred__pw { color: var(--brand); }
</style>
