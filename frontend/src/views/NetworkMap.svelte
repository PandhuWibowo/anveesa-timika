<script lang="ts">
  // The map: what talks to what. A diagram with the end-to-end path through
  // whatever you click, and the same connections as a table with every detail.
  import { Loader2, Search, Download, X, ArrowRight, Info, Lock, Plug, Server, KeyRound, Check } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { monitor, errMsg } from '../lib/api'
  import { navigate } from '../lib/router.svelte'
  import { layout, pathThrough, edgeKey, place, edgeKind, kindText, ago, csv, HEADERS } from '../lib/map'
  import { isAdmin } from '../lib/session.svelte'
  import MapSources from '../components/MapSources.svelte'
  import type { MapEdge, MapNode, NetworkMap } from '../gen/timika/v1/monitor_pb'
  import MapDiagram from '../components/MapDiagram.svelte'

  const RANGES = ['1h', '24h', '7d', '30d']
  let range = $state('24h')
  const q = createQuery(() => ({ queryKey: keys.map(range), queryFn: () => monitor.getMap({ range }), refetchInterval: 30000, placeholderData: (prev: NetworkMap | undefined) => prev }))
  const d = $derived(q.data)
  const error = $derived(q.error && !q.data ? errMsg(q.error) : '')

  let tab = $state<'diagram' | 'table'>('diagram')
  let selected = $state('')
  let idle = $state(true)
  let outsideToo = $state(true)
  let text = $state('')
  let sourcesOpen = $state(false)
  let prefill = $state<{ kind: string; regions: string[] } | null>(null)
  let switching = $state(false)
  const PROVIDER: Record<string, string> = { tencent: 'Tencent Cloud', aws: 'AWS', gcp: 'Google Cloud', azure: 'Azure' }
  const method = $derived(d?.method || 'vm')
  const detected = $derived(d?.detected ?? [])
  async function setMethod(m: string) {
    if (m === method || switching) return
    switching = true
    try {
      await monitor.setMapMethod({ method: m })
      await q.refetch()
      // Cloud API for a cloud the servers run in, without a key yet: ask for it now, prefilled.
      const missing = (q.data?.detected ?? []).find((x) => !x.connected)
      if (m === 'api' && missing) connect(missing.provider, missing.regions)
    } finally { switching = false }
  }
  function connect(kind: string, regions: string[]) {
    prefill = { kind, regions }
    sourcesOpen = true
  }
  const admin = isAdmin()

  // Traffic that leaves for the internet (through a NAT gateway) ends on the
  // right, not back at the "Internet" that callers come from on the left.
  const OUT = 'internet#out'
  const leaving = $derived((d?.edges ?? []).some((e) => e.to === 'internet'))
  const allNodes = $derived<MapNode[]>([...(d?.nodes ?? []), ...(leaving ? [{ ...(d!.nodes.find((n) => n.id === 'internet') as MapNode), id: OUT, label: 'Internet (outbound)' }] : [])])
  const allEdges = $derived<MapEdge[]>((d?.edges ?? []).map((e) => (e.to === 'internet' ? { ...e, to: OUT } : e)))
  const by = $derived(new Map(allNodes.map((n) => [n.id, n])))
  const edges = $derived(allEdges.filter((e) => outsideToo || (by.get(e.from)?.group && by.get(e.to)?.group)))
  const touched = $derived(new Set(edges.flatMap((e) => [e.from, e.to])))
  const nodes = $derived(allNodes.filter((n) => HEADERS.includes(n.kind) || (n.group ? idle || touched.has(n.id) : outsideToo && touched.has(n.id))))
  const plan = $derived(layout(nodes, edges, 14))
  // A whole card selected: the paths through everything in it.
  const path = $derived.by(() => {
    const head = by.get(selected)
    if (!head || !HEADERS.includes(head.kind)) return pathThrough(selected, edges)
    const all = { nodes: new Set<string>([selected]), edges: new Set<string>() }
    for (const n of nodes.filter((x) => x.group === head.group)) {
      const p = pathThrough(n.id, edges)
      p.nodes.forEach((x) => all.nodes.add(x))
      p.edges.forEach((x) => all.edges.add(x))
    }
    return all
  })
  const servers = $derived((d?.nodes ?? []).filter((n) => HEADERS.includes(n.kind)).length)

  const node = $derived(by.get(selected))
  const edge = $derived(edges.find((e) => edgeKey(e) === selected))
  const outOf = $derived(node ? edges.filter((e) => e.from === node.id) : [])
  const into = $derived(node ? edges.filter((e) => e.to === node.id) : [])
  // A server's own row, plus everything on it.
  const onServer = $derived(node && HEADERS.includes(node.kind) ? edges.filter((e) => by.get(e.from)?.group === node.group || by.get(e.to)?.group === node.group) : [])

  const shown = $derived.by(() => {
    const t = text.trim().toLowerCase()
    const list = selected ? edges.filter((e) => path.edges.has(edgeKey(e))) : edges
    return list.filter((e) => !t || `${place(by.get(e.from))} ${place(by.get(e.to))} ${e.port} ${e.fromProcess} ${e.toProcess} ${e.sites.join(' ')} ${e.note}`.toLowerCase().includes(t))
  })
  function download() {
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([csv(allNodes, shown)], { type: 'text/csv' }))
    a.download = `timika-connections-${range}.csv`
    a.click()
    URL.revokeObjectURL(a.href)
  }
  const open = (n: MapNode) => navigate(n.kind === 'container' ? `/usage/${n.asset}/${n.label}` : `/monitoring/${n.asset}`)
</script>

{#snippet line(e: MapEdge, other: 'from' | 'to')}
  <button class="ln" onclick={() => (selected = edgeKey(e))}>
    <span class="mono ln__p">{e.port || 'any'}</span>
    <span class="ln__n">{other === 'to' ? '→' : '←'} {place(by.get(e[other]))}</span>
    <span class="muted ln__w">{e.observed ? ago(e.lastSeen) : 'configured'}</span>
  </button>
{/snippet}

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Monitoring</div>
          <h1 class="page-title">Map</h1>
          <p class="page-subtitle">What talks to what — servers, the processes listening on them, containers, and every connection seen between them and to the outside; load balancers, NAT gateways, VMs and Kubernetes ingresses, services and workloads from your cloud accounts and clusters. Click anything to follow its path end to end.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{(d?.nodes ?? []).filter((n) => n.kind === 'server').length}</span><span class="page-metric__label">Servers</span></div>
          <div class="page-metric"><span class="page-metric__value">{(d?.nodes ?? []).filter((n) => n.group && !HEADERS.includes(n.kind)).length}</span><span class="page-metric__label">Services</span></div>
          <div class="page-metric"><span class="page-metric__value">{d?.edges.length ?? 0}</span><span class="page-metric__label">Connections</span></div>
          <div class="page-metric"><span class="page-metric__value">{(d?.nodes ?? []).filter((n) => n.kind === 'external').length}</span><span class="page-metric__label">Outside addresses</span></div>
        </div>
      </section>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      <div class="mp-bar">
        <div class="seg">
          <button class:is-on={tab === 'diagram'} onclick={() => (tab = 'diagram')}>Diagram</button>
          <button class:is-on={tab === 'table'} onclick={() => (tab = 'table')}>Connections {shown.length !== (d?.edges.length ?? 0) ? `${shown.length}` : ''}</button>
        </div>
        <div class="seg" title="Connections seen within">{#each RANGES as r (r)}<button class:is-on={range === r} onclick={() => (range = r)}>{r}</button>{/each}</div>
        <label class="chk"><input type="checkbox" bind:checked={idle} /> Idle services</label>
        <label class="chk"><input type="checkbox" bind:checked={outsideToo} /> Outside</label>
        {#if selected}<button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (selected = '')}><X size={12} /> Clear selection</button>{/if}
        <span class="mp-spacer"></span>
        <span class="mp-legend"><i class="lg lg--a"></i> seen <i class="lg lg--c"></i> configured, not seen</span>
        {#if admin}<button class="base-btn base-btn--ghost base-btn--sm" onclick={() => { prefill = null; sourcesOpen = true }} title="Cloud API keys and Kubernetes clusters"><Plug size={13} /> Sources</button>{/if}
        {#if q.isFetching}<Loader2 size={13} class="spin" />{/if}
      </div>

      {#if d}
        <div class="mp-how">
          <span class="mp-how__l">Detect with</span>
          {#if admin}
            <div class="seg">
              <button class:is-on={method === 'vm'} disabled={switching} onclick={() => setMethod('vm')} title="Only what your servers tell: their cloud, network, public address and connections. No cloud key is used."><Server size={12} /> VMs only</button>
              <button class:is-on={method === 'api'} disabled={switching} onclick={() => setMethod('api')} title="Also the cloud providers' APIs, with a read-only API key: load balancers, NAT gateways and other VMs by name."><KeyRound size={12} /> Cloud API</button>
            </div>
          {:else}
            <span class="badge badge--default">{method === 'api' ? 'Cloud API' : 'VMs only'}</span>
          {/if}
          {#if switching}<Loader2 size={13} class="spin" />{/if}
          <span class="mp-how__t">
            {#if method === 'vm'}
              Built from your servers alone — no cloud key is used. Load balancers and NAT gateways are inferred.
            {:else}
              Your servers, plus what the cloud providers' APIs list.
              {#each detected as dc (dc.provider)}
                {#if dc.connected}
                  <span class="badge badge--success"><Check size={11} /> {PROVIDER[dc.provider] ?? dc.provider}</span>
                {:else if admin}
                  <button class="base-btn base-btn--primary base-btn--xs" onclick={() => connect(dc.provider, dc.regions)}><KeyRound size={11} /> Connect {PROVIDER[dc.provider] ?? dc.provider}{dc.regions.length ? ` (${dc.regions.join(', ')})` : ''}</button>
                {/if}
              {:else}
                {#if admin}<button class="linkish" onclick={() => { prefill = null; sourcesOpen = true }}>Add a cloud account</button>{/if}
              {/each}
            {/if}
          </span>
        </div>
      {/if}

      {#if !d}
        {#if !error}<div class="empty-state"><Loader2 size={20} class="spin" /></div>{/if}
      {:else if !servers}
        <section class="page-card"><div class="empty-state mp-empty">
          <div class="mp-empty__t">Nothing to draw yet</div>
          <div class="muted">The map is built from monitored servers. Turn monitoring on for a server, and its listeners and connections appear here within a minute.{admin ? ' Add a cluster or a cloud account under Sources for load balancers, NAT gateways, services and workloads.' : ''}</div>
          <button class="base-btn base-btn--primary" onclick={() => navigate('/monitoring')}>Monitoring → Systems</button>
        </div></section>
      {:else}
        <div class="mp-main" class:has-side={tab === 'diagram' && (node || edge)}>
          <section class="page-card mp-card">
            {#if tab === 'diagram'}
              <MapDiagram {plan} {edges} bind:selected {path} />
              {#if plan.hidden}<div class="mp-more muted">{plan.hidden} more outside addresses are in the Connections table.</div>{/if}
            {:else}
              <div class="mp-tools">
                <div class="mp-search"><Search size={14} /><input bind:value={text} placeholder="Filter by server, process, port, site…" /></div>
                {#if selected}<span class="badge badge--info">only the selected path</span>{/if}
                <button class="base-btn base-btn--ghost base-btn--sm" onclick={download} disabled={!shown.length}><Download size={13} /> CSV</button>
              </div>
              <div class="data-table-wrap"><table class="data-table">
                <thead><tr><th>From</th><th></th><th>To</th><th>Port</th><th>Evidence</th><th>Processes</th><th>First seen</th><th>Last seen</th><th>Peak</th><th>Notes</th></tr></thead>
                <tbody>
                  {#each shown as e (edgeKey(e))}
                    <tr class="mp-row" class:is-sel={selected === edgeKey(e)} onclick={() => (selected = edgeKey(e))}>
                      <td><span class="strong">{place(by.get(e.from))}</span> <span class="muted mp-k">{kindText[by.get(e.from)?.kind ?? '']}</span></td>
                      <td class="muted"><ArrowRight size={12} /></td>
                      <td><span class="strong">{place(by.get(e.to))}</span> <span class="muted mp-k">{kindText[by.get(e.to)?.kind ?? '']}</span></td>
                      <td class="mono strong">{e.port || 'any'}{#if e.tls} <Lock size={10} />{/if}</td>
                      <td><span class="badge badge--{!e.observed ? 'warning' : e.declared ? 'success' : 'default'}">{edgeKind(e)}</span></td>
                      <td class="mono">{e.fromProcess || e.toProcess ? `${e.fromProcess || '?'} → ${e.toProcess || '?'}` : '—'}</td>
                      <td title={e.firstSeen}>{e.observed ? ago(e.firstSeen) : '—'}</td>
                      <td title={e.lastSeen}>{e.observed ? ago(e.lastSeen) : '—'}</td>
                      <td class="mono">{e.observed ? e.peak : '—'}</td>
                      <td class="mp-note">{[e.sites.join(', '), e.note].filter(Boolean).join(' · ') || '—'}</td>
                    </tr>
                  {:else}
                    <tr><td colspan="10"><div class="empty-state">{d.edges.length ? 'Nothing matches.' : 'No connection seen in this range yet.'}</div></td></tr>
                  {/each}
                </tbody>
              </table></div>
            {/if}
          </section>

          {#if tab === 'diagram' && (node || edge)}
            <aside class="page-card mp-side">
              <div class="mp-side__h">
                <div><div class="page-kicker">{edge ? 'Connection' : kindText[node!.kind]}</div><div class="mp-side__t mono">{edge ? (edge.port ? `port ${edge.port}` : 'any port') : node!.label}</div></div>
                <button class="icon-btn" title="Close" onclick={() => (selected = '')}><X size={15} /></button>
              </div>
              {#if edge}
                <dl>
                  <dt>From</dt><dd><button class="linkish" onclick={() => (selected = edge.from)}>{place(by.get(edge.from))}</button></dd>
                  <dt>To</dt><dd><button class="linkish" onclick={() => (selected = edge.to)}>{place(by.get(edge.to))}</button></dd>
                  <dt>Port</dt><dd class="mono">{edge.port || 'any'}{edge.tls ? ' · TLS at nginx' : ''}</dd>
                  <dt>Evidence</dt><dd>{edgeKind(edge)}</dd>
                  {#if edge.fromProcess || edge.toProcess}<dt>Processes</dt><dd class="mono">{edge.fromProcess || '?'} → {edge.toProcess || '?'}</dd>{/if}
                  {#if edge.observed}
                    <dt>First seen</dt><dd title={edge.firstSeen}>{new Date(edge.firstSeen).toLocaleString()}</dd>
                    <dt>Last seen</dt><dd title={edge.lastSeen}>{ago(edge.lastSeen)}</dd>
                    <dt>Readings</dt><dd>{edge.seen}</dd>
                    <dt>Peak</dt><dd>{edge.peak} {by.get(edge.from)?.kind === 'internet' ? 'clients at once' : 'connections at once'}</dd>
                  {/if}
                  {#if edge.sites.length}<dt>nginx sites</dt><dd class="mono">{edge.sites.join(', ')}</dd>{/if}
                  {#if edge.note}<dt>Note</dt><dd>{edge.note}</dd>{/if}
                </dl>
                {#if !edge.observed}<div class="mp-hint"><Info size={12} /> {edge.declaredBy === 'nginx' ? 'nginx is configured to forward here, but no open connection was caught in this range. Short requests are easy to miss between readings.' : `${edge.declaredBy} says traffic goes this way. timika sees connections only on monitored servers, so this one is known from the configuration.`}</div>{/if}
              {:else if node}
                <dl>
                  {#if node.server && !HEADERS.includes(node.kind)}<dt>{node.asset ? 'Server' : 'Source'}</dt><dd><button class="linkish" onclick={() => (selected = node.group)}>{node.server}</button></dd>{/if}
                  {#if node.scope && node.kind !== 'server'}<dt>{['ingress', 'service', 'workload'].includes(node.kind) ? 'Namespace' : 'Zone'}</dt><dd class="mono">{node.scope}</dd>{/if}
                  {#if node.detail}<dt>{node.kind === 'container' || node.kind === 'workload' ? 'Image' : node.kind === 'server' ? 'Host' : node.kind === 'ingress' ? 'Hosts' : node.kind === 'cloud' ? 'Network' : node.kind === 'cluster' ? 'Provider' : node.kind === 'vpc' || node.kind === 'subnet' ? 'Range' : 'Type'}</dt><dd class="mono">{node.detail}</dd>{/if}
                  {#if node.state}<dt>State</dt><dd>{node.state}</dd>{/if}
                  {#if node.project}<dt>Compose</dt><dd class="mono">{node.project}</dd>{/if}
                  {#if node.public && node.group}<dt>Exposure</dt><dd>reachable from the internet</dd>{/if}
                  {#each node.attrs as a (a)}<dt>{a.split(': ')[0]}</dt><dd class="mono">{a.split(': ').slice(1).join(': ')}</dd>{/each}
                  {#if node.kind === 'external'}<dt>Network</dt><dd>{node.public ? 'public (internet)' : 'private — a machine timika does not monitor'}</dd>{/if}
                  {#if node.addresses.length}<dt>Addresses</dt><dd class="mono">{node.addresses.join('\n')}</dd>{/if}
                </dl>
                {#if node.ports.length}
                  <div class="mp-sub">Listens on</div>
                  <div class="mp-ports">{#each node.ports as p, i (i)}<span class="badge badge--{p.local ? 'default' : 'info'}" title={p.bind}><span class="mono">{p.port}</span>/{p.proto} {p.local ? '· local' : node.asset ? '' : p.bind}</span>{/each}</div>
                {/if}
                {#if HEADERS.includes(node.kind)}
                  <div class="mp-sub">Connections {node.kind === 'server' ? 'on this server' : 'in it'} <span class="muted">{onServer.length}</span></div>
                  {#each onServer.slice(0, 40) as e (edgeKey(e))}<button class="ln" onclick={() => (selected = edgeKey(e))}><span class="mono ln__p">{e.port || 'any'}</span><span class="ln__n">{place(by.get(e.from))} → {place(by.get(e.to))}</span></button>{/each}
                {:else}
                  <div class="mp-sub">Calls <span class="muted">{outOf.length}</span></div>
                  {#each outOf as e (edgeKey(e))}{@render line(e, 'to')}{:else}<div class="muted mp-none">Nothing seen.</div>{/each}
                  <div class="mp-sub">Called by <span class="muted">{into.length}</span></div>
                  {#each into as e (edgeKey(e))}{@render line(e, 'from')}{:else}<div class="muted mp-none">Nothing seen.</div>{/each}
                {/if}
                {#if node.asset}<button class="base-btn base-btn--ghost base-btn--sm mp-open" onclick={() => open(node)}>{node.kind === 'container' ? 'Container usage' : 'Server monitoring'} <ArrowRight size={12} /></button>{/if}
              {/if}
            </aside>
          {/if}
        </div>

        {#each d.notes as n (n)}<div class="mp-hint"><Info size={12} /> {n}</div>{/each}
        <div class="mp-hint"><Info size={12} /> Built from a reading of open connections once a minute{d.collectedAt ? ` (last ${ago(d.collectedAt)})` : ''}: long-lived connections show at once, short requests appear over time. Internet clients are counted, not listed.</div>
      {/if}
    </div>
  </div>
</div>

{#if sourcesOpen}<MapSources {prefill} onclose={() => { sourcesOpen = false; prefill = null; q.refetch() }} />{/if}

<style>
  .mp-bar { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; font-size: 12px; }
  .seg { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .seg button { padding: 5px 11px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .mp-how { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 8px 12px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); font-size: 12px; }
  .mp-how__l { font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); }
  .mp-how__t { display: inline-flex; align-items: center; gap: 8px; flex-wrap: wrap; color: var(--text-secondary); }
  .mp-how .seg button { display: inline-flex; align-items: center; gap: 5px; }
  .mp-how :global(svg) { vertical-align: -2px; }
  .chk { display: inline-flex; align-items: center; gap: 5px; color: var(--text-secondary); cursor: pointer; }
  .mp-spacer { flex: 1; }
  .mp-legend { color: var(--text-muted); font-size: 11.5px; }
  .lg { display: inline-block; width: 22px; height: 0; border-top: 2px solid var(--text-muted); vertical-align: middle; margin: 0 4px 0 8px; }
  .lg--c { border-top: 2px dashed var(--warning); }
  .mp-main { display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; align-items: start; }
  .mp-main.has-side { grid-template-columns: minmax(0, 1fr) 300px; }
  .mp-card { overflow: hidden; background: var(--bg-body); }
  .mp-more { padding: 0 22px 12px; font-size: 11.5px; }
  .mp-tools { display: flex; align-items: center; gap: 10px; padding: 10px 12px; background: var(--bg-surface); border-bottom: 1px solid var(--border); }
  .mp-search { flex: 1; display: flex; align-items: center; gap: 8px; padding: 0 10px; height: 32px; border: 1px solid var(--border); border-radius: var(--r); color: var(--text-muted); }
  .mp-search input { flex: 1; background: none; border: 0; outline: 0; color: var(--text-primary); font-size: 12.5px; }
  .data-table-wrap { background: var(--bg-surface); }
  .mp-row { cursor: pointer; }
  .mp-row.is-sel td { background: var(--brand-soft); }
  .mp-row :global(svg) { vertical-align: -1px; }
  .mp-k { font-size: 10.5px; }
  .mp-note { font-size: 11.5px; max-width: 260px; }
  .mp-side { padding: 14px; display: flex; flex-direction: column; gap: 8px; position: sticky; top: 0; max-height: calc(100vh - 140px); overflow: auto; }
  .mp-side__h { display: flex; justify-content: space-between; align-items: flex-start; gap: 8px; }
  .mp-side__t { font-size: 14px; font-weight: 700; color: var(--text-primary); word-break: break-all; }
  dl { display: grid; grid-template-columns: 92px 1fr; gap: 5px 10px; margin: 0; font-size: 12px; }
  dt { color: var(--text-muted); }
  dd { margin: 0; color: var(--text-primary); word-break: break-word; white-space: pre-line; }
  .mp-sub { font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); margin-top: 6px; }
  .mp-ports { display: flex; gap: 5px; flex-wrap: wrap; }
  .ln { display: flex; align-items: center; gap: 8px; padding: 5px 6px; border: 0; border-radius: var(--r); background: none; font: inherit; font-size: 12px; color: var(--text-primary); cursor: pointer; text-align: left; }
  .ln:hover { background: var(--bg-hover); }
  .ln__p { flex: 0 0 44px; color: var(--brand); }
  .ln__n { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ln__w { font-size: 10.5px; white-space: nowrap; }
  .mp-none { font-size: 12px; padding: 0 6px; }
  .mp-open { align-self: flex-start; margin-top: 6px; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; text-align: left; }
  .mp-hint { font-size: 11.5px; color: var(--text-muted); }
  .mp-hint :global(svg) { vertical-align: -2px; }
  .mp-empty { gap: 10px; padding: 36px 20px; text-align: center; }
  .mp-empty__t { font-size: 15px; font-weight: 700; color: var(--text-primary); }
  @media (max-width: 1100px) { .mp-main.has-side { grid-template-columns: 1fr; } }
</style>
