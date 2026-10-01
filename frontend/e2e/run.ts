// bun e2e/run.ts [filter…]   — filter by category letter (`E`) or scenario id (`E07`).
// Needs: `cargo build` and `cargo build --example ssh_target` in backend/.

import { writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { CATEGORIES, scenarios } from './suite'
import { WORK, killAll, errText, AssertError } from './lib'
import './scenarios/a_protocol'
import './scenarios/b_lifecycle'
import './scenarios/c_auth'
import './scenarios/d_users'
import './scenarios/e_kv'
import './scenarios/f_bastion'
import './scenarios/g_cluster'
import './scenarios/h_audit_cli'
import './scenarios/i_people_audit'
import './scenarios/j_files'
import './scenarios/k_mfa'

const filters = process.argv.slice(2)
const selected = filters.length ? scenarios.filter((s) => filters.some((f) => s.id === f || s.cat === f)) : scenarios
const TIMEOUT = Number(process.env.E2E_TIMEOUT_MS ?? 60000)

type Result = { id: string; cat: string; title: string; ok: boolean; ms: number; error?: string; kind?: 'assert' | 'error' }
const results: Result[] = []

console.log(`timika e2e — ${selected.length}/${scenarios.length} scenarios · work dir ${WORK}\n`)
for (const s of selected) {
  const t0 = performance.now()
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    await Promise.race([
      s.fn(),
      new Promise((_, rej) => (timer = setTimeout(() => rej(new Error(`timed out after ${TIMEOUT} ms`)), TIMEOUT))),
    ])
    results.push({ id: s.id, cat: s.cat, title: s.title, ok: true, ms: performance.now() - t0 })
    console.log(`  ✓ ${s.id}  ${s.title}`)
  } catch (e) {
    const error = errText(e)
    results.push({ id: s.id, cat: s.cat, title: s.title, ok: false, ms: performance.now() - t0, error, kind: e instanceof AssertError ? 'assert' : 'error' })
    console.log(`  ✗ ${s.id}  ${s.title}\n        ${error.split('\n').join('\n        ')}`)
  } finally {
    clearTimeout(timer)
  }
}
killAll()

const failed = results.filter((r) => !r.ok)
const byCat = Object.entries(CATEGORIES).map(([c, name]) => {
  const rs = results.filter((r) => r.cat === c)
  return { c, name, total: rs.length, passed: rs.filter((r) => r.ok).length }
}).filter((x) => x.total)

const md = [
  `# timika e2e report`,
  ``,
  `${results.length - failed.length}/${results.length} passed · ${new Date().toISOString()}`,
  ``,
  `| Category | Passed |`,
  `|---|---|`,
  ...byCat.map((x) => `| ${x.c} · ${x.name} | ${x.passed}/${x.total} |`),
  ``,
  `| ID | Scenario | Result | ms |`,
  `|---|---|---|---|`,
  ...results.map((r) => `| ${r.id} | ${r.title.replace(/\|/g, '\\|')} | ${r.ok ? 'pass' : `**FAIL** — ${(r.error ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ')}`} | ${Math.round(r.ms)} |`),
  ``,
].join('\n')
writeFileSync(join(WORK, 'report.md'), md)
writeFileSync(join(WORK, 'report.json'), JSON.stringify(results, null, 2))
if (!filters.length) writeFileSync(join(import.meta.dir, 'REPORT.md'), md)

console.log(`\n${results.length - failed.length}/${results.length} passed`)
for (const x of byCat) console.log(`  ${x.c} ${x.name.padEnd(22)} ${x.passed}/${x.total}`)
console.log(`report: ${join(WORK, 'report.md')}`)
process.exit(failed.length ? 1 : 0)
