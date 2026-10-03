<script lang="ts">
  // The diagram: servers as cards in columns (traffic flows left to right),
  // their processes and containers as rows, connections as curves between them.
  // Cards are plain HTML; an SVG layer on top draws the edges between measured rows.
  import { tick } from 'svelte'
  import type { Component } from 'svelte'
  import { Server, Container as Box, Cog, Globe, Cloud, Split, ArrowRightLeft, Cpu, DoorOpen, Network, Boxes, Hexagon, Router, Layers } from '@lucide/svelte'
  import type { MapEdge, MapNode } from '../gen/timika/v1/monitor_pb'
  import { edgeKey, type Layout } from '../lib/map'

  let { plan, edges, selected = $bindable(''), path }: { plan: Layout; edges: MapEdge[]; selected: string; path: { nodes: Set<string>; edges: Set<string> } } = $props()

  type Rect = { l: number; r: number; y: number }
  let wrap = $state<HTMLElement>()
  let rects = $state<Record<string, Rect>>({})
  let size = $state({ w: 0, h: 0 })

  function measure() {
    if (!wrap) return
    const base = wrap.getBoundingClientRect()
    const next: Record<string, Rect> = {}
    for (const el of wrap.querySelectorAll<HTMLElement>('[data-node]')) {
      const b = el.getBoundingClientRect()
      next[el.dataset.node!] = { l: b.left - base.left + wrap.scrollLeft, r: b.right - base.left + wrap.scrollLeft, y: b.top - base.top + wrap.scrollTop + b.height / 2 }
    }
    rects = next
    size = { w: wrap.scrollWidth, h: wrap.scrollHeight }
  }
  $effect(() => {
    void plan
    tick().then(measure)
  })
  $effect(() => {
    if (!wrap) return
    const ro = new ResizeObserver(measure)
    ro.observe(wrap)
    return () => ro.disconnect()
  })

  type Line = { key: string; d: string; x: number; y: number; e: MapEdge }
  const lines = $derived.by(() => {
    const out: Line[] = []
    // Edges that share an end are spread a little, so they don't draw over each other.
    const used: Record<string, number> = {}
    const slot = (k: string) => { const n = used[k] ?? 0; used[k] = n + 1; return (n % 2 ? -1 : 1) * Math.ceil(n / 2) * 5 }
    for (const e of edges) {
      const a = rects[e.from], b = rects[e.to]
      if (!a || !b) continue
      const y1 = a.y + slot('o' + e.from), y2 = b.y + slot('i' + e.to)
      // p0 → p3 with control points c1, c2; the port is written near the start,
      // where edges out of different rows are still apart.
      let p: [number, number][], t = 0.14
      if (b.l > a.r + 24) {
        const k = Math.max(40, (b.l - a.r) / 2)
        p = [[a.r, y1], [a.r + k, y1], [b.l - k, y2], [b.l, y2]]
      } else if (a.l > b.r + 24) {
        // Back to an earlier column: out of the left side, into the right.
        const k = Math.max(40, (a.l - b.r) / 2)
        p = [[a.l, y1], [a.l - k, y1], [b.r + k, y2], [b.r, y2]]
      } else {
        // The same column: around the right side, wider the further apart.
        const k = 26 + Math.min(44, Math.abs(y2 - y1) / 3)
        const r = Math.max(a.r, b.r)
        p = [[a.r, y1], [r + k, y1], [r + k, y2], [b.r, y2]]
        t = 0.5
      }
      const at = (i: 0 | 1) => (1 - t) ** 3 * p[0][i] + 3 * (1 - t) ** 2 * t * p[1][i] + 3 * (1 - t) * t ** 2 * p[2][i] + t ** 3 * p[3][i]
      out.push({ key: edgeKey(e), d: `M ${p[0][0]} ${p[0][1]} C ${p[1][0]} ${p[1][1]}, ${p[2][0]} ${p[2][1]}, ${p[3][0]} ${p[3][1]}`, x: at(0), y: at(1), e })
    }
    return out
  })

  const lit = (id: string) => !selected || path.nodes.has(id)
  const pick = (id: string) => (selected = selected === id ? '' : id)
  const ICON: Record<string, Component<{ size?: number }>> = { container: Box, process: Cog, vm: Server, pubip: Globe, vpc: Router, subnet: Layers, lb: Split, nat: ArrowRightLeft, node: Cpu, ingress: DoorOpen, service: Network, workload: Boxes, server: Server, cloud: Cloud, cluster: Hexagon }
  const portsOf = (n: MapNode) => [...new Set(n.ports.map((p) => p.port))].sort((a, b) => a - b)
</script>

{#snippet row(n: MapNode)}
  {@const Ico = ICON[n.kind] ?? Cog}
  <button class="row row--{n.kind}" class:is-sel={selected === n.id} class:is-dim={!lit(n.id)} class:is-off={n.kind === 'container' && n.state !== 'running'} data-node={n.id} onclick={() => pick(n.id)} title="{n.kind}{n.scope ? ` · ${n.scope}` : ''}{n.detail ? ` · ${n.detail}` : ''}">
    <Ico size={13} />
    <span class="row__l">{n.label}</span>{#if n.public}<span class="row__pub" title="Reachable from the internet">public</span>{/if}
    {#if (n.kind === 'vpc' || n.kind === 'subnet') && n.detail}<span class="row__p mono row__cidr">{n.detail}</span>{:else}<span class="row__p mono">{#each portsOf(n).slice(0, 4) as p (p)}<i>{p}</i>{/each}{#if portsOf(n).length > 4}<i>+{portsOf(n).length - 4}</i>{/if}</span>{/if}
  </button>
{/snippet}
{#snippet outside(n: MapNode)}
  <button class="out" class:is-sel={selected === n.id} class:is-dim={!lit(n.id)} class:out--net={n.kind === 'internet'} data-node={n.id} onclick={() => pick(n.id)}>
    {#if n.kind === 'internet'}<Cloud size={14} />{:else}<Globe size={13} />{/if}
    <span class="mono">{n.label}</span>
    {#if n.kind === 'external'}<span class="out__t">{n.public ? 'public' : 'private'}</span>{/if}
  </button>
{/snippet}

<div class="wrap" bind:this={wrap} role="presentation" onclick={(e) => { if (e.target === e.currentTarget) selected = '' }}>
  <svg class="wires" width={size.w} height={size.h} aria-hidden="true">
    <defs>
      {#each ['a', 'b', 'c'] as m (m)}
        <marker id="arrow-{m}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0 L8 4 L0 8 z" class="head head--{m}" /></marker>
      {/each}
    </defs>
    {#each lines as l (l.key)}
      {@const on = !selected || path.edges.has(l.key)}
      {@const tone = !l.e.observed ? 'c' : selected && on ? 'b' : 'a'}
      <g class="wire wire--{tone}" class:is-dim={!on} class:is-sel={selected === l.key}>
        <path d={l.d} class="wire__line" class:wire__line--dash={!l.e.observed} marker-end="url(#arrow-{tone})" />
        <path d={l.d} class="wire__hit" role="presentation" onclick={() => pick(l.key)}><title>{l.e.port}{l.e.sites.length ? ` · ${l.e.sites.join(', ')}` : ''}</title></path>
        {#if l.e.port}<text x={l.x} y={l.y - 4} class="wire__t">{l.e.port}</text>{/if}
      </g>
    {/each}
  </svg>

  <div class="cols">
    {#if plan.clients.length}
      <div class="col col--out">
        <div class="col__t">Callers</div>
        {#each plan.clients as n (n.id)}{@render outside(n)}{/each}
      </div>
    {/if}
    {#each plan.columns as col, i (i)}
      <div class="col">
        {#each col as g (g.server.id)}
          <div class="card" class:is-dim={selected !== '' && !lit(g.server.group) && !g.children.some((c) => path.nodes.has(c.id))}>
            <button class="card__h" class:is-sel={selected === g.server.group} data-node={g.server.id} onclick={() => pick(g.server.group)}>
              {#if g.server.kind === 'server'}<span class="dot dot--{g.server.state}"></span>{/if}
              {#if g.server.kind === 'cloud'}<Cloud size={14} />{:else if g.server.kind === 'cluster'}<Hexagon size={14} />{:else}<Server size={14} />{/if}
              <span class="card__n">{g.server.label}</span>
              <span class="card__a mono">{g.server.kind === 'server' ? (g.server.addresses[0] ?? g.server.detail) : g.server.detail}</span>
            </button>
            {#if g.server.kind === 'server' && g.server.scope}<div class="card__where">{g.server.scope}</div>{/if}
            {#each g.children as n (n.id)}{@render row(n)}{/each}
            {#if g.more}<div class="card__none">+{g.more} more without connections — see the table</div>{/if}
            {#if !g.children.length}<div class="card__none">{g.server.kind === 'server' ? 'nothing listening seen' : 'nothing found'}</div>{/if}
          </div>
        {/each}
      </div>
    {/each}
    {#if plan.services.length}
      <div class="col col--out">
        <div class="col__t">Called</div>
        {#each plan.services as n (n.id)}{@render outside(n)}{/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .wrap { position: relative; overflow: auto; padding: 22px 76px 26px 22px; min-height: 280px; }
  /* Wires run under the cards, so a long connection never strikes through a row. */
  .wires { position: absolute; left: 0; top: 0; pointer-events: none; z-index: 0; }
  .cols { position: relative; z-index: 1; pointer-events: none; display: flex; gap: 84px; align-items: flex-start; width: max-content; min-width: 100%; }
  .card, .out { pointer-events: auto; }
  .col { display: flex; flex-direction: column; gap: 22px; flex: 0 0 auto; }
  .col--out { gap: 8px; padding-top: 4px; }
  .col__t { font-size: 10.5px; text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
  .card { width: 224px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); box-shadow: var(--shadow-sm); overflow: hidden; transition: opacity .15s; }
  .card__h { display: flex; align-items: center; gap: 7px; width: 100%; padding: 9px 11px; border: 0; border-bottom: 1px solid var(--border); background: var(--bg-elevated); color: var(--text-primary); font: inherit; cursor: pointer; text-align: left; }
  .card__n { font-weight: 700; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .card__a { margin-left: auto; font-size: 10.5px; color: var(--text-muted); white-space: nowrap; }
  .card__where { padding: 4px 11px; font-size: 10.5px; color: var(--text-muted); border-bottom: 1px solid var(--border); background: var(--bg-elevated); }
  .card__where + .row { border-top: 0; }
  .row--pubip :global(svg) { color: var(--info); }
  .row--vpc, .row--subnet { cursor: default; }
  .row--vpc :global(svg), .row--subnet :global(svg) { color: var(--text-muted); }
  .row__cidr { font-size: 10.5px; color: var(--text-secondary); }
  .card__none { padding: 9px 11px; font-size: 11.5px; color: var(--text-muted); }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--text-muted); flex: 0 0 auto; }
  .dot--up { background: var(--success); }
  .dot--down { background: var(--danger); }
  .row { display: flex; align-items: center; gap: 7px; width: 100%; padding: 6px 11px; border: 0; border-top: 1px solid var(--border); background: none; color: var(--text-secondary); font: inherit; font-size: 12px; cursor: pointer; text-align: left; transition: opacity .15s; }
  .card__h + .row { border-top: 0; }
  .row:hover, .out:hover { background: var(--bg-hover); }
  .row__l { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-primary); }
  .row__p { margin-left: auto; display: flex; gap: 3px; flex: 0 0 auto; }
  .row__p i { font-style: normal; font-size: 10px; padding: 0 4px; border-radius: 3px; background: var(--bg-elevated); color: var(--text-muted); }
  .row.is-off { opacity: 0.5; }
  .row__pub { font-size: 9.5px; padding: 0 4px; border-radius: 3px; background: var(--info-bg, var(--bg-elevated)); color: var(--info); flex: 0 0 auto; }
  .row--lb :global(svg), .row--ingress :global(svg) { color: var(--info); }
  .row--nat :global(svg) { color: var(--warning); }
  .row--service :global(svg), .row--workload :global(svg) { color: var(--brand); }
  .out { display: flex; align-items: center; gap: 7px; min-width: 150px; position: relative; padding: 6px 10px; border-radius: 999px; border: 1px solid var(--border); background: var(--bg-surface); color: var(--text-secondary); font: inherit; font-size: 11.5px; cursor: pointer; transition: opacity .15s; }
  .out--net { color: var(--info); border-color: var(--info); font-weight: 600; }
  .out__t { margin-left: auto; font-size: 10px; color: var(--text-muted); }
  .is-sel, .card__h.is-sel { box-shadow: inset 0 0 0 2px var(--brand); }
  .out.is-sel { box-shadow: 0 0 0 2px var(--brand); border-color: var(--brand); }
  .is-dim { opacity: 0.28; }
  .wire__line { fill: none; stroke-width: 1.4; transition: opacity .15s; }
  .wire__line--dash { stroke-dasharray: 5 4; }
  .wire__hit { fill: none; stroke: transparent; stroke-width: 12; pointer-events: stroke; cursor: pointer; }
  .wire__t { font-family: var(--mono); font-size: 9.5px; text-anchor: middle; paint-order: stroke; stroke: var(--bg-body); stroke-width: 3px; }
  .wire--a .wire__line { stroke: var(--text-muted); }
  .wire--a .wire__t { fill: var(--text-muted); }
  .wire--b .wire__line { stroke: var(--brand); stroke-width: 2; }
  .wire--b .wire__t { fill: var(--brand); font-weight: 700; }
  .wire--c .wire__line { stroke: var(--warning); }
  .wire--c .wire__t { fill: var(--warning); }
  .wire.is-sel .wire__line { stroke-width: 2.6; }
  .wire.is-dim { opacity: 0.1; }
  .head--a { fill: var(--text-muted); }
  .head--b { fill: var(--brand); }
  .head--c { fill: var(--warning); }
</style>
