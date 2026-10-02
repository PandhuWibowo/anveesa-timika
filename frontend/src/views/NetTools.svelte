<script lang="ts">
  // Network tools: ping, port check, DNS, trace route, HTTP, TLS certificate and
  // listening ports — run from one or several of your servers, side by side.
  // #/network/<server> starts with that server chosen.
  import { Loader2, Play, RotateCw, X, Search, Server, Check, CircleAlert, TriangleAlert } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { bastion, errMsg } from '../lib/api'
  import { router, navigate } from '../lib/router.svelte'
  import { TOOLS, net, run, removeRun, clearRuns, parsePorts, type Run, type Row } from '../lib/net.svelte'

  const list = createQuery(() => ({ queryKey: keys.assets, queryFn: () => bastion.listAssets({}) }))
  const servers = $derived((list.data?.assets ?? []).filter((a) => a.allowedAccounts.length).sort((a, b) => a.name.localeCompare(b.name)))

  let tool = $state('ping')
  let target = $state('')
  let ports = $state('22, 80, 443')
  let tlsPort = $state('443')
  let record = $state('A')
  let resolver = $state('')
  let filter = $state('')
  let problem = $state('')
  // svelte-ignore state_referenced_locally
  let chosen = $state<string[]>(router.path.split('/')[2] ? [decodeURIComponent(router.path.split('/')[2])] : [])
  const T = $derived(TOOLS.find((t) => t.id === tool)!)

  // One server: it is the obvious source.
  $effect(() => { if (servers.length === 1 && !chosen.length) chosen = [servers[0].id] })
  const shownServers = $derived(servers.filter((s) => !filter || `${s.name} ${s.host} ${s.tags.join(' ')}`.toLowerCase().includes(filter.trim().toLowerCase())))
  const toggle = (id: string) => { chosen = chosen.includes(id) ? chosen.filter((x) => x !== id) : [...chosen, id] }

  function start() {
    problem = ''
    const sources = servers.filter((s) => chosen.includes(s.id)).map((s) => ({ asset: s.id, name: s.name }))
    if (!sources.length) return (problem = 'Choose the server to run from.')
    if (tool !== 'listen' && !target.trim()) return (problem = tool === 'http' ? 'Type the URL to open.' : 'Type the host to check.')
    const p = tool === 'port' ? parsePorts(ports) : tool === 'tls' ? parsePorts(tlsPort) : []
    if (p.some((x) => !Number.isInteger(x) || x < 1 || x > 65535)) return (problem = 'Ports are numbers from 1 to 65535.')
    if (tool === 'port' && !p.length) return (problem = 'Type at least one port.')
    run({ tool, target: tool === 'listen' ? '' : target.trim(), ports: p, record: tool === 'dns' ? record : '', resolver: tool === 'dns' ? resolver.trim() : '' }, sources)
  }
  function again(r: Run) {
    run({ tool: r.tool, target: r.target, ports: r.ports, record: r.record, resolver: r.resolver }, r.rows.map((x) => ({ asset: x.asset, name: x.name })))
  }
  const title = (r: Run) => {
    const t = TOOLS.find((x) => x.id === r.tool)!
    return r.tool === 'listen' ? t.label : `${t.label} · ${r.tool === 'dns' ? `${r.record} ` : ''}${r.target}${r.tool === 'port' ? ` : ${r.ports.join(', ')}` : r.tool === 'tls' && r.ports[0] && r.ports[0] !== 443 ? `:${r.ports[0]}` : ''}${r.resolver ? ` @${r.resolver}` : ''}`
  }
  const good = (x: Row) => !!x.result?.ok
  const summary = (r: Run) => {
    if (r.rows.some((x) => x.busy)) return ''
    const n = r.rows.filter(good).length
    return r.rows.length === 1 ? '' : n === r.rows.length ? `fine from all ${n} servers` : n === 0 ? `fails from all ${r.rows.length} servers` : `fine from ${n} of ${r.rows.length} servers`
  }
  const badge = (l: string) => (l === 'ok' ? 'success' : l === 'warn' ? 'warning' : l === 'bad' ? 'danger' : 'default')
  const STATE: Record<string, [string, string]> = { open: ['open', 'success'], closed: ['refused', 'danger'], timeout: ['no answer', 'warning'], unresolved: ['unknown host', 'danger'], failed: ['failed', 'danger'] }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Access</div>
          <h1 class="page-title">Network tools</h1>
          <p class="page-subtitle">Check the network the way your servers see it. Each check runs on the server you pick, over the SSH access timika already has — choose several to compare them side by side.</p>
        </div>
      </section>

      <section class="page-card nt-form">
        <div class="nt-tools">
          {#each TOOLS as t (t.id)}
            <button class="nt-tool" class:is-on={tool === t.id} onclick={() => { tool = t.id; problem = '' }} title={t.hint}>
              <t.ico size={15} /><span class="nt-tool__l">{t.label}</span><span class="nt-tool__s mono">{t.like}</span>
            </button>
          {/each}
        </div>
        <div class="nt-hint">{T.hint}.</div>

        <form class="nt-fields" onsubmit={(e) => { e.preventDefault(); start() }}>
          {#if tool !== 'listen'}
            <label class="nt-field nt-grow"><span>{tool === 'http' ? 'URL' : tool === 'dns' ? 'Name' : 'Host'}</span>
              <!-- svelte-ignore a11y_autofocus -->
              <input class="base-input mono" bind:value={target} placeholder={T.target} spellcheck="false" autocapitalize="off" autocomplete="off" autofocus /></label>
          {/if}
          {#if tool === 'port'}
            <label class="nt-field"><span>Ports</span><input class="base-input mono" bind:value={ports} placeholder="22, 80, 443" /></label>
          {:else if tool === 'tls'}
            <label class="nt-field nt-narrow"><span>Port</span><input class="base-input mono" bind:value={tlsPort} placeholder="443" /></label>
          {:else if tool === 'dns'}
            <label class="nt-field nt-narrow"><span>Record</span>
              <select class="base-input" bind:value={record}>{#each ['A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS', 'SOA', 'PTR', 'SRV', 'CAA'] as r (r)}<option>{r}</option>{/each}</select></label>
            <label class="nt-field"><span>Resolver <em>optional</em></span><input class="base-input mono" bind:value={resolver} placeholder="the server's own" spellcheck="false" /></label>
          {/if}
          <button class="base-btn base-btn--primary nt-run" type="submit" disabled={list.isPending}><Play size={14} /> Run</button>
        </form>

        <div class="nt-from">
          <div class="nt-from__head">
            <span>Run from {#if chosen.length > 1}<b>{chosen.length} servers</b>{/if}</span>
            {#if servers.length > 8}<div class="nt-search"><Search size={13} /><input bind:value={filter} placeholder="Filter servers…" /></div>{/if}
            {#if servers.length > 1}
              <button class="linkish" type="button" onclick={() => (chosen = chosen.length === servers.length ? [] : servers.map((s) => s.id))}>{chosen.length === servers.length ? 'None' : 'All'}</button>
            {/if}
          </div>
          {#if list.isPending}
            <div class="muted nt-pad"><Loader2 size={14} class="spin" /></div>
          {:else if list.error && !list.data}
            <div class="notice notice--error">{errMsg(list.error)}</div>
          {:else if !servers.length}
            <div class="muted nt-pad">You have no server to run from yet. <button class="linkish" onclick={() => navigate('/servers')}>Servers</button></div>
          {:else}
            <div class="nt-chips">
              {#each shownServers as s (s.id)}
                <button class="nt-chip" class:is-on={chosen.includes(s.id)} type="button" onclick={() => toggle(s.id)} title="{s.allowedAccounts[0]}@{s.host}">
                  {#if chosen.includes(s.id)}<Check size={12} />{:else}<Server size={12} />{/if} {s.name}
                </button>
              {/each}
            </div>
          {/if}
        </div>
        {#if problem}<div class="notice notice--warning nt-problem">{problem}</div>{/if}
      </section>

      {#if net.runs.length}
        <div class="nt-bar"><span class="nt-bar__t">Results</span><button class="base-btn base-btn--ghost base-btn--xs" onclick={clearRuns}>Clear</button></div>
      {/if}

      {#each net.runs as r (r.id)}
        <section class="page-card">
          <div class="page-card__head nt-head">
            <div class="page-card__title mono">{title(r)}</div>
            <span class="muted nt-when">{r.at.toLocaleTimeString()}{summary(r) ? ` · ${summary(r)}` : ''}</span>
            <span class="nt-spacer"></span>
            <button class="icon-btn" title="Run again" onclick={() => again(r)}><RotateCw size={13} /></button>
            <button class="icon-btn" title="Remove" onclick={() => removeRun(r.id)}><X size={14} /></button>
          </div>
          <div class="nt-rows">
            {#each r.rows as x (x.asset)}
              {@const res = x.result}
              <div class="nt-row">
                <div class="nt-row__top">
                  {#if x.busy}<Loader2 size={15} class="spin" />
                  {:else if res?.level === 'ok'}<span class="nt-ico nt-ico--ok"><Check size={13} /></span>
                  {:else if res?.level === 'warn'}<span class="nt-ico nt-ico--warn"><TriangleAlert size={12} /></span>
                  {:else}<span class="nt-ico nt-ico--bad"><CircleAlert size={13} /></span>{/if}
                  <span class="strong">{x.name}</span>
                  <span class="nt-verdict" class:nt-verdict--bad={!x.busy && !good(x)}>{x.busy ? 'running…' : x.error || res?.verdict}</span>
                  {#if res}<span class="muted nt-took mono">{res.tool}{res.tool ? ' · ' : ''}{res.tookMs < 1000 ? `${res.tookMs} ms` : `${(res.tookMs / 1000).toFixed(1)} s`}</span>{/if}
                </div>
                {#if res}
                  {#if res.facts.length}
                    <div class="nt-facts">
                      {#each res.facts as f (f.label)}<div class="nt-fact"><span>{f.label}</span><b class="nt-lv--{f.level}">{f.value}</b></div>{/each}
                    </div>
                  {/if}
                  {#if res.ports.length}
                    <div class="nt-ports">
                      {#each res.ports as p (p.port)}
                        <span class="badge badge--{STATE[p.state]?.[1] ?? 'default'}" title={p.detail}><span class="mono">{p.port}</span> {STATE[p.state]?.[0] ?? p.state}{p.ms !== undefined && p.state === 'open' ? ` · ${p.ms} ms` : ''}</span>
                      {/each}
                    </div>
                  {/if}
                  {#if res.records.length}
                    <table class="data-table nt-table"><thead><tr><th>Type</th><th>Value</th><th>Name</th><th>TTL</th></tr></thead><tbody>
                      {#each res.records as d, i (i)}<tr><td><span class="badge badge--{badge('')}">{d.type}</span></td><td class="mono strong">{d.value}</td><td class="mono">{d.name}</td><td class="mono">{d.ttl !== undefined ? `${d.ttl} s` : '—'}</td></tr>{/each}
                    </tbody></table>
                  {/if}
                  {#if res.hops.length}
                    <table class="data-table nt-table"><thead><tr><th>Hop</th><th>Address</th><th>Time</th></tr></thead><tbody>
                      {#each res.hops as h (h.n)}<tr><td class="mono">{h.n}</td><td class="mono" class:muted={!h.addr}>{h.addr || 'no answer'}</td><td class="mono">{h.ms !== undefined ? `${h.ms < 10 ? h.ms.toFixed(1) : h.ms.toFixed(0)} ms` : '—'}</td></tr>{/each}
                    </tbody></table>
                  {/if}
                  {#if res.listeners.length}
                    <table class="data-table nt-table"><thead><tr><th>Port</th><th>Protocol</th><th>Address</th><th>Process</th></tr></thead><tbody>
                      {#each res.listeners as l, i (i)}
                        {@const local = l.addr.startsWith('127.') || l.addr === '::1'}
                        <tr><td class="mono strong">{l.port}</td><td>{l.proto}</td><td class="mono">{l.addr} {#if local}<span class="badge badge--default">this server only</span>{/if}</td><td class="mono">{l.process || '—'}{l.pid !== undefined ? ` (${l.pid})` : ''}</td></tr>
                      {/each}
                    </tbody></table>
                    {#if res.listeners.some((l) => !l.process)}<div class="muted nt-note">A process shows only when the account may see it (its own, or as root).</div>{/if}
                  {/if}
                  {#if res.output}
                    <details class="nt-raw"><summary>Output</summary><pre class="mono">{res.output}</pre></details>
                  {/if}
                {/if}
              </div>
            {/each}
          </div>
        </section>
      {/each}
    </div>
  </div>
</div>

<style>
  .nt-form { padding: 14px 16px 16px; display: flex; flex-direction: column; gap: 12px; }
  .nt-tools { display: grid; grid-template-columns: repeat(auto-fit, minmax(128px, 1fr)); gap: 8px; }
  .nt-tool { display: grid; grid-template-columns: auto 1fr; column-gap: 8px; align-items: center; padding: 9px 11px; border-radius: var(--r); border: 1px solid var(--border); background: var(--bg-body); color: var(--text-secondary); text-align: left; cursor: pointer; font: inherit; }
  .nt-tool:hover { background: var(--bg-hover); }
  .nt-tool.is-on { border-color: var(--brand); background: var(--brand-soft); color: var(--brand); }
  .nt-tool__l { font-size: 12.5px; font-weight: 700; white-space: nowrap; }
  .nt-tool__s { grid-column: 2; font-size: 10.5px; color: var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .nt-hint { font-size: 12px; color: var(--text-muted); margin-top: -4px; }
  .nt-fields { display: flex; gap: 10px; align-items: flex-end; flex-wrap: wrap; }
  .nt-field { display: flex; flex-direction: column; gap: 4px; font-size: 11.5px; color: var(--text-muted); flex: 0 1 200px; min-width: 120px; }
  .nt-field em { font-style: normal; opacity: 0.7; }
  .nt-grow { flex: 1 1 280px; }
  .nt-narrow { flex: 0 0 100px; min-width: 90px; }
  .nt-run { height: 34px; }
  .nt-from { display: flex; flex-direction: column; gap: 8px; }
  .nt-from__head { display: flex; align-items: center; gap: 12px; font-size: 11.5px; color: var(--text-muted); }
  .nt-from__head b { color: var(--brand); font-weight: 600; }
  .nt-search { display: flex; align-items: center; gap: 6px; padding: 0 8px; height: 26px; border: 1px solid var(--border); border-radius: var(--r); }
  .nt-search input { background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12px; width: 140px; }
  .nt-chips { display: flex; gap: 6px; flex-wrap: wrap; max-height: 132px; overflow: auto; }
  .nt-chip { display: inline-flex; align-items: center; gap: 6px; padding: 5px 10px; border-radius: 999px; border: 1px solid var(--border); background: var(--bg-body); color: var(--text-secondary); font-size: 12px; cursor: pointer; }
  .nt-chip:hover { background: var(--bg-hover); }
  .nt-chip.is-on { border-color: var(--brand); background: var(--brand-soft); color: var(--brand); }
  .nt-pad { font-size: 12.5px; }
  .nt-problem { margin: 0; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; }
  .nt-bar { display: flex; align-items: center; justify-content: space-between; }
  .nt-bar__t { font-size: 13px; font-weight: 700; color: var(--text-primary); }
  .nt-head { display: flex; align-items: center; gap: 10px; }
  .nt-when { font-size: 11.5px; }
  .nt-spacer { flex: 1; }
  .nt-rows { display: flex; flex-direction: column; }
  .nt-row { padding: 11px 16px 12px; border-top: 1px solid var(--border); display: flex; flex-direction: column; gap: 9px; min-width: 0; }
  .nt-row__top { display: flex; align-items: center; gap: 9px; font-size: 12.5px; min-width: 0; }
  .nt-ico { display: grid; place-items: center; width: 19px; height: 19px; border-radius: 50%; flex: 0 0 auto; }
  .nt-ico--ok { background: var(--success-bg); color: var(--success); }
  .nt-ico--warn { background: var(--warning-bg); color: var(--warning); }
  .nt-ico--bad { background: var(--danger-bg); color: var(--danger); }
  .nt-verdict { color: var(--text-primary); min-width: 0; }
  .nt-verdict--bad { color: var(--danger); }
  .nt-took { margin-left: auto; font-size: 11px; white-space: nowrap; }
  .nt-facts { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 8px; }
  .nt-fact { display: flex; flex-direction: column; gap: 1px; padding: 7px 10px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); font-size: 11px; color: var(--text-muted); min-width: 0; }
  .nt-fact b { font-size: 12.5px; font-weight: 600; color: var(--text-primary); word-break: break-word; }
  .nt-fact b.nt-lv--ok { color: var(--success); }
  .nt-fact b.nt-lv--warn { color: var(--warning); }
  .nt-fact b.nt-lv--bad { color: var(--danger); }
  .nt-ports { display: flex; gap: 6px; flex-wrap: wrap; }
  .nt-table { border: 1px solid var(--border); border-radius: var(--r); }
  .nt-note { font-size: 11.5px; }
  .nt-raw summary { font-size: 11.5px; color: var(--text-muted); cursor: pointer; }
  .nt-raw pre { margin: 6px 0 0; padding: 10px 12px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); font-size: 11.5px; line-height: 1.5; max-height: 320px; overflow: auto; white-space: pre-wrap; word-break: break-word; }
</style>
