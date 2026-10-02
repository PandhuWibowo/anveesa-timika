// Formatting and constants for the Monitoring pages.

export function size(n: number | bigint): string {
  const v = Number(n)
  if (v < 1024) return `${v} B`
  const u = ['KB', 'MB', 'GB', 'TB', 'PB']
  let x = v / 1024
  let i = 0
  while (x >= 1024 && i < u.length - 1) { x /= 1024; i++ }
  return `${x >= 100 ? x.toFixed(0) : x.toFixed(1)} ${u[i]}`
}

/** Bytes per second. */
export function rate(v?: number): string {
  if (v === undefined || Number.isNaN(v)) return '—'
  if (v >= 1e9) return `${(v / 1e9).toFixed(1)} GB/s`
  if (v >= 1e6) return `${(v / 1e6).toFixed(1)} MB/s`
  if (v >= 1e3) return `${(v / 1e3).toFixed(0)} kB/s`
  return `${v > 0 && v < 10 ? v.toFixed(1) : v.toFixed(0)} B/s`
}

export const pct = (v?: number) => (v === undefined || Number.isNaN(v) ? '—' : `${v.toFixed(v < 10 ? 1 : 0)}%`)

export function uptime(s: number | bigint): string {
  const v = Number(s)
  if (!v) return '—'
  const d = Math.floor(v / 86400)
  const h = Math.floor((v % 86400) / 3600)
  if (d) return `${d}d ${h}h`
  const m = Math.floor((v % 3600) / 60)
  return h ? `${h}h ${m}m` : `${m}m`
}

/** ok · warn · bad for a percentage. */
export const level = (v?: number) => (v === undefined ? 'none' : v >= 90 ? 'bad' : v >= 70 ? 'warn' : 'ok')

export function since(t?: string): string {
  if (!t) return ''
  const s = (Date.now() - new Date(t).getTime()) / 1000
  if (s < 90) return 'just now'
  if (s < 3600) return `${Math.round(s / 60)} min`
  if (s < 86400) return `${Math.round(s / 3600)} h`
  return `${Math.round(s / 86400)} d`
}

export const RANGES = ['1h', '6h', '24h', '7d', '30d', '90d'] as const
export type Range = (typeof RANGES)[number]

/** Alert metrics: label, unit and how the stored threshold maps to the input. */
export const RULES: { id: string; label: string; unit: string; scale: number; threshold: number; minutes: number; hint?: string }[] = [
  { id: 'status', label: 'Server is down', unit: '', scale: 1, threshold: 0, minutes: 2, hint: 'no answer over SSH' },
  { id: 'cpu', label: 'CPU above', unit: '%', scale: 1, threshold: 90, minutes: 10 },
  { id: 'mem', label: 'Memory above', unit: '%', scale: 1, threshold: 90, minutes: 10 },
  { id: 'disk', label: 'Disk above', unit: '%', scale: 1, threshold: 90, minutes: 10 },
  { id: 'swap', label: 'Swap above', unit: '%', scale: 1, threshold: 80, minutes: 10 },
  { id: 'load', label: 'Load (1 min) above', unit: '', scale: 1, threshold: 8, minutes: 10 },
  { id: 'temp', label: 'Temperature above', unit: '°C', scale: 1, threshold: 85, minutes: 10 },
  { id: 'rx', label: 'Network in above', unit: 'MB/s', scale: 1e6, threshold: 50e6, minutes: 10 },
  { id: 'tx', label: 'Network out above', unit: 'MB/s', scale: 1e6, threshold: 50e6, minutes: 10 },
]

export const ruleLabel = (m: string) => RULES.find((r) => r.id === m)?.label.replace(/ above$/, '').replace('Server is down', 'Down') ?? m

export type RuleRow = { metric: string; threshold: number; minutes: number }
