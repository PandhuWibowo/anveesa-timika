<script lang="ts">
  // One captcha control for every provider. Binds `value` to what the server
  // expects ({ provider, token }) once solved, null until then.
  //
  // If the configured provider's script can't load (e.g. a US provider behind
  // the Great Firewall) and CAPTCHA_FALLBACK=pow, it switches to the built-in
  // proof-of-work so users are never locked out by a blocked domain.
  import { onDestroy } from 'svelte'
  import { Check, Loader2, ShieldCheck, RotateCw } from '@lucide/svelte'
  import { authApi } from '../lib/api'
  import { ui } from '../lib/ui.svelte'
  import { t } from '../lib/i18n.svelte'
  import { renderWidget, runPopup, solvePow, type CaptchaConfig, type CaptchaValue } from '../lib/captcha'

  let { config, value = $bindable(null) }: { config: CaptchaConfig; value: CaptchaValue | null } = $props()

  // Starts from the configured provider; may switch to the fallback later.
  // svelte-ignore state_referenced_locally
  let active = $state<CaptchaConfig['provider']>(config.provider)
  let phase = $state<'idle' | 'working' | 'done' | 'error'>('idle')
  let message = $state('')
  let el = $state<HTMLDivElement>()
  let resetWidget: (() => void) | null = null
  let started = false

  const isWidget = $derived(['turnstile', 'recaptcha', 'hcaptcha'].includes(active))
  const isPopup = $derived(['geetest', 'tencent'].includes(active))

  async function fallBack(reason: string) {
    if (config.fallback === 'pow' && active !== 'pow') {
      message = `${reason} — using the built-in check instead.`
      active = 'pow'
      phase = 'idle'
      return
    }
    phase = 'error'
    message = reason
  }

  // Mount in-page widgets as soon as their container exists.
  $effect(() => {
    if (!isWidget || !el || started) return
    started = true
    phase = 'working'
    renderWidget({ ...config, provider: active }, el, ui.theme, (token) => {
      value = token ? { provider: active, token } : null
      phase = token ? 'done' : 'idle'
    })
      .then((reset) => {
        resetWidget = reset
        if (phase === 'working') phase = 'idle'
      })
      .catch((e) => fallBack(e.message))
  })

  async function runPow() {
    phase = 'working'
    message = ''
    try {
      const token = await solvePow(async () => {
        const c = await authApi.getCaptchaChallenge({})
        return { algorithm: c.algorithm, challenge: c.challenge, maxnumber: Number(c.maxnumber), salt: c.salt, signature: c.signature }
      })
      value = { provider: 'pow', token }
      phase = 'done'
    } catch (e) {
      phase = 'error'
      message = e instanceof Error ? e.message : String(e)
    }
  }

  async function openPopup() {
    phase = 'working'
    try {
      const token = await runPopup({ ...config, provider: active })
      value = { provider: active, token }
      phase = 'done'
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      if (msg === 'captcha closed') { phase = 'idle'; return }
      await fallBack(msg)
    }
  }

  /** Called after a failed sign-in: every token is single-use. */
  export function reset() {
    value = null
    phase = 'idle'
    if (resetWidget) resetWidget()
  }

  onDestroy(() => { value = null })
</script>

{#if active !== 'none'}
  <div class="captcha">
    {#if isWidget}
      <div bind:this={el} class="captcha__widget"></div>
      {#if phase === 'working'}<div class="captcha__note"><Loader2 size={12} class="spin" /> {t('Loading verification…')}</div>{/if}
    {:else}
      <!-- pow and pop-up providers share one checkbox-style control -->
      <button
        type="button"
        class="captcha__box"
        class:is-done={phase === 'done'}
        disabled={phase === 'working' || phase === 'done'}
        onclick={() => (active === 'pow' ? runPow() : openPopup())}
      >
        <span class="captcha__check">
          {#if phase === 'working'}<Loader2 size={14} class="spin" />
          {:else if phase === 'done'}<Check size={14} />{/if}
        </span>
        <span class="captcha__label">
          {#if phase === 'done'}{t('Verified')}{:else if phase === 'working'}{t('Verifying')}{:else}{t("I'm not a robot")}{/if}
        </span>
        <span class="captcha__brand">
          <ShieldCheck size={12} />
          {active === 'pow' ? t('proof-of-work') : active === 'geetest' ? 'GeeTest' : 'Tencent'}
        </span>
      </button>
    {/if}
    {#if message}
      <div class="captcha__note" class:captcha__note--err={phase === 'error'}>
        {message}
        {#if phase === 'error'}<button type="button" class="captcha__retry" onclick={reset}><RotateCw size={11} /> {t('retry')}</button>{/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .captcha { margin-top: 14px; }
  .captcha__widget { min-height: 65px; }
  .captcha__box {
    width: 100%; display: flex; align-items: center; gap: 10px; padding: 10px 12px;
    border: 1px solid var(--border-2); border-radius: var(--r); background: var(--bg-body);
    color: var(--text-primary); font-family: inherit; font-size: 13px; cursor: pointer; text-align: left;
    transition: border-color var(--dur) var(--ease);
  }
  .captcha__box:hover:not(:disabled) { border-color: var(--brand-ring); }
  .captcha__box:disabled { cursor: default; }
  .captcha__check {
    width: 20px; height: 20px; border-radius: 5px; border: 1.5px solid var(--border-2);
    display: grid; place-items: center; flex-shrink: 0; color: var(--brand);
  }
  .captcha__box.is-done .captcha__check { border-color: var(--brand); background: var(--brand-dim); }
  .captcha__label { flex: 1; }
  .captcha__brand { display: inline-flex; align-items: center; gap: 4px; font-size: 10.5px; color: var(--text-muted); }
  .captcha__note { margin-top: 6px; font-size: 11.5px; color: var(--text-muted); display: flex; align-items: center; gap: 6px; }
  .captcha__note--err { color: var(--danger); }
  .captcha__retry { border: none; background: none; color: inherit; cursor: pointer; display: inline-flex; gap: 3px; align-items: center; font-size: 11px; text-decoration: underline; }
</style>
