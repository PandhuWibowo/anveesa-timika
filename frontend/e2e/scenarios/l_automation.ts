// Infrastructure automation: Git repositories, project detection, runs on a
// runner server (the SSH target, with real terraform / ansible on this
// machine), plans → apply, state in the vault, webhooks, cancel, audit.
import { createHmac } from 'node:crypto'
import { chmodSync, existsSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { main, uniq } from '../fixtures'
import { freshVault, web, createUser, startSsh, ok, eq, fails, waitFor, auditEntries, WORK, Code, type Clients } from '../lib'
import type { Run } from '../../src/gen/timika/v1/automation_pb'

const SECRET = 's3cret-greeting-value'
const MASK = '••••••'

const TF = `variable "greeting" {
  type = string
}

resource "terraform_data" "hello" {
  input = var.greeting
}

output "said" {
  value = var.greeting
}
`
const PLAYBOOK = `- name: hello
  hosts: all
  gather_facts: false
  tasks:
    - name: say hi
      ansible.builtin.command: echo hi
`
const INVENTORY = 'localhost ansible_connection=local ansible_python_interpreter="{{ ansible_playbook_python }}"\n'

/** A local Git repository (served as file://) with commits we control. */
function gitRepo(files: Record<string, string>) {
  const dir = join(WORK, 'git', uniq('repo'))
  mkdirSync(dir, { recursive: true })
  const env = { PATH: process.env.PATH ?? '', HOME: WORK, GIT_CONFIG_NOSYSTEM: '1', GIT_AUTHOR_NAME: 'E2E', GIT_AUTHOR_EMAIL: 'e2e@example.com', GIT_COMMITTER_NAME: 'E2E', GIT_COMMITTER_EMAIL: 'e2e@example.com' }
  const git = (...args: string[]) => {
    const r = Bun.spawnSync(['git', ...args], { cwd: dir, env })
    if (r.exitCode !== 0) throw new Error(`git ${args.join(' ')}: ${r.stderr}`)
    return r.stdout.toString().trim()
  }
  const commit = (fs: Record<string, string>, msg: string) => {
    for (const [p, text] of Object.entries(fs)) {
      mkdirSync(join(dir, p, '..'), { recursive: true })
      writeFileSync(join(dir, p), text)
    }
    git('add', '-A')
    git('commit', '-q', '-m', msg)
    return git('rev-parse', 'HEAD')
  }
  git('init', '-q', '-b', 'main')
  const first = commit(files, 'initial')
  return { dir, url: `file://${dir}`, first, commit, git }
}

const iac = fixture(async () => {
  const v = await freshVault({ AUTOMATION_ALLOW_FILE_URLS: 'true', AUTOMATION_SCHEDULE_TICK_SECS: '1', PUBLIC_URL: 'https://timika.test' })
  const admin = await createUser(v, 'iac-admin', ['admin'])
  const reader = await createUser(v, 'iac-reader', ['read-only'])
  const sshOnly = await createUser(v, 'iac-ssh', ['ssh'])
  const t = await startSsh({ user: 'runner', password: 'runner-pass', hostKeyName: 'iac_host' })
  const c = web(v.node, admin)
  const a = await c.bastion.createAsset({ name: 'runner-1', host: '127.0.0.1', port: t.port, accounts: [{ username: 'runner', password: 'runner-pass' }], test: true })
  return { v, node: v.node, admin, reader, sshOnly, t, c, runner: a.asset!.id }
})

type Ctx = Awaited<ReturnType<typeof iac>>

/** Connect a fresh copy of the standard repository. */
async function connect(x: Ctx, opts: { files?: Record<string, string>; vars?: { name: string; value: string; secret: boolean }[]; autoPlan?: boolean; tfBin?: string; runner?: string } = {}) {
  const g = gitRepo(opts.files ?? { 'infra/main.tf': TF, 'modules/net/main.tf': 'variable "cidr" {}\n', 'site.yml': PLAYBOOK, 'inventory.ini': INVENTORY, 'README.md': '# infra\n' })
  const repo = await x.c.automation.createRepo({
    url: g.url, auth: 'none', runnerAsset: opts.runner ?? x.runner, runnerAccount: 'runner', autoPlan: opts.autoPlan ?? true, tfBin: opts.tfBin ?? 'auto',
    variables: opts.vars ?? [{ name: 'TF_VAR_greeting', value: SECRET, secret: true }, { name: 'REGION', value: 'eu-west-1', secret: false }],
  })
  return { g, repo }
}

async function finished(c: Clients, id: string, ms = 50000): Promise<Run> {
  return waitFor(`run ${id} to finish`, async () => {
    const r = await c.automation.getRun({ id })
    return ['succeeded', 'failed', 'cancelled', 'lost'].includes(r.status) && r
  }, ms)
}

async function output(c: Clients, id: string) {
  let text = ''
  let last: Run | undefined
  for await (const ev of c.automation.watchRun({ id })) {
    text += ev.output
    if (ev.run) last = ev.run
  }
  return { text, last }
}

const tf = Bun.which('terraform') || Bun.which('tofu')
const ansible = Bun.which('ansible-playbook')

scenario('L', 'connecting a repository clones it, finds the default branch and its Terraform and Ansible projects (not modules)', async () => {
  const x = await iac()
  const { g, repo } = await connect(x)
  eq(repo.branch, 'main', 'default branch')
  eq(repo.head?.sha, g.first, 'head')
  eq(repo.projects.map((p) => p.id), ['ansible:site.yml', 'terraform:infra'], 'projects')
  eq(repo.runnerName, 'runner@runner-1', 'runner')
  ok(repo.name.endsWith(g.dir.split('/').pop()!), `name from URL: ${repo.name}`)
  ok(repo.webhookPath === `/v1/automation/hooks/${repo.id}` && (repo.webhookSecret ?? '').length >= 32, 'webhook path + secret for admins')
  const d = await x.c.automation.getRepo({ id: repo.id })
  eq(d.commits.map((c) => c.message), ['initial'], 'commits')
})

scenario('L', "a repository that can't be read, or a bad URL, is refused and nothing is saved", async () => {
  const x = await iac()
  const before = (await x.c.automation.listRepos({})).repos.length
  await fails(x.c.automation.createRepo({ url: `file://${WORK}/no-such-repo`, auth: 'none', runnerAsset: x.runner, runnerAccount: 'runner' }), Code.InvalidArgument, "can't read the repository")
  await fails(x.c.automation.createRepo({ url: '-oProxyCommand=touch /tmp/x', auth: 'none', runnerAsset: x.runner, runnerAccount: 'runner' }), Code.InvalidArgument, 'Git URL')
  await fails(x.c.automation.createRepo({ url: 'ext::sh -c id', auth: 'none', runnerAsset: x.runner, runnerAccount: 'runner' }), Code.InvalidArgument)
  const g = gitRepo({ 'main.tf': TF })
  await fails(x.c.automation.createRepo({ url: g.url, auth: 'none', runnerAsset: 'nope', runnerAccount: 'runner' }), Code.InvalidArgument, 'runner')
  await fails(x.c.automation.createRepo({ url: g.url, branch: 'does-not-exist', auth: 'none', runnerAsset: x.runner, runnerAccount: 'runner' }), Code.InvalidArgument, 'branch')
  eq((await x.c.automation.listRepos({})).repos.length, before, 'nothing saved')
})

scenario('L', 'local file:// repositories are refused unless explicitly allowed', async () => {
  const m = await main()
  await fails(web(m.node, m.root).automation.createRepo({ url: 'file:///etc', auth: 'none', runnerAsset: 'x', runnerAccount: 'y' }), Code.InvalidArgument, 'file://')
})

scenario('L', 'readers see repositories and runs; only admins connect, run and cancel; the ssh role sees nothing', async () => {
  const x = await iac()
  const { repo } = await connect(x)
  const r = web(x.node, x.reader).automation
  ok((await r.listRepos({})).repos.some((y) => y.id === repo.id), 'reader lists')
  const seen = (await r.getRepo({ id: repo.id })).repo!
  eq(seen.webhookSecret, undefined, 'no webhook secret for readers')
  await r.listRuns({})
  await fails(r.createRepo({ url: 'https://example.com/a.git', auth: 'none', runnerAsset: x.runner, runnerAccount: 'runner' }), Code.PermissionDenied)
  await fails(r.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' }), Code.PermissionDenied)
  await fails(r.syncRepo({ id: repo.id }), Code.PermissionDenied)
  await fails(r.newDeployKey({}), Code.PermissionDenied)
  await fails(web(x.node, x.sshOnly).automation.listRepos({}), Code.PermissionDenied)
})

scenario('L', 'Plan runs on the runner server and reports +1 ~0 -0; secret variables are masked and the run folder is removed', async () => {
  if (!tf) return console.log('    (skipped: terraform / tofu not installed)')
  const x = await iac()
  const { repo } = await connect(x)
  const run = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })
  eq([run.status, run.trigger, run.user, run.runnerName], ['queued', 'manual', 'iac-admin', 'runner@runner-1'], 'queued run')
  const done = await finished(x.c, run.id)
  const { text } = await output(x.c, run.id)
  eq(done.status, 'succeeded', `status (${done.error}) ${text.slice(-600)}`)
  eq([done.summary, done.changes], ['+1 ~0 -0', true], 'summary')
  ok(text.includes('Plan: 1 to add, 0 to change, 0 to destroy.'), 'plan output')
  ok(text.includes('state: kept encrypted in timika'), 'local state notice')
  ok(!text.includes(SECRET) && text.includes(MASK), 'secret masked')
  ok(!existsSync(join(x.t.files, '.timika/runs', run.id)), 'run folder removed')
})

scenario('L', 'Apply applies exactly the reviewed plan, once; state stays in the vault so the next plan shows no changes', async () => {
  if (!tf) return console.log('    (skipped: terraform / tofu not installed)')
  const x = await iac()
  const { repo } = await connect(x)
  const plan = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })).id)
  eq(plan.changes, true, 'plan has changes')
  const apply = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'apply', planRun: plan.id })
  eq([apply.planRun, apply.sha], [plan.id, plan.sha], 'apply of that plan, at its commit')
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'apply', planRun: plan.id }), Code.Aborted, 'already')
  const done = await finished(x.c, apply.id)
  const { text } = await output(x.c, apply.id)
  eq(done.status, 'succeeded', `apply (${done.error}) ${text.slice(-500)}`)
  eq(done.summary, '1 added · 0 changed · 0 destroyed', 'apply summary')
  ok(text.includes('state saved (encrypted) in timika') && !text.includes(SECRET), 'state saved, secret masked')
  eq((await x.c.automation.getRun({ id: plan.id })).appliedBy, apply.id, 'plan marked applied')
  const again = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })).id)
  eq([again.status, again.summary, again.changes], ['succeeded', 'no changes', false], 'state kept → no changes')
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'apply', planRun: again.id }), Code.InvalidArgument, 'no changes')
})

scenario('L', 'Apply needs a plan: none, a failed one or another project’s is refused', async () => {
  const x = await iac()
  const { repo } = await connect(x, { files: { 'a/main.tf': 'this is not HCL {{{', 'b/main.tf': TF } })
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:a', action: 'apply' }), Code.InvalidArgument, 'plan')
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:a', action: 'destroy' }), Code.InvalidArgument, 'plan or apply')
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:zzz', action: 'plan' }), Code.NotFound)
  if (!tf) return
  const bad = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:a', action: 'plan' })).id)
  eq(bad.status, 'failed', 'broken HCL fails')
  ok(bad.error?.includes('exited with code'), `error: ${bad.error}`)
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:a', action: 'apply', planRun: bad.id }), Code.InvalidArgument, 'successful')
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:b', action: 'apply', planRun: bad.id }), Code.InvalidArgument, 'another project')
})

scenario('L', 'Pull picks up new commits and projects; runs use the head at the time', async () => {
  const x = await iac()
  const { g, repo } = await connect(x)
  const sha = g.commit({ 'staging/main.tf': TF }, 'add staging')
  const pulled = await x.c.automation.syncRepo({ id: repo.id })
  eq(pulled.head?.sha, sha, 'new head')
  ok(pulled.projects.some((p) => p.id === 'terraform:staging'), 'new project found')
  eq((await x.c.automation.getRepo({ id: repo.id })).commits.map((c) => c.message), ['add staging', 'initial'], 'commits')
  const run = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:staging', action: 'plan' })
  eq([run.sha, run.commitMessage], [sha, 'add staging'], 'run at head')
  await finished(x.c, run.id)
})

scenario('L', 'push webhooks: bad signatures refused, ping answered, other branches ignored, pushes plan only the projects they touch', async () => {
  const x = await iac()
  const { g, repo } = await connect(x)
  const secret = repo.webhookSecret!
  const url = `${x.node.url}${repo.webhookPath}`
  const post = (body: object, headers: Record<string, string>) => fetch(url, { method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body: JSON.stringify(body) })
  const sign = (body: object) => `sha256=${createHmac('sha256', secret).update(JSON.stringify(body)).digest('hex')}`

  eq((await post({ zen: 'hi' }, { 'x-github-event': 'ping', 'x-hub-signature-256': 'sha256=00' })).status, 401, 'bad signature')
  eq((await post({ zen: 'hi' }, { 'x-github-event': 'push' })).status, 401, 'unsigned')
  eq((await post({ zen: 'hi' }, { 'x-github-event': 'ping', 'x-hub-signature-256': sign({ zen: 'hi' }) })).status, 200, 'ping')
  eq((await post({ ref: 'refs/heads/other' }, { 'x-gitlab-event': 'Push Hook', 'x-gitlab-token': secret })).status, 202, 'gitlab token, other branch')
  eq((await fetch(`${x.node.url}/v1/automation/hooks/nope`, { method: 'POST', body: '{}' })).status, 404, 'unknown repository')

  const sha = g.commit({ 'infra/main.tf': TF + '\n# tweak\n' }, 'tweak infra')
  const push = { ref: 'refs/heads/main', after: sha, pusher: { name: 'octocat' }, commits: [{ added: [], modified: ['infra/main.tf'], removed: [] }] }
  eq((await post(push, { 'x-github-event': 'push', 'x-hub-signature-256': sign(push) })).status, 202, 'push accepted')
  const run = await waitFor('a plan started by the push', async () => (await x.c.automation.listRuns({ repo: repo.id })).runs.find((r) => r.trigger === 'push'))
  eq([run.project, run.action, run.user, run.sha], ['terraform:infra', 'plan', 'octocat', sha], 'push plan')
  const r = await waitFor('delivery recorded', async () => (await x.c.automation.getRepo({ id: repo.id })).repo?.lastHook)
  ok(r.result.includes('plan infra') && r.by === 'octocat', `delivery: ${r.result}`)
  eq((await x.c.automation.listRuns({ repo: repo.id })).runs.filter((y) => y.trigger === 'push').length, 1, 'ansible not touched → not run')
  await finished(x.c, run.id)
})

scenario('L', 'Ansible: check runs the playbook in check mode, run applies it', async () => {
  if (!ansible) return console.log('    (skipped: ansible-playbook not installed)')
  const x = await iac()
  const { repo } = await connect(x)
  const check = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'ansible:site.yml', action: 'check' })).id, 90000)
  const { text } = await output(x.c, check.id)
  eq(check.status, 'succeeded', `check (${check.error}) ${text.slice(-600)}`)
  ok(text.includes('--check --diff') && text.includes('PLAY RECAP'), 'check mode')
  eq(check.summary, '1 host · ok 0 · changed 0', 'command skipped in check mode')
  const run = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'ansible:site.yml', action: 'run' })).id, 90000)
  eq([run.status, run.summary, run.changes], ['succeeded', '1 host · ok 1 · changed 1', true], 'run')
})

scenario('L', 'a running run can be cancelled from any page; a second run of the same project waits its turn', async () => {
  const x = await iac()
  const bin = join(WORK, uniq('fakebin'))
  mkdirSync(bin, { recursive: true })
  writeFileSync(join(bin, 'terraform'), '#!/bin/sh\necho "fake terraform $*"\n[ "$1" = init ] && exit 0\necho waiting\nsleep 60\necho finished > /dev/null\n')
  chmodSync(join(bin, 'terraform'), 0o755)
  const { repo } = await connect(x, { tfBin: 'terraform', vars: [{ name: 'PATH', value: `${bin}:${process.env.PATH}`, secret: false }] })
  const run = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })
  await waitFor('the plan to be waiting', async () => {
    const r = await x.c.automation.getRun({ id: run.id })
    return r.status === 'running' && (await output2(x.c, run.id)).includes('waiting')
  }, 20000)
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' }), Code.Aborted, 'plan running')
  await web(x.node, x.reader).automation.cancelRun({ id: run.id }).catch(() => {}) // readers can't
  eq((await x.c.automation.getRun({ id: run.id })).status, 'running', 'reader could not cancel')
  await x.c.automation.cancelRun({ id: run.id })
  const done = await finished(x.c, run.id, 25000)
  eq(done.status, 'cancelled', 'cancelled')
  ok(!existsSync(join(x.t.files, '.timika/runs', run.id)), 'run folder removed')
})

/** The output stored so far (without waiting for the end). */
async function output2(c: Clients, id: string) {
  let text = ''
  const ctl = new AbortController()
  setTimeout(() => ctl.abort(), 800)
  try { for await (const ev of c.automation.watchRun({ id }, { signal: ctl.signal })) text += ev.output } catch { /* aborted */ }
  return text
}

scenario('L', 'an unreachable runner fails the run with a clear error', async () => {
  const x = await iac()
  const dead = await x.c.bastion.createAsset({ name: uniq('dead'), host: '127.0.0.1', port: 9, accounts: [{ username: 'runner', password: 'x' }], test: false })
  const { repo } = await connect(x, { runner: dead.asset!.id })
  const done = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })).id)
  eq(done.status, 'failed', 'failed')
  ok(/connect/.test(done.error ?? ''), `error: ${done.error}`)
})

scenario('L', 'watching a run streams all of its output and ends with the final status', async () => {
  if (!tf) return
  const x = await iac()
  const { repo } = await connect(x)
  const run = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })
  const live = await output(x.c, run.id) // started right away: follows it live
  ok(live.last && ['succeeded', 'failed'].includes(live.last.status), `final status: ${live.last?.status}`)
  const again = await output(x.c, run.id) // after the end: the same output
  eq(again.text, live.text, 'same output')
  ok(live.text.startsWith('▶ terraform plan · infra') && /✓ succeeded in \d+s/.test(live.text), 'header and footer')
})

scenario('L', 'secret variables are never returned; saving without a value keeps them; bad names are refused', async () => {
  const x = await iac()
  const { repo } = await connect(x)
  const vars = (await x.c.automation.getRepo({ id: repo.id })).repo!.variables
  eq(vars.map((v) => [v.name, v.value, v.secret]), [['TF_VAR_greeting', '', true], ['REGION', 'eu-west-1', false]], 'secret value hidden')
  const input = { url: repo.url, auth: 'none', runnerAsset: x.runner, runnerAccount: 'runner', autoPlan: true, tfBin: 'auto' }
  await x.c.automation.updateRepo({ id: repo.id, repo: { ...input, variables: [{ name: 'TF_VAR_greeting', value: '', secret: true }] } })
  if (tf) {
    const { text } = await output(x.c, (await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })).id)).id)
    ok(text.includes(MASK) && !text.includes(SECRET), 'kept value still used and masked')
  }
  await fails(x.c.automation.updateRepo({ id: repo.id, repo: { ...input, variables: [{ name: '1BAD', value: 'x', secret: false }] } }), Code.InvalidArgument, 'not a valid variable name')
  await fails(x.c.automation.updateRepo({ id: repo.id, repo: { ...input, variables: [{ name: 'NEW_SECRET', value: '', secret: true }] } }), Code.InvalidArgument, 'needs a value')
})

scenario('L', 'deploy keys: a generated key is kept with the repository; unknown keys are refused', async () => {
  const x = await iac()
  const k = await x.c.automation.newDeployKey({})
  ok(k.publicKey.startsWith('ssh-ed25519 ') && k.id, 'ed25519 public key')
  const g = gitRepo({ 'main.tf': TF })
  await fails(x.c.automation.createRepo({ url: g.url, auth: 'deploy_key', deployKeyId: 'nope', runnerAsset: x.runner, runnerAccount: 'runner' }), Code.InvalidArgument, 'expired')
  const repo = await x.c.automation.createRepo({ url: g.url, auth: 'deploy_key', deployKeyId: k.id, runnerAsset: x.runner, runnerAccount: 'runner' })
  eq([repo.auth, repo.deployPublicKey], ['deploy_key', k.publicKey], 'stored')
  await fails(x.c.automation.createRepo({ url: g.url, auth: 'deploy_key', deployKeyId: k.id, runnerAsset: x.runner, runnerAccount: 'runner' }), Code.InvalidArgument, 'expired')
})

scenario('L', 'the runner check lists the tools installed on the runner server', async () => {
  const x = await iac()
  const r = await x.c.automation.checkRunner({ asset: x.runner, account: 'runner' })
  ok(r.ok, r.error)
  if (tf) ok(r.tools.some((t) => t.name === 'terraform' || t.name === 'tofu'), `tools: ${r.tools.map((t) => t.name)}`)
  await fails(x.c.automation.checkRunner({ asset: x.runner, account: 'nobody' }), Code.InvalidArgument, 'no account')
})

scenario('L', 'the audit log records repository changes, runs and webhooks — never secret values', async () => {
  const x = await iac()
  const { repo } = await connect(x)
  const run = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })
  await finished(x.c, run.id)
  await fetch(`${x.node.url}${repo.webhookPath}`, { method: 'POST', body: '{}' })
  const all = auditEntries(x.node)
  const raw = JSON.stringify(all)
  ok(!raw.includes(SECRET) && !raw.includes(repo.webhookSecret!), 'no secrets in the audit log')
  const started = all.find((e) => e.type === 'response' && e.request?.rpc === 'timika.v1.AutomationService/StartRun' && e.target?.includes(run.id))
  ok(started && started.auth.display_name === 'iac-admin', 'StartRun with the run id and who')
  ok(all.some((e) => e.type === 'response' && e.request?.rpc === 'timika.v1.AutomationService/CreateRepo' && e.target?.includes(repo.url)), 'CreateRepo')
  ok(all.some((e) => e.type === 'response' && e.request?.path === repo.webhookPath && e.response?.status === 401), 'refused webhook')
  const ev = await x.c.audit.listEvents({ category: 'automation', limit: 50 })
  ok(ev.events.some((e) => e.action === 'AutomationService/StartRun'), 'automation category')
})

scenario('L', 'removing a repository deletes its runs, output, saved plans and state', async () => {
  const x = await iac()
  const { repo } = await connect(x)
  const run = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })).id)
  await x.c.automation.deleteRepo({ id: repo.id })
  await fails(x.c.automation.getRepo({ id: repo.id }), Code.NotFound)
  await fails(x.c.automation.getRun({ id: run.id }), Code.NotFound)
  ok(!(await x.c.automation.listRuns({})).runs.some((r) => r.repo === repo.id), 'runs gone')
  const left = readdirSync(join(x.node.dir, 'data/automation')).filter((f) => f.startsWith(repo.id))
  eq(left, [], 'local clone removed')
})

// ─── learned from Semaphore: options, approvals, schedules, notifications, CI ──

/** A local HTTP endpoint that records what it receives (Slack / webhook stand-in). */
function inbox() {
  const got: { path: string; body: any }[] = []
  const srv = Bun.serve({ port: 0, async fetch(req) { got.push({ path: new URL(req.url).pathname, body: await req.json().catch(() => null) }); return new Response('ok') } })
  return { url: `http://127.0.0.1:${srv.port}`, got, stop: () => srv.stop(true) }
}

async function secondAdmin(x: Ctx) {
  const name = uniq('approver')
  return web(x.node, await createUser(x.v, name, ['admin'])).automation
}

scenario('L', 'run options: Terraform workspaces keep separate state; a destroy plan removes what apply created', async () => {
  if (!tf) return console.log('    (skipped: terraform / tofu not installed)')
  const x = await iac()
  const { repo } = await connect(x)
  const p = 'terraform:infra'
  const plan = async (options: object) => finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: p, action: 'plan', options })).id)
  const apply = async (pl: Run) => finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: p, action: 'apply', planRun: pl.id })).id)
  const ws = await plan({ workspace: 'staging' })
  eq([ws.status, ws.summary, ws.optionsText], ['succeeded', '+1 ~0 -0', 'workspace staging'], `staging plan (${ws.error})`)
  const wsApply = await apply(ws)
  eq([wsApply.status, wsApply.optionsText], ['succeeded', 'workspace staging'], 'apply keeps the plan’s workspace')
  eq((await plan({ workspace: 'staging' })).summary, 'no changes', 'staging state kept')
  eq((await plan({})).summary, '+1 ~0 -0', 'default workspace has its own state')
  const d = await plan({ workspace: 'staging', destroy: true })
  eq([d.summary, d.optionsText], ['+0 ~0 -1', 'destroy · workspace staging'], 'destroy plan')
  eq((await apply(d)).summary, '0 added · 0 changed · 1 destroyed', 'destroyed')
  await fails(x.c.automation.startRun({ repo: repo.id, project: p, action: 'plan', options: { workspace: 'a b' } }), Code.InvalidArgument, 'workspace')
})

scenario('L', 'run options: Ansible against timika’s servers by tag — host keys checked against the pins, passwords masked', async () => {
  if (!ansible) return console.log('    (skipped: ansible-playbook not installed)')
  const x = await iac()
  const tag = uniq('web').replace(/-/g, '')
  const pinned = await x.c.bastion.createAsset({ name: uniq('web-a'), host: '127.0.0.1', port: x.t.port, tags: [tag], accounts: [{ username: 'runner', password: 'runner-pass' }], test: true })
  const loose = await x.c.bastion.createAsset({ name: uniq('web-b'), host: '127.0.0.1', port: x.t.port, tags: [tag], accounts: [{ username: 'runner', password: 'runner-pass' }], test: false })
  const play = `- hosts: all\n  connection: local\n  gather_facts: false\n  tasks:\n    - ansible.builtin.debug: var=ansible_password\n`
  const { repo } = await connect(x, { files: { 'hosts.yml': play } })
  const run = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'ansible:hosts.yml', action: 'run', options: { servers: tag, limit: tag, extraVars: 'release=1.2' } })).id, 90000)
  const { text } = await output(x.c, run.id)
  eq(run.status, 'succeeded', `run (${run.error}) ${text.slice(-800)}`)
  ok(text.includes('hosts: 2 servers from timika'), 'inventory from timika')
  ok(text.includes(`✓ ${pinned.asset!.name}`) && text.includes(`✗ ${loose.asset!.name}: no pinned host key`), `host keys: ${text.slice(0, 900)}`)
  ok(text.includes(`--limit '${tag}'`) && text.includes('-e @extra.json'), 'options on the command line')
  ok(!text.includes('runner-pass') && text.includes(MASK), 'server passwords masked')
  eq(run.summary, '2 hosts · ok 2 · changed 0', 'both hosts')
  const none = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'ansible:hosts.yml', action: 'run', options: { servers: 'no-such-tag' } })).id)
  ok(none.status === 'failed' && none.error?.includes('no servers in timika match'), `no servers: ${none.error}`)
})

scenario('L', 'approvals: Apply / Run wait for another admin; the requester can’t approve; rejecting frees the plan', async () => {
  const x = await iac()
  const { repo } = await connect(x)
  const other = await secondAdmin(x)
  await x.c.automation.setRepoPolicy({ id: repo.id, requireApproval: true })
  // Safe actions don't wait.
  const check = await x.c.automation.startRun({ repo: repo.id, project: 'ansible:site.yml', action: 'check' })
  eq(check.status, 'queued', 'check runs')
  await finished(x.c, check.id, 90000)
  const run = await x.c.automation.startRun({ repo: repo.id, project: 'ansible:site.yml', action: 'run' })
  eq(run.status, 'approval', 'run parked')
  await fails(x.c.automation.startRun({ repo: repo.id, project: 'ansible:site.yml', action: 'check' }), Code.Aborted, 'waiting for approval')
  await fails(x.c.automation.approveRun({ id: run.id }), Code.PermissionDenied, 'someone else')
  await fails(web(x.node, x.reader).automation.approveRun({ id: run.id }), Code.PermissionDenied)
  const approved = await other.approveRun({ id: run.id })
  ok(approved.approvedBy && approved.status === 'queued', 'approved')
  await fails(other.approveRun({ id: run.id }), Code.InvalidArgument, 'not waiting')
  await finished(x.c, run.id, 90000)
  if (!tf) return
  const plan = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'plan' })).id)
  const apply = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'apply', planRun: plan.id })
  eq(apply.status, 'approval', 'apply parked')
  const rejected = await other.rejectRun({ id: apply.id })
  ok(rejected.status === 'cancelled' && rejected.error?.startsWith('rejected by'), 'rejected')
  eq((await x.c.automation.getRun({ id: plan.id })).appliedBy, undefined, 'plan free again')
  const again = await x.c.automation.startRun({ repo: repo.id, project: 'terraform:infra', action: 'apply', planRun: plan.id })
  eq((await finished(x.c, (await other.approveRun({ id: again.id })).id)).status, 'succeeded', 'approved apply ran')
})

scenario('L', 'schedules: validated, fire on time exactly once with trigger "schedule", report drift, pause and run now', async () => {
  const x = await iac()
  const { repo } = await connect(x)
  const box = inbox()
  await x.c.automation.saveNotifiers({ repo: repo.id, notifiers: [{ kind: 'webhook', url: `${box.url}/drift`, on: ['drift'] }] })
  const base = { repo: repo.id, project: 'terraform:infra', action: 'plan', timezone: '+07:00', enabled: true }
  await fails(x.c.automation.saveSchedule({ ...base, action: 'apply', cron: '* * * * *' }), Code.InvalidArgument, 'reviewed plan')
  await fails(x.c.automation.saveSchedule({ ...base, cron: '61 * * * *' }), Code.InvalidArgument, 'out of range')
  await fails(x.c.automation.saveSchedule({ ...base, cron: '* * * * *', timezone: 'Mars/Olympus' }), Code.InvalidArgument, 'time zone')
  await fails(web(x.node, x.reader).automation.saveSchedule({ ...base, cron: '* * * * *' }), Code.PermissionDenied)
  const s = await x.c.automation.saveSchedule({ ...base, cron: '* * * * *', name: 'drift check' })
  ok(s.nextRun && new Date(s.nextRun).getTime() - Date.now() <= 61000, `next run within a minute: ${s.nextRun}`)
  const run = await waitFor('the scheduled run', async () => (await x.c.automation.listRuns({ repo: repo.id })).runs.find((r) => r.trigger === 'schedule'), 75000)
  eq([run.project, run.action, run.user], ['terraform:infra', 'plan', 'schedule “drift check”'], 'scheduled run')
  await x.c.automation.saveSchedule({ ...base, id: s.id, cron: '* * * * *', name: 'drift check', enabled: false })
  const done = await finished(x.c, run.id)
  if (tf) {
    eq(done.changes, true, 'drift found')
    const n = await waitFor('drift notification', async () => box.got.find((g) => g.body?.event === 'drift'), 10000)
    ok(n.body.text.includes('found changes: +1 ~0 -0') && n.body.run.url === `https://timika.test/#/automation/run/${run.id}`, `drift: ${JSON.stringify(n.body)}`)
  }
  const listed = (await x.c.automation.listSchedules({ repo: repo.id })).schedules[0]
  ok(!listed.enabled && !listed.nextRun && listed.lastRun === run.id, 'paused, last run recorded')
  const now = await x.c.automation.runScheduleNow({ id: s.id })
  eq(now.trigger, 'schedule', 'run now')
  await finished(x.c, now.id)
  eq((await x.c.automation.listRuns({ repo: repo.id })).runs.filter((r) => r.trigger === 'schedule').length, 2, 'fired once + run now')
  await x.c.automation.deleteSchedule({ id: s.id })
  eq((await x.c.automation.listSchedules({ repo: repo.id })).schedules.length, 0, 'deleted')
  box.stop()
}, { timeout: 150000 })

scenario('L', 'notifications: targets validated and never returned; test message; failed runs and approvals are announced', async () => {
  const x = await iac()
  const { repo } = await connect(x, { files: { 'bad/main.tf': 'not HCL {{{', 'site.yml': PLAYBOOK, 'inventory.ini': INVENTORY } })
  const box = inbox()
  await fails(x.c.automation.saveNotifiers({ repo: repo.id, notifiers: [{ kind: 'slack', url: 'hooks.slack.com/x', on: ['failed'] }] }), Code.InvalidArgument, 'https://')
  await fails(x.c.automation.saveNotifiers({ repo: repo.id, notifiers: [{ kind: 'telegram', url: '123:abc', on: ['failed'] }] }), Code.InvalidArgument, 'Telegram')
  await fails(x.c.automation.saveNotifiers({ repo: repo.id, notifiers: [{ kind: 'slack', url: `${box.url}/s`, on: [] }] }), Code.InvalidArgument, 'when')
  const saved = await x.c.automation.saveNotifiers({ repo: repo.id, notifiers: [
    { kind: 'slack', url: `${box.url}/slack-secret-path`, on: ['failed', 'approval'] },
    { kind: 'webhook', url: `${box.url}/hook`, on: ['failed'] },
  ] })
  eq(saved.notifiers.map((n) => n.kind), ['slack', 'webhook'], 'saved')
  ok(!JSON.stringify(saved).includes('slack-secret-path') && saved.notifiers[0].label.startsWith('127.0.0.1:'), 'only a label comes back')
  const t = await x.c.automation.testNotifier({ repo: repo.id, notifier: { id: saved.notifiers[0].id, kind: 'slack', on: ['failed'] } })
  ok(t.ok, t.error)
  ok(box.got.some((g) => g.path === '/slack-secret-path' && g.body.text.includes('test notification')), 'test arrived')
  // Keep both when saving again without URLs.
  await x.c.automation.saveNotifiers({ repo: repo.id, notifiers: saved.notifiers.map((n) => ({ id: n.id, kind: n.kind, on: n.on })) })
  if (tf) {
    const bad = await finished(x.c, (await x.c.automation.startRun({ repo: repo.id, project: 'terraform:bad', action: 'plan' })).id)
    const f = await waitFor('failure notification', async () => box.got.find((g) => g.path === '/hook' && g.body.event === 'failed'), 10000)
    eq([f.body.run.id, f.body.run.status], [bad.id, 'failed'], 'webhook payload')
    ok(box.got.some((g) => g.path === '/slack-secret-path' && g.body.text.startsWith('✗ ') && g.body.text.includes('Plan bad failed')), 'slack text')
  }
  await x.c.automation.setRepoPolicy({ id: repo.id, requireApproval: true })
  const parked = await x.c.automation.startRun({ repo: repo.id, project: 'ansible:site.yml', action: 'run' })
  await waitFor('approval notification', async () => box.got.find((g) => g.body?.text?.includes('waits for approval')), 10000)
  await x.c.automation.rejectRun({ id: parked.id })
  box.stop()
})

scenario('L', 'CI trigger: bearer token, project by name, wait for the result (200 / 409), rotate', async () => {
  const x = await iac()
  const { repo } = await connect(x, { files: { 'infra/main.tf': TF, 'bad/main.tf': 'not HCL {{{' } })
  const token = repo.triggerToken!
  ok(token.startsWith('tmkci.'), 'token for admins')
  eq((await web(x.node, x.reader).automation.getRepo({ id: repo.id })).repo?.triggerToken, undefined, 'hidden from readers')
  const call = (body: object, tok = token) => fetch(`${x.node.url}${repo.triggerPath}`, { method: 'POST', headers: { authorization: `Bearer ${tok}`, 'content-type': 'application/json' }, body: JSON.stringify(body) })
  eq((await call({ project: 'infra' }, 'tmkci.wrong')).status, 401, 'bad token')
  eq((await fetch(`${x.node.url}${repo.triggerPath}`, { method: 'POST', body: '{}' })).status, 401, 'no token')
  eq((await call({ project: 'nope' })).status, 404, 'unknown project')
  const quick = await call({ project: 'infra', action: 'plan', by: 'github-actions' })
  eq(quick.status, 202, 'started')
  const q = (await quick.json()).run
  const r = await finished(x.c, q.id)
  eq([r.trigger, r.user], ['ci', 'github-actions'], 'ci run')
  if (tf) {
    const ok200 = await call({ project: 'terraform:infra', action: 'apply', plan: 'latest', wait: true })
    const body = await ok200.json()
    eq([ok200.status, body.run.status, body.run.summary], [200, 'succeeded', '1 added · 0 changed · 0 destroyed'], 'apply latest plan, waited')
    const bad = await call({ project: 'bad', wait: true })
    eq([bad.status, (await bad.json()).run.status], [409, 'failed'], 'failed → 409')
  }
  const rotated = await x.c.automation.rotateTriggerToken({ id: repo.id })
  ok(rotated.triggerToken && rotated.triggerToken !== token, 'new token')
  eq((await call({ project: 'infra' })).status, 401, 'old token refused')
})

scenario('L', 'the file viewer lists folders and reads files at the pulled commit; paths, binaries and big files are handled; admins only', async () => {
  const x = await iac()
  const big = 'x'.repeat(1024 * 1024 + 10)
  const { g, repo } = await connect(x, { files: { 'envs/prod/main.tf': TF, 'envs/prod/vars.tf': '# vars\n', 'README.md': '# hi\n', 'docs/big.txt': big, 'empty.txt': '' } })
  const a = x.c.automation
  const root = await a.listRepoFiles({ repo: repo.id })
  eq([root.kind, root.sha], ['dir', g.first], 'root at head')
  eq(root.entries.map((e) => [e.name, e.kind]), [['docs', 'dir'], ['envs', 'dir'], ['empty.txt', 'file'], ['README.md', 'file']].sort((p, q) => (p[1] === 'dir' ? 0 : 1) - (q[1] === 'dir' ? 0 : 1) || p[0].toLowerCase().localeCompare(q[0].toLowerCase())), 'folders first, then by name')
  eq((await a.listRepoFiles({ repo: repo.id, path: '/envs/prod/' })).entries.map((e) => e.name), ['main.tf', 'vars.tf'], 'nested folder (slashes trimmed)')
  const kind = await a.listRepoFiles({ repo: repo.id, path: 'envs/prod/main.tf' })
  eq(kind.kind, 'file', 'a file is reported as one')
  const f = await a.readRepoFile({ repo: repo.id, path: 'envs/prod/main.tf' })
  ok(f.content === TF && !f.binary && !f.tooLarge && f.size === BigInt(TF.length), 'content')
  eq((await a.readRepoFile({ repo: repo.id, path: 'empty.txt' })).content, '', 'empty file')
  const b = await a.readRepoFile({ repo: repo.id, path: 'docs/big.txt' })
  ok(b.tooLarge && b.content === '', 'over 1 MB: size only')
  // Binary: commit one and pull.
  const png = join(g.dir, 'logo.bin')
  writeFileSync(png, Buffer.from([0x89, 0x50, 0x00, 0x01, 0x02]))
  g.git('add', '-A'); g.git('commit', '-q', '-m', 'binary')
  await a.syncRepo({ id: repo.id })
  const bin = await a.readRepoFile({ repo: repo.id, path: 'logo.bin' })
  ok(bin.binary && bin.content === '', 'binary detected')
  // An older commit stays readable by its id.
  const old = await a.readRepoFile({ repo: repo.id, path: 'README.md', sha: g.first })
  eq([old.content, old.sha], ['# hi\n', g.first], 'older commit')
  await fails(a.readRepoFile({ repo: repo.id, path: 'logo.bin', sha: g.first }), Code.InvalidArgument, 'not in this commit')
  for (const bad of ['../etc/passwd', 'envs/../../x', '-rf', 'a//b']) await fails(a.listRepoFiles({ repo: repo.id, path: bad }), Code.InvalidArgument, 'invalid path')
  await fails(a.readRepoFile({ repo: repo.id, path: 'envs' }), Code.InvalidArgument, 'folder')
  await fails(a.readRepoFile({ repo: repo.id, path: 'nope.txt' }), Code.InvalidArgument, 'not in this commit')
  await fails(a.readRepoFile({ repo: repo.id, path: 'README.md', sha: 'zz' }), Code.InvalidArgument, 'invalid commit')
  await fails(web(x.node, x.reader).automation.listRepoFiles({ repo: repo.id }), Code.PermissionDenied)
  await fails(web(x.node, x.reader).automation.readRepoFile({ repo: repo.id, path: 'README.md' }), Code.PermissionDenied)
  const raw = JSON.stringify(auditEntries(x.node))
  ok(raw.includes('read /README.md at') && !raw.includes('# hi'), 'reads are audited by path, never content')
})
