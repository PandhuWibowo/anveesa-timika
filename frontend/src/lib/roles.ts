import type { Component } from 'svelte'
import { Shield, BookOpen, SquareTerminal } from '@lucide/svelte'

export type Role = 'ssh' | 'read-only' | 'admin'

export const ROLES: { id: Role; label: string; hint: string; ico: Component<{ size?: number }>; badge: string }[] = [
  { id: 'ssh', label: 'SSH access', hint: 'Only the servers you grant — no vault data', ico: SquareTerminal, badge: 'info' },
  { id: 'read-only', label: 'Vault reader', hint: 'Reads secrets and status; servers you grant', ico: BookOpen, badge: 'default' },
  { id: 'admin', label: 'Administrator', hint: 'Everything, every server', ico: Shield, badge: 'danger' },
]

export const roleOf = (policies: string[]): Role =>
  policies.includes('admin') || policies.includes('root') ? 'admin' : policies.includes('read-only') ? 'read-only' : 'ssh'

export const roleLabel = (policies: string[]) => ROLES.find((r) => r.id === roleOf(policies))?.label ?? policies.join(', ')
export const roleBadge = (policies: string[]) => ROLES.find((r) => r.id === roleOf(policies))?.badge ?? 'default'
