// Shared bits for the Infrastructure (automation) pages.
import type { Component } from 'svelte'
import { Layers, ListChecks, Cloud } from '@lucide/svelte'
import type { Project, Run } from '../gen/timika/v1/automation_pb'

type Icon = Component<{ size?: number; class?: string }>

export const KINDS: Record<string, { label: string; ico: Icon; badge: string }> = {
  terraform: { label: 'Terraform', ico: Layers, badge: 'info' },
  ansible: { label: 'Ansible', ico: ListChecks, badge: 'danger' },
  pulumi: { label: 'Pulumi', ico: Cloud, badge: 'warning' },
}

/** [safe action, action that changes things] per kind. */
export const ACTIONS: Record<string, [string, string]> = {
  terraform: ['plan', 'apply'],
  ansible: ['check', 'run'],
  pulumi: ['preview', 'up'],
}

export const ACTION_LABEL: Record<string, string> = {
  plan: 'Plan', apply: 'Apply', check: 'Check', run: 'Run', preview: 'Preview', up: 'Up',
}

export const kindLabel = (k: string, tfBin = 'auto') =>
  k === 'terraform' && tfBin === 'tofu' ? 'OpenTofu' : KINDS[k]?.label ?? k

export const STATUS_BADGE: Record<string, string> = {
  approval: 'warning', queued: 'default', running: 'info', succeeded: 'success', failed: 'danger', cancelled: 'default', lost: 'warning',
}

export const isActive = (r: Run) => r.status === 'queued' || r.status === 'running'
export const STATUS_TEXT: Record<string, string> = { approval: 'awaiting approval' }

export function ago(t?: string): string {
  if (!t) return ''
  const s = (Date.now() - new Date(t).getTime()) / 1000
  if (s < 45) return 'just now'
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))} min ago`
  if (s < 86400) return `${Math.round(s / 3600)} h ago`
  return `${Math.round(s / 86400)} d ago`
}

export function duration(r: Run): string {
  if (!r.startedAt) return ''
  const end = r.finishedAt ? new Date(r.finishedAt).getTime() : Date.now()
  const s = Math.max(0, Math.round((end - new Date(r.startedAt).getTime()) / 1000))
  return s < 60 ? `${s}s` : s < 3600 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

export const short = (sha: string) => sha.slice(0, 7)

/** The latest run per project. */
export function latestByProject(runs: Run[]): Map<string, Run> {
  const m = new Map<string, Run>()
  for (const r of runs) if (!m.has(r.project)) m.set(r.project, r)
  return m
}

/** A plan that can be applied: the project's newest plan, successful, with changes, not applied. */
export function applicablePlan(runs: Run[], p: Project): Run | undefined {
  const plan = runs.find((r) => r.project === p.id && r.action === 'plan')
  if (!plan || plan.status !== 'succeeded' || !plan.changes || plan.appliedBy) return
  // An apply since then makes it stale.
  const applied = runs.find((r) => r.project === p.id && r.action === 'apply' && r.createdAt > plan.createdAt)
  return applied ? undefined : plan
}

/** `owner/repo` and a web link, for the common hosts. */
export function repoWeb(url: string): { host: string; path: string; web?: string } {
  let host = ''
  let path = ''
  const scp = url.match(/^[^@/]+@([^:]+):(.+)$/)
  if (scp) [, host, path] = scp
  else {
    try {
      const u = new URL(url.replace(/^ssh:\/\/[^@]+@/, 'ssh://'))
      host = u.hostname
      path = u.pathname.replace(/^\//, '')
    } catch { return { host: '', path: url } }
  }
  path = path.replace(/\.git$/, '').replace(/\/$/, '')
  const web = /github\.com|gitlab\.com|bitbucket\.org|codeberg\.org/.test(host) ? `https://${host}/${path}` : undefined
  return { host, path, web }
}

/** Where to add a deploy key, for the common hosts. */
export function deployKeyPage(url: string): string | undefined {
  const { host, web } = repoWeb(url)
  if (!web) return
  if (host.includes('github.com')) return `${web}/settings/keys/new`
  if (host.includes('gitlab.com')) return `${web}/-/settings/repository#js-deploy-keys-settings`
  if (host.includes('bitbucket.org')) return `${web}/admin/access-keys/`
}

/** Where to add a webhook, for the common hosts. */
export function webhookPage(url: string): string | undefined {
  const { host, web } = repoWeb(url)
  if (!web) return
  if (host.includes('github.com')) return `${web}/settings/hooks/new`
  if (host.includes('gitlab.com')) return `${web}/-/hooks`
}

/** https://github.com/a/b(.git) → git@github.com:a/b.git (deploy keys need SSH). */
export function sshUrl(url: string): string | undefined {
  const m = url.match(/^https?:\/\/([^/]+)\/(.+?)(\.git)?\/?$/)
  return m ? `git@${m[1]}:${m[2]}.git` : undefined
}

export const isSshUrl = (url: string) => /^ssh:\/\//.test(url) || (!url.includes('://') && url.includes('@'))

// ── run options (prompts) ────────────────────────────────────────────────────

/** Form model for RunOptions (lists edited as one-per-line text). */
export type Opts = {
  limit: string; tags: string; skipTags: string; extraVars: string; verbose: number; servers: string
  destroy: boolean; targets: string; replace: string; workspace: string; refresh: boolean
}

export const emptyOpts = (): Opts => ({ limit: '', tags: '', skipTags: '', extraVars: '', verbose: 0, servers: '', destroy: false, targets: '', replace: '', workspace: '', refresh: false })

const lines = (s: string) => s.split('\n').map((x) => x.trim()).filter(Boolean)

export const optsInput = (o: Opts) => ({ ...o, targets: lines(o.targets), replace: lines(o.replace) })

export const optsFrom = (o?: { limit: string; tags: string; skipTags: string; extraVars: string; verbose: number; servers: string; destroy: boolean; targets: string[]; replace: string[]; workspace: string; refresh: boolean }): Opts =>
  o ? { limit: o.limit, tags: o.tags, skipTags: o.skipTags, extraVars: o.extraVars, verbose: o.verbose, servers: o.servers, destroy: o.destroy, targets: o.targets.join('\n'), replace: o.replace.join('\n'), workspace: o.workspace, refresh: o.refresh } : emptyOpts()

// ── schedules ────────────────────────────────────────────────────────────────

export type Preset = 'hourly' | 'daily' | 'weekdays' | 'weekly' | 'monthly' | 'custom'

export const PRESETS: { id: Preset; label: string }[] = [
  { id: 'hourly', label: 'Every hour' }, { id: 'daily', label: 'Every day' }, { id: 'weekdays', label: 'Every weekday' },
  { id: 'weekly', label: 'Every Monday' }, { id: 'monthly', label: 'On the 1st of each month' }, { id: 'custom', label: 'Custom (cron)' },
]

export function cronFor(p: Preset, time: string, custom: string): string {
  const [h, m] = (time || '02:00').split(':').map((x) => String(Number(x) || 0))
  switch (p) {
    case 'hourly': return `${m} * * * *`
    case 'daily': return `${m} ${h} * * *`
    case 'weekdays': return `${m} ${h} * * 1-5`
    case 'weekly': return `${m} ${h} * * 1`
    case 'monthly': return `${m} ${h} 1 * *`
    default: return custom.trim()
  }
}

/** Back from a cron expression to a preset + time, when it is one. */
export function presetOf(cron: string): { preset: Preset; time: string } {
  const f = cron.trim().split(/\s+/)
  const pad = (x: string) => x.padStart(2, '0')
  if (f.length === 5 && /^\d+$/.test(f[0])) {
    const time = /^\d+$/.test(f[1]) ? `${pad(f[1])}:${pad(f[0])}` : '02:00'
    const rest = f.slice(1).join(' ')
    if (rest === '* * * *') return { preset: 'hourly', time: `00:${pad(f[0])}` }
    if (/^\d+$/.test(f[1])) {
      const tail = f.slice(2).join(' ')
      if (tail === '* * *') return { preset: 'daily', time }
      if (tail === '* * 1-5') return { preset: 'weekdays', time }
      if (tail === '* * 1') return { preset: 'weekly', time }
      if (tail === '1 * *') return { preset: 'monthly', time }
    }
  }
  return { preset: 'custom', time: '02:00' }
}

export function describeCron(cron: string): string {
  const { preset, time } = presetOf(cron)
  if (preset === 'custom') return cron
  if (preset === 'hourly') return `every hour at :${time.slice(3)}`
  const label = PRESETS.find((p) => p.id === preset)!.label
  return `${label[0].toLowerCase()}${label.slice(1)} at ${time}`
}

/** The browser's UTC offset as +07:00. */
export function localOffset(): string {
  const m = -new Date().getTimezoneOffset()
  const sign = m >= 0 ? '+' : '-'
  const a = Math.abs(m)
  return `${sign}${String(Math.floor(a / 60)).padStart(2, '0')}:${String(a % 60).padStart(2, '0')}`
}

// ── file viewer ──────────────────────────────────────────────────────────────

export type Tok = { t: string; c: string }

const TOKEN = /(?:^|\s)#.*$|^\s*\/\/.*$|("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')|\b(resource|variable|output|module|provider|data|locals|terraform|backend|required_providers|hosts|tasks|handlers|roles|vars|become|when|loop|name|import_playbook|include_tasks|true|false|null|yes|no)\b|\b(\d+(?:\.\d+)?)\b/g

/** A light colouring for Terraform / YAML / shell lines: comments, strings, keywords, numbers. */
export function highlight(line: string): Tok[] {
  const out: Tok[] = []
  let last = 0
  TOKEN.lastIndex = 0
  for (let m = TOKEN.exec(line); m; m = TOKEN.exec(line)) {
    if (m[0] === '') { TOKEN.lastIndex++; continue }
    if (m.index > last) out.push({ t: line.slice(last, m.index), c: '' })
    out.push({ t: m[0], c: m[1] ? 'str' : m[2] ? 'kw' : m[3] ? 'num' : 'com' })
    last = m.index + m[0].length
  }
  if (last < line.length) out.push({ t: line.slice(last), c: '' })
  return out
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}
