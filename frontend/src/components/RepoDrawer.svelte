<script lang="ts">
  // Connect a Git repository (or edit one): paste the URL, choose how timika
  // reads it, pick the runner server — done. Everything else is optional.
  import { onMount } from 'svelte'
  import { fade, fly } from 'svelte/transition'
  import { X, Loader2, Copy, Check, ExternalLink, KeyRound, Globe, Lock, Plus, Trash2, Activity, CircleCheck, CircleAlert, Eye, EyeOff } from '@lucide/svelte'
  import { automation, bastion, errMsg } from '../lib/api'
  import { repoWeb, deployKeyPage, sshUrl, isSshUrl } from '../lib/automation'
  import type { Repo, RunnerStatus } from '../gen/timika/v1/automation_pb'
  import type { Asset } from '../gen/timika/v1/bastion_pb'

  let { repo = null, onclose, onsaved }: { repo?: Repo | null; onclose: () => void; onsaved: (r: Repo) => void } = $props()
  // svelte-ignore state_referenced_locally
  const editing = repo

  type Var = { name: string; value: string; secret: boolean; stored: boolean; show: boolean }

  let url = $state(editing?.url ?? '')
  let auth = $state<'none' | 'token' | 'deploy_key'>((editing?.auth as 'none') ?? 'none')
  let token = $state('')
  let key = $state<{ id: string; publicKey: string } | null>(null)
  let runner = $state(editing ? `${editing.runnerAsset}\u0000${editing.runnerAccount}` : '')
  let branch = $state(editing?.branch ?? '')
  let name = $state(editing?.name ?? '')
  let autoPlan = $state(editing?.autoPlan ?? true)
  let tfBin = $state(editing?.tfBin ?? 'auto')
  let vars = $state<Var[]>(editing?.variables.map((v) => ({ name: v.name, value: v.value, secret: v.secret, stored: v.secret, show: false })) ?? [])
  let newSecret = $state(false)

  let assets = $state<Asset[]>([])
  let busy = $state(false)
  let keyBusy = $state(false)
  let error = $state('')
  let copied = $state('')
  let check = $state<{ busy: boolean; result?: RunnerStatus; error?: string }>({ busy: false })

  const runners = $derived(assets.flatMap((a) => a.accounts.map((acc) => ({ id: `${a.id}\u0000${acc.username}`, label: `${acc.username}@${a.name}`, host: a.host }))))
  const web = $derived(repoWeb(url.trim()))
  const keyPage = $derived(deployKeyPage(url.trim()))
  const wantsSsh = $derived(auth === 'deploy_key' && url.trim() !== '' && !isSshUrl(url.trim()))
  const suggestedName = $derived(web.path.split('/').slice(-2).join('/') || 'repository')
  const valid = $derived(
    url.trim() !== '' && runner !== '' && !wantsSsh &&
    (auth !== 'token' || token.trim() !== '' || (editing?.auth === 'token')) &&
    (auth !== 'deploy_key' || !!key || (editing?.auth === 'deploy_key' && !!editing.deployPublicKey)),
  )

  onMount(async () => {
    try {
      assets = (await bastion.listAssets({})).assets
      if (!runner && runners.length) runner = runners[0].id
    } catch (e) { error = errMsg(e) }
  })

  // SSH URLs only work with a key: offer one as soon as one is pasted.
  $effect(() => {
    if (!editing && isSshUrl(url.trim()) && auth === 'none') chooseAuth('deploy_key')
  })

  async function chooseAuth(a: typeof auth) {
    auth = a
    if (a === 'deploy_key' && !key && !(editing?.auth === 'deploy_key')) await generateKey()
  }

  async function generateKey() {
    keyBusy = true
    try { key = await automation.newDeployKey({}) } catch (e) { error = errMsg(e) } finally { keyBusy = false }
  }

  function copy(text: string, tag: string) {
    navigator.clipboard.writeText(text)
    copied = tag
    setTimeout(() => { if (copied === tag) copied = '' }, 1400)
  }

  async function checkRunner() {
    const [asset, account] = runner.split('\u0000')
    check = { busy: true }
    try {
      const r = await automation.checkRunner({ asset, account })
      check = { busy: false, result: r, error: r.ok ? undefined : r.error }
    } catch (e) { check = { busy: false, error: errMsg(e) } }
  }

  async function save() {
    busy = true
    error = ''
    const [runnerAsset, runnerAccount] = runner.split('\u0000')
    const input = {
      url: url.trim(), branch: branch.trim(), name: name.trim(), auth, token: token.trim(),
      deployKeyId: key?.id ?? '', runnerAsset, runnerAccount, autoPlan, tfBin,
      variables: vars.filter((v) => v.name.trim()).map((v) => ({ name: v.name.trim(), value: v.value, secret: v.secret })),
    }
    try {
      const r = editing
        ? await automation.updateRepo({ id: editing.id, repo: input, newWebhookSecret: newSecret })
        : await automation.createRepo(input)
      onsaved(r)
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }

  const KNOWN = ['AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_REGION', 'ARM_CLIENT_ID', 'ARM_CLIENT_SECRET', 'GOOGLE_CREDENTIALS', 'PULUMI_ACCESS_TOKEN', 'TF_VAR_']
  const looksSecret = (n: string) => /SECRET|TOKEN|PASSWORD|PASS|KEY|CREDENTIAL/i.test(n) && !/_ID$|REGION/i.test(n)
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="drawer-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="drawer" role="dialog" aria-modal="true" transition:fly={{ x: 40, duration: 180 }}>
    <header class="drawer__head">
      <div>
        <div class="page-kicker">{editing ? 'Repository' : 'Infrastructure'}</div>
        <div class="drawer__title">{editing ? editing.name : 'Connect a repository'}</div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>

    <div class="drawer__body">
      <label class="f">
        <span>Git repository</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input class="base-input mono" bind:value={url} placeholder="https://github.com/acme/infra  or  git@github.com:acme/infra.git" autofocus={!editing} spellcheck="false" />
      </label>

      <div class="f">
        <span>Access</span>
        <div class="seg">
          <button class:is-on={auth === 'none'} onclick={() => chooseAuth('none')}><Globe size={12} /> Public</button>
          <button class:is-on={auth === 'token'} onclick={() => chooseAuth('token')}><Lock size={12} /> Token</button>
          <button class:is-on={auth === 'deploy_key'} onclick={() => chooseAuth('deploy_key')}><KeyRound size={12} /> Deploy key</button>
        </div>
      </div>

      {#if auth === 'token'}
        <label class="f">
          <span>Access token</span>
          <input class="base-input mono" type="password" bind:value={token} placeholder={editing?.auth === 'token' ? '•••••• unchanged' : 'github_pat_… / glpat-…'} autocomplete="off" />
          <small class="muted">Read access is enough (GitHub fine-grained token: <i>Contents → Read-only</i>). Stored in the vault.</small>
        </label>
      {:else if auth === 'deploy_key'}
        <div class="key">
          {#if keyBusy}
            <Loader2 size={16} class="spin" />
          {:else if key || editing?.deployPublicKey}
            <div class="key__head"><KeyRound size={13} /> {key ? 'Add this key to the repository (read-only)' : 'Deploy key in use'}</div>
            <div class="key__row">
              <code class="mono key__text">{key?.publicKey ?? editing?.deployPublicKey}</code>
              <button class="icon-btn" title="Copy" onclick={() => copy(key?.publicKey ?? editing?.deployPublicKey ?? '', 'key')}>{#if copied === 'key'}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
            </div>
            <div class="key__actions">
              {#if key && keyPage}<a class="base-btn base-btn--ghost base-btn--sm" href={keyPage} target="_blank" rel="noreferrer"><ExternalLink size={12} /> Open deploy keys on {web.host}</a>{/if}
              {#if !key}<button class="base-btn base-btn--ghost base-btn--sm" onclick={generateKey}>Replace with a new key</button>{/if}
            </div>
            {#if key}<small class="muted">The private half never leaves the vault. Add it, then press Connect.</small>{/if}
          {/if}
          {#if wantsSsh}
            <div class="notice notice--warning key__ssh">
              Deploy keys need the SSH address.
              {#if sshUrl(url.trim())}<button class="base-btn base-btn--ghost base-btn--xs" onclick={() => (url = sshUrl(url.trim()) ?? url)}>Use {sshUrl(url.trim())}</button>{/if}
            </div>
          {/if}
        </div>
      {/if}

      <div class="f">
        <span>Runner server</span>
        <div class="runner">
          <select class="base-select mono" bind:value={runner} onchange={() => (check = { busy: false })}>
            {#if !runners.length}<option value="">Add a server first (Servers → Add server)</option>{/if}
            {#each runners as r (r.id)}<option value={r.id}>{r.label}</option>{/each}
          </select>
          <button class="base-btn base-btn--ghost base-btn--sm" disabled={!runner || check.busy} onclick={checkRunner} title="Which tools does it have?">
            {#if check.busy}<Loader2 size={13} class="spin" />{:else}<Activity size={13} />{/if} Check
          </button>
        </div>
        {#if check.result?.ok}
          <div class="tools">
            {#each ['terraform', 'tofu', 'ansible-playbook', 'pulumi'] as t (t)}
              {@const tool = check.result.tools.find((x) => x.name === t)}
              <span class="badge badge--{tool ? 'success' : 'default'}" title={tool?.version ?? 'not installed'}>
                {#if tool}<CircleCheck size={11} />{:else}<CircleAlert size={11} />{/if} {t}{tool?.version ? ` ${tool.version}` : ''}
              </span>
            {/each}
          </div>
        {:else if check.error}
          <div class="notice notice--error">{check.error}</div>
        {/if}
        <small class="muted">Runs execute here over SSH — not on the vault. It needs the tools your repository uses.</small>
      </div>

      <label class="check"><input type="checkbox" bind:checked={autoPlan} /> On every push: plan / check / preview what changed</label>

      <details class="more" open={!!editing && vars.length > 0}>
        <summary>Variables & secrets {#if vars.length}<span class="muted">({vars.length})</span>{/if}</summary>
        <p class="muted hint">Environment variables for every run — cloud credentials, <code>TF_VAR_…</code>, <code>PULUMI_ACCESS_TOKEN</code>. Secrets stay in the vault and are masked in the output.</p>
        <div class="vars">
          {#each vars as v, i (i)}
            <div class="var">
              <input class="base-input mono var__name" list="var-names" bind:value={v.name} placeholder="NAME" oninput={() => { if (!v.stored && looksSecret(v.name)) v.secret = true }} />
              <input class="base-input mono var__value" type={v.secret && !v.show ? 'password' : 'text'} bind:value={v.value} placeholder={v.stored ? '•••••• unchanged' : 'value'} autocomplete="off" />
              {#if v.secret}<button class="icon-btn" title={v.show ? 'Hide' : 'Show'} onclick={() => (v.show = !v.show)}>{#if v.show}<EyeOff size={13} />{:else}<Eye size={13} />{/if}</button>{/if}
              <button class="icon-btn" class:var__lock--on={v.secret} title={v.secret ? 'Secret (masked, never shown again)' : 'Plain value'} onclick={() => (v.secret = !v.secret)}><Lock size={13} /></button>
              <button class="icon-btn" title="Remove" onclick={() => vars.splice(i, 1)}><Trash2 size={13} /></button>
            </div>
          {/each}
          <datalist id="var-names">{#each KNOWN as k (k)}<option value={k}></option>{/each}</datalist>
          <button class="base-btn base-btn--ghost base-btn--sm vars__add" onclick={() => vars.push({ name: '', value: '', secret: false, stored: false, show: false })}><Plus size={13} /> Add variable</button>
        </div>
      </details>

      <details class="more">
        <summary>More options</summary>
        <div class="grid2">
          <label class="f"><span>Branch</span><input class="base-input mono" bind:value={branch} placeholder="default branch" /></label>
          <label class="f"><span>Name</span><input class="base-input" bind:value={name} placeholder={suggestedName} /></label>
          <label class="f"><span>Terraform runs with</span>
            <select class="base-select" bind:value={tfBin}>
              <option value="auto">terraform (or tofu if that's all)</option>
              <option value="terraform">terraform</option>
              <option value="tofu">OpenTofu (tofu)</option>
            </select>
          </label>
        </div>
        {#if editing}<label class="check"><input type="checkbox" bind:checked={newSecret} /> New webhook secret (update it in GitHub / GitLab afterwards)</label>{/if}
      </details>

      {#if error}<div class="notice notice--error">{error}</div>{/if}
    </div>

    <footer class="drawer__foot">
      <button class="base-btn base-btn--ghost" onclick={onclose}>Cancel</button>
      <button class="base-btn base-btn--primary" disabled={!valid || busy} onclick={save}>
        {#if busy}<Loader2 size={14} class="spin" /> {editing ? 'Saving…' : 'Cloning…'}{:else}{editing ? 'Save' : 'Connect'}{/if}
      </button>
    </footer>
  </div>
</div>

<style>
  .drawer-mask { position: fixed; inset: 0; z-index: 900; background: rgba(0, 0, 0, 0.45); backdrop-filter: blur(2px); display: flex; justify-content: flex-end; }
  .drawer { width: min(540px, 100vw); height: 100%; display: flex; flex-direction: column; background: var(--bg-surface); border-left: 1px solid var(--border); box-shadow: var(--shadow-lg); }
  .drawer__head { display: flex; align-items: flex-start; justify-content: space-between; padding: 20px 22px 12px; }
  .drawer__title { font-size: 18px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .drawer__body { flex: 1; overflow-y: auto; padding: 8px 22px 22px; display: flex; flex-direction: column; gap: 14px; }
  .drawer__foot { display: flex; justify-content: flex-end; gap: 8px; padding: 14px 22px; border-top: 1px solid var(--border); }
  .f { display: flex; flex-direction: column; gap: 5px; }
  .f > span { font-size: 11px; color: var(--text-muted); letter-spacing: .03em; text-transform: uppercase; }
  .f input, .f select { width: 100%; }
  .f small, .hint { font-size: 11.5px; line-height: 1.45; margin: 0; }
  .seg { display: inline-flex; align-self: flex-start; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); }
  .seg button { display: flex; align-items: center; gap: 5px; padding: 5px 10px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .key { display: flex; flex-direction: column; gap: 8px; padding: 12px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .key__head { display: flex; align-items: center; gap: 6px; font-size: 12.5px; font-weight: 600; color: var(--text-primary); }
  .key__row { display: flex; gap: 6px; align-items: flex-start; }
  .key__text { flex: 1; min-width: 0; word-break: break-all; font-size: 11.5px; padding: 8px 10px; border-radius: var(--r-sm); background: var(--bg-surface); border: 1px solid var(--border); }
  .key__actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .key__ssh { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .runner { display: flex; gap: 8px; }
  .tools { display: flex; gap: 6px; flex-wrap: wrap; }
  .tools :global(svg) { vertical-align: -1px; }
  .check { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-secondary); cursor: pointer; }
  .more summary { cursor: pointer; font-size: 12.5px; color: var(--text-secondary); margin-bottom: 8px; }
  .vars { display: flex; flex-direction: column; gap: 6px; margin-top: 8px; }
  .var { display: flex; gap: 6px; align-items: center; }
  .var__name { flex: 0 0 42%; }
  .var__value { flex: 1; min-width: 0; }
  .var__lock--on { color: var(--brand); }
  .vars__add { align-self: flex-start; }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .grid2 .f:last-child { grid-column: 1 / -1; }
  code { font-size: 11.5px; }
</style>
