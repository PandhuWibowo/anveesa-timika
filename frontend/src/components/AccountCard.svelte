<script lang="ts" module>
  export type Sudo = '' | 'none' | 'password' | 'nopasswd'
  export type Acc = {
    username: string
    kind: 'password' | 'key'
    password: string
    privateKey: string
    passphrase: string
    /** Credentials already saved in the vault. */
    stored: boolean
    /** Apply on the server: create (new) or manage (saved) — by signing in as `via`. */
    provision: boolean
    via: string
    /** Saved accounts: what to do with the password when managing. */
    pwMode: 'keep' | 'generate' | 'type'
    /** Saved accounts, not managing: just replace timika's saved copy. */
    replace: boolean
    show: boolean
    sudo: Sudo
    groups: string[]
    groupInput: string
    /** What timika last applied (saved accounts). */
    sudoNow?: string
    groupsNow: string[]
  }

  export const blankAccount = (username = ''): Acc => ({
    username, kind: 'password', password: '', privateKey: '', passphrase: '', stored: false,
    provision: false, via: '', pwMode: 'generate', replace: false, show: false,
    sudo: 'none', groups: [], groupInput: '', groupsNow: [],
  })

  export const sudoText = (s?: string) => (s === 'nopasswd' ? 'sudo · no password' : s === 'password' ? 'sudo' : s === 'none' ? 'no sudo' : '')

  /** Is this account ready to save? */
  export function accountValid(a: Acc): boolean {
    if (!a.username.trim()) return false
    if (!a.stored) {
      const hasSecret = a.kind === 'password' ? !!a.password : !!a.privateKey.trim()
      return hasSecret && (!a.provision || !!a.via)
    }
    if (a.provision) {
      const pw = a.pwMode !== 'keep' && !!a.password
      return !!a.via && (pw || a.sudo !== '' || a.groups.length > 0)
    }
    return true
  }

  /** The AccountInput sent to the server. */
  export function accountInput(a: Acc) {
    const newPw = a.stored ? (a.provision ? (a.pwMode !== 'keep' ? a.password : '') : a.replace ? a.password : '') : a.password
    return {
      username: a.username.trim(),
      password: a.kind === 'password' && newPw ? newPw : undefined,
      privateKey: a.kind === 'key' && a.privateKey.trim() ? a.privateKey : undefined,
      passphrase: a.kind === 'key' && a.passphrase ? a.passphrase : undefined,
      provision: a.provision,
      provisionVia: a.provision ? a.via : '',
      sudo: a.provision ? a.sudo : '',
      groups: a.provision ? a.groups : [],
    }
  }
</script>

<script lang="ts">
  import { Lock, KeyRound, X, Wand2, Copy, Check, Eye, EyeOff, Settings2, ChevronUp, ShieldAlert, RefreshCw } from '@lucide/svelte'
  import { generatePassword } from '../lib/password'

  let { a = $bindable(), admins, removable, onremove }: { a: Acc; admins: string[]; removable: boolean; onremove: () => void } = $props()

  const SUDO: { id: Sudo; label: string }[] = [
    { id: '', label: 'Keep' },
    { id: 'none', label: 'No sudo' },
    { id: 'password', label: 'Sudo' },
    { id: 'nopasswd', label: 'Sudo, no password' },
  ]
  const SUGGEST = ['docker', 'adm', 'systemd-journal', 'www-data', 'wheel']

  let copied = $state(false)

  const viaLabel = (v: string) => (a.stored && v === a.username.trim() ? `${v} (itself)` : v)
  const defaultVia = () => (admins.includes('root') ? 'root' : a.stored && admins.includes(a.username.trim()) ? a.username.trim() : admins[0] ?? '')

  function generate() {
    a.password = generatePassword(24, a.username.trim())
    a.show = true
  }
  function setPwMode(m: Acc['pwMode']) {
    a.pwMode = m
    if (m === 'generate') generate()
    else { a.password = ''; a.show = false }
  }
  function toggleManage() {
    a.provision = !a.provision
    if (a.provision) {
      if (!a.via || !admins.includes(a.via)) a.via = defaultVia()
      a.replace = false
      if (a.stored) { a.pwMode = 'keep'; a.password = '' }
    }
  }
  function toggleCreate() {
    a.provision = !a.provision
    if (a.provision && (!a.via || !admins.includes(a.via))) a.via = defaultVia()
  }
  function copy() {
    navigator.clipboard.writeText(a.password)
    copied = true
    setTimeout(() => (copied = false), 1400)
  }
  function addGroup(g = a.groupInput) {
    for (const x of g.split(/[,\s]+/).map((v) => v.trim()).filter(Boolean)) if (!a.groups.includes(x)) a.groups.push(x)
    a.groupInput = ''
  }
  // New accounts: generate a password the first time "Password" is shown empty.
  $effect(() => {
    if (!a.stored && a.kind === 'password' && a.pwMode === 'generate' && !a.password && admins.length) generate()
  })
</script>

<div class="ac" class:ac--open={a.provision}>
  <!-- ── header ── -->
  <div class="ac__head">
    <span class="ac__ico">{#if a.kind === 'key'}<KeyRound size={14} />{:else}<Lock size={14} />{/if}</span>
    {#if a.stored}
      <span class="ac__name mono">{a.username}</span>
      <span class="ac__meta">{a.kind === 'key' ? 'key' : 'password'} saved</span>
      {#if a.sudoNow && a.sudoNow !== 'none'}<span class="badge badge--warning">{sudoText(a.sudoNow)}</span>{/if}
      {#each a.groupsNow as g (g)}<span class="badge badge--info">{g}</span>{/each}
    {:else}
      <input class="ac__user mono" bind:value={a.username} placeholder="username" autocomplete="off" spellcheck="false" />
      <div class="seg seg--sm">
        <button class:is-on={a.kind === 'password'} onclick={() => (a.kind = 'password')}>Password</button>
        <button class:is-on={a.kind === 'key'} onclick={() => (a.kind = 'key')}>Key</button>
      </div>
    {/if}
    {#if a.stored}<span class="ac__spacer"></span>{/if}
    {#if a.stored && admins.length && a.username.trim()}
      <button class="base-btn base-btn--ghost base-btn--xs" onclick={toggleManage}>
        {#if a.provision}<ChevronUp size={12} /> Close{:else}<Settings2 size={12} /> Manage on server{/if}
      </button>
    {/if}
    {#if removable}<button class="icon-btn" title="Remove from timika (doesn't touch the server)" onclick={onremove}><X size={14} /></button>{/if}
  </div>

  <!-- ── new account: credentials ── -->
  {#if !a.stored}
    {#if a.kind === 'password'}
      <div class="row">
        <span class="row__label">Password</span>
        <div class="row__body">
          {#if admins.length}
            <div class="seg seg--sm">
              <button class:is-on={a.pwMode === 'generate'} onclick={() => setPwMode('generate')}><Wand2 size={11} /> Generate</button>
              <button class:is-on={a.pwMode === 'type'} onclick={() => setPwMode('type')}>Type</button>
            </div>
          {/if}
          {@render pwField(a.pwMode === 'generate' && admins.length > 0)}
        </div>
      </div>
    {:else}
      <textarea class="base-input mono ac__key" rows="3" bind:value={a.privateKey} spellcheck="false" placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"></textarea>
      <input class="base-input" type="password" autocomplete="off" bind:value={a.passphrase} placeholder="Key passphrase (if any)" />
    {/if}

    {#if a.kind === 'password' && admins.length}
      <button class="switch" class:is-on={a.provision} role="switch" aria-checked={a.provision} onclick={toggleCreate}>
        <span class="switch__track"><span class="switch__dot"></span></span>
        <span class="switch__text"><b>Create on the server</b> — signs in as {a.via || defaultVia()} and runs useradd</span>
      </button>
    {:else if a.kind === 'password' && a.password}
      <p class="ac__note">This saves the account's existing password. To create new accounts on the server, add an account timika can sign in with first (e.g. root).</p>
    {/if}
  {/if}

  <!-- ── manage on the server (saved accounts) / create options (new) ── -->
  {#if a.provision}
    <div class="panel">
      <div class="row">
        <span class="row__label">Sign in as</span>
        <div class="row__body">
          <select class="base-select row__select" bind:value={a.via}>
            {#each admins as v (v)}<option value={v}>{viaLabel(v)}</option>{/each}
          </select>
        </div>
      </div>

      {#if a.stored && a.kind === 'password'}
        <div class="row">
          <span class="row__label">Password</span>
          <div class="row__body">
            <div class="seg seg--sm">
              <button class:is-on={a.pwMode === 'keep'} onclick={() => setPwMode('keep')}>Keep</button>
              <button class:is-on={a.pwMode === 'generate'} onclick={() => setPwMode('generate')}><Wand2 size={11} /> Generate new</button>
              <button class:is-on={a.pwMode === 'type'} onclick={() => setPwMode('type')}>Type new</button>
            </div>
            {#if a.pwMode !== 'keep'}{@render pwField(a.pwMode === 'generate')}{/if}
            <button class="ac__link" onclick={() => { a.provision = false; a.replace = true; a.password = ''; a.show = true }}>Already changed on the server? Update timika's saved copy only</button>
          </div>
        </div>
      {/if}

      <div class="row">
        <span class="row__label">Sudo</span>
        <div class="row__body">
          <div class="seg seg--sm">
            {#each SUDO.filter((x) => x.id !== '' || a.stored) as o (o.id)}
              <button class:is-on={a.sudo === o.id} onclick={() => (a.sudo = o.id)}>{o.label}</button>
            {/each}
          </div>
          {#if a.sudo === 'nopasswd'}<p class="ac__warn"><ShieldAlert size={12} /> Root access without a password prompt — best for automation accounts.</p>{/if}
        </div>
      </div>

      <div class="row">
        <span class="row__label">Groups</span>
        <div class="row__body">
          <div class="groups">
            {#each a.groups as g, gi (g)}
              <span class="chip is-on mono">{g}<button title="Remove" onclick={() => a.groups.splice(gi, 1)}><X size={10} /></button></span>
            {/each}
            <input class="groups__input mono" bind:value={a.groupInput} placeholder={a.groups.length ? 'add…' : 'type a group, Enter'}
                   onkeydown={(e) => { if (e.key === 'Enter' || e.key === ',') { e.preventDefault(); addGroup() } }} onblur={() => a.groupInput && addGroup()} />
          </div>
          <div class="suggest">
            {#each SUGGEST.filter((g) => !a.groups.includes(g)) as g (g)}
              <button class="chip" onclick={() => addGroup(g)}>+ {g}</button>
            {/each}
          </div>
        </div>
      </div>
      <p class="ac__note">
        {#if a.stored}Applied over SSH as <b>{a.via}</b> when you save; the sudo rule is checked with visudo first.
        {:else}On save: creates <b>{a.username || 'the account'}</b> if it doesn't exist, sets the password, adds groups that exist, writes a visudo-checked sudo rule, then tests the login.{/if}
      </p>
    </div>
  {:else if a.stored && a.kind === 'password'}
    {#if a.replace}
      <div class="row">
        <span class="row__label">Saved copy</span>
        <div class="row__body">
          {@render pwField(false)}
          <p class="ac__warn"><ShieldAlert size={12} /> Only timika's saved copy changes — the server must already use this password.</p>
          <button class="ac__link" onclick={() => { a.replace = false; a.password = '' }}>Cancel</button>
        </div>
      </div>
    {/if}
  {/if}
</div>

{#snippet pwField(generated: boolean)}
  <div class="pw">
    <input class="base-input" class:mono={a.show} type={a.show ? 'text' : 'password'} bind:value={a.password} readonly={generated}
           autocomplete="new-password" placeholder="password" />
    {#if generated}<button class="icon-btn" title="Generate another" onclick={generate}><RefreshCw size={13} /></button>{/if}
    <button class="icon-btn" title={a.show ? 'Hide' : 'Show'} onclick={() => (a.show = !a.show)}>{#if a.show}<EyeOff size={13} />{:else}<Eye size={13} />{/if}</button>
    <button class="icon-btn" title="Copy" disabled={!a.password} onclick={copy}>{#if copied}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
  </div>
{/snippet}

<style>
  .ac { display: flex; flex-direction: column; gap: 10px; padding: 12px; border-radius: var(--r-lg); background: var(--bg-body); border: 1px solid var(--border); transition: border-color var(--dur) var(--ease); }
  .ac--open { border-color: var(--brand-ring); }
  .ac__head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; min-height: 30px; }
  .ac__ico { width: 26px; height: 26px; border-radius: var(--r-sm); display: grid; place-items: center; background: var(--brand-soft); color: var(--brand); flex: 0 0 auto; }
  .ac__name { font-weight: 700; color: var(--text-primary); font-size: 13.5px; }
  .ac__meta { font-size: 11.5px; color: var(--text-muted); }
  .ac__spacer { flex: 1; }
  .ac__user { flex: 1; min-width: 120px; height: 30px; padding: 0 10px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); color: var(--text-primary); font-size: 13px; outline: 0; }
  .ac__user:focus { border-color: var(--brand-ring); box-shadow: 0 0 0 3px var(--brand-dim); }
  .ac__key { width: 100%; resize: vertical; font-size: 11.5px; }
  .ac__note { margin: 0; font-size: 11.5px; line-height: 1.5; color: var(--text-muted); }
  .ac__warn { display: flex; align-items: center; gap: 5px; margin: 4px 0 0; font-size: 11.5px; color: var(--warning); }
  .ac__link { align-self: flex-start; padding: 0; border: 0; background: none; color: var(--text-muted); font-size: 11.5px; cursor: pointer; }
  .ac__link:hover { text-decoration: underline; }
  .ac__link:hover { color: var(--text-secondary); }

  .panel { display: flex; flex-direction: column; gap: 10px; padding-top: 10px; border-top: 1px solid var(--border); }
  .row { display: grid; grid-template-columns: 76px 1fr; gap: 10px; align-items: start; }
  .row__label { font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: .03em; padding-top: 7px; }
  .row__body { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .row__select { align-self: flex-start; min-width: 180px; }

  .seg { display: inline-flex; align-self: flex-start; flex-wrap: wrap; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); }
  .seg button { display: flex; align-items: center; gap: 4px; padding: 4px 9px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; white-space: nowrap; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }

  .pw { display: flex; gap: 2px; align-items: center; }
  .pw input { flex: 1; min-width: 0; }

  .switch { display: flex; align-items: center; gap: 10px; padding: 0; border: 0; background: none; cursor: pointer; text-align: left; color: var(--text-secondary); font-size: 12.5px; }
  .switch__track { width: 30px; height: 17px; border-radius: 9px; background: var(--bg-elevated); border: 1px solid var(--border); position: relative; flex: 0 0 auto; transition: background var(--dur) var(--ease); }
  .switch__dot { position: absolute; top: 2px; left: 2px; width: 11px; height: 11px; border-radius: 50%; background: var(--text-muted); transition: transform var(--dur) var(--ease), background var(--dur) var(--ease); }
  .switch.is-on .switch__track { background: var(--brand-soft); border-color: var(--brand-ring); }
  .switch.is-on .switch__dot { transform: translateX(13px); background: var(--brand); }
  .switch__text b { color: var(--text-primary); font-weight: 600; }

  .groups { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; min-height: 32px; padding: 3px 6px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .groups:focus-within { border-color: var(--brand-ring); }
  .groups__input { flex: 1; min-width: 110px; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12px; padding: 3px; }
  .chip { display: inline-flex; align-items: center; gap: 4px; padding: 2px 8px; border-radius: 12px; border: 1px solid var(--border); background: none; color: var(--text-secondary); cursor: pointer; font-size: 11.5px; }
  .chip:hover { border-color: var(--brand-ring); color: var(--brand); }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
  .chip button { display: flex; background: none; border: 0; padding: 0; color: inherit; cursor: pointer; }
  .suggest { display: flex; gap: 5px; flex-wrap: wrap; }
</style>
