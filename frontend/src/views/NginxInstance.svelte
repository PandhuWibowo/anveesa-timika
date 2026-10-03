<script lang="ts">
  // One nginx: its sites, config files, certificates and logs. Every change
  // goes through "test, then reload" on the server and is undone when nginx
  // rejects it.
  import { ArrowLeft, Loader2, RefreshCw, Plus, Lock, CircleAlert, Check, FileText, ScrollText, Container as Box, Server, RotateCw, ArrowRight, Pencil } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { nginxApi, errMsg } from '../lib/api'
  import { navigate } from '../lib/router.svelte'
  import { siteName, ports, base, applyText, daysLevel, daysText } from '../lib/nginx'
  import type { NginxApply, NginxSite } from '../gen/timika/v1/nginx_pb'
  import NginxEditor from '../components/NginxEditor.svelte'
  import ContainerLogs from '../components/ContainerLogs.svelte'

  let { asset, container, back = true }: { asset: string; container: string; back?: boolean } = $props()

  // One SSH round trip; kept for half a minute, refreshed after every change.
  const q = createQuery(() => ({ queryKey: keys.nginx(asset, container), queryFn: () => nginxApi.getInstance({ asset, container }), staleTime: 30_000, retry: false }))
  const d = $derived(q.data)
  const error = $derived(q.error && !q.data ? errMsg(q.error) : '')

  type Note = { ok: boolean; text: string; detail: string }
  let tab = $state<'sites' | 'files' | 'certs' | 'logs'>('sites')
  let note = $state<Note | null>(null)
  let busy = $state('')
  let editor = $state<{ path: string; line: number } | null>(null)
  let log = $state('')
  let containerLog = $state(false)

  const fileOf = (path: string) => d?.files.find((f) => f.path === path)
  const certDays = (s: NginxSite) => {
    const days = s.certificates.map((p) => d?.certificates.find((c) => c.path === p)?.daysLeft).filter((x): x is number => x !== undefined)
    return days.length ? Math.min(...days) : undefined
  }
  const soon = $derived(d?.certificates.filter((c) => c.daysLeft !== undefined && c.daysLeft < 21).length ?? 0)

  async function act(key: string, what: string, fn: () => Promise<NginxApply>) {
    if (busy) return
    busy = key
    try { note = applyText(await fn(), what) } catch (e) { note = { ok: false, text: errMsg(e), detail: '' } } finally { busy = ''; q.refetch() }
  }
  const toggle = (path: string, on: boolean) => act(path, `${base(path)} switched ${on ? 'on' : 'off'}`, () => nginxApi.setEnabled({ asset, container, path, enabled: on }))
  const reload = () => act('reload', 'Configuration', () => nginxApi.reload({ asset, container }))

  async function openLog(path: string) {
    if (!container) return (log = path)
    // In a container the log is often the container's own output.
    try {
      const r = await nginxApi.readLog({ asset, container, path, tail: 1 })
      if (r.containerOutput) containerLog = true
      else log = path
    } catch (e) { note = { ok: false, text: errMsg(e), detail: '' } }
  }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      {#if back}<div><button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate('/nginx')}><ArrowLeft size={13} /> Nginx</button></div>{/if}
      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if !d}
        {#if !error}<div class="empty-state"><Loader2 size={20} class="spin" /></div>{/if}
      {:else}
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">Nginx · {d.system}</div>
            <h1 class="page-title nx-title">
              <span class="nx-dot" class:nx-dot--on={d.running}></span>{container ? container : d.system}
              <span class="badge badge--{d.running ? 'success' : 'default'}">{d.running ? 'running' : 'not running'}</span>
              <span class="badge badge--{d.testOk ? 'success' : 'danger'}">{#if d.testOk}<Check size={11} /> config ok{:else}<CircleAlert size={11} /> config broken{/if}</span>
              {#if soon}<span class="badge badge--warning"><Lock size={11} /> {soon} certificate{soon === 1 ? '' : 's'} expiring</span>{/if}
            </h1>
            <div class="nx-meta">
              <span class="mono">nginx {d.version}</span>
              <span>{#if container}<Box size={12} /> in a {d.runtime || 'docker'} container{:else}<Server size={12} /> on the server{/if}</span>
              <span class="mono">{d.confPath}</span>
            </div>
          </div>
          <div class="page-hero__actions nx-actions">
            <button class="base-btn base-btn--primary base-btn--sm" onclick={() => (editor = { path: '', line: 0 })}><Plus size={13} /> New site</button>
            <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!busy} onclick={reload} title="nginx -t, then reload">{#if busy === 'reload'}<Loader2 size={13} class="spin" />{:else}<RotateCw size={13} />{/if} Test &amp; reload</button>
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(container ? `/containers/${asset}/${container}` : `/servers/${asset}`)} title="Shell, files and usage are on the {container ? 'container' : 'server'} page">{container ? 'Container' : 'Server'} <ArrowRight size={12} /></button>
            <button class="icon-btn" title="Read again from the server" onclick={() => q.refetch()}>{#if q.isFetching}<Loader2 size={14} class="spin" />{:else}<RefreshCw size={14} />{/if}</button>
          </div>
        </section>

        {#if note}
          <div class="notice notice--{note.ok ? 'success' : 'error'} nx-note">
            <div>{note.text}{#if note.detail}<pre class="mono">{note.detail}</pre>{/if}</div>
            <button class="linkish" onclick={() => (note = null)}>Dismiss</button>
          </div>
        {/if}
        {#if !d.testOk}
          <div class="notice notice--error"><b>The configuration on the server does not pass <span class="mono">nginx -t</span>.</b> nginx keeps running with the last good one until it is fixed — open the file named below.<pre class="mono">{d.testOutput}</pre></div>
        {:else if !d.running}
          <div class="notice notice--warning">nginx is not running. Changes are saved and tested, and take effect when it starts.</div>
        {/if}

        <section class="page-card">
          <div class="page-tabs nx-tabs">
            <button class="page-tab" class:is-active={tab === 'sites'} onclick={() => (tab = 'sites')}>Sites <span class="nx-n">{d.sites.length}</span></button>
            <button class="page-tab" class:is-active={tab === 'files'} onclick={() => (tab = 'files')}>Files <span class="nx-n">{d.files.length}</span></button>
            <button class="page-tab" class:is-active={tab === 'certs'} onclick={() => (tab = 'certs')}>Certificates <span class="nx-n">{d.certificates.length}</span></button>
            <button class="page-tab" class:is-active={tab === 'logs'} onclick={() => (tab = 'logs')}>Logs <span class="nx-n">{d.logs.length}</span></button>
          </div>

          {#if tab === 'sites'}
            <div class="data-table-wrap"><table class="data-table">
              <thead><tr><th>Site</th><th>Listens on</th><th>Serves</th><th>Certificate</th><th>File</th><th></th></tr></thead>
              <tbody>
                {#each d.sites as s, i (s.file + ':' + s.line + ':' + i)}
                  {@const f = fileOf(s.file)}
                  {@const days = certDays(s)}
                  <tr class="nx-row" class:nx--off={!s.enabled} onclick={() => (editor = { path: s.file, line: s.line })}>
                    <td><span class="nx-name"><i class="nx-sdot" class:is-on={s.enabled}></i><span class="strong mono">{siteName(s)}</span>{#if s.names.length > 1}<span class="muted" title={s.names.join('\n')}>+{s.names.length - 1}</span>{/if}{#if !s.enabled}<span class="badge badge--default">off</span>{/if}</span></td>
                    <td class="mono">{#if s.tls}<Lock size={11} class="nx-lock" />{/if} {ports(s).join(', ') || '—'}</td>
                    <td><span class="badge badge--{s.kind === 'proxy' ? 'info' : s.kind === 'redirect' ? 'warning' : 'default'}">{s.kind}</span> <span class="mono nx-target" title={s.targets.join('\n')}>{s.targets[0] ?? ''}{s.targets.length > 1 ? ` +${s.targets.length - 1}` : ''}</span></td>
                    <td>{#if s.certificates.length}<span class="badge badge--{daysLevel(days)}">{daysText(days)}</span>{:else}<span class="muted">—</span>{/if}</td>
                    <td class="mono muted" title={s.file}>{base(s.file)}:{s.line}</td>
                    <td class="nx-act">
                      {#if f?.switchable}
                        <button class="sw" class:is-on={s.enabled} disabled={!!busy} title={s.enabled ? 'Switch this file off' : 'Switch this file on'} aria-label={s.enabled ? 'Switch off' : 'Switch on'} onclick={(e) => { e.stopPropagation(); toggle(s.file, !s.enabled) }}>{#if busy === s.file}<Loader2 size={11} class="spin" />{:else}<i></i>{/if}</button>
                      {/if}
                    </td>
                  </tr>
                {:else}
                  <tr><td colspan="6"><div class="empty-state">No <span class="mono">server</span> block yet. <button class="linkish" onclick={() => (editor = { path: '', line: 0 })}>Add the first site</button></div></td></tr>
                {/each}
              </tbody>
            </table></div>
          {:else if tab === 'files'}
            <div class="data-table-wrap"><table class="data-table">
              <thead><tr><th>File</th><th>State</th><th></th></tr></thead>
              <tbody>
                {#each d.files as f (f.path)}
                  <tr class="nx-row" onclick={() => (editor = { path: f.path, line: 0 })}>
                    <td><span class="nx-name"><FileText size={13} /><span class="mono" class:strong={f.path === d.confPath}>{f.path}</span></span></td>
                    <td>{#if f.loaded}<span class="badge badge--success">in use</span>{:else if f.switchable}<span class="badge badge--default">off</span>{:else}<span class="muted">not included</span>{/if}</td>
                    <td class="nx-act">
                      {#if f.switchable}<button class="sw" class:is-on={f.enabled} disabled={!!busy} aria-label={f.enabled ? 'Switch off' : 'Switch on'} title={f.enabled ? 'Switch off' : 'Switch on'} onclick={(e) => { e.stopPropagation(); toggle(f.path, !f.enabled) }}>{#if busy === f.path}<Loader2 size={11} class="spin" />{:else}<i></i>{/if}</button>{/if}
                      <Pencil size={13} class="nx-pen" />
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table></div>
          {:else if tab === 'certs'}
            <div class="data-table-wrap"><table class="data-table">
              <thead><tr><th>Certificate file</th><th>Issued to</th><th>Issued by</th><th>Expires</th><th>Used by</th></tr></thead>
              <tbody>
                {#each [...d.certificates].sort((a, b) => (a.daysLeft ?? 1e9) - (b.daysLeft ?? 1e9)) as c (c.path)}
                  <tr>
                    <td class="mono">{c.path}</td>
                    <td class="mono strong">{c.subject || '—'}</td>
                    <td>{c.issuer || '—'}</td>
                    <td>{#if c.notAfter}<span class="badge badge--{daysLevel(c.daysLeft)}">{daysText(c.daysLeft)}</span> <span class="muted">{new Date(c.notAfter).toLocaleDateString()}</span>{:else}<span class="muted">{c.error || 'unknown'}</span>{/if}</td>
                    <td class="mono">{c.usedBy.join(', ') || '—'}</td>
                  </tr>
                {:else}
                  <tr><td colspan="5"><div class="empty-state">No site uses a certificate (<span class="mono">ssl_certificate</span>) yet.</div></td></tr>
                {/each}
              </tbody>
            </table></div>
          {:else}
            <div class="nx-logs">
              {#each d.logs as l (l)}
                <button class="nx-log" onclick={() => openLog(l)}><ScrollText size={14} /><span class="mono">{l}</span><span class="muted">{d.sites.filter((s) => s.logs.includes(l)).map(siteName).slice(0, 3).join(', ')}</span></button>
              {:else}
                <div class="empty-state">The configuration names no log file{container ? ' — nginx writes to the container’s output' : ''}.</div>
              {/each}
              {#if container}<button class="nx-log" onclick={() => (containerLog = true)}><Box size={14} /><span>The container's output</span><span class="muted">stdout / stderr</span></button>{/if}
            </div>
          {/if}
        </section>
      {/if}
    </div>
  </div>
</div>

{#if editor && d}
  <NginxEditor {asset} {container} system={d.system} path={editor.path} line={editor.line} sitesDir={d.sitesDir} mainConf={d.confPath}
    onclose={() => (editor = null)} ondone={(n) => { note = n; editor = null; q.refetch() }} />
{/if}
{#if log && d}
  <ContainerLogs {asset} name={log} system={d.system} kicker="Nginx log" fetcher={(tail) => nginxApi.readLog({ asset, container, path: log, tail })} onclose={() => (log = '')} />
{/if}
{#if containerLog && d}<ContainerLogs {asset} name={container} system={d.system} onclose={() => (containerLog = false)} />{/if}

<style>
  .nx-title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .nx-title .badge { font-size: 11px; }
  .nx-title :global(svg) { vertical-align: -1px; }
  .nx-dot { width: 11px; height: 11px; border-radius: 50%; background: var(--text-muted); }
  .nx-dot--on { background: var(--success); box-shadow: 0 0 0 4px var(--success-bg); }
  .nx-meta { display: flex; gap: 14px; flex-wrap: wrap; font-size: 12px; color: var(--text-secondary); margin-top: 6px; }
  .nx-meta :global(svg) { vertical-align: -2px; }
  .nx-actions { flex: 0 0 auto; flex-wrap: nowrap; align-items: center; }
  .nx-note { display: flex; justify-content: space-between; gap: 12px; align-items: flex-start; }
  .notice pre { margin: 6px 0 0; font-size: 11.5px; white-space: pre-wrap; word-break: break-word; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; font-weight: 600; white-space: nowrap; }
  .nx-tabs { padding: 10px 14px 0; }
  .nx-n { font-size: 11px; color: var(--text-muted); margin-left: 3px; }
  .nx-row { cursor: pointer; }
  .nx--off { opacity: 0.6; }
  .nx-name { display: inline-flex; align-items: center; gap: 8px; }
  .nx-sdot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-muted); flex: 0 0 auto; }
  .nx-sdot.is-on { background: var(--success); }
  .nx-row :global(.nx-lock) { color: var(--success); vertical-align: -1px; }
  .nx-target { font-size: 12px; margin-left: 4px; }
  .nx-act { text-align: right; white-space: nowrap; }
  .nx-act :global(.nx-pen) { color: var(--text-muted); vertical-align: -2px; margin-left: 10px; }
  .sw { position: relative; width: 30px; height: 17px; border-radius: 999px; border: 0; background: var(--bg-elevated); cursor: pointer; vertical-align: middle; display: inline-grid; place-items: center; color: var(--text-muted); box-shadow: inset 0 0 0 1px var(--border); }
  .sw i { position: absolute; top: 2px; left: 2px; width: 13px; height: 13px; border-radius: 50%; background: var(--text-muted); transition: transform .15s; }
  .sw.is-on { background: var(--success); box-shadow: none; }
  .sw.is-on i { transform: translateX(13px); background: #fff; }
  .sw:disabled { cursor: default; opacity: 0.7; }
  .nx-logs { display: flex; flex-direction: column; padding: 8px; }
  .nx-log { display: flex; align-items: center; gap: 10px; padding: 9px 10px; border: 0; border-radius: var(--r); background: none; text-align: left; font: inherit; font-size: 12.5px; color: var(--text-primary); cursor: pointer; }
  .nx-log:hover { background: var(--bg-hover); }
  .nx-log .muted { margin-left: auto; font-size: 11.5px; }
</style>
