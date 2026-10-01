<script lang="ts">
  // Add / edit a server, who may use it, and its pinned host key — one drawer.
  import { onMount } from 'svelte'
  import { fade, fly } from 'svelte/transition'
  import { X, Plus, Loader2, ShieldCheck, ShieldAlert, Trash2, UserRound, Users, RotateCcw, Copy, Check, CircleCheck } from '@lucide/svelte'
  import AccountCard, { blankAccount, accountValid, accountInput, sudoText, type Acc } from './AccountCard.svelte'
  import { bastion, authApi, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import type { Asset, Grant } from '../gen/timika/v1/bastion_pb'

  let { asset, onclose, onsaved }: { asset: Asset | null; onclose: () => void; onsaved: () => void } = $props()

  // svelte-ignore state_referenced_locally
  const editing = asset
  let tab = $state<'connection' | 'access' | 'hostkey'>('connection')
  let name = $state(editing?.name ?? '')
  let host = $state(editing?.host ?? '')
  let port = $state(editing?.port ?? 22)
  let tags = $state(editing?.tags.join(', ') ?? '')
  let description = $state(editing?.description ?? '')
  // A new server starts with root, whose real password must be typed in.
  let accounts = $state<Acc[]>(
    editing?.accounts.map((a) => ({ ...blankAccount(a.username), kind: a.auth === 'key' ? ('key' as const) : ('password' as const), stored: true, pwMode: 'keep' as const, sudo: '' as const, sudoNow: a.sudo, groupsNow: [...a.groups] })) ??
      [{ ...blankAccount('root'), pwMode: 'type' }],
  )
  let busy = $state<'' | 'save' | 'test'>('')
  let error = $state('')
  let hostKey = $state(editing?.hostKey)

  const valid = $derived(name.trim() !== '' && host.trim() !== '' && accounts.length > 0 && accounts.every(accountValid))

  /** Accounts that could sign in to manage `self`: others with credentials, or
   *  — for a saved account — itself, with its current password. */
  const admins = (self: Acc) => {
    const others = accounts
      .filter((x) => x !== self && x.username.trim() && !(x.provision && !x.stored) && (x.stored || x.password || x.privateKey.trim()))
      .map((x) => x.username.trim())
    return self.stored && self.username.trim() ? [self.username.trim(), ...others.filter((o) => o !== self.username.trim())] : others
  }

  /** New accounts default to a generated password, created on the server — when
   *  there's an account to sign in with; otherwise the existing password is typed. */
  function addAccount() {
    const a = blankAccount()
    const canCreate = accounts.some((x) => x.username.trim() && (x.stored || x.password || x.privateKey.trim()))
    a.pwMode = canCreate ? 'generate' : 'type'
    a.provision = canCreate
    accounts.push(a)
  }

  let copiedPw = $state('')
  function copyPw(pw: string) {
    navigator.clipboard.writeText(pw)
    copiedPw = pw
    setTimeout(() => { if (copiedPw === pw) copiedPw = '' }, 1400)
  }
  const describeChange = (a: Acc) =>
    [a.stored ? (a.pwMode !== 'keep' && a.password ? 'new password' : 'updated') : 'created', sudoText(a.sudo), a.groups.length ? `groups ${a.groups.join(', ')}` : '']
      .filter(Boolean).join(' · ')

  /** After saving: accounts created / updated on the server, with their passwords to hand over. */
  let done = $state<{ username: string; password: string; what: string }[] | null>(null)

  async function save(test: boolean) {
    busy = test ? 'test' : 'save'
    error = ''
    const input = {
      name: name.trim(),
      host: host.trim(),
      port: Number(port) || 22,
      tags: tags.split(',').map((t) => t.trim()).filter(Boolean),
      description: description.trim(),
      accounts: accounts.map(accountInput),
      test,
    }
    try {
      if (editing) {
        await bastion.updateAsset({ id: editing.id, asset: input })
        if (test) {
          const r = await bastion.testAsset({ id: editing.id })
          if (!r.ok) throw new Error(r.error ?? 'test failed')
        }
      } else {
        await bastion.createAsset(input)
      }
      onsaved()
      const made = accounts.filter((a) => a.provision)
      if (made.length) {
        done = made.map((a) => ({ username: a.username.trim(), password: a.kind === 'password' && (!a.stored || a.pwMode !== 'keep') ? a.password : '', what: describeChange(a) }))
      } else {
        onclose()
      }
    } catch (e) {
      error = errMsg(e)
    } finally {
      busy = ''
    }
  }

  // ── access ──
  let grants = $state<Grant[]>([])
  let users = $state<string[]>([])
  let roles = $state<string[]>(['admin', 'read-only'])
  let gType = $state<'user' | 'role'>('user')
  let gSubject = $state('')
  let gAccounts = $state<string[]>([])
  let gHours = $state('0')
  let gError = $state('')

  async function loadAccess() {
    if (!editing) return
    try {
      grants = (await bastion.listGrants({ id: editing.id })).grants
      const u = await authApi.listUsers({})
      users = u.users.map((x) => x.username)
      roles = u.roles
    } catch (e) { gError = errMsg(e) }
  }
  onMount(loadAccess)

  async function addGrant() {
    if (!editing || !gSubject) return
    gError = ''
    try {
      await bastion.createGrant({ asset: editing.id, subjectType: gType, subject: gSubject, accounts: gAccounts, hours: gHours === '0' ? undefined : BigInt(gHours) })
      gSubject = ''
      gAccounts = []
      await loadAccess()
      onsaved()
    } catch (e) { gError = errMsg(e) }
  }

  async function removeGrant(g: Grant) {
    if (!editing) return
    try { await bastion.deleteGrant({ asset: editing.id, grantId: g.id }); await loadAccess() } catch (e) { gError = errMsg(e) }
  }

  async function resetKey() {
    if (!editing) return
    const ok = await confirm({
      title: 'Forget the pinned host key?',
      message: 'Only do this if the server was legitimately reinstalled. The next connection trusts whatever key the server presents.',
      subject: hostKey,
      confirmText: 'Reset host key',
      variant: 'warning',
    })
    if (!ok) return
    try { hostKey = (await bastion.resetHostKey({ id: editing.id })).hostKey; onsaved() } catch (e) { error = errMsg(e) }
  }

  const fmtExpiry = (g: Grant) => (g.expiresAt ? (g.expired ? 'expired' : `until ${new Date(g.expiresAt).toLocaleString()}`) : 'permanent')
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="drawer-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="drawer" role="dialog" aria-modal="true" transition:fly={{ x: 40, duration: 180 }}>
    <header class="drawer__head">
      <div>
        <div class="page-kicker">{editing ? 'Server' : 'New server'}</div>
        <div class="drawer__title">{editing ? editing.name : 'Add a server'}</div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>

    {#if editing}
      <div class="page-tabs drawer__tabs">
        <button class="page-tab" class:is-active={tab === 'connection'} onclick={() => (tab = 'connection')}>Connection</button>
        <button class="page-tab" class:is-active={tab === 'access'} onclick={() => (tab = 'access')}>Access <span class="drawer__count">{grants.length}</span></button>
        <button class="page-tab" class:is-active={tab === 'hostkey'} onclick={() => (tab = 'hostkey')}>Host key</button>
      </div>
    {/if}

    <div class="drawer__body">
      {#if done}
        <div class="done">
          <div class="done__icon"><CircleCheck size={28} /></div>
          <div class="done__title">Saved {name}</div>
          <p class="muted">Applied on the server and saved in the vault. Copy a password now if someone needs it — it isn't shown again.</p>
          {#each done as d (d.username)}
            <div class="cred">
              <div class="cred__row"><span>Account</span><span class="mono">{d.username}</span><span class="badge badge--success done__what">{d.what}</span></div>
              {#if d.password}
                <div class="cred__row"><span>Password</span><span class="mono cred__pw">{d.password}</span>
                  <button class="icon-btn" title="Copy" onclick={() => copyPw(d.password)}>{#if copiedPw === d.password}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
              {/if}
            </div>
          {/each}
          <button class="base-btn base-btn--primary" onclick={onclose}>Done</button>
        </div>
      {:else if tab === 'connection'}
        <div class="f-grid">
          <label class="f"><span>Name</span><input class="base-input" bind:value={name} placeholder="web-1 (prod)" /></label>
          <label class="f f--host"><span>Host</span><input class="base-input mono" bind:value={host} placeholder="10.0.4.12 or web-1.internal" /></label>
          <label class="f f--port"><span>Port</span><input class="base-input mono" type="number" min="1" max="65535" bind:value={port} /></label>
        </div>

        <div class="f-label">Accounts</div>
        <div class="accs">
          {#each accounts as a, i (i)}
            <AccountCard bind:a={accounts[i]} admins={admins(a)} removable={accounts.length > 1} onremove={() => accounts.splice(i, 1)} />
          {/each}
        </div>
        <button class="base-btn base-btn--ghost base-btn--sm" onclick={addAccount}><Plus size={13} /> Add account</button>

        <details class="more" open={!!(editing?.tags.length || editing?.description)}>
          <summary>Tags & description</summary>
          <label class="f"><span>Tags</span><input class="base-input" bind:value={tags} placeholder="prod, web, eu" /></label>
          <label class="f"><span>Description</span><input class="base-input" bind:value={description} placeholder="What runs here" /></label>
        </details>

        {#if error}<div class="notice notice--error">{error}</div>{/if}

      {:else if tab === 'access'}
        <p class="muted drawer__hint">Admins can always connect. Grant others access by user or by role, optionally to some accounts and for a limited time.</p>
        <div class="grant-form">
          <div class="seg">
            <button class:is-on={gType === 'user'} onclick={() => { gType = 'user'; gSubject = '' }}><UserRound size={12} /> User</button>
            <button class:is-on={gType === 'role'} onclick={() => { gType = 'role'; gSubject = '' }}><Users size={12} /> Role</button>
          </div>
          <select class="base-select" bind:value={gSubject}>
            <option value="" disabled>{gType === 'user' ? 'Choose a user…' : 'Choose a role…'}</option>
            {#each gType === 'user' ? users : roles as s (s)}<option value={s}>{s}</option>{/each}
          </select>
          <select class="base-select" bind:value={gHours} title="How long">
            <option value="0">permanent</option>
            <option value="1">1 hour</option>
            <option value="8">8 hours</option>
            <option value="24">1 day</option>
            <option value="168">7 days</option>
          </select>
          <button class="base-btn base-btn--primary base-btn--sm" disabled={!gSubject} onclick={addGrant}><Plus size={13} /> Grant</button>
        </div>
        {#if (editing?.accounts.length ?? 0) > 1}
          <div class="grant-accounts">
            <span class="muted">Accounts:</span>
            {#each editing?.accounts ?? [] as a (a.username)}
              <label class="chip" class:is-on={gAccounts.includes(a.username)}>
                <input type="checkbox" value={a.username} bind:group={gAccounts} /> <span class="mono">{a.username}</span>
              </label>
            {/each}
            <span class="muted">{gAccounts.length ? '' : '(none ticked = all)'}</span>
          </div>
        {/if}
        {#if gError}<div class="notice notice--error">{gError}</div>{/if}
        <div class="grants">
          {#each grants as g (g.id)}
            <div class="grant" class:grant--expired={g.expired}>
              <span class="grant__ico">{#if g.subjectType === 'user'}<UserRound size={14} />{:else}<Users size={14} />{/if}</span>
              <div class="grant__text">
                <div><b>{g.subject}</b> <span class="muted">{g.subjectType}</span></div>
                <div class="muted grant__sub">{g.accounts.length ? g.accounts.join(', ') : 'all accounts'} · {fmtExpiry(g)} · by {g.createdBy}</div>
              </div>
              <button class="icon-btn" title="Revoke" onclick={() => removeGrant(g)}><Trash2 size={14} /></button>
            </div>
          {:else}
            <div class="empty-state">Only administrators can connect to this server.</div>
          {/each}
        </div>

      {:else}
        <div class="hostkey">
          {#if hostKey}
            <div class="hostkey__state"><ShieldCheck size={16} /> Pinned</div>
            <div class="mono hostkey__fp">{hostKey}</div>
            <p class="muted">Every connection checks the server presents this key. A different key is refused as a possible man-in-the-middle.</p>
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={resetKey}><RotateCcw size={13} /> Reset (server was reinstalled)</button>
          {:else}
            <div class="hostkey__state hostkey__state--warn"><ShieldAlert size={16} /> Not pinned yet</div>
            <p class="muted">The key the server presents on the next connection (or test) is trusted and pinned.</p>
          {/if}
          {#if error}<div class="notice notice--error">{error}</div>{/if}
        </div>
      {/if}
    </div>

    {#if tab === 'connection' && !done}
      <footer class="drawer__foot">
        <button class="base-btn base-btn--ghost" onclick={() => save(false)} disabled={!valid || !!busy} title="Save without logging in">
          {#if busy === 'save'}<Loader2 size={14} class="spin" />{/if} Save only
        </button>
        <button class="base-btn base-btn--primary" onclick={() => save(true)} disabled={!valid || !!busy} title="Log in once to check the credentials and pin the host key">
          {#if busy === 'test'}<Loader2 size={14} class="spin" />{/if} Test & save
        </button>
      </footer>
    {/if}
  </div>
</div>

<style>
  .drawer-mask { position: fixed; inset: 0; z-index: 900; background: rgba(0, 0, 0, 0.45); backdrop-filter: blur(2px); display: flex; justify-content: flex-end; }
  .drawer { width: min(520px, 100vw); height: 100%; display: flex; flex-direction: column; background: var(--bg-surface); border-left: 1px solid var(--border); box-shadow: var(--shadow-lg); }
  .drawer__head { display: flex; align-items: flex-start; justify-content: space-between; padding: 20px 22px 12px; }
  .drawer__title { font-size: 18px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .drawer__tabs { padding: 0 22px; }
  .drawer__count { font-size: 10.5px; padding: 1px 6px; border-radius: 10px; background: var(--bg-elevated); margin-left: 4px; }
  .drawer__body { flex: 1; overflow-y: auto; padding: 14px 22px 22px; display: flex; flex-direction: column; gap: 12px; }
  .drawer__hint { font-size: 12.5px; margin: 0; line-height: 1.5; }
  .drawer__foot { display: flex; justify-content: flex-end; gap: 8px; padding: 14px 22px; border-top: 1px solid var(--border); }
  .f-grid { display: grid; grid-template-columns: 1fr 96px; gap: 10px; }
  .f-grid .f:first-child { grid-column: 1 / -1; }
  .f { display: flex; flex-direction: column; gap: 5px; }
  .f > span, .f-label { font-size: 11px; color: var(--text-muted); letter-spacing: .03em; text-transform: uppercase; }
  .f-label { margin-top: 4px; }
  .f input { width: 100%; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); flex: 0 0 auto; }
  .seg button { display: flex; align-items: center; gap: 5px; padding: 5px 9px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .more summary { cursor: pointer; font-size: 12.5px; color: var(--text-secondary); margin-bottom: 8px; }
  .more .f { margin-bottom: 8px; }
  .grant-form { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .grant-form select { flex: 1; min-width: 120px; }
  .grant-accounts { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; font-size: 12px; }
  .chip { display: inline-flex; align-items: center; gap: 4px; padding: 3px 9px; border-radius: 12px; border: 1px solid var(--border); cursor: pointer; font-size: 12px; }
  .chip input { display: none; }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
  .grants { display: flex; flex-direction: column; gap: 6px; }
  .grant { display: flex; align-items: center; gap: 10px; padding: 9px 10px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .grant--expired { opacity: 0.55; }
  .grant__ico { color: var(--brand); display: flex; }
  .grant__text { flex: 1; min-width: 0; font-size: 13px; }
  .grant__sub { font-size: 11.5px; }
  .hostkey { display: flex; flex-direction: column; gap: 10px; align-items: flex-start; }
  .hostkey p { margin: 0; font-size: 12.5px; line-height: 1.5; }
  .hostkey__state { display: flex; align-items: center; gap: 6px; font-weight: 700; color: var(--success); }
  .hostkey__state--warn { color: var(--warning); }
  .hostkey__fp { padding: 10px 12px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); word-break: break-all; font-size: 12px; width: 100%; }
  .done { display: flex; flex-direction: column; align-items: center; gap: 10px; text-align: center; padding-top: 10px; }
  .done p { margin: 0; font-size: 12.5px; }
  .done__icon { color: var(--success); }
  .done__title { font-size: 17px; font-weight: 700; color: var(--text-primary); }
  .cred { width: 100%; text-align: left; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); padding: 4px 12px; }
  .cred__row { display: flex; align-items: center; gap: 10px; padding: 6px 0; font-size: 13px; }
  .cred__row + .cred__row { border-top: 1px solid var(--border); }
  .cred__row > span:first-child { width: 72px; color: var(--text-muted); flex: 0 0 auto; }
  .cred__row > span:nth-child(2) { flex: 1; min-width: 0; word-break: break-all; }
  .cred__pw { color: var(--brand); }
  .chip { display: inline-flex; align-items: center; gap: 4px; padding: 2px 8px; border-radius: 12px; border: 1px solid var(--border); background: none; color: var(--text-secondary); cursor: pointer; font-size: 11.5px; }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
  .done__what { white-space: normal; text-align: left; }
  .accs { display: flex; flex-direction: column; gap: 8px; }
</style>
