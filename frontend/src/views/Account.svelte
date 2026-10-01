<script lang="ts">
  // Your own account: two-factor authentication and password.
  import { onMount } from 'svelte'
  import { ShieldCheck, ShieldAlert, KeyRound, Loader2, RefreshCw, Copy, Check, Download } from '@lucide/svelte'
  import { authApi, errMsg } from '../lib/api'
  import { session, clearToken } from '../lib/session.svelte'
  import type { MfaStatus } from '../gen/timika/v1/auth_pb'
  import MfaSetup from '../components/MfaSetup.svelte'

  let status = $state<MfaStatus | null>(null)
  let error = $state('')
  let setup = $state(false)
  // turn off / new codes: both need a current code
  let action = $state<'' | 'disable' | 'codes'>('')
  let code = $state('')
  let busy = $state(false)
  let newCodes = $state<string[]>([])
  let copied = $state(false)

  // password
  let oldPw = $state('')
  let newPw = $state('')
  let pwMsg = $state('')
  let pwErr = $state('')

  const isRoot = !session.me?.username

  async function load() {
    if (isRoot) return
    try { status = await authApi.getMfaStatus({}) } catch (e) { error = errMsg(e) }
  }
  onMount(load)

  async function run() {
    busy = true
    error = ''
    try {
      if (action === 'disable') {
        await authApi.disableMfa({ code: code.trim() })
      } else {
        newCodes = (await authApi.newRecoveryCodes({ code: code.trim() })).codes
      }
      action = ''
      code = ''
      await load()
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }

  async function changePassword(e: SubmitEvent) {
    e.preventDefault()
    pwErr = ''
    try {
      await authApi.changePassword({ oldPassword: oldPw, newPassword: newPw })
      clearToken('Password changed. Sign in with your new password.')
    } catch (err) { pwErr = errMsg(err) }
  }

  function download(codes: string[]) {
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([`Timika recovery codes — each works once.\n\n${codes.join('\n')}\n`], { type: 'text/plain' }))
    a.download = 'timika-recovery-codes.txt'
    a.click()
    URL.revokeObjectURL(a.href)
  }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack acct">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Account</div>
          <h1 class="page-title">{session.me?.username ?? 'root token'}</h1>
          <p class="page-subtitle">{isRoot ? 'You are signed in with the root token — it has no password or two-factor of its own.' : 'Sign-in security for your account.'}</p>
        </div>
      </section>

      {#if !isRoot}
        <section class="page-card">
          <div class="page-card__head">
            <div>
              <div class="page-card__title">Two-factor authentication</div>
              <div class="page-card__sub">A code from your phone in addition to your password.</div>
            </div>
            {#if status}
              {#if status.enabled}<span class="badge badge--success"><ShieldCheck size={11} /> On</span>
              {:else}<span class="badge badge--{status.required ? 'danger' : 'default'}"><ShieldAlert size={11} /> Off</span>{/if}
            {/if}
          </div>
          <div class="page-card__body">
            {#if error}<div class="notice notice--error acct__gap">{error}</div>{/if}
            {#if !status}
              <Loader2 size={16} class="spin" />
            {:else if setup}
              <div class="acct__setup"><MfaSetup oncancel={() => (setup = false)} ondone={() => { setup = false; load() }} /></div>
            {:else if !status.enabled}
              <p class="acct__p">{status.required ? 'Your organization requires two-factor authentication for your account.' : 'Recommended — it stops anyone who learns your password.'}</p>
              <button class="base-btn base-btn--primary" onclick={() => (setup = true)}><ShieldCheck size={14} /> Set up two-factor</button>
            {:else}
              <div class="acct__facts">
                <div><span>Since</span>{status.enabledAt ? new Date(status.enabledAt).toLocaleString() : '—'}</div>
                <div><span>Recovery codes</span>
                  <b class:acct__low={status.recoveryRemaining <= 3}>{status.recoveryRemaining} left</b>
                  {#if status.recoveryRemaining <= 3}<span class="muted"> — make new ones soon</span>{/if}
                </div>
              </div>

              {#if newCodes.length}
                <div class="notice notice--success acct__gap">New recovery codes — the old ones no longer work. Save these now.</div>
                <div class="acct__codes">{#each newCodes as c (c)}<span class="mono">{c}</span>{/each}</div>
                <div class="acct__row">
                  <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => { navigator.clipboard.writeText(newCodes.join('\n')); copied = true }}>{#if copied}<Check size={13} /> Copied{:else}<Copy size={13} /> Copy{/if}</button>
                  <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => download(newCodes)}><Download size={13} /> Download .txt</button>
                  <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (newCodes = [])}>Done</button>
                </div>
              {:else if action}
                <form class="acct__confirm" onsubmit={(e) => { e.preventDefault(); run() }}>
                  <label for="acct-code">{action === 'disable' ? 'Enter a code from your app (or a recovery code) to turn two-factor off' : 'Enter a code from your app to make new recovery codes'}</label>
                  <div class="acct__row">
                    <!-- svelte-ignore a11y_autofocus -->
                    <input id="acct-code" class="base-input mono" bind:value={code} autocomplete="one-time-code" placeholder="123456" autofocus />
                    <button class="base-btn base-btn--{action === 'disable' ? 'danger' : 'primary'}" disabled={busy || !code.trim()}>
                      {#if busy}<Loader2 size={14} class="spin" />{/if} {action === 'disable' ? 'Turn off' : 'Make new codes'}
                    </button>
                    <button type="button" class="base-btn base-btn--ghost" onclick={() => { action = ''; code = ''; error = '' }}>Cancel</button>
                  </div>
                </form>
              {:else}
                <div class="acct__row">
                  <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (action = 'codes')}><RefreshCw size={13} /> New recovery codes</button>
                  {#if !status.required}
                    <button class="base-btn base-btn--ghost base-btn--sm acct__danger" onclick={() => (action = 'disable')}>Turn off</button>
                  {:else}
                    <span class="muted acct__note">Required for your account — can't be turned off. Lost your phone? An administrator can reset it.</span>
                  {/if}
                </div>
              {/if}
            {/if}
          </div>
        </section>

        <section class="page-card">
          <div class="page-card__head">
            <div>
              <div class="page-card__title">Password</div>
              <div class="page-card__sub">Changing it signs you out; sign in again with the new one.</div>
            </div>
          </div>
          <form class="page-card__body acct__pw" onsubmit={changePassword}>
            <input class="base-input" type="password" autocomplete="current-password" placeholder="Current password" bind:value={oldPw} />
            <input class="base-input" type="password" autocomplete="new-password" placeholder="New password" bind:value={newPw} />
            <button class="base-btn base-btn--primary" disabled={!oldPw || !newPw}><KeyRound size={14} /> Change password</button>
            {#if pwErr}<div class="notice notice--error">{pwErr}</div>{/if}
            {#if pwMsg}<div class="notice notice--success">{pwMsg}</div>{/if}
          </form>
        </section>
      {/if}
    </div>
  </div>
</div>

<style>
  .acct { max-width: 720px; }
  .acct__p { margin: 0 0 10px; font-size: 12.5px; color: var(--text-secondary); }
  .acct__gap { margin-bottom: 10px; }
  .acct__setup { max-width: 380px; }
  .acct__facts { display: grid; gap: 6px; font-size: 12.5px; margin-bottom: 12px; }
  .acct__facts div { display: flex; gap: 10px; align-items: baseline; }
  .acct__facts div > span:first-child { width: 120px; color: var(--text-muted); }
  .acct__low { color: var(--warning); }
  .acct__row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .acct__row input { width: 150px; }
  .acct__confirm { display: flex; flex-direction: column; gap: 8px; font-size: 12.5px; color: var(--text-secondary); }
  .acct__danger { color: var(--danger); }
  .acct__note { font-size: 12px; }
  .acct__codes { display: grid; grid-template-columns: repeat(2, minmax(0, 160px)); gap: 6px 14px; padding: 10px 12px; margin-bottom: 8px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); font-size: 13px; }
  .acct__pw { display: flex; flex-direction: column; gap: 8px; max-width: 360px; }
</style>
