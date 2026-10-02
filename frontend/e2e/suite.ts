// Scenario registry. Each scenario is independent of the order it runs in,
// except that it may share a lazily created fixture (a running vault).

export type Scenario = { id: string; cat: string; title: string; fn: () => Promise<void>; timeout?: number }

export const CATEGORIES: Record<string, string> = {
  A: 'HTTP & protocol',
  B: 'Init, unseal, seal',
  C: 'Sign-in & sessions',
  D: 'User administration',
  E: 'KV secrets',
  F: 'Bastion',
  G: 'Raft cluster',
  H: 'Audit log & CLI',
  I: 'People, roles & audit trail',
  J: 'Files (SFTP)',
  K: 'Two-factor (MFA)',
  L: 'Infrastructure automation',
  M: 'Monitoring',
}

export const scenarios: Scenario[] = []
const seq: Record<string, number> = {}

export function scenario(cat: keyof typeof CATEGORIES & string, title: string, fn: () => Promise<void>, opts: { timeout?: number } = {}) {
  seq[cat] = (seq[cat] ?? 0) + 1
  scenarios.push({ id: `${cat}${String(seq[cat]).padStart(2, '0')}`, cat, title, fn, timeout: opts.timeout })
}

/** Created on first use, then shared. */
export function fixture<T>(make: () => Promise<T>): () => Promise<T> {
  let p: Promise<T> | undefined
  return () => (p ??= make())
}
