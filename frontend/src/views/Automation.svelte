<script lang="ts">
  // Infrastructure: Git repositories with Terraform / OpenTofu, Ansible and
  // Pulumi projects, and every run across them.
  import { onMount } from 'svelte'
  import { Plus, Loader2, GitBranch, RefreshCw, Workflow, Webhook, CircleAlert } from '@lucide/svelte'
  import { automation, errMsg } from '../lib/api'
  import { isAdmin } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { KINDS, repoWeb, ago, short, isActive } from '../lib/automation'
  import type { Repo, Run } from '../gen/timika/v1/automation_pb'
  import RepoDrawer from '../components/RepoDrawer.svelte'
  import RunsTable from '../components/RunsTable.svelte'

  let repos = $state<Repo[]>([])
  let runs = $state<Run[]>([])
  let loading = $state(true)
  let error = $state('')
  let drawer = $state(false)
  let pulling = $state<Record<string, boolean>>({})
  const canManage = isAdmin()

  async function load() {
    try {
      const [r, x] = await Promise.all([automation.listRepos({}), automation.listRuns({ limit: 30 })])
      repos = r.repos
      runs = x.runs
      error = ''
    } catch (e) { error = errMsg(e) } finally { loading = false }
  }

  onMount(() => {
    load()
    const t = setInterval(() => { if (!document.hidden) load() }, 5000)
    return () => clearInterval(t)
  })

  async function pull(r: Repo) {
    pulling[r.id] = true
    try {
      const n = await automation.syncRepo({ id: r.id })
      repos = repos.map((x) => (x.id === n.id ? n : x))
    } catch (e) { error = errMsg(e) } finally { pulling[r.id] = false }
  }

  const counts = (r: Repo) => Object.entries(r.projects.reduce<Record<string, number>>((m, p) => ((m[p.kind] = (m[p.kind] ?? 0) + 1), m), {}))
  const lastRun = (r: Repo) => runs.find((x) => x.repo === r.id)
  const today = $derived(runs.filter((r) => Date.now() - new Date(r.createdAt).getTime() < 86400_000))
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Automation</div>
          <h1 class="page-title">Infrastructure</h1>
          <p class="page-subtitle">Terraform, OpenTofu, Ansible and Pulumi straight from Git. Pull, plan, apply — on your own runner server, with credentials from the vault.</p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">{repos.length}</span><span class="page-metric__label">Repositories</span></div>
          <div class="page-metric"><span class="page-metric__value">{repos.reduce((n, r) => n + r.projects.length, 0)}</span><span class="page-metric__label">Projects</span></div>
          <div class="page-metric"><span class="page-metric__value">{today.length}</span><span class="page-metric__label">Runs today</span></div>
          <div class="page-metric"><span class="page-metric__value">{runs.filter(isActive).length}</span><span class="page-metric__label">Running</span></div>
        </div>
      </section>

      {#if error}<div class="notice notice--error">{error}</div>{/if}

      {#if loading}
        <div class="empty-state"><Loader2 size={20} class="spin" /></div>
      {:else if !repos.length}
        <section class="page-card">
          <div class="empty-state iac-empty">
            <Workflow size={30} />
            <div class="iac-empty__title">Run your infrastructure code from here</div>
            <div class="iac-empty__steps">
              <span><b>1</b> Paste a Git URL</span>
              <span><b>2</b> Pick a runner server</span>
              <span><b>3</b> Plan → Apply</span>
            </div>
            <div class="muted">Terraform / OpenTofu folders, Ansible playbooks and Pulumi stacks are found automatically.</div>
            {#if canManage}
              <button class="base-btn base-btn--primary" onclick={() => (drawer = true)}><Plus size={14} /> Connect a repository</button>
            {:else}
              <div class="muted">An administrator connects repositories.</div>
            {/if}
          </div>
        </section>
      {:else}
        <div class="iac-bar">
          <span class="iac-bar__title">Repositories</span>
          {#if canManage}<button class="base-btn base-btn--primary base-btn--sm" onclick={() => (drawer = true)}><Plus size={13} /> Connect a repository</button>{/if}
        </div>
        <div class="iac-grid">
          {#each repos as r (r.id)}
            {@const w = repoWeb(r.url)}
            {@const last = lastRun(r)}
            <article class="iac-card">
              <button class="iac-card__main" onclick={() => navigate(`/automation/${r.id}`)}>
                <div class="iac-card__top">
                  <div class="iac-card__icon"><GitBranch size={17} /></div>
                  <div class="iac-card__id">
                    <div class="iac-card__name">{r.name}</div>
                    <div class="mono iac-card__url">{w.host} · {r.branch}</div>
                  </div>
                </div>
                {#if r.head}
                  <div class="iac-card__commit"><span class="mono">{short(r.head.sha)}</span> {r.head.message}</div>
                  <div class="muted iac-card__sub">{r.head.author} · pulled {ago(r.syncedAt)}</div>
                {/if}
                {#if r.syncError}<div class="iac-card__err"><CircleAlert size={12} /> {r.syncError}</div>{/if}
                <div class="iac-card__meta">
                  {#each counts(r) as [k, n] (k)}
                    {@const K = KINDS[k]}
                    <span class="badge badge--{K?.badge ?? 'default'}">{#if K}<K.ico size={11} />{/if} {n} {K?.label ?? k}</span>
                  {:else}
                    <span class="badge badge--default">no projects found</span>
                  {/each}
                  {#if r.autoPlan && r.lastHook}<span class="badge badge--default" title="Last push {ago(r.lastHook.time)}: {r.lastHook.result}"><Webhook size={11} /> {ago(r.lastHook.time)}</span>{/if}
                </div>
                {#if last}
                  <div class="iac-card__last">Last: {last.projectName} · {last.action} · <span class="iac-st iac-st--{last.status}">{last.status}</span>{#if last.summary} · <span class="mono">{last.summary}</span>{/if}</div>
                {/if}
              </button>
              {#if canManage}
                <div class="iac-card__actions">
                  <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => pull(r)} disabled={pulling[r.id]} title="git pull">
                    {#if pulling[r.id]}<Loader2 size={13} class="spin" />{:else}<RefreshCw size={13} />{/if} Pull
                  </button>
                </div>
              {/if}
            </article>
          {/each}
        </div>

        <section class="page-card">
          <div class="page-card__head"><div class="page-card__title">Recent runs</div></div>
          <RunsTable {runs} showRepo empty="No runs yet — open a repository and press Plan." />
        </section>
      {/if}
    </div>
  </div>
</div>

{#if drawer}
  <RepoDrawer onclose={() => (drawer = false)} onsaved={(r) => { drawer = false; navigate(`/automation/${r.id}`) }} />
{/if}

<style>
  .iac-bar { display: flex; align-items: center; justify-content: space-between; }
  .iac-bar__title { font-size: 13px; font-weight: 700; color: var(--text-primary); }
  .iac-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 12px; }
  .iac-card { display: flex; flex-direction: column; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); box-shadow: var(--shadow-sm); overflow: hidden; }
  .iac-card__main { display: flex; flex-direction: column; gap: 8px; padding: 14px; border: 0; background: none; text-align: left; color: inherit; font: inherit; cursor: pointer; flex: 1; }
  .iac-card__main:hover { background: var(--bg-hover); }
  .iac-card__top { display: flex; align-items: center; gap: 10px; }
  .iac-card__icon { width: 30px; height: 30px; border-radius: var(--r); display: grid; place-items: center; background: var(--brand-soft); color: var(--brand); flex: 0 0 auto; }
  .iac-card__id { min-width: 0; }
  .iac-card__name { font-weight: 700; color: var(--text-primary); }
  .iac-card__url { font-size: 11.5px; color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .iac-card__commit { font-size: 12.5px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .iac-card__commit .mono { color: var(--brand); margin-right: 4px; }
  .iac-card__sub { font-size: 11.5px; margin-top: -4px; }
  .iac-card__err { font-size: 12px; color: var(--danger); display: flex; gap: 5px; align-items: flex-start; }
  .iac-card__meta { display: flex; gap: 6px; flex-wrap: wrap; }
  .iac-card__meta :global(svg) { vertical-align: -1px; }
  .iac-card__last { font-size: 11.5px; color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .iac-card__actions { display: flex; justify-content: flex-end; gap: 6px; padding: 8px 12px; border-top: 1px solid var(--border); }
  .iac-st--succeeded { color: var(--success); }
  .iac-st--failed, .iac-st--lost { color: var(--danger); }
  .iac-st--running, .iac-st--queued { color: var(--info); }
  .iac-empty { gap: 10px; padding: 36px 20px; }
  .iac-empty__title { font-size: 15px; font-weight: 700; color: var(--text-primary); }
  .iac-empty__steps { display: flex; gap: 18px; flex-wrap: wrap; justify-content: center; font-size: 12.5px; color: var(--text-secondary); }
  .iac-empty__steps b { display: inline-grid; place-items: center; width: 18px; height: 18px; border-radius: 50%; background: var(--brand-soft); color: var(--brand); font-size: 11px; margin-right: 4px; }
</style>
