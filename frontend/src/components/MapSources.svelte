<script lang="ts">
  // Sources for the map: Kubernetes clusters and cloud accounts. Each is read
  // every few minutes, through a CLI on a monitored server or with credentials
  // kept in the vault. Credentials are write-only here.
  import { fade, scale } from 'svelte/transition'
  import { X, Loader2, Plus, RefreshCw, Trash2, Pencil, CircleAlert, Check, ArrowLeft } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { sourcesApi, monitor, errMsg } from '../lib/api'
  import { confirm } from '../lib/ui.svelte'
  import { ago, kindText } from '../lib/map'
  import type { SourceInfo } from '../gen/timika/v1/sources_pb'

  // `prefill`: a cloud the servers were found in — go straight to its key.
  let { onclose, prefill = null }: { onclose: () => void; prefill?: { kind: string; regions: string[] } | null } = $props()

  type Field = { id: string; label: string; area?: boolean; optional?: boolean; hint?: string }
  const KINDS: Record<string, { label: string; cli: string; reads: string; secret: Field[]; how: string }> = {
    kubernetes: { label: 'Kubernetes', cli: 'kubectl', reads: 'nodes, workloads, services, ingresses', how: 'A service-account token that may get and list nodes, pods, services and ingresses in all namespaces (the built-in “view” ClusterRole plus nodes).',
      secret: [{ id: 'token', label: 'Service-account token', area: true }, { id: 'ca', label: 'Cluster CA certificate (PEM)', area: true, optional: true, hint: 'needed when the API server’s certificate is not from a public authority' }] },
    aws: { label: 'AWS', cli: 'aws', reads: 'EC2 instances, load balancers and targets, NAT gateways', how: 'An access key of a user or role with ReadOnlyAccess, or just ec2:Describe* and elasticloadbalancing:Describe*.',
      secret: [{ id: 'access_key_id', label: 'Access key id' }, { id: 'secret_access_key', label: 'Secret access key' }, { id: 'session_token', label: 'Session token', optional: true }] },
    tencent: { label: 'Tencent Cloud', cli: 'tccli', reads: 'CVM instances, CLB load balancers and targets, NAT gateways', how: 'An API key of a sub-account with the read-only policies QcloudCVMReadOnlyAccess, QcloudCLBReadOnlyAccess and QcloudVPCReadOnlyAccess.',
      secret: [{ id: 'secret_id', label: 'SecretId' }, { id: 'secret_key', label: 'SecretKey' }] },
    gcp: { label: 'Google Cloud', cli: 'gcloud', reads: 'Compute instances, load balancers and backends, Cloud NAT', how: 'The JSON key of a service account with the role Compute Viewer (roles/compute.viewer).',
      secret: [{ id: 'service_account', label: 'Service account key (JSON)', area: true }] },
    azure: { label: 'Azure', cli: 'az', reads: 'virtual machines, load balancers, application gateways, NAT gateways', how: 'An app registration (service principal) with the Reader role on the subscription.',
      secret: [{ id: 'tenant_id', label: 'Tenant id' }, { id: 'client_id', label: 'Client (application) id' }, { id: 'client_secret', label: 'Client secret' }] },
  }

  const list = createQuery(() => ({ queryKey: keys.sources, queryFn: () => sourcesApi.listSources({}), refetchInterval: 15000 }))
  const systems = createQuery(() => ({ queryKey: keys.systems, queryFn: () => monitor.listSystems({}) }))
  const sources = $derived(list.data?.sources ?? [])
  const servers = $derived(systems.data?.systems ?? [])

  let form = $state<{ id: string; name: string; kind: string; access: string; asset: string; regions: string; scope: string; endpoint: string; hasSecret: boolean } | null>(null)
  let secret = $state<Record<string, string>>({})
  let busy = $state('')
  let error = $state('')
  const K = $derived(form ? KINDS[form.kind] : null)

  function edit(s?: SourceInfo) {
    error = ''
    secret = {}
    form = s
      ? { id: s.id, name: s.name, kind: s.kind, access: s.access, asset: s.asset, regions: s.regions.join(', '), scope: s.scope, endpoint: s.endpoint, hasSecret: s.hasSecret }
      : { id: '', name: '', kind: 'kubernetes', access: 'server', asset: servers[0]?.asset ?? '', regions: '', scope: '', endpoint: '', hasSecret: false }
  }

  async function save() {
    if (!form) return
    busy = 'save'
    error = ''
    try {
      const saved = await sourcesApi.saveSource({
        source: { id: form.id, name: form.name, kind: form.kind, access: form.access, asset: form.access === 'server' ? form.asset : '', regions: form.regions.split(/[\s,]+/).filter(Boolean), scope: form.scope.trim(), endpoint: form.access === 'vault' ? form.endpoint.trim() : '' },
        secret: form.access === 'vault' ? Object.fromEntries(Object.entries(secret).filter(([, v]) => v.trim())) : {},
      })
      form = null
      await list.refetch()
      // Read it at once, so a wrong key or a missing CLI shows now.
      refresh(saved.id)
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }
  async function refresh(id: string) {
    busy = id
    try { await sourcesApi.refreshSource({ id }) } catch (e) { error = errMsg(e) } finally { busy = ''; list.refetch() }
  }
  async function remove(s: SourceInfo) {
    if (!(await confirm({ title: `Remove ${s.name}?`, message: 'It disappears from the map, and its stored credentials are deleted. Nothing changes in the cluster or the cloud account.', confirmText: 'Remove', variant: 'danger' }))) return
    try { await sourcesApi.deleteSource({ id: s.id }); list.refetch() } catch (e) { error = errMsg(e) }
  }
  // svelte-ignore state_referenced_locally
  if (prefill && KINDS[prefill.kind]) {
    form = { id: '', name: `${KINDS[prefill.kind].label}${prefill.regions[0] ? ` · ${prefill.regions[0]}` : ''}`, kind: prefill.kind, access: 'vault', asset: '', regions: prefill.regions.join(', '), scope: '', endpoint: '', hasSecret: false }
  }
  const found = (s: SourceInfo) => s.counts.map((c) => `${c.count} ${(kindText[c.kind] ?? c.kind).toLowerCase()}${c.count === 1 ? '' : 's'}`).join(' · ')
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="md-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="md" role="dialog" aria-modal="true" transition:scale={{ duration: 140, start: 0.97 }}>
    <header class="md__head">
      <div>
        <div class="page-kicker">Map</div>
        <div class="md__title">{form ? (form.id ? `Change ${form.name}` : prefill ? `Connect ${KINDS[form.kind]?.label ?? form.kind}` : 'Add a source') : 'Sources'}</div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>

    <div class="md__body">
      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if !form}
        <p class="lead">Cloud accounts add what a server can’t see from the inside: load balancers, NAT gateways and VMs you don’t monitor — used while the map detects with <b>Cloud API</b>. Kubernetes clusters add ingresses, services and workloads, in either mode. Everything is read every five minutes, read-only.</p>
        {#if list.isPending}
          <div class="empty-state"><Loader2 size={18} class="spin" /></div>
        {:else}
          {#each sources as s (s.id)}
            <div class="src">
              <div class="src__main">
                <div class="src__t"><span class="strong">{s.name}</span> <span class="badge badge--default">{KINDS[s.kind]?.label ?? s.kind}</span> <span class="muted">{s.access === 'server' ? `${KINDS[s.kind]?.cli} on ${servers.find((x) => x.asset === s.asset)?.name ?? s.asset}` : 'credentials in the vault'}</span></div>
                {#if busy === s.id}
                  <div class="src__s muted"><Loader2 size={12} class="spin" /> reading…</div>
                {:else if s.error}
                  <div class="src__s src__s--bad"><CircleAlert size={12} /> {s.error}</div>
                {:else if s.readAt}
                  <div class="src__s src__s--ok"><Check size={12} /> {found(s) || 'nothing found'} <span class="muted">· read {ago(s.readAt)}</span></div>
                {:else}
                  <div class="src__s muted">not read yet</div>
                {/if}
                {#each s.notes as n (n)}<div class="src__s muted">{n}</div>{/each}
              </div>
              <button class="icon-btn" title="Read now" disabled={!!busy} onclick={() => refresh(s.id)}><RefreshCw size={14} /></button>
              <button class="icon-btn" title="Change" onclick={() => edit(s)}><Pencil size={14} /></button>
              <button class="icon-btn" title="Remove" onclick={() => remove(s)}><Trash2 size={14} /></button>
            </div>
          {:else}
            <div class="empty-state">No source yet.</div>
          {/each}
          <button class="base-btn base-btn--primary add" onclick={() => edit()}><Plus size={14} /> Add a source</button>
        {/if}
      {:else if K}
        <button class="base-btn base-btn--ghost base-btn--sm back" onclick={() => (form = null)}><ArrowLeft size={13} /> Sources</button>
        {#if prefill && !form.id}<div class="notice notice--info">Detected from your servers: {K.label}{form.regions ? ` in ${form.regions}` : ''}. Only the API key is needed.</div>{/if}
        {#if !form.id && !prefill}
          <div class="kinds">
            {#each Object.entries(KINDS) as [id, k] (id)}<button class="kind" class:is-on={form.kind === id} onclick={() => { form!.kind = id; secret = {} }}><b>{k.label}</b><span>{k.reads}</span></button>{/each}
          </div>
        {/if}
        <div class="grid">
          <label><span>Name</span><input class="base-input" bind:value={form.name} placeholder="prod" /></label>
          <div class="field"><span>How timika reaches it</span>
            <div class="seg">
              <button class:is-on={form.access === 'server'} onclick={() => (form!.access = 'server')}><span class="mono">{K.cli}</span> on a server</button>
              <button class:is-on={form.access === 'vault'} onclick={() => (form!.access = 'vault')}>Credentials in the vault</button>
            </div>
          </div>
          {#if form.kind === 'aws' || form.kind === 'tencent'}
            <label class="wide"><span>Regions</span><input class="base-input mono" bind:value={form.regions} placeholder={form.kind === 'aws' ? 'ap-southeast-1, us-east-1' : 'ap-singapore, ap-jakarta'} spellcheck="false" /></label>
          {:else if form.kind === 'gcp'}
            <label class="wide"><span>Project id</span><input class="base-input mono" bind:value={form.scope} placeholder="my-project-123" spellcheck="false" /></label>
          {:else if form.kind === 'azure'}
            <label class="wide"><span>Subscription id</span><input class="base-input mono" bind:value={form.scope} placeholder="00000000-0000-0000-0000-000000000000" spellcheck="false" /></label>
          {/if}

          {#if form.access === 'server'}
            <label class="wide"><span>Server</span>
              <select class="base-input" bind:value={form.asset}>{#each servers as s (s.asset)}<option value={s.asset}>{s.name}</option>{/each}</select>
            </label>
            {#if form.kind === 'kubernetes'}<label class="wide"><span>kubectl context <em>optional</em></span><input class="base-input mono" bind:value={form.scope} placeholder="the current one" spellcheck="false" /></label>{/if}
            <div class="hint wide">timika runs <span class="mono">{K.cli}</span> on that server over SSH, as its monitoring account. It must be installed there and already signed in — nothing is stored here. {#if !servers.length}<b>No server is monitored yet.</b>{/if}</div>
          {:else}
            {#if form.kind === 'kubernetes'}
              <label class="wide"><span>API server</span><input class="base-input mono" bind:value={form.endpoint} placeholder="https://10.0.0.1:6443" spellcheck="false" /></label>
            {/if}
            {#each K.secret as f (f.id)}
              <label class="wide"><span>{f.label} {#if f.optional}<em>optional</em>{/if}</span>
                {#if f.area}
                  <textarea class="base-input mono" rows="3" bind:value={secret[f.id]} placeholder={form.hasSecret ? 'stored — leave empty to keep it' : (f.hint ?? '')} spellcheck="false"></textarea>
                {:else}
                  <input class="base-input mono" type={f.id.includes('secret') || f.id.includes('token') ? 'password' : 'text'} bind:value={secret[f.id]} placeholder={form.hasSecret ? 'stored — leave empty to keep it' : ''} autocomplete="off" spellcheck="false" />
                {/if}
              </label>
            {/each}
            {#if form.kind !== 'kubernetes'}
              <label class="wide"><span>API endpoint <em>optional</em></span><input class="base-input mono" bind:value={form.endpoint} placeholder="the provider’s public one" spellcheck="false" /></label>
            {/if}
            <div class="hint wide"><b>Read-only is enough.</b> {K.how} The credentials are kept encrypted in the vault and never shown again; timika calls the API itself, so the timika host must be able to reach it.</div>
          {/if}
        </div>
        <div class="foot">
          <button class="base-btn base-btn--ghost" onclick={() => (form = null)}>Cancel</button>
          <button class="base-btn base-btn--primary" disabled={!!busy || !form.name.trim()} onclick={save}>{#if busy === 'save'}<Loader2 size={14} class="spin" />{/if} Save and read</button>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .md-mask { position: fixed; inset: 0; z-index: 900; display: flex; align-items: center; justify-content: center; background: rgba(0, 0, 0, 0.5); backdrop-filter: blur(3px); }
  .md { width: min(760px, calc(100vw - 48px)); max-height: calc(100vh - 64px); display: flex; flex-direction: column; background: var(--bg-surface); border: 1px solid var(--border); border-radius: 14px; box-shadow: var(--shadow-lg); overflow: hidden; }
  .md__head { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; border-bottom: 1px solid var(--border); }
  .md__title { font-size: 15px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .md__body { padding: 16px 18px 18px; overflow: auto; display: flex; flex-direction: column; gap: 12px; }
  .lead { margin: 0; font-size: 12.5px; color: var(--text-secondary); line-height: 1.5; }
  .src { display: flex; align-items: center; gap: 6px; padding: 10px 12px; border: 1px solid var(--border); border-radius: var(--r); background: var(--bg-body); }
  .src__main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .src__t { font-size: 12.5px; }
  .src__t .muted { font-size: 11.5px; }
  .src__s { font-size: 11.5px; word-break: break-word; }
  .src__s :global(svg) { vertical-align: -2px; }
  .src__s--ok { color: var(--success); }
  .src__s--bad { color: var(--danger); }
  .add { align-self: flex-start; }
  .back { align-self: flex-start; }
  .kinds { display: grid; grid-template-columns: repeat(auto-fit, minmax(130px, 1fr)); gap: 8px; }
  .kind { display: flex; flex-direction: column; gap: 3px; padding: 9px 11px; border-radius: var(--r); border: 1px solid var(--border); background: var(--bg-body); color: var(--text-secondary); text-align: left; cursor: pointer; font: inherit; }
  .kind b { font-size: 12.5px; color: var(--text-primary); }
  .kind span { font-size: 10.5px; color: var(--text-muted); line-height: 1.35; }
  .kind.is-on { border-color: var(--brand); background: var(--brand-soft); }
  .kind.is-on b { color: var(--brand); }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .grid label, .field { display: flex; flex-direction: column; gap: 4px; font-size: 11.5px; color: var(--text-muted); min-width: 0; }
  .grid em { font-style: normal; opacity: 0.7; }
  .wide { grid-column: 1 / -1; }
  textarea { resize: vertical; font-size: 11.5px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); align-self: flex-start; }
  .seg button { padding: 6px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; white-space: nowrap; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .hint { font-size: 11.5px; color: var(--text-muted); line-height: 1.5; }
  .foot { display: flex; justify-content: flex-end; gap: 8px; }
  @media (max-width: 640px) { .grid { grid-template-columns: 1fr; } }
</style>
