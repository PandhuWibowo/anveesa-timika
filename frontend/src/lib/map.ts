// The network map: layout and path helpers (no drawing here).
import type { MapEdge, MapNode } from '../gen/timika/v1/monitor_pb'

export const edgeKey = (e: MapEdge) => `${e.from}>${e.to}:${e.port}`

/** A card: a server, a cloud account or a cluster, with what is in it. */
export type Group = { server: MapNode; children: MapNode[]; more: number }
export type Layout = { clients: MapNode[]; columns: Group[][]; services: MapNode[]; hidden: number }

export const HEADERS = ['server', 'cloud', 'cluster']
const ORDER = ['vpc', 'subnet', 'pubip', 'lb', 'ingress', 'service', 'workload', 'container', 'process', 'nat', 'vm', 'node']
const order = (n: MapNode) => ORDER.indexOf(n.kind)

/**
 * Servers in columns, left to right in the direction traffic flows; whoever
 * only calls in on the left, whatever is only called on the right.
 */
export function layout(nodes: MapNode[], edges: MapEdge[], maxOutside: number, maxRows = 26): Layout {
  const by = new Map(nodes.map((n) => [n.id, n]))
  // A cloud account is drawn as two cards: what traffic comes in through (load
  // balancers) before the servers, and the rest (NAT gateways, other VMs) after.
  const card = new Map<string, string>()
  const groupOf = (id: string) => card.get(id) ?? by.get(id)?.group ?? ''
  const busyIds = new Set(edges.flatMap((e) => [e.from, e.to]))
  const groups = new Map<string, Group>()
  for (const n of nodes) if (HEADERS.includes(n.kind)) groups.set(n.group, { server: n, children: [], more: 0 })
  for (const n of nodes) if (n.group && !HEADERS.includes(n.kind)) groups.get(n.group)?.children.push(n)
  for (const [k, g] of [...groups]) {
    if (g.server.kind !== 'cloud') continue
    const front = g.children.filter((n) => n.kind !== 'nat' && n.kind !== 'vm')
    const back = g.children.filter((n) => n.kind === 'nat' || n.kind === 'vm')
    if (!front.length || !back.length) continue
    g.children = front
    back.forEach((n) => card.set(n.id, `${k}#net`))
    groups.set(`${k}#net`, { server: { ...g.server, id: `${g.server.id}#net`, detail: `${g.server.detail} · network` } as MapNode, children: back, more: 0 })
  }
  for (const g of groups.values()) {
    g.children.sort((a, b) => order(a) - order(b) || a.scope.localeCompare(b.scope) || a.label.localeCompare(b.label))
    // A big cluster: what is connected first, the rest is in the table.
    if (g.children.length > maxRows) {
      const keep = [...g.children].sort((a, b) => Number(busyIds.has(b.id)) - Number(busyIds.has(a.id))).slice(0, maxRows)
      g.more = g.children.length - keep.length
      g.children = g.children.filter((n) => keep.includes(n))
    }
  }

  // Depth = the longest chain of servers calling this one (cycles stop growing).
  const depth = new Map([...groups.keys()].map((k) => [k, 0]))
  const between = edges.map((e) => [groupOf(e.from), groupOf(e.to)]).filter(([a, b]) => a && b && a !== b && groups.has(a) && groups.has(b))
  for (let round = 0; round < groups.size; round++) {
    let moved = false
    for (const [a, b] of between) {
      const d = depth.get(a)! + 1
      if (d > depth.get(b)! && d < groups.size) { depth.set(b, d); moved = true }
    }
    if (!moved) break
  }
  const columns: Group[][] = []
  for (const [k, g] of groups) (columns[depth.get(k)!] ??= []).push(g)
  const cols = columns.filter(Boolean)
  for (const c of cols) c.sort((a, b) => a.server.label.localeCompare(b.server.label))

  const outside = nodes.filter((n) => !n.group)
  const calls = (n: MapNode) => edges.filter((e) => e.from === n.id).length
  const called = (n: MapNode) => edges.filter((e) => e.to === n.id).length
  const busy = (a: MapNode, b: MapNode) => Number(b.kind === 'internet') - Number(a.kind === 'internet') || calls(b) + called(b) - calls(a) - called(a) || a.label.localeCompare(b.label)
  const clients = outside.filter((n) => calls(n) > 0 && calls(n) >= called(n)).sort(busy)
  const services = outside.filter((n) => !clients.includes(n) && called(n) > 0).sort(busy)
  return { clients: clients.slice(0, maxOutside), columns: cols, services: services.slice(0, maxOutside), hidden: Math.max(0, clients.length - maxOutside) + Math.max(0, services.length - maxOutside) }
}

/** Everything up- and downstream of a node or an edge: the end-to-end path through it. */
export function pathThrough(selected: string, edges: MapEdge[]): { nodes: Set<string>; edges: Set<string> } {
  const out = { nodes: new Set<string>(), edges: new Set<string>() }
  if (!selected) return out
  const picked = edges.find((e) => edgeKey(e) === selected)
  const walk = (start: string, forward: boolean) => {
    const queue = [start]
    const seen = new Set([start])
    while (queue.length) {
      const id = queue.pop()!
      out.nodes.add(id)
      // The internet is where a path ends, not a way through.
      if (id.startsWith('internet') && id !== start) continue
      for (const e of edges) {
        if ((forward ? e.from : e.to) !== id) continue
        out.edges.add(edgeKey(e))
        const next = forward ? e.to : e.from
        if (!seen.has(next)) { seen.add(next); queue.push(next) }
      }
    }
  }
  if (picked) {
    out.edges.add(selected)
    walk(picked.to, true)
    walk(picked.from, false)
  } else {
    walk(selected, true)
    walk(selected, false)
  }
  return out
}

export const kindText: Record<string, string> = { server: 'Server', process: 'Process', container: 'Container', external: 'Address', internet: 'Internet', cloud: 'Cloud account', cluster: 'Cluster', vm: 'VM', pubip: 'Public address', vpc: 'VPC', subnet: 'Subnet', lb: 'Load balancer', nat: 'NAT gateway', node: 'Cluster node', ingress: 'Ingress', service: 'Service', workload: 'Workload' }

/** "web-1 · nginx" */
export const place = (n?: MapNode) => (!n ? '?' : HEADERS.includes(n.kind) || !n.server ? n.label : `${n.server} · ${n.scope && ['ingress', 'service', 'workload'].includes(n.kind) ? n.scope + '/' : ''}${n.label}`)

export const edgeKind = (e: MapEdge) => (e.observed && e.declared ? `seen · ${e.declaredBy || 'configured'}` : e.observed ? 'seen' : `${e.declaredBy || 'configured'}, not seen`)

export function ago(t: string): string {
  if (!t) return '—'
  const s = (Date.now() - new Date(t).getTime()) / 1000
  if (s < 120) return 'just now'
  if (s < 5400) return `${Math.round(s / 60)} min ago`
  if (s < 172800) return `${Math.round(s / 3600)} h ago`
  return `${Math.round(s / 86400)} d ago`
}

export function csv(nodes: MapNode[], edges: MapEdge[]): string {
  const by = new Map(nodes.map((n) => [n.id, n]))
  const cell = (v: string | number) => `"${String(v).replace(/"/g, '""')}"`
  const rows = edges.map((e) => [place(by.get(e.from)), by.get(e.from)?.kind ?? '', place(by.get(e.to)), by.get(e.to)?.kind ?? '', e.port || 'any', edgeKind(e), e.fromProcess, e.toProcess, e.firstSeen, e.lastSeen, e.seen, e.peak, e.sites.join(' '), e.note])
  return [['from', 'from kind', 'to', 'to kind', 'port', 'evidence', 'from process', 'to process', 'first seen', 'last seen', 'readings', 'peak', 'nginx sites', 'note'], ...rows].map((r) => r.map(cell).join(',')).join('\n') + '\n'
}
