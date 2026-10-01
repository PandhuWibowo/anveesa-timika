<script lang="ts">
  // Sign-in: username + password + captcha (and the system-use notice when the
  // policy requires it), or the root token as break-glass. Handles the forced
  // password change (first sign-in / expired password) inline.
  import { onMount, tick } from 'svelte'
  import { Loader2, ShieldCheck, Eye, EyeOff, KeyRound, UserRound, Check, X, Info, Wand2, Copy } from '@lucide/svelte'
  import { generatePassword } from '../lib/password'
  import { authApi, errMsg } from '../lib/api'
  import { session, setToken, clearToken } from '../lib/session.svelte'
  import type { CaptchaConfig, CaptchaValue } from '../lib/captcha'
  import type { LoginConfig, LoginResponse } from '../gen/timika/v1/auth_pb'
  import MfaSetup from '../components/MfaSetup.svelte'
  import Captcha from '../components/Captcha.svelte'
  import { i18n, setLang, t } from '../lib/i18n.svelte'

  let cfg = $state<LoginConfig | null>(null)
  let tab = $state<'account' | 'token'>('account')
  let busy = $state(false)
  let error = $state('')
  let info = $state(session.signedOutReason)

  // account
  let username = $state('')
  let password = $state('')
  let showPw = $state(false)
  let ack = $state(false)
  let captcha = $state<CaptchaValue | null>(null)
  let captchaRef = $state<{ reset: () => void }>()
  const captchaCfg = $derived<CaptchaConfig>({
    provider: (cfg?.captcha?.provider ?? 'none') as CaptchaConfig['provider'],
    fallback: (cfg?.captcha?.fallback ?? null) as CaptchaConfig['fallback'],
    site_key: cfg?.captcha?.siteKey,
    domain: cfg?.captcha?.domain,
  })
  const needsCaptcha = $derived(!!cfg && captchaCfg.provider !== 'none')
  const canSubmit = $derived(
    !busy && username.trim() !== '' && password !== '' && (!needsCaptcha || !!captcha) && (!cfg?.bannerAckRequired || ack),
  )

  // forced change
  let change = $state<{ token: string; expired: boolean } | null>(null)
  let oldPw = $state('')
  let newPw = $state('')
  let newPw2 = $state('')
  let showNew = $state(false)
  let generated = $state(false)
  let copiedNew = $state(false)

  function generateNew() {
    newPw = generatePassword(Math.max(20, cfg?.policy?.minLength ?? 15), username.trim())
    newPw2 = newPw
    showNew = true
    generated = true
    copiedNew = false
  }
  function copyNew() {
    navigator.clipboard.writeText(newPw)
    copiedNew = true
    setTimeout(() => (copiedNew = false), 1500)
  }

  // token
  let rootToken = $state('')

  // two-factor: the code step after the password, and forced setup
  let mfaStep = $state<{ token: string; username: string } | null>(null)
  let mfaCode = $state('')
  let useRecovery = $state(false)
  let mfaSetup = $state<LoginResponse | null>(null)

  /** A completed sign-in: forced password change, forced 2FA setup, or in. */
  function signedIn(r: LoginResponse) {
    if (r.mustChangePassword) {
      change = { token: r.token, expired: r.passwordExpired }
      oldPw = password
      password = ''
      return
    }
    if (r.mustSetupMfa) {
      mfaSetup = r
      return
    }
    if (r.usedRecoveryCode) info = `${t('Signed in with a recovery code')} — ${r.recoveryCodesLeft} ${t('left')}`
    setToken(r.token, {
      username: r.username,
      policies: r.policies,
      expiresAt: r.expiresAt,
      maxExpiresAt: r.maxExpiresAt,
      lastLogin: r.lastLogin && { time: r.lastLogin.time, ip: r.lastLogin.ip },
      lastFailedLogin: r.lastFailedLogin && { time: r.lastFailedLogin.time, ip: r.lastFailedLogin.ip },
    })
  }

  async function verifyMfa(e?: SubmitEvent) {
    e?.preventDefault()
    if (!mfaStep || busy) return
    const code = useRecovery ? mfaCode.trim() : mfaCode.replace(/\D/g, '')
    if (!code || (!useRecovery && code.length !== 6)) return
    busy = true
    error = ''
    try {
      const r = await authApi.verifyMfa({ mfaToken: mfaStep.token, code })
      mfaStep = null
      mfaCode = ''
      signedIn(r)
    } catch (err) {
      error = errMsg(err)
      mfaCode = ''
      // The attempt is over (too many tries / expired): back to the password.
      if (/password again|expired/.test(error)) { mfaStep = null; captchaRef?.reset() }
    } finally {
      busy = false
    }
  }

  onMount(async () => {
    try { cfg = await authApi.getLoginConfig({}) } catch (e) { error = errMsg(e) }
  })

  async function signIn(e: SubmitEvent) {
    e.preventDefault()
    busy = true
    error = ''
    info = ''
    try {
      const r = await authApi.login({ username, password, captcha: captcha ?? undefined, bannerAck: ack })
      if (r.mfaRequired && r.mfaToken) {
        mfaStep = { token: r.mfaToken, username: r.username }
        useRecovery = false
        mfaCode = ''
        return
      }
      signedIn(r)
    } catch (e) {
      error = errMsg(e)
      // Every captcha token is single-use.
      captchaRef?.reset()
    } finally {
      busy = false
    }
  }

  // Live checks mirroring the policy (the server also enforces blocklist/history).
  const rules = $derived.by(() => {
    if (!cfg) return []
    const p = cfg.policy
    if (!p) return []
    const classes = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^A-Za-z0-9]/].filter((r) => r.test(newPw)).length
    const out = [{ ok: [...newPw].length >= p.minLength, text: `${t('At least')} ${p.minLength} ${t('characters')}` }]
    if (p.requireClasses) out.push({ ok: classes >= p.requireClasses, text: `${p.requireClasses} ${t('of: lowercase, uppercase, digit, symbol')}` })
    out.push({ ok: !!newPw && !newPw.toLowerCase().includes(username.trim().toLowerCase()), text: t('Does not contain your username') })
    out.push({ ok: !!newPw && newPw === newPw2, text: t('Both entries match') })
    return out
  })

  async function changePassword(e: SubmitEvent) {
    e.preventDefault()
    if (!change) return
    busy = true
    error = ''
    try {
      // The restricted session token is only used for this one call.
      await authApi.changePassword(
        { oldPassword: oldPw, newPassword: newPw },
        { headers: { 'x-timika-token': change.token } },
      )
      change = null
      oldPw = newPw = newPw2 = ''
      info = t('Password changed. Sign in with your new password.')
      await tick()
      captchaRef?.reset()
    } catch (e) {
      error = errMsg(e)
    } finally {
      busy = false
    }
  }

  async function signInWithToken(e: SubmitEvent) {
    e.preventDefault()
    busy = true
    error = ''
    try {
      setToken(rootToken.trim())
      const me = await authApi.lookupSelf({})
      setToken(rootToken.trim(), {
        username: me.username,
        policies: me.policies,
        expiresAt: me.expiresAt,
        maxExpiresAt: me.maxExpiresAt,
      })
    } catch (e) {
      clearToken()
      error = errMsg(e)
    } finally {
      busy = false
    }
  }
</script>

{#if mfaSetup}
  <div class="lg__title"><ShieldCheck size={18} /> {t('Set up two-factor authentication')}</div>
  <p class="lg__hint">{t('Your organization requires a second step when signing in. It takes a minute.')}</p>
  <MfaSetup token={mfaSetup.token} oncancel={() => { mfaSetup = null; clearToken() }} ondone={(s) => { const done = s; mfaSetup = null; if (done) signedIn(done) }} />
{:else if mfaStep}
  <form onsubmit={verifyMfa} class="lg__mfa">
    <div class="lg__title"><ShieldCheck size={18} /> {t('Two-factor authentication')}</div>
    <p class="lg__hint">
      {useRecovery ? t('Enter one of your recovery codes.') : t('Enter the 6-digit code from your authenticator app.')}
    </p>
    {#if useRecovery}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="base-input mono lg__mfa-code lg__mfa-code--rec" bind:value={mfaCode} placeholder="abcde-12345" autocomplete="off" autofocus />
    {:else}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="base-input mono lg__mfa-code" bind:value={mfaCode} inputmode="numeric" autocomplete="one-time-code" maxlength="7" placeholder="123 456" autofocus
             oninput={() => { if (mfaCode.replace(/\D/g, '').length === 6) verifyMfa() }} />
    {/if}
    <button class="base-btn base-btn--primary lg__submit" disabled={busy}>{#if busy}<Loader2 size={14} class="spin" />{/if} {t('Verify')}</button>
    <div class="lg__mfa-links">
      <button type="button" class="lg__link" onclick={() => { useRecovery = !useRecovery; mfaCode = ''; error = '' }}>
        {useRecovery ? t('Use the authenticator app') : t('Lost your phone? Use a recovery code')}
      </button>
      <button type="button" class="lg__link" onclick={() => { mfaStep = null; error = ''; captchaRef?.reset() }}>{t('Back')}</button>
    </div>
  </form>
{:else if change}
  <form onsubmit={changePassword}>
    <div class="lg__title"><KeyRound size={18} /> {change.expired ? t('Your password has expired') : t('Set a new password')}</div>
    <p class="lg__hint">
      {change.expired
        ? `${t('Passwords must be changed every')} ${cfg?.policy?.maxAgeDays} ${t('days')}`
        : t('Your administrator set this password — choose your own before continuing.')}
    </p>
    <label class="lg__label" for="old">{t('Current password')}</label>
    <input id="old" class="base-input" type="password" autocomplete="current-password" bind:value={oldPw} />
    <div class="lg__label-row">
      <label class="lg__label" for="new">{t('New password')}</label>
      <button type="button" class="lg__gen" title={t('Generate a strong password')} onclick={generateNew}><Wand2 size={12} /> {t('Generate')}</button>
    </div>
    <div class="lg__pw lg__pw--two">
      <!-- svelte-ignore a11y_autofocus -->
      <input id="new" class="base-input" class:mono={generated && showNew} type={showNew ? 'text' : 'password'} autocomplete="new-password"
             bind:value={newPw} oninput={() => (generated = false)} autofocus />
      {#if generated}
        <button type="button" class="icon-btn" title={t('Copy')} onclick={copyNew}>{#if copiedNew}<Check size={15} />{:else}<Copy size={15} />{/if}</button>
      {/if}
      <button type="button" class="icon-btn" title={showNew ? t('Hide password') : t('Show password')} onclick={() => (showNew = !showNew)}>
        {#if showNew}<EyeOff size={15} />{:else}<Eye size={15} />{/if}
      </button>
    </div>
    {#if generated}<p class="lg__gen-note">{copiedNew ? t('Copied') + ' · ' : ''}{t('Save it in your password manager now — it is not shown again.')}</p>{/if}
    <label class="lg__label" for="new2">{t('Repeat new password')}</label>
    <input id="new2" class="base-input" type={showNew ? 'text' : 'password'} autocomplete="new-password" bind:value={newPw2} />
    <ul class="lg__rules">
      {#each rules as r (r.text)}
        <li class:ok={r.ok}>{#if r.ok}<Check size={12} />{:else}<X size={12} />{/if} {r.text}</li>
      {/each}
      {#if cfg?.policy?.history}<li class="muted"><Info size={12} /> {t('Not one of your last')} {cfg.policy?.history} {t('passwords')}</li>{/if}
      <li class="muted"><Info size={12} /> {t('Not a commonly used password')}</li>
    </ul>
    <button class="base-btn base-btn--primary lg__submit" disabled={busy || rules.some((r) => !r.ok)}>
      {#if busy}<Loader2 size={14} class="spin" />{/if} {t('Change password')}
    </button>
    <button type="button" class="base-btn base-btn--ghost lg__submit" onclick={() => (change = null)}>{t('Cancel')}</button>
  </form>
{:else}
  <div class="lg__top">
    <div class="page-tabs">
      <button class="page-tab" class:is-active={tab === 'account'} onclick={() => (tab = 'account')}><UserRound size={13} /> {t('Account')}</button>
      <button class="page-tab" class:is-active={tab === 'token'} onclick={() => (tab = 'token')}><KeyRound size={13} /> {t('Root token')}</button>
    </div>
    <div class="lg__lang" role="group" aria-label="Language">
      <button class:is-on={i18n.lang === 'en'} onclick={() => setLang('en')}>EN</button>
      <button class:is-on={i18n.lang === 'zh'} onclick={() => setLang('zh')}>中文</button>
    </div>
  </div>

  {#if info}<div class="lg__info">{info}</div>{/if}

  {#if tab === 'account'}
    <form onsubmit={signIn} autocomplete="on">
      <label class="lg__label" for="username">{t('Username')}</label>
      <!-- svelte-ignore a11y_autofocus -->
      <input id="username" class="base-input" autocomplete="username" autocapitalize="none" spellcheck="false" bind:value={username} autofocus />
      <label class="lg__label" for="password">{t('Password')}</label>
      <div class="lg__pw">
        <!-- NIST 800-63B: allow paste and let users see what they typed. -->
        <input id="password" class="base-input" type={showPw ? 'text' : 'password'} autocomplete="current-password" bind:value={password} />
        <button type="button" class="icon-btn" title={showPw ? t('Hide password') : t('Show password')} onclick={() => (showPw = !showPw)}>
          {#if showPw}<EyeOff size={15} />{:else}<Eye size={15} />{/if}
        </button>
      </div>

      {#if cfg && needsCaptcha}
        <Captcha bind:this={captchaRef} config={captchaCfg} bind:value={captcha} />
      {/if}

      {#if cfg?.banner}
        <div class="lg__banner" role="note">
          <div class="lg__banner-title"><ShieldCheck size={13} /> {t('System use notification')}</div>
          <p>{i18n.lang === 'zh' && cfg.bannerZh ? cfg.bannerZh : cfg.banner}</p>
          {#if cfg.bannerAckRequired}
            <label class="lg__check"><input type="checkbox" bind:checked={ack} /> {t('I understand and agree')}</label>
          {/if}
        </div>
      {/if}

      <button class="base-btn base-btn--primary lg__submit" disabled={!canSubmit}>
        {#if busy}<Loader2 size={14} class="spin" />{/if} {t('Sign in')}
      </button>
      {#if cfg}
        <p class="lg__fine">
          {cfg.policy?.lockoutThreshold ? `${cfg.policy?.lockoutThreshold} ${t('failed attempts lock the account for')} ${cfg.policy?.lockoutMinutes} ${t('min')}${i18n.lang === 'zh' ? '，' : '. '}` : ''}
          {t('Sessions end after')} {cfg.policy?.sessionIdleMinutes} {t('min of inactivity.')}
          {#if cfg.privacyNoticeUrl}<a href={cfg.privacyNoticeUrl} target="_blank" rel="noopener">{t('Privacy notice')}</a>{/if}
        </p>
      {/if}
    </form>
  {:else}
    <form onsubmit={signInWithToken}>
      <p class="lg__hint">{t("Break-glass access with the root token from initialization. Prefer a named account — it's what the audit log can attribute.")}</p>
      <label class="lg__label" for="token">{t('Token')}</label>
      <input id="token" class="base-input mono" type="password" autocomplete="off" placeholder="tmk.…" bind:value={rootToken} />
      <button class="base-btn base-btn--primary lg__submit" disabled={busy || !rootToken.trim()}>
        {#if busy}<Loader2 size={14} class="spin" />{/if} {t('Sign in')}
      </button>
    </form>
  {/if}
{/if}

{#if error}<div class="lg__error" role="alert">{error}</div>{/if}

<style>
  .lg__top { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 14px; }
  .lg__lang { display: inline-flex; border: 1px solid var(--border-2); border-radius: 999px; overflow: hidden; }
  .lg__lang button { border: none; background: none; color: var(--text-muted); font: inherit; font-size: 11px; padding: 4px 9px; cursor: pointer; }
  .lg__lang button.is-on { background: var(--brand-dim); color: var(--brand); }
  .lg__title { display: flex; align-items: center; gap: 8px; font-size: 16px; font-weight: 600; margin-bottom: 10px; }
  .lg__hint { font-size: 12px; color: var(--text-muted); margin: 0 0 12px; line-height: 1.55; }
  .lg__label { display: block; font-size: 11px; color: var(--text-muted); margin: 12px 0 5px; letter-spacing: .03em; text-transform: uppercase; }
  .base-input { width: 100%; }
  .lg__pw { position: relative; }
  .lg__pw .base-input { padding-right: 38px; }
  .lg__pw .icon-btn { position: absolute; right: 4px; top: 50%; transform: translateY(-50%); }
  .lg__pw--two .base-input { padding-right: 68px; }
  .lg__pw--two .icon-btn:nth-of-type(1):not(:last-of-type) { right: 34px; }
  .lg__submit { width: 100%; margin-top: 14px; }
  .lg__banner { margin-top: 14px; padding: 10px 12px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border-2); }
  .lg__banner-title { display: flex; align-items: center; gap: 6px; font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: .04em; color: var(--text-secondary); margin-bottom: 6px; }
  .lg__banner p { font-size: 11.5px; line-height: 1.5; color: var(--text-muted); max-height: 110px; overflow-y: auto; }
  .lg__check { display: flex; align-items: center; gap: 8px; margin-top: 8px; font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
  .lg__fine { margin-top: 10px; font-size: 11px; color: var(--text-muted); line-height: 1.5; }
  .lg__rules { list-style: none; margin: 10px 0 0; display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--danger); }
  .lg__rules li { display: flex; align-items: center; gap: 6px; }
  .lg__rules li.ok { color: var(--success); }
  .lg__rules li.muted { color: var(--text-muted); }
  .lg__info { margin-bottom: 10px; padding: 9px 11px; border-radius: 8px; font-size: 12px; background: var(--info-bg); color: var(--info); }
  .lg__error { margin-top: 14px; padding: 9px 11px; border-radius: 8px; font-size: 12px; background: var(--danger-bg); color: var(--danger); border: 1px solid rgba(217,91,91,0.3); }
  .lg__label-row { display: flex; align-items: center; justify-content: space-between; }
  .lg__gen { display: inline-flex; align-items: center; gap: 4px; padding: 2px 8px; border-radius: var(--r-sm); border: 1px solid var(--border); background: none; color: var(--brand); font-size: 11.5px; cursor: pointer; margin-top: 10px; }
  .lg__gen:hover { background: var(--brand-dim); border-color: var(--brand-ring); }
  .lg__gen-note { margin: 6px 0 0; font-size: 11.5px; color: var(--warning); }
  .lg__mfa { display: flex; flex-direction: column; gap: 10px; }
  .lg__mfa-code { font-size: 22px; letter-spacing: .32em; text-align: center; padding: 10px; }
  .lg__mfa-code--rec { font-size: 16px; letter-spacing: .08em; }
  .lg__mfa-links { display: flex; justify-content: space-between; }
  .lg__link { padding: 0; border: 0; background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .lg__link:hover { color: var(--brand); text-decoration: underline; }
</style>
