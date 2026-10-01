<script lang="ts">
  // The bare pre-shell screen. Three stages, driven entirely by seal status:
  //   uninitialized → init (generate root key, split into shares)
  //   sealed        → unseal (submit shares until threshold)
  //   no token      → sign in with a vault token
  import { Loader2, ShieldCheck, KeyRound, LockOpen, Copy, Check, Download } from '@lucide/svelte'
  import { sys, errMsg } from '../lib/api'
  import { session, refreshStatus, setToken, type Instance } from '../lib/session.svelte'
  import type { InstanceUnseal } from '../gen/timika/v1/sys_pb'
  import Login from './Login.svelte'
  import { t } from '../lib/i18n.svelte'

  const auto = $derived(!!session.status?.sealType && session.status.sealType !== 'shamir')
  const stage = $derived(
    !session.status?.initialized ? 'init' : session.status.sealed ? 'unseal' : 'token',
  )

  let busy = $state(false)
  let error = $state('')

  // ── init ──
  let shares = $state(5)
  let threshold = $state(3)
  let initResult = $state<{ keys: string[]; rootToken: string; sealType: string } | null>(null)
  let saved = $state(false)
  let copied = $state('')

  async function submitInit(e: SubmitEvent) {
    e.preventDefault()
    await run(async () => {
      const r = await sys.init(auto ? {} : { secretShares: shares, secretThreshold: threshold })
      initResult = { keys: r.keys, rootToken: r.rootToken, sealType: r.sealType }
      setToken(r.rootToken)
    })
  }

  function copy(text: string) {
    navigator.clipboard.writeText(text)
    copied = text
    setTimeout(() => { if (copied === text) copied = '' }, 1400)
  }

  function download() {
    if (!initResult) return
    const blob = new Blob([JSON.stringify({ keys: initResult.keys, root_token: initResult.rootToken, seal_type: initResult.sealType, threshold }, null, 2)], { type: 'application/json' })
    const a = document.createElement('a')
    a.href = URL.createObjectURL(blob)
    a.download = 'timika-init.json'
    a.click()
    URL.revokeObjectURL(a.href)
  }

  async function finishInit() {
    initResult = null
    await refreshStatus()
  }

  // ── unseal ──
  let share = $state('')
  // Scaled out: behind a load balancer this page reaches one replica at random,
  // so by default a key is sent to every instance (server-side fan-out).
  let instances = $state<Instance[]>([])
  let unsealAll = $state(true)
  let fanout = $state<InstanceUnseal[]>([])
  const live = $derived(instances.filter((i) => !i.stale))

  async function loadInstances() {
    try {
      instances = (await sys.instances({})).instances
    } catch { instances = [] }
  }
  $effect(() => { if (stage === 'unseal') loadInstances() })

  async function submitShare(e: SubmitEvent) {
    e.preventDefault()
    await run(async () => {
      const all = unsealAll && live.length > 1
      const res = await sys.unseal({ key: share, all })
      fanout = res.instances
      session.status = res.status ?? null
      share = ''
      if (all) loadInstances()
    })
  }

  async function resetUnseal() {
    await run(async () => {
      session.status = (await sys.unseal({ reset: true })).status ?? null
    })
  }

  async function run(fn: () => Promise<void>) {
    busy = true
    error = ''
    try { await fn() } catch (e) {
      error = errMsg(e)
      await refreshStatus().catch(() => {})
    } finally { busy = false }
  }
</script>

<div class="gate">
  <div class="gate__card" class:gate__card--wide={initResult} class:gate__card--login={stage === 'token' && !initResult}>
    <div class="gate__brand">
      <div class="gate__logo">t</div>
      <div class="gate__name">Anveesa <span>Timika</span></div>
    </div>

    {#if initResult}
      <div class="gate__title"><ShieldCheck size={18} /> Vault initialized</div>
      {#if initResult.keys.length}
        <p class="gate__hint">
          The root key was split into <b>{initResult.keys.length}</b> shares; any <b>{threshold}</b> of them rebuild it.
          These are shown <b>once</b> and never stored — not in Redis, not anywhere. Lose more than
          {initResult.keys.length - threshold} and the data is unrecoverable.
        </p>
      {:else}
        <p class="gate__hint">
          Auto-unseal (<b>{initResult.sealType}</b>): the root key is wrapped by your key service, so there are no
          unseal keys — every instance unseals itself. Store the root token below; it is shown <b>once</b>.
          If the key service's key is ever deleted, the vault is unrecoverable.
        </p>
      {/if}
      <div class="gate__keys">
        {#each initResult.keys as k, i (k)}
          <div class="gate__key">
            <span class="gate__key-n">{i + 1}</span>
            <span class="mono gate__key-v">{k}</span>
            <button class="icon-btn" title="Copy" onclick={() => copy(k)}>
              {#if copied === k}<Check size={14} />{:else}<Copy size={14} />{/if}
            </button>
          </div>
        {/each}
        <div class="gate__key gate__key--token">
          <span class="gate__key-n">root</span>
          <span class="mono gate__key-v">{initResult.rootToken}</span>
          <button class="icon-btn" title="Copy" onclick={() => copy(initResult!.rootToken)}>
            {#if copied === initResult.rootToken}<Check size={14} />{:else}<Copy size={14} />{/if}
          </button>
        </div>
      </div>
      <button class="base-btn base-btn--ghost gate__submit" onclick={download}><Download size={14} /> Download as JSON</button>
      <label class="gate__check"><input type="checkbox" bind:checked={saved} /> I have stored {initResult.keys.length ? 'these keys' : 'the root token'} somewhere safe</label>
      <button class="base-btn base-btn--primary gate__submit" disabled={!saved} onclick={finishInit}>
        {initResult.keys.length ? 'Continue to unseal' : 'Continue'}
      </button>

    {:else if stage === 'init'}
      <form onsubmit={submitInit}>
        <div class="gate__title"><KeyRound size={18} /> Initialize vault</div>
        <p class="gate__hint">
          Generates a 256-bit root key and the data-encryption keyring, then splits the root key with
          Shamir's Secret Sharing. Storage only ever receives ciphertext.
        </p>
        {#if session.status?.storage === 'raft'}
          <div class="gate__join">
            Raft node <b>{session.status.node}</b>: initializing makes it a new one-node cluster. Initialize
            <b>only one</b> node — the others join it automatically and are unsealed with the same keys.
          </div>
        {/if}
        {#if auto}
          <div class="gate__join">
            Auto-unseal is configured: <b>{session.status?.autoUnseal}</b>. The root key will be wrapped by that
            key service — no unseal keys are issued and every instance unseals itself.
          </div>
        {:else}
        <div class="gate__row">
          <div>
            <label class="gate__label" for="shares">Key shares</label>
            <input id="shares" class="base-input" type="number" min="1" max="16" bind:value={shares} />
          </div>
          <div>
            <label class="gate__label" for="threshold">Threshold</label>
            <input id="threshold" class="base-input" type="number" min="1" max={shares} bind:value={threshold} />
          </div>
        </div>
        {/if}
        <button class="base-btn base-btn--primary gate__submit" disabled={busy || (!auto && (threshold < 1 || threshold > shares))}>
          {#if busy}<Loader2 size={14} class="spin" />{/if} Initialize
        </button>
      </form>

    {:else if stage === 'unseal' && auto}
      <div class="gate__title"><LockOpen size={18} /> Auto-unseal</div>
      <p class="gate__hint">
        This vault unseals itself through <b>{session.status?.autoUnseal ?? session.status?.sealType}</b>.
      </p>
      {#if session.status?.autoUnsealPaused}
        <div class="gate__join">
          This instance was <b>sealed on purpose</b>, so auto-unseal is paused. Restart it
          (<span class="mono">docker restart</span> / <span class="mono">kubectl rollout restart</span>) to unseal again.
        </div>
      {:else if session.status?.autoUnsealError}
        <div class="gate__error">{session.status.autoUnsealError}</div>
        <p class="gate__hint">Retrying every 5 seconds.</p>
      {:else}
        <p class="gate__hint"><Loader2 size={12} class="spin" /> Contacting the key service…</p>
      {/if}
      <button type="button" class="base-btn base-btn--ghost gate__submit" onclick={() => refreshStatus()}>Refresh</button>

    {:else if stage === 'unseal'}
      <form onsubmit={submitShare}>
        <div class="gate__title"><LockOpen size={18} /> Unseal vault</div>
        {#if session.status?.joining}
          <div class="gate__join">
            <b>Joining a Raft cluster</b> via <span class="mono">{session.status.joining}</span>.
            Unseal <b>{session.status.node}</b> with that cluster's unseal keys — decrypting its challenge proves this
            node belongs, and it is then added as a voter.
          </div>
        {/if}
        <p class="gate__hint">
          The vault is sealed: its data is in {session.status?.storage === 'raft' ? `this node's Raft store` : 'Redis'},
          but the key to read it exists only in memory, rebuilt from
          <b>{session.status?.t}</b> of <b>{session.status?.n}</b> unseal keys. Every restart starts sealed.
        </p>
        <div class="gate__progress">
          {#each Array(session.status?.t ?? 0) as _, i}
            <span class="gate__pip" class:is-on={i < (session.status?.progress ?? 0)}></span>
          {/each}
          <span class="gate__progress-label">{session.status?.progress}/{session.status?.t} keys provided</span>
        </div>
        <label class="gate__label" for="share">Unseal key</label>
        <!-- svelte-ignore a11y_autofocus -->
        <input id="share" class="base-input mono" type="password" autocomplete="off" bind:value={share} autofocus />
        {#if live.length > 1}
          <label class="gate__check">
            <input type="checkbox" bind:checked={unsealAll} />
            Send this key to all {live.length} instances ({live.filter((i) => i.sealed !== false).length} sealed)
          </label>
        {/if}
        <button class="base-btn base-btn--primary gate__submit" disabled={busy || !share.trim()}>
          {#if busy}<Loader2 size={14} class="spin" />{/if} Submit key
        </button>
        {#if fanout.length}
          <div class="gate__fanout">
            {#each fanout as f (f.id)}
              <div class="gate__fanout-row">
                <span class="mono">{f.id}</span>
                <span class:gate__ok={f.sealed === false}>
                  {f.error ? f.error : f.sealed === false ? 'unsealed' : `${f.progress}/${session.status?.t}`}
                </span>
              </div>
            {/each}
          </div>
        {/if}
        {#if session.status?.progress}
          <button type="button" class="base-btn base-btn--ghost gate__submit" onclick={resetUnseal}>Reset progress</button>
        {/if}
      </form>

    {:else}
      <div class="gate__title"><ShieldCheck size={18} /> {t('Sign in')}</div>
      <Login />
    {/if}

    {#if error}<div class="gate__error">{error}</div>{/if}
  </div>
</div>

<style>
  .gate { min-height: 100vh; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 16px;
    background: var(--bg-body); overflow-y: auto; padding: 24px 0; }
  .gate__card { width: 380px; max-width: 92vw; background: var(--bg-elevated); border: 1px solid var(--border-2);
    border-radius: 14px; padding: 26px; box-shadow: 0 20px 60px rgba(0,0,0,0.45); }
  .gate__card--wide { width: 560px; }
  .gate__card--login { width: 420px; }
  .gate__brand { display: flex; align-items: center; gap: 12px; margin-bottom: 22px; }
  .gate__logo { width: 38px; height: 38px; border-radius: 10px; display: grid; place-items: center; font-weight: 800;
    font-size: 20px; color: #fff; background: linear-gradient(135deg, var(--brand), #6c9eff); }
  .gate__name { font-weight: 700; font-size: 16px; }
  .gate__name span { color: var(--brand); }
  .gate__title { display: flex; align-items: center; gap: 8px; font-size: 16px; font-weight: 600; margin-bottom: 10px; }
  .gate__hint { font-size: 12px; color: var(--text-muted); margin: 0 0 14px; line-height: 1.55; }
  .gate__hint b { color: var(--text-secondary); }
  .gate__label { display: block; font-size: 11px; color: var(--text-muted); margin: 12px 0 5px; letter-spacing: .03em; text-transform: uppercase; }
  .gate__row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .base-input { width: 100%; }
  .gate__submit { width: 100%; margin-top: 14px; }
  .gate__check { display: flex; align-items: center; gap: 8px; margin-top: 14px; font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
  .gate__keys { display: flex; flex-direction: column; gap: 6px; }
  .gate__key { display: flex; align-items: center; gap: 10px; padding: 7px 8px 7px 10px; border-radius: var(--r);
    background: var(--bg-body); border: 1px solid var(--border-2); }
  .gate__key--token { border-color: var(--brand-ring); }
  .gate__key-n { font-size: 10px; font-weight: 700; color: var(--text-muted); min-width: 28px; text-transform: uppercase; }
  .gate__key-v { flex: 1; min-width: 0; word-break: break-all; color: var(--text-primary); font-size: 11.5px; }
  .gate__progress { display: flex; align-items: center; gap: 6px; margin: 4px 0 6px; }
  .gate__pip { width: 22px; height: 6px; border-radius: 99px; background: var(--border-2); transition: background var(--dur) var(--ease); }
  .gate__pip.is-on { background: var(--brand); }
  .gate__progress-label { margin-left: 6px; font-size: 11.5px; color: var(--text-muted); }
  .gate__error { margin-top: 14px; padding: 9px 11px; border-radius: 8px; font-size: 12px;
    background: var(--danger-bg); color: var(--danger); border: 1px solid rgba(217,91,91,0.3); }
  .gate__join { margin: 0 0 12px; padding: 9px 11px; border-radius: 8px; font-size: 12px; line-height: 1.5;
    background: var(--info-bg); color: var(--info); }
  .gate__join b { color: inherit; }
  .gate__fanout { margin-top: 12px; display: flex; flex-direction: column; gap: 4px; font-size: 11.5px; }
  .gate__fanout-row { display: flex; justify-content: space-between; gap: 8px; color: var(--text-muted); }
  .gate__ok { color: var(--success); }
</style>
