// Network tools: the checks of this visit (memory only; gone on sign-out or reload).
import type { Component } from 'svelte'
import { Radio, PlugZap, Globe, Route, Link, ShieldCheck, Ear } from '@lucide/svelte'
import { netApi, errMsg } from './api'
import { onClear } from './query'
import type { NetResult } from '../gen/timika/v1/net_pb'

export type Tool = { id: string; label: string; like: string; hint: string; target: string; ico: Component<{ size?: number }> }

export const TOOLS: Tool[] = [
  { id: 'ping', label: 'Ping', like: 'ping', hint: 'Does it answer, how fast, how many packets are lost', target: 'db.internal or 10.0.0.5', ico: Radio },
  { id: 'port', label: 'Port check', like: 'telnet · nc', hint: 'Is a port open, refused, or dropped by a firewall', target: 'db.internal or 10.0.0.5', ico: PlugZap },
  { id: 'dns', label: 'DNS lookup', like: 'dig · nslookup', hint: 'What a name resolves to, as this server sees it', target: 'example.com', ico: Globe },
  { id: 'trace', label: 'Trace route', like: 'traceroute · mtr', hint: 'The hops on the way, and where it stops', target: 'db.internal or 10.0.0.5', ico: Route },
  { id: 'http', label: 'HTTP check', like: 'curl', hint: 'Status and where the time goes: DNS, connect, TLS, first byte', target: 'https://api.example.com/health', ico: Link },
  { id: 'tls', label: 'TLS certificate', like: 'openssl', hint: 'Who it is for, who issued it, when it expires, whether this server trusts it', target: 'example.com', ico: ShieldCheck },
  { id: 'listen', label: 'Listening ports', like: 'ss · netstat', hint: 'What the server itself listens on, and which process', target: '', ico: Ear },
]

export type Ask = { tool: string; target: string; ports: number[]; record: string; resolver: string }
export type Source = { asset: string; name: string }
export type Row = Source & { busy: boolean; error: string; result: NetResult | null }
export type Run = Ask & { id: number; at: Date; rows: Row[] }

let seq = 0
export const net = $state<{ runs: Run[] }>({ runs: [] })

/** Run one check from every source at once; each row fills in as its server answers. */
export function run(ask: Ask, sources: Source[]) {
  const id = ++seq
  net.runs.unshift({ ...ask, id, at: new Date(), rows: sources.map((s) => ({ ...s, busy: true, error: '', result: null })) })
  net.runs.length = Math.min(net.runs.length, 20)
  // Through the store, so the page sees each answer arrive.
  const row = (asset: string) => net.runs.find((r) => r.id === id)?.rows.find((x) => x.asset === asset)
  for (const s of sources) {
    netApi
      .run({ tool: ask.tool, asset: s.asset, target: ask.target, ports: ask.ports, record: ask.record, resolver: ask.resolver })
      .then((result) => { const x = row(s.asset); if (x) { x.result = result; x.busy = false } })
      .catch((e) => { const x = row(s.asset); if (x) { x.error = errMsg(e); x.busy = false } })
  }
}

export const removeRun = (id: number) => { net.runs = net.runs.filter((r) => r.id !== id) }
export const clearRuns = () => { net.runs = [] }
onClear(clearRuns)

/** "22, 80 443" → [22, 80, 443] */
export const parsePorts = (s: string) => [...new Set(s.split(/[\s,;]+/).filter(Boolean).map(Number))]
