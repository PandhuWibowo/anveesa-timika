<script lang="ts">
  // One repository: its projects (with Plan / Apply etc.), runs, commits and setup.
  import { createQuery } from '@tanstack/svelte-query'
  import { keys, queryClient } from '../lib/query'
  import { FileCode, ArrowLeft, RefreshCw, Pencil, Loader2, ExternalLink, Copy, Check, Eye, EyeOff, Trash2, CircleAlert, KeyRound, Globe, Lock, Server, Webhook, Play, ShieldCheck, SlidersHorizontal, CalendarClock, Plus, UserCheck, Rocket, Hourglass } from '@lucide/svelte'
  import { automation, errMsg } from '../lib/api'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { confirm } from '../lib/ui.svelte'
  import { KINDS, ACTIONS, ACTION_LABEL, STATUS_BADGE, STATUS_TEXT, kindLabel, ago, short, repoWeb, webhookPage, latestByProject, applicablePlan, isActive, describeCron } from '../lib/automation'
  import type { Repo, Run, Commit, Project, Schedule } from '../gen/timika/v1/automation_pb'
  import RepoDrawer from '../components/RepoDrawer.svelte'
  import RunsTable from '../components/RunsTable.svelte'
  import RunDialog from '../components/RunDialog.svelte'
  import ScheduleDialog from '../components/ScheduleDialog.svelte'
  import NotifySettings from '../components/NotifySettings.svelte'
  import RepoFiles from '../components/RepoFiles.svelte'

  let { id, tab: initialTab = '', path = '' }: { id: string; tab?: string; path?: string } = $props()
  type Tab = 'projects' | 'runs' | 'schedules' | 'files' | 'commits' | 'setup'
  // svelte-ignore state_referenced_locally
  let tab = $state<Tab>((['projects', 'runs', 'schedules', 'files', 'commits', 'setup'] as Tab[]).includes(initialTab as Tab) ? (initialTab as Tab) : 'projects')

  // A link to /files/… (the code buttons) switches to that tab.
  $effect(() => { if (initialTab === 'files') tab = 'files' })

  const repoQ = createQuery(() => ({ queryKey: keys.repo(id), queryFn: () => automation.getRepo({ id }), refetchInterval: 4000 }))
  const runsQ = createQuery(() => ({ queryKey: keys.runs(id), queryFn: () => automation.listRuns({ repo: id, limit: 100 }), refetchInterval: 4000 }))
  const schedQ = createQuery(() => ({ queryKey: keys.schedules(id), queryFn: () => automation.listSchedules({ repo: id }), refetchInterval: 15000 }))
  const repo = $derived<Repo | null>(repoQ.data?.repo ?? null)
  const commits = $derived<Commit[]>(repoQ.data?.commits ?? [])
  const runs = $derived<Run[]>(runsQ.data?.runs ?? [])
  const schedules = $derived<Schedule[]>(schedQ.data?.schedules ?? [])
  let actionError = $state('')
  const error = $derived(actionError || (repoQ.error && !repoQ.data ? errMsg(repoQ.error) : ''))
  const load = () => { repoQ.refetch(); runsQ.refetch(); schedQ.refetch() }
  /** Put a repository an action returned straight into the cache. */
  const setRepo = (r: Repo) => queryClient.setQueryData(keys.repo(id), (old: typeof repoQ.data) => (old ? { ...old, repo: r } : old))
  let pulling = $state(false)
  let starting = $state('')
  let drawer = $state(false)
  let showSecret = $state(false)
  let copied = $state('')
  let runDialog = $state<Project | null>(null)
  let schedDialog = $state<{ open: boolean; s: Schedule | null }>({ open: false, s: null })
  let showToken = $state(false)
  const canManage = isAdmin()

  async function pull() {
    pulling = true
    try { setRepo(await automation.syncRepo({ id })); load(); actionError = '' } catch (e) { actionError = errMsg(e) } finally { pulling = false }
  }

  async function start(p: Project, action: string, plan?: Run) {
    const changing = action === ACTIONS[p.kind]?.[1]
    if (changing) {
      const ok = await confirm(
        plan
          ? { title: `Apply this plan to ${p.name}?`, message: `${plan.summary ?? ''} — exactly the changes reviewed in plan ${plan.id.slice(0, 8)} (commit ${short(plan.sha)}).`, confirmText: 'Apply', variant: 'danger' }
          : { title: `${ACTION_LABEL[action]} ${p.name}?`, message: `This makes real changes${p.kind === 'ansible' ? ' on the hosts in the inventory' : ''}, from commit ${short(repo?.head?.sha ?? '')}. Run ${ACTION_LABEL[ACTIONS[p.kind][0]]} first to see what would change.`, confirmText: ACTION_LABEL[action], variant: 'danger' },
      )
      if (!ok) return
    }
    starting = `${p.id}:${action}`
    try {
      const r = await automation.startRun({ repo: id, project: p.id, action, planRun: plan?.id })
      navigate(`/automation/run/${r.id}`)
    } catch (e) { actionError = errMsg(e) } finally { starting = '' }
  }

  async function remove() {
    if (!repo) return
    const ok = await confirm({ title: `Remove ${repo.name}?`, message: 'Its runs, output and saved plans are deleted from timika. Nothing changes in the repository or your infrastructure.', confirmText: 'Remove', variant: 'danger' })
    if (!ok) return
    try { await automation.deleteRepo({ id }); queryClient.invalidateQueries({ queryKey: keys.repos }); navigate('/automation') } catch (e) { actionError = errMsg(e) }
  }

  async function setApproval(on: boolean) {
    try { setRepo(await automation.setRepoPolicy({ id, requireApproval: on })) } catch (e) { actionError = errMsg(e) }
  }

  async function rotateToken() {
    const ok = await confirm({ title: 'New CI token?', message: 'The current token stops working — update it in your CI secrets.', confirmText: 'Rotate', variant: 'warning' })
    if (!ok) return
    try { setRepo(await automation.rotateTriggerToken({ id })); showToken = true } catch (e) { actionError = errMsg(e) }
  }

  async function toggleSchedule(sc: Schedule) {
    try {
      await automation.saveSchedule({ id: sc.id, repo: sc.repo, project: sc.project, action: sc.action, name: sc.name, cron: sc.cron, timezone: sc.timezone, enabled: !sc.enabled, options: sc.options })
      load()
    } catch (e) { actionError = errMsg(e) }
  }

  async function runNow(sc: Schedule) {
    try { const r = await automation.runScheduleNow({ id: sc.id }); navigate(`/automation/run/${r.id}`) } catch (e) { actionError = errMsg(e) }
  }

  async function removeSchedule(sc: Schedule) {
    const ok = await confirm({ title: `Delete “${sc.name}”?`, message: 'Runs it already started are kept.', confirmText: 'Delete', variant: 'danger' })
    if (!ok) return
    try { await automation.deleteSchedule({ id: sc.id }); load() } catch (e) { actionError = errMsg(e) }
  }

  function copy(text: string, tag: string) {
    navigator.clipboard.writeText(text)
    copied = tag
    setTimeout(() => { if (copied === tag) copied = '' }, 1400)
  }

  const latest = $derived(latestByProject(runs))
  const web = $derived(repo ? repoWeb(repo.url) : { host: '', path: '' })
  const hookUrl = $derived(repo ? `${location.origin}${repo.webhookPath}` : '')
  const triggerUrl = $derived(repo ? `${location.origin}${repo.triggerPath}` : '')
  const firstProject = $derived(repo?.projects.find((p) => p.kind === 'terraform') ?? repo?.projects[0])
  const ciSnippet = $derived(repo ? `- name: Plan with timika
  run: |
    curl --fail -sS -X POST "${triggerUrl}" \\
      -H "Authorization: Bearer \${{ secrets.TIMIKA_TOKEN }}" \\
      -d '{"project": "${firstProject?.name ?? 'envs/prod'}", "action": "${firstProject ? ACTIONS[firstProject.kind]?.[0] : 'plan'}", "wait": true, "by": "github-actions"}'` : '')
  const commitUrl = (sha: string) => (web.web ? (web.host.includes('gitlab') ? `${web.web}/-/commit/${sha}` : `${web.web}/commit/${sha}`) : undefined)
  const AUTH: Record<string, { label: string; ico: typeof Globe }> = { none: { label: 'Public', ico: Globe }, token: { label: 'Token', ico: Lock }, deploy_key: { label: 'Deploy key', ico: KeyRound } }
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <div><button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate('/automation')}><ArrowLeft size={13} /> Infrastructure</button></div>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if !repo}
        {#if !error}<div class="empty-state"><Loader2 size={20} class="spin" /></div>{/if}
      {:else}
        {@const A = AUTH[repo.auth] ?? AUTH.none}
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">Repository</div>
            <h1 class="page-title">{repo.name}</h1>
            <div class="rp-meta">
              {#if web.web}<a class="mono rp-link" href={web.web} target="_blank" rel="noreferrer">{web.host}/{web.path} <ExternalLink size={11} /></a>{:else}<span class="mono rp-link">{repo.url}</span>{/if}
              <span class="badge badge--default">{repo.branch}</span>
              <span class="badge badge--default"><A.ico size={11} /> {A.label}</span>
              <span class="badge badge--default"><Server size={11} /> {repo.runnerName}</span>
              {#if repo.autoPlan}<span class="badge badge--info" title="Pushes start a plan / check / preview of what changed"><Webhook size={11} /> plan on push</span>{/if}
            </div>
            {#if repo.head}
              <p class="page-subtitle rp-head"><span class="mono">{short(repo.head.sha)}</span> {repo.head.message} <span class="muted">— {repo.head.author}, pulled {ago(repo.syncedAt)}</span></p>
            {/if}
          </div>
          {#if canManage}
            <div class="page-hero__actions rp-hero-actions">
              <button class="base-btn base-btn--primary base-btn--sm" onclick={pull} disabled={pulling} title="git pull">{#if pulling}<Loader2 size={13} class="spin" />{:else}<RefreshCw size={13} />{/if} Pull</button>
              <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (drawer = true)}><Pencil size={13} /> Edit</button>
            </div>
          {/if}
        </section>

        {#if repo.syncError}<div class="notice notice--error"><CircleAlert size={13} /> Last pull failed: {repo.syncError}</div>{/if}

        <section class="page-card">
          <div class="page-tabs rp-tabs">
            <button class="page-tab" class:is-active={tab === 'projects'} onclick={() => (tab = 'projects')}>Projects <span class="rp-count">{repo.projects.length}</span></button>
            <button class="page-tab" class:is-active={tab === 'runs'} onclick={() => (tab = 'runs')}>Runs <span class="rp-count">{runs.length}</span></button>
            <button class="page-tab" class:is-active={tab === 'schedules'} onclick={() => (tab = 'schedules')}>Schedules <span class="rp-count">{schedules.length}</span></button>
            {#if canManage}<button class="page-tab" class:is-active={tab === 'files'} onclick={() => { tab = 'files'; if (!path) navigate(`/automation/${id}/files`) }}>Files</button>{/if}
            <button class="page-tab" class:is-active={tab === 'commits'} onclick={() => (tab = 'commits')}>Commits</button>
            {#if canManage}<button class="page-tab" class:is-active={tab === 'setup'} onclick={() => (tab = 'setup')}>Setup</button>{/if}
          </div>

          {#if tab === 'projects'}
            <div class="data-table-wrap">
              <table class="data-table">
                <thead><tr><th>Project</th><th>Type</th><th>Last run</th><th></th></tr></thead>
                <tbody>
                  {#each repo.projects as p (p.id)}
                    {@const K = KINDS[p.kind]}
                    {@const last = latest.get(p.id)}
                    {@const plan = p.kind === 'terraform' ? applicablePlan(runs, p) : undefined}
                    {@const [safe, change] = ACTIONS[p.kind] ?? ['', '']}
                    <tr>
                      <td><div class="rp-proj">{#if K}<K.ico size={15} />{/if}<span class="mono strong">{p.name}</span></div></td>
                      <td><span class="badge badge--{K?.badge ?? 'default'}">{kindLabel(p.kind, repo.tfBin)}</span></td>
                      <td>
                        {#if last}
                          <button class="rp-last" onclick={() => navigate(`/automation/run/${last.id}`)}>
                            <span class="badge badge--{STATUS_BADGE[last.status] ?? 'default'}">{#if isActive(last)}<Loader2 size={11} class="spin" />{/if}{ACTION_LABEL[last.action]} {STATUS_TEXT[last.status] ?? last.status}</span>
                            {#if last.summary}<span class="mono">{last.summary}</span>{/if}
                            <span class="muted">{ago(last.createdAt)}</span>
                          </button>
                        {:else}<span class="muted">never run</span>{/if}
                      </td>
                      <td class="rp-actions">
                        {#if canManage}
                          {#if last && isActive(last)}
                            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(`/automation/run/${last.id}`)}><Loader2 size={13} class="spin" /> Watch</button>
                          {:else if last && last.status === 'approval'}
                            <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(`/automation/run/${last.id}`)}><Hourglass size={13} /> Review approval</button>
                          {:else}
                            <button class="icon-btn" title="View the code" onclick={() => { tab = 'files'; navigate(`/automation/${id}/files/${[p.dir, p.kind === 'ansible' ? p.file : ''].filter(Boolean).join('/')}`) }}><FileCode size={14} /></button>
                            <button class="icon-btn" title="Run with options… (limit, tags, extra vars, workspace, destroy, targets)" disabled={!!starting} onclick={() => (runDialog = p)}><SlidersHorizontal size={14} /></button>
                            <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!starting} onclick={() => start(p, safe)}>
                              {#if starting === `${p.id}:${safe}`}<Loader2 size={13} class="spin" />{:else}<Play size={12} />{/if} {ACTION_LABEL[safe]}
                            </button>
                            {#if p.kind === 'terraform'}
                              <button class="base-btn base-btn--primary base-btn--sm" disabled={!plan || !!starting} onclick={() => plan && start(p, 'apply', plan)}
                                      title={plan ? `Apply plan ${plan.id.slice(0, 8)}: ${plan.summary}` : 'Plan first — Apply runs exactly the reviewed plan'}>
                                <ShieldCheck size={12} /> Apply{#if plan} <span class="mono">{plan.summary}</span>{/if}
                              </button>
                            {:else}
                              <button class="base-btn base-btn--primary base-btn--sm" disabled={!!starting} onclick={() => start(p, change)}>{ACTION_LABEL[change]}…</button>
                            {/if}
                          {/if}
                        {/if}
                      </td>
                    </tr>
                  {:else}
                    <tr><td colspan="4"><div class="empty-state">
                      No Terraform / OpenTofu folders, Ansible playbooks or Pulumi projects found at {short(repo.head?.sha ?? '')}.
                      <span class="muted">Folders with <code>*.tf</code>, playbooks (YAML with <code>- hosts:</code>) and <code>Pulumi.yaml</code> are picked up; <code>modules/</code> and <code>roles/</code> are skipped.</span>
                    </div></td></tr>
                  {/each}
                </tbody>
              </table>
            </div>

          {:else if tab === 'runs'}
            <RunsTable {runs} />

          {:else if tab === 'schedules'}
            <div class="sc-bar">
              <span class="muted">Run a project on a schedule — a nightly Plan catches drift, a weekly playbook keeps servers in shape.</span>
              {#if canManage && repo.projects.length}<button class="base-btn base-btn--primary base-btn--sm" onclick={() => (schedDialog = { open: true, s: null })}><Plus size={13} /> New schedule</button>{/if}
            </div>
            <div class="data-table-wrap">
              <table class="data-table">
                <thead><tr><th>Schedule</th><th>Runs</th><th>When</th><th>Next</th><th>Last</th><th></th></tr></thead>
                <tbody>
                  {#each schedules as sc (sc.id)}
                    <tr class:sc--off={!sc.enabled}>
                      <td><div class="strong">{sc.name}</div>{#if sc.options && (sc.options.limit || sc.options.servers || sc.options.tags || sc.options.workspace || sc.options.destroy)}<div class="muted sc-sub">with options</div>{/if}</td>
                      <td><span class="mono">{ACTION_LABEL[sc.action]} {sc.projectName}</span></td>
                      <td><CalendarClock size={12} /> {describeCron(sc.cron)} <span class="muted">({sc.timezone})</span></td>
                      <td>{#if sc.enabled && sc.nextRun}<span title={new Date(sc.nextRun).toLocaleString()}>{new Date(sc.nextRun).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })}</span>{:else}<span class="muted">paused</span>{/if}</td>
                      <td class="sc-last">
                        {#if sc.lastRun}<button class="linkish" onclick={() => navigate(`/automation/run/${sc.lastRun}`)}>{sc.lastResult ?? 'last run'}</button>{:else if sc.lastResult}<span class="muted">{sc.lastResult}</span>{:else}<span class="muted">—</span>{/if}
                      </td>
                      <td class="rp-actions">
                        {#if canManage}
                          <label class="sc-toggle" title={sc.enabled ? 'Pause' : 'Resume'}><input type="checkbox" checked={sc.enabled} onchange={() => toggleSchedule(sc)} /> on</label>
                          <button class="icon-btn" title="Run now" onclick={() => runNow(sc)}><Play size={13} /></button>
                          <button class="icon-btn" title="Edit" onclick={() => (schedDialog = { open: true, s: sc })}><Pencil size={13} /></button>
                          <button class="icon-btn" title="Delete" onclick={() => removeSchedule(sc)}><Trash2 size={13} /></button>
                        {/if}
                      </td>
                    </tr>
                  {:else}
                    <tr><td colspan="6"><div class="empty-state">No schedules.</div></td></tr>
                  {/each}
                </tbody>
              </table>
            </div>

          {:else if tab === 'files'}
            <RepoFiles repo={id} {path} />

          {:else if tab === 'commits'}
            <div class="rp-commits">
              {#each commits as c (c.sha)}
                {@const u = commitUrl(c.sha)}
                <div class="rp-commit" class:rp-commit--head={c.sha === repo.head?.sha}>
                  {#if u}<a class="mono rp-commit__sha" href={u} target="_blank" rel="noreferrer">{short(c.sha)}</a>{:else}<span class="mono rp-commit__sha">{short(c.sha)}</span>{/if}
                  <span class="rp-commit__msg">{c.message}</span>
                  {#if c.sha === repo.head?.sha}<span class="badge badge--info">head</span>{/if}
                  <span class="muted rp-commit__by">{c.author} · {ago(c.time)}</span>
                </div>
              {:else}
                <div class="empty-state">Pull to see the commits.</div>
              {/each}
            </div>

          {:else}
            <div class="setup">
              <div class="setup__block">
                <div class="setup__title"><UserCheck size={14} /> Approvals</div>
                <label class="setup__check"><input type="checkbox" checked={repo.requireApproval} onchange={(e) => setApproval((e.currentTarget as HTMLInputElement).checked)} /> <span>Apply, Run and Up wait until <b>another</b> admin approves</span></label>
                <p class="muted">Plans, checks and previews still start right away. Whoever asked can't approve their own run.</p>
              </div>

              <NotifySettings {repo} onsaved={setRepo} />

              <div class="setup__block">
                <div class="setup__title"><Rocket size={14} /> CI trigger</div>
                <p class="muted">Start a run from GitHub Actions, GitLab CI, Jenkins… <code>"wait": true</code> returns when it finishes — HTTP 200 if it succeeded, 409 if not, so the CI step fails with it.</p>
                <div class="setup__row"><span>URL</span><code class="mono">{triggerUrl}</code><button class="icon-btn" title="Copy" onclick={() => copy(triggerUrl, 'turl')}>{#if copied === 'turl'}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
                <div class="setup__row"><span>Token</span>
                  {#if repo.triggerToken}
                    <code class="mono">{showToken ? repo.triggerToken : '•'.repeat(24)}</code>
                    <button class="icon-btn" title={showToken ? 'Hide' : 'Show'} onclick={() => (showToken = !showToken)}>{#if showToken}<EyeOff size={13} />{:else}<Eye size={13} />{/if}</button>
                    <button class="icon-btn" title="Copy" onclick={() => copy(repo?.triggerToken ?? '', 'tok')}>{#if copied === 'tok'}<Check size={13} />{:else}<Copy size={13} />{/if}</button>
                  {:else}<span class="muted">none yet</span>{/if}
                  <button class="base-btn base-btn--ghost base-btn--xs" onclick={rotateToken}>{repo.triggerToken ? 'Rotate' : 'Create token'}</button>
                </div>
                <pre class="mono setup__snippet">{ciSnippet}</pre>
                <div><button class="base-btn base-btn--ghost base-btn--xs" onclick={() => copy(ciSnippet, 'snip')}>{#if copied === 'snip'}<Check size={12} /> Copied{:else}<Copy size={12} /> Copy GitHub Actions step{/if}</button></div>
              </div>

              <div class="setup__block">
                <div class="setup__title"><Webhook size={14} /> Push webhook {#if repo.lastHook}<span class="badge badge--success">last push {ago(repo.lastHook.time)}</span>{/if}</div>
                <p class="muted">Pushes to <b>{repo.branch}</b> pull the repository{repo.autoPlan ? ' and plan / check / preview the projects they touch' : ''}. Add a webhook in {web.host || 'your Git host'}:</p>
                <div class="setup__row"><span>Payload URL</span><code class="mono">{hookUrl}</code><button class="icon-btn" title="Copy" onclick={() => copy(hookUrl, 'url')}>{#if copied === 'url'}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
                <div class="setup__row"><span>Secret</span><code class="mono">{showSecret ? repo.webhookSecret : '•'.repeat(24)}</code>
                  <button class="icon-btn" title={showSecret ? 'Hide' : 'Show'} onclick={() => (showSecret = !showSecret)}>{#if showSecret}<EyeOff size={13} />{:else}<Eye size={13} />{/if}</button>
                  <button class="icon-btn" title="Copy" onclick={() => copy(repo?.webhookSecret ?? '', 'secret')}>{#if copied === 'secret'}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
                <div class="setup__row"><span>Content type</span><code class="mono">application/json</code></div>
                <div class="setup__row"><span>Events</span><span>Just the push event</span></div>
                <div class="setup__actions">
                  {#if webhookPage(repo.url)}<a class="base-btn base-btn--ghost base-btn--sm" href={webhookPage(repo.url)} target="_blank" rel="noreferrer"><ExternalLink size={12} /> Add it on {web.host}</a>{/if}
                </div>
                {#if repo.lastHook}<p class="muted">Last delivery: {new Date(repo.lastHook.time).toLocaleString()} · {short(repo.lastHook.sha)} by {repo.lastHook.by} — {repo.lastHook.result}</p>{/if}
                <p class="muted setup__note">GitHub / GitLab must reach this address — timika behind a VPN needs the URL exposed (or skip webhooks and press Pull).</p>
              </div>

              {#if repo.auth === 'deploy_key' && repo.deployPublicKey}
                <div class="setup__block">
                  <div class="setup__title"><KeyRound size={14} /> Deploy key</div>
                  <div class="setup__row"><code class="mono setup__key">{repo.deployPublicKey}</code><button class="icon-btn" title="Copy" onclick={() => copy(repo?.deployPublicKey ?? '', 'key')}>{#if copied === 'key'}<Check size={13} />{:else}<Copy size={13} />{/if}</button></div>
                </div>
              {/if}

              <div class="setup__block">
                <div class="setup__title"><Lock size={14} /> Variables & secrets</div>
                {#each repo.variables as v (v.name)}
                  <div class="setup__row"><span class="mono">{v.name}</span><code class="mono">{v.secret ? '•••••• (secret)' : v.value}</code></div>
                {:else}
                  <p class="muted">None. Cloud credentials (AWS_ACCESS_KEY_ID, ARM_CLIENT_SECRET…) go here — <button class="linkish" onclick={() => (drawer = true)}>add variables</button>.</p>
                {/each}
              </div>

              <div class="setup__block setup__block--danger">
                <div class="setup__title">Remove repository</div>
                <p class="muted">Deletes its runs, output and saved plans from timika. Nothing changes in Git or in your infrastructure.</p>
                <div><button class="base-btn base-btn--danger base-btn--sm" onclick={remove}><Trash2 size={13} /> Remove</button></div>
              </div>
            </div>
          {/if}
        </section>
      {/if}
    </div>
  </div>
</div>

{#if runDialog && repo}
  <RunDialog {repo} project={runDialog} onclose={() => (runDialog = null)} onstarted={(r) => { runDialog = null; navigate(`/automation/run/${r.id}`) }} />
{/if}

{#if schedDialog.open && repo}
  <ScheduleDialog {repo} schedule={schedDialog.s} onclose={() => (schedDialog = { open: false, s: null })} onsaved={() => { schedDialog = { open: false, s: null }; load() }} />
{/if}

{#if drawer && repo}
  <RepoDrawer {repo} onclose={() => (drawer = false)} onsaved={(r) => { drawer = false; setRepo(r); load() }} />
{/if}

<style>
  .rp-meta { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; margin-top: 6px; font-size: 12px; }
  .rp-meta :global(svg) { vertical-align: -1px; }
  .rp-link { color: var(--text-secondary); text-decoration: none; word-break: break-all; }
  .rp-hero-actions { flex: 0 0 auto; flex-direction: row; flex-wrap: nowrap; align-items: flex-start; gap: 8px; }
  .rp-link:hover { color: var(--brand); }
  .rp-head { margin-top: 8px; }
  .rp-head .mono { color: var(--brand); }
  .rp-tabs { padding: 10px 14px 0; }
  .rp-count { font-size: 10.5px; padding: 1px 6px; border-radius: 10px; background: var(--bg-elevated); margin-left: 4px; }
  .rp-proj { display: flex; align-items: center; gap: 8px; }
  .rp-proj :global(svg) { color: var(--text-muted); flex: 0 0 auto; }
  .rp-last { display: inline-flex; align-items: center; gap: 8px; border: 0; background: none; padding: 0; cursor: pointer; font: inherit; color: inherit; font-size: 12px; }
  .rp-last :global(svg) { vertical-align: -1px; }
  .rp-actions { text-align: right; white-space: nowrap; }
  .rp-actions .base-btn + .base-btn { margin-left: 6px; }
  .rp-commits { display: flex; flex-direction: column; }
  .rp-commit { display: flex; align-items: center; gap: 10px; padding: 9px 16px; border-top: 1px solid var(--border); font-size: 12.5px; }
  .rp-commit--head { background: var(--brand-dim); }
  .rp-commit__sha { color: var(--brand); text-decoration: none; flex: 0 0 auto; }
  .rp-commit__msg { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-primary); }
  .rp-commit__by { flex: 0 0 auto; font-size: 11.5px; }
  .setup { display: flex; flex-direction: column; gap: 14px; padding: 16px; }
  .setup__block { display: flex; flex-direction: column; gap: 8px; padding: 14px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .setup__block--danger { border-color: color-mix(in srgb, var(--danger) 35%, var(--border)); }
  .setup__block p { margin: 0; font-size: 12.5px; line-height: 1.5; }
  .setup__title { display: flex; align-items: center; gap: 7px; font-weight: 700; color: var(--text-primary); font-size: 13px; }
  .setup__row { display: flex; align-items: center; gap: 8px; font-size: 12.5px; }
  .setup__row > span:first-child { min-width: 110px; max-width: 300px; flex: 0 0 auto; color: var(--text-muted); overflow-wrap: anywhere; }
  .setup__row code { flex: 1; min-width: 0; word-break: break-all; padding: 6px 9px; border-radius: var(--r-sm); background: var(--bg-surface); border: 1px solid var(--border); font-size: 11.5px; }
  .setup__key { flex: 1; }
  .setup__actions { display: flex; gap: 8px; }
  .setup__note { font-size: 11.5px !important; }
  .sc-bar { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 16px; font-size: 12.5px; }
  .sc--off { opacity: 0.55; }
  .sc-sub { font-size: 11px; }
  .sc-last { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
  .sc-toggle { display: inline-flex; align-items: center; gap: 4px; font-size: 11.5px; color: var(--text-muted); margin-right: 4px; cursor: pointer; }
  td :global(svg) { vertical-align: -2px; }
  .setup__check { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-primary); cursor: pointer; }
  .setup__snippet { margin: 0; padding: 10px 12px; border-radius: var(--r-sm); background: #0d0f12; color: #d4d4d8; font-size: 11.5px; overflow-x: auto; white-space: pre; }
  .rp-actions .icon-btn { vertical-align: middle; margin-right: 2px; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; }
</style>
