<script lang="ts">
  // Set up two-factor authentication: scan → enter a code → save recovery codes.
  // `token` is used instead of the stored session (the forced-setup sign-in).
  import { onMount } from 'svelte'
  import { Loader2, Copy, Check, Download, ShieldCheck, Smartphone } from '@lucide/svelte'
  import { authApi, errMsg } from '../lib/api'
  import type { LoginResponse } from '../gen/timika/v1/auth_pb'

  let { token, ondone, oncancel }: {
    token?: string
    ondone: (session: LoginResponse | undefined) => void
    oncancel?: () => void
  } = $props()

  const opts = () => (token ? { headers: { 'x-timika-token': token } } : {})

  let step = $state<'scan' | 'codes'>('scan')
  let secret = $state('')
  let qr = $state('')
  let code = $state('')
  let busy = $state(false)
  let error = $state('')
  let showSecret = $state(false)
  let codes = $state<string[]>([])
  let session = $state<LoginResponse | undefined>()
  let saved = $state(false)
  let copied = $state('')

  onMount(async () => {
    try {
      const s = await authApi.beginMfaSetup({}, opts())
      secret = s.secret
      qr = s.qrSvg
    } catch (e) { error = errMsg(e) }
  })

  async function confirm(e?: SubmitEvent) {
    e?.preventDefault()
    if (code.replace(/\D/g, '').length !== 6) return
    busy = true
    error = ''
    try {
      const r = await authApi.confirmMfaSetup({ code: code.replace(/\D/g, '') }, opts())
      codes = r.recoveryCodes
      session = r.session
      step = 'codes'
    } catch (err) {
      error = errMsg(err)
      code = ''
    } finally { busy = false }
  }

  function copy(text: string, tag: string) {
    navigator.clipboard.writeText(text)
    copied = tag
    setTimeout(() => { if (copied === tag) copied = '' }, 1400)
  }
  function download() {
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([`Timika recovery codes — each works once.\n\n${codes.join('\n')}\n`], { type: 'text/plain' }))
    a.download = 'timika-recovery-codes.txt'
    a.click()
    URL.revokeObjectURL(a.href)
    saved = true
  }
  const grouped = (s: string) => s.replace(/(.{4})/g, '$1 ').trim()
</script>

{#if step === 'scan'}
  <div class="mfa">
    <ol class="mfa__steps">
      <li><Smartphone size={14} /> Open an authenticator app — Google Authenticator, Microsoft Authenticator, 1Password, Authy…</li>
      <li>Scan this code (or enter the key by hand)</li>
    </ol>
    <div class="mfa__qr">
      {#if qr}{@html qr}{:else if !error}<Loader2 size={20} class="spin" />{/if}
    </div>
    {#if secret}
      {#if showSecret}
        <div class="mfa__secret">
          <span class="mono">{grouped(secret)}</span>
          <button type="button" class="icon-btn" title="Copy" onclick={() => copy(secret, 's')}>{#if copied === 's'}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
        </div>
      {:else}
        <button type="button" class="mfa__link" onclick={() => (showSecret = true)}>Can't scan? Show the key</button>
      {/if}
    {/if}
    <form class="mfa__form" onsubmit={confirm}>
      <label for="mfa-code">Then enter the 6-digit code it shows</label>
      <!-- svelte-ignore a11y_autofocus -->
      <input id="mfa-code" class="base-input mono mfa__code" inputmode="numeric" autocomplete="one-time-code" maxlength="7" placeholder="123 456"
             bind:value={code} autofocus oninput={() => { if (code.replace(/\D/g, '').length === 6) confirm() }} />
      {#if error}<div class="notice notice--error">{error}</div>{/if}
      <div class="mfa__actions">
        {#if oncancel}<button type="button" class="base-btn base-btn--ghost" onclick={oncancel}>Cancel</button>{/if}
        <button class="base-btn base-btn--primary" disabled={busy || code.replace(/\D/g, '').length !== 6}>
          {#if busy}<Loader2 size={14} class="spin" />{/if} Turn on
        </button>
      </div>
    </form>
  </div>
{:else}
  <div class="mfa">
    <div class="mfa__ok"><ShieldCheck size={18} /> Two-factor authentication is on</div>
    <p class="muted mfa__p">Save these recovery codes somewhere safe. If you lose your phone, each one signs you in once. They won't be shown again.</p>
    <div class="mfa__codes">
      {#each codes as c (c)}<span class="mono">{c}</span>{/each}
    </div>
    <div class="mfa__actions mfa__actions--left">
      <button type="button" class="base-btn base-btn--ghost base-btn--sm" onclick={() => { copy(codes.join('\n'), 'c'); saved = true }}>
        {#if copied === 'c'}<Check size={13} /> Copied{:else}<Copy size={13} /> Copy{/if}
      </button>
      <button type="button" class="base-btn base-btn--ghost base-btn--sm" onclick={download}><Download size={13} /> Download .txt</button>
    </div>
    <label class="mfa__check"><input type="checkbox" bind:checked={saved} /> I've saved my recovery codes</label>
    <div class="mfa__actions">
      <button type="button" class="base-btn base-btn--primary" disabled={!saved} onclick={() => ondone(session)}>Continue</button>
    </div>
  </div>
{/if}

<style>
  .mfa { display: flex; flex-direction: column; gap: 10px; }
  .mfa__steps { margin: 0; padding-left: 18px; display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--text-secondary); }
  .mfa__steps li :global(svg) { vertical-align: -2px; color: var(--brand); }
  .mfa__qr { align-self: center; width: 184px; height: 184px; display: grid; place-items: center; border-radius: var(--r); background: #fff; padding: 2px; }
  .mfa__qr :global(svg) { width: 180px; height: 180px; }
  .mfa__secret { display: flex; align-items: center; justify-content: center; gap: 6px; font-size: 13px; letter-spacing: .04em; }
  .mfa__link { align-self: center; padding: 0; border: 0; background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .mfa__link:hover { text-decoration: underline; }
  .mfa__form { display: flex; flex-direction: column; gap: 8px; }
  .mfa__form label { font-size: 12px; color: var(--text-secondary); }
  .mfa__code { font-size: 20px; letter-spacing: .3em; text-align: center; padding: 8px; }
  .mfa__actions { display: flex; justify-content: flex-end; gap: 8px; }
  .mfa__actions--left { justify-content: flex-start; }
  .mfa__ok { display: flex; align-items: center; gap: 8px; font-weight: 700; color: var(--success); }
  .mfa__p { margin: 0; font-size: 12.5px; line-height: 1.5; }
  .mfa__codes { display: grid; grid-template-columns: repeat(2, 1fr); gap: 6px 14px; padding: 12px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); font-size: 13px; text-align: center; }
  .mfa__check { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
</style>
