<script lang="ts">
  // One container: details, a shell, its files, logs, start / stop / restart.
  import { ArrowLeft, Loader2, SquareTerminal, ScrollText, RotateCw, Square, Play, FolderOpen, Info, Eye, EyeOff, HardDrive, Network, Activity } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys, queryClient } from '../lib/query'
  import { containerApi, monitor, errMsg } from '../lib/api'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { confirm } from '../lib/ui.svelte'
  import { openTerminal } from '../lib/terminals.svelte'
  import { pct, rate, size } from '../lib/monitor'
  import ContainerFiles from '../components/ContainerFiles.svelte'
  import ContainerLogs from '../components/ContainerLogs.svelte'

  let { asset, name, tab: initialTab = '', path = '/' }: { asset: string; name: string; tab?: string; path?: string } = $props()
  const canManage = isAdmin()
  // svelte-ignore state_referenced_locally
  let tab = $state<'overview' | 'files'>(initialTab === 'files' ? 'files' : 'overview')
  $effect(() => { if (initialTab === 'files') tab = 'files' })

  const all = createQuery(() => ({ queryKey: keys.containers, queryFn: () => monitor.listContainers({}), refetchInterval: 10000 }))
  const systems = createQuery(() => ({ queryKey: keys.systems, queryFn: () => monitor.listSystems({}) }))
  const info = createQuery(() => ({ queryKey: keys.container(asset, name), queryFn: () => containerApi.inspectContainer({ asset, name }), enabled: canManage, retry: false }))

  const row = $derived(all.data?.containers.find((r) => r.asset === asset && r.container?.name === name))
  const c = $derived(row?.container)
  const system = $derived(systems.data?.systems.find((s) => s.asset === asset))
  const d = $derived(info.data)
  let showEnv = $state(false)
  let logs = $state(false)
  let acting = $state('')
  let note = $state<{ ok: boolean; text: string } | null>(null)
  const running = $derived((c?.state ?? d?.state) === 'running')

  function shell() {
    if (!system) return
    openTerminal(asset, system.name, system.account, name)
    navigate('/terminal')
  }

  async function act(action: 'start' | 'stop' | 'restart') {
    if (action !== 'start') {
      const ok = await confirm({ title: `${action === 'stop' ? 'Stop' : 'Restart'} ${name}?`, message: action === 'stop' ? 'It stays stopped until someone starts it.' : 'It will be unavailable for a moment.', confirmText: action === 'stop' ? 'Stop' : 'Restart', variant: action === 'stop' ? 'danger' : 'warning' })
      if (!ok) return
    }
    acting = action
    note = null
    try {
      const r = await monitor.containerAction({ asset, name, action })
      note = { ok: r.ok, text: r.ok ? `${action === 'stop' ? 'Stopped' : action === 'start' ? 'Started' : 'Restarted'}. The numbers update with the next reading.` : r.output || 'the runtime refused' }
      queryClient.invalidateQueries({ queryKey: keys.container(asset, name) })
    } catch (e) { note = { ok: false, text: errMsg(e) } } finally { acting = '' }
  }
  const when = (t?: string) => (t && !t.startsWith('0001') ? new Date(t).toLocaleString() : '—')
  const secretish = (n: string) => /SECRET|TOKEN|PASSWORD|PASS|KEY|CREDENTIAL|PRIVATE/i.test(n)
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <div><button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate('/containers')}><ArrowLeft size={13} /> Containers</button></div>

      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Container · {row?.system ?? system?.name ?? asset}{c?.runtime ? ` · ${c.runtime}` : ''}</div>
          <h1 class="page-title cd-title">
            <span class="cd-dot cd-dot--{c?.state ?? d?.state ?? ''}"></span><span class="mono">{name}</span>
            <span class="badge badge--{running ? 'success' : 'default'}">{c?.status || d?.state || '…'}</span>
            {#if c?.health}<span class="badge badge--{c.health === 'healthy' ? 'success' : c.health === 'unhealthy' ? 'danger' : 'warning'}">{c.health}</span>{/if}
          </h1>
          <div class="cd-meta">
            <span class="mono">{c?.image || d?.image || ''}</span>
            {#if c && running}
              <span><Activity size={12} /> CPU {pct(c.cpu)}</span>
              <span>Memory {size(c.mem)}{Number(c.memLimit) ? ` / ${size(c.memLimit)}` : ''}</span>
              {#if c.rx !== undefined}<span>↓ {rate(c.rx)} ↑ {rate(c.tx)}</span>{/if}
            {/if}
          </div>
        </div>
        {#if canManage}
          <div class="page-hero__actions cd-actions">
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(`/usage/${asset}/${name}`)} title="CPU, memory, disk and network over time"><Activity size={13} /> Usage</button>
            <button class="base-btn base-btn--primary base-btn--sm" disabled={!running || !system} onclick={shell} title={running ? 'A shell inside the container (bash, or sh) — recorded like any terminal session' : 'Start the container first'}><SquareTerminal size={13} /> Shell</button>
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (logs = true)}><ScrollText size={13} /> Logs</button>
            {#if running}
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!acting} onclick={() => act('restart')}>{#if acting === 'restart'}<Loader2 size={13} class="spin" />{:else}<RotateCw size={13} />{/if} Restart</button>
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!acting} onclick={() => act('stop')}>{#if acting === 'stop'}<Loader2 size={13} class="spin" />{:else}<Square size={12} />{/if} Stop</button>
            {:else}
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!acting} onclick={() => act('start')}>{#if acting === 'start'}<Loader2 size={13} class="spin" />{:else}<Play size={13} />{/if} Start</button>
            {/if}
          </div>
        {/if}
      </section>

      {#if note}<div class="notice notice--{note.ok ? 'success' : 'error'}">{note.text}</div>{/if}

      {#if !canManage}
        <div class="notice notice--info">An administrator can open a shell, browse files, read logs and start or stop this container.</div>
      {:else}
        <section class="page-card">
          <div class="page-tabs cd-tabs">
            <button class="page-tab" class:is-active={tab === 'overview'} onclick={() => { tab = 'overview'; navigate(`/containers/${asset}/${name}`) }}><Info size={13} /> Overview</button>
            <button class="page-tab" class:is-active={tab === 'files'} onclick={() => { tab = 'files'; navigate(`/containers/${asset}/${name}/files`) }}><FolderOpen size={13} /> Files</button>
          </div>

          {#if tab === 'files'}
            {#if running}
              <ContainerFiles {asset} {name} {path} />
            {:else}
              <div class="empty-state">Start the container to browse its files.</div>
            {/if}
          {:else if info.isPending}
            <div class="empty-state"><Loader2 size={18} class="spin" /></div>
          {:else if info.error}
            <div class="notice notice--error cd-pad">{errMsg(info.error)}</div>
          {:else if d}
            <div class="cd-grid">
              <div class="cd-block">
                <div class="cd-h">About</div>
                <dl>
                  <dt>Image</dt><dd class="mono">{d.image}</dd>
                  <dt>Command</dt><dd class="mono">{d.command || '—'}</dd>
                  <dt>Id</dt><dd class="mono">{d.id}</dd>
                  <dt>Created</dt><dd>{when(d.created)}</dd>
                  <dt>Started</dt><dd>{when(d.started)}</dd>
                  <dt>Restart policy</dt><dd>{d.restartPolicy || 'no'}</dd>
                  {#if d.workingDir}<dt>Working dir</dt><dd class="mono">{d.workingDir}</dd>{/if}
                  {#if d.user}<dt>User</dt><dd class="mono">{d.user}</dd>{/if}
                  {#if d.composeProject}<dt>Compose</dt><dd class="mono">{d.composeProject} / {d.composeService}</dd>{/if}
                  {#if !running}<dt>Exit code</dt><dd class="mono">{d.exitCode}</dd>{/if}
                </dl>
              </div>
              <div class="cd-block">
                <div class="cd-h"><Network size={13} /> Ports & networks</div>
                {#each d.ports as p (p)}<div class="mono cd-line">{p}</div>{:else}<div class="muted cd-line">No ports.</div>{/each}
                {#each d.networks as n (n.name)}<div class="cd-line"><span class="badge badge--default">{n.name}</span> <span class="mono">{n.ip || '—'}</span></div>{/each}
              </div>
              <div class="cd-block cd-wide">
                <div class="cd-h"><HardDrive size={13} /> Mounts</div>
                {#each d.mounts as m (m.destination)}
                  <div class="cd-mount">
                    <span class="badge badge--{m.kind === 'volume' ? 'info' : 'default'}">{m.kind}</span>
                    <span class="mono cd-src" title={m.source}>{m.source}</span>
                    <span class="muted">→</span>
                    <button class="linkish mono" disabled={!running} onclick={() => { tab = 'files'; navigate(`/containers/${asset}/${name}/files${m.destination}`) }}>{m.destination}</button>
                    {#if m.readOnly}<span class="muted">read-only</span>{/if}
                  </div>
                {:else}<div class="muted cd-line">No volumes or bind mounts: what the container writes is lost when it is removed.</div>{/each}
              </div>
              <div class="cd-block cd-wide">
                <div class="cd-h">Environment <span class="muted">{d.env.length}</span>
                  <button class="base-btn base-btn--ghost base-btn--xs cd-reveal" onclick={() => (showEnv = !showEnv)}>{#if showEnv}<EyeOff size={12} /> Hide values{:else}<Eye size={12} /> Show values{/if}</button>
                </div>
                {#each d.env as e (e.name)}
                  <div class="cd-env"><span class="mono strong">{e.name}</span><span class="mono cd-val">{showEnv ? e.value : secretish(e.name) ? '••••••' : e.value.length > 60 ? e.value.slice(0, 60) + '…' : e.value}</span></div>
                {:else}<div class="muted cd-line">None.</div>{/each}
              </div>
            </div>
          {/if}
        </section>
      {/if}
    </div>
  </div>
</div>

{#if logs}<ContainerLogs {asset} system={row?.system ?? asset} {name} onclose={() => (logs = false)} />{/if}

<style>
  .cd-title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .cd-title .badge { font-size: 11px; }
  .cd-title .mono { font-size: inherit; }
  .cd-dot { width: 11px; height: 11px; border-radius: 50%; background: var(--text-muted); }
  .cd-dot--running { background: var(--success); box-shadow: 0 0 0 4px var(--success-bg); }
  .cd-meta { display: flex; gap: 14px; flex-wrap: wrap; font-size: 12px; color: var(--text-secondary); margin-top: 6px; }
  .cd-meta :global(svg) { vertical-align: -2px; }
  .cd-actions { flex: 0 0 auto; flex-wrap: nowrap; }
  .cd-tabs { padding: 10px 14px 0; }
  .cd-tabs :global(svg) { vertical-align: -2px; }
  .cd-pad { margin: 14px 16px; }
  .cd-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; padding: 14px 16px 16px; }
  .cd-block { padding: 12px 14px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); min-width: 0; }
  .cd-wide { grid-column: 1 / -1; }
  .cd-h { display: flex; align-items: center; gap: 6px; font-size: 12.5px; font-weight: 700; color: var(--text-primary); margin-bottom: 8px; }
  .cd-reveal { margin-left: auto; }
  dl { display: grid; grid-template-columns: 110px 1fr; gap: 5px 12px; margin: 0; font-size: 12.5px; }
  dt { color: var(--text-muted); }
  dd { margin: 0; color: var(--text-primary); word-break: break-word; }
  .cd-line { font-size: 12.5px; padding: 2px 0; }
  .cd-mount { display: flex; align-items: center; gap: 8px; font-size: 12.5px; padding: 3px 0; min-width: 0; }
  .cd-src { max-width: 46%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cd-env { display: flex; gap: 12px; font-size: 12px; padding: 3px 0; border-top: 1px solid var(--border); }
  .cd-env .strong { flex: 0 0 260px; overflow: hidden; text-overflow: ellipsis; }
  .cd-val { color: var(--text-secondary); word-break: break-all; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font-size: 12.5px; }
  .linkish:disabled { color: var(--text-secondary); cursor: default; }
  @media (max-width: 900px) { .cd-grid { grid-template-columns: 1fr; } }
</style>
