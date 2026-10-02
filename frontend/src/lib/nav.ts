import type { Component } from 'svelte'
import { LayoutDashboard, Server, SquareTerminal, History, Users, ScrollText, Workflow, Activity, Container } from '@lucide/svelte'
import { liveCount } from './terminals.svelte'

/** `admin`: administrators only · `vault`: needs vault read access (not the `ssh` role). */
export type NavItem = { to: string; label: string; hint: string; ico: Component<{ size?: number; class?: string }>; admin?: boolean; vault?: boolean; badge?: () => number }
export type NavGroup = { label: string; items: NavItem[] }

export const navGroups: NavGroup[] = [
  {
    label: 'Vault',
    items: [
      { to: '/', label: 'Overview', hint: 'Seal state, keyring and storage at a glance', ico: LayoutDashboard, vault: true },
    ],
  },
  {
    label: 'Access',
    items: [
      { to: '/servers', label: 'Servers', hint: 'Connect to a server in one click', ico: Server },
      { to: '/terminal', label: 'Terminal', hint: 'Your open terminal sessions', ico: SquareTerminal, badge: liveCount },
      { to: '/sessions', label: 'Sessions', hint: 'Recorded sessions — replay or end them', ico: History },
      { to: '/people', label: 'People', hint: 'Add users, set passwords, roles and server access', ico: Users, admin: true },
      { to: '/audit', label: 'Audit trail', hint: 'Who did what, when, from where', ico: ScrollText, admin: true },
    ],
  },
  {
    label: 'Monitoring',
    items: [
      { to: '/monitoring', label: 'Systems', hint: 'CPU, memory, disk, network, containers and alerts for your servers', ico: Activity },
      { to: '/containers', label: 'Containers', hint: 'Every Docker container on your monitored servers', ico: Container },
    ],
  },
  {
    label: 'Automation',
    items: [
      { to: '/automation', label: 'Infrastructure', hint: 'Terraform, OpenTofu, Ansible & Pulumi from Git — plan, apply, run', ico: Workflow, vault: true },
    ],
  },
]

export const allNav = navGroups.flatMap((g) => g.items)

export function titleFor(path: string): string {
  const top = '/' + (path.split('/')[1] ?? '')
  if (top === '/account') return 'My account'
  return allNav.find((i) => i.to === top)?.label ?? 'Overview'
}
