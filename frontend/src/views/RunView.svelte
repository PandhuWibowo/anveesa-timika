<script lang="ts">
  // One run: live output (streamed), result, and "Apply this plan".
  import { onMount, tick } from 'svelte'
  import { ArrowLeft, Loader2, Square, ShieldCheck, Download, GitCommitHorizontal, Server, Webhook, User, RotateCcw, Hourglass, Check, X, CalendarClock, Rocket, SlidersHorizontal } from '@lucide/svelte'
  import { automation, errMsg, isNetworkError } from '../lib/api'
  import { isAdmin, session } from '../lib/session.svelte'
  import { navigate } from '../lib/router.svelte'
  import { confirm } from '../lib/ui.svelte'
  import { KINDS, ACTION_LABEL, STATUS_BADGE, STATUS_TEXT, duration, short, isActive } from '../lib/automation'
  import type { Run } from '../gen/timika/v1/automation_pb'

  let { id }: { id: string } = $props()

  const SHOWN_LINES = 5000
  let run = $state<Run | null>(null)
  let text = $state('')
  let error = $state('')
  let busy = $state('')
  let follow = $state(true)
  let logEl = $state<HTMLElement>()
  let now = $state(Date.now())
  let plan = $state<Run | null>(null)
  // The plan an apply uses: shown to whoever approves it.
  $effect(() => {
    const pid = run?.planRun
    if (pid && plan?.id !== pid) automation.getRun({ id: pid }).then((p) => (plan = p)).catch(() => {})
  })
  const canManage = isAdmin()

  // Output → lines with a style, for plans and step markers.
  function cls(l: string): string {
    if (l.startsWith('▶')) return 'step'
    if (l.startsWith('✓')) return 'ok'
    if (l.startsWith('✗') || /^(Error|error|fatal|FAILED|ERROR)\b|^fatal:/.test(l.trimStart())) return 'bad'
    if (l.startsWith('■') || l.startsWith('!')) return 'warn'
    const t = l.trimStart()
    if (/^\+ /.test(t) || t.startsWith('changed: [')) return 'add'
    if (/^- /.test(t) && l.startsWith('  ')) return 'del'
    if (/^~ /.test(t) || /^-\/\+ /.test(t)) return 'chg'
    if (/^Plan: |^Apply complete!|^No changes\.|^PLAY RECAP|^Resources:/.test(t)) return 'sum'
    return ''
  }
  const lines = $derived(text.split('\n'))
  const shown = $derived(lines.length > SHOWN_LINES ? lines.slice(-SHOWN_LINES) : lines)

  async function watch(signal: AbortSignal) {
    for (let attempt = 0; !signal.aborted; attempt++) {
      try {
        let fresh = ''
        for await (const ev of automation.watchRun({ id }, { signal })) {
          if (ev.output) {
            fresh += ev.output
            text = fresh
          }
          if (ev.run) run = ev.run
          if (follow) scrollDown()
        }
        error = ''
        return
      } catch (e) {
        if (signal.aborted) return
        error = isNetworkError(e) ? 'Reconnecting…' : errMsg(e)
        if (!isNetworkError(e)) return
        await new Promise((r) => setTimeout(r, Math.min(5000, 1000 * (attempt + 1))))
      }
    }
  }

  async function scrollDown() {
    await tick()
    logEl?.scrollTo({ top: logEl.scrollHeight })
  }

  function onScroll() {
    if (!logEl) return
    follow = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 40
  }

  onMount(() => {
    const ctl = new AbortController()
    watch(ctl.signal)
    const t = setInterval(() => (now = Date.now()), 1000)
    return () => { ctl.abort(); clearInterval(t) }
  })

  async function cancel() {
    if (!run) return
    busy = 'cancel'
    try { run = await automation.cancelRun({ id }) } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  async function apply() {
    if (!run) return
    const ok = await confirm({ title: `Apply this plan to ${run.projectName}?`, message: `${run.summary} — exactly what's shown here, from commit ${short(run.sha)}.`, confirmText: 'Apply', variant: 'danger' })
    if (!ok) return
    busy = 'apply'
    try {
      const r = await automation.startRun({ repo: run.repo, project: run.project, action: 'apply', planRun: run.id })
      navigate(`/automation/run/${r.id}`)
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  async function decide(approve: boolean) {
    if (!run) return
    if (!approve) {
      const ok = await confirm({ title: 'Reject this run?', message: 'It will not run. A plan it was going to apply can be applied later.', confirmText: 'Reject', variant: 'warning' })
      if (!ok) return
    }
    busy = approve ? 'approve' : 'reject'
    try { run = approve ? await automation.approveRun({ id }) : await automation.rejectRun({ id }) } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  async function again() {
    if (!run) return
    busy = 'again'
    try {
      const r = await automation.startRun({ repo: run.repo, project: run.project, action: run.action === 'apply' ? 'plan' : run.action })
      navigate(`/automation/run/${r.id}`)
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  function download() {
    if (!run) return
    const a = document.createElement('a')
    a.href = URL.createObjectURL(new Blob([text], { type: 'text/plain' }))
    a.download = `${run.repoName.replace(/\W+/g, '-')}-${run.action}-${run.id.slice(0, 8)}.log`
    a.click()
    URL.revokeObjectURL(a.href)
  }

  const canApply = $derived(!!run && run.kind === 'terraform' && run.action === 'plan' && run.status === 'succeeded' && run.changes === true && !run.appliedBy)
  const K = $derived(run ? KINDS[run.kind] : undefined)
  // Re-evaluated every second while running.
  const took = $derived(now && run ? duration(run) : '')
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <div class="rv-nav">
        <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => navigate(run ? `/automation/${run.repo}` : '/automation')}><ArrowLeft size={13} /> {run?.repoName ?? 'Infrastructure'}</button>
      </div>

      {#if run}
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">{K?.label ?? run.kind} · run {run.id.slice(0, 8)}</div>
            <h1 class="page-title rv-title">
              {ACTION_LABEL[run.action] ?? run.action} <span class="mono">{run.projectName}</span>
              <span class="badge badge--{STATUS_BADGE[run.status] ?? 'default'}">{#if isActive(run)}<Loader2 size={11} class="spin" />{/if}{STATUS_TEXT[run.status] ?? run.status}</span>
            </h1>
            <div class="rv-meta">
              <span title={run.commitMessage}><GitCommitHorizontal size={12} /> <span class="mono">{short(run.sha)}</span> {run.commitMessage}</span>
              <span>{#if run.trigger === 'push'}<Webhook size={12} /> push by{:else if run.trigger === 'schedule'}<CalendarClock size={12} />{:else if run.trigger === 'ci'}<Rocket size={12} /> CI:{:else}<User size={12} />{/if} {run.user}</span>
              {#if run.approvedBy}<span><Check size={12} /> approved by {run.approvedBy}</span>{/if}
              {#if run.optionsText}<span class="rv-opts"><SlidersHorizontal size={12} /> {run.optionsText}</span>{/if}
              <span><Server size={12} /> {run.runnerName}</span>
              {#if took}<span>{took}</span>{/if}
            </div>
            {#if run.summary || run.error}
              <div class="rv-result">
                {#if run.summary}<span class="mono rv-summary">{run.summary}</span>{/if}
                {#if run.error}<span class="rv-error">{run.error}</span>{/if}
                {#if run.appliedBy}<button class="linkish" onclick={() => navigate(`/automation/run/${run?.appliedBy}`)}>applied in run {run.appliedBy.slice(0, 8)} →</button>{/if}
                {#if run.planRun}<button class="linkish" onclick={() => navigate(`/automation/run/${run?.planRun}`)}>← the plan</button>{/if}
              </div>
            {/if}
          </div>
          <div class="page-hero__actions">
            {#if canManage && isActive(run)}
              <button class="base-btn base-btn--danger base-btn--sm" disabled={busy === 'cancel'} onclick={cancel}>{#if busy === 'cancel'}<Loader2 size={13} class="spin" />{:else}<Square size={12} />{/if} Cancel</button>
            {/if}
            {#if canManage && canApply}
              <button class="base-btn base-btn--primary base-btn--sm" disabled={!!busy} onclick={apply}>{#if busy === 'apply'}<Loader2 size={13} class="spin" />{:else}<ShieldCheck size={13} />{/if} Apply this plan</button>
            {/if}
            {#if canManage && !isActive(run) && run.status !== 'approval'}
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!busy} onclick={again} title="Same project, latest commit">{#if busy === 'again'}<Loader2 size={13} class="spin" />{:else}<RotateCcw size={13} />{/if} {run.action === 'apply' ? 'Plan again' : 'Run again'}</button>
            {/if}
            <button class="base-btn base-btn--ghost base-btn--sm" onclick={download} disabled={!text}><Download size={13} /> Output</button>
          </div>
        </section>
      {/if}

      {#if run && run.status === 'approval'}
        <div class="rv-approval">
          <Hourglass size={18} />
          <div class="rv-approval__text">
            <b>Waiting for approval.</b> {run.user} asked to {ACTION_LABEL[run.action]?.toLowerCase()} <span class="mono">{run.projectName}</span>{run.optionsText ? ` (${run.optionsText})` : ''}. Another admin must approve before it runs.
            {#if plan}
              <div class="rv-approval__plan">The plan: <span class="mono rv-summary rv-summary--sm">{plan.summary}</span> from {plan.user}, {new Date(plan.createdAt).toLocaleString()} — <button class="linkish" onclick={() => navigate(`/automation/run/${plan?.id}`)}>review its output →</button></div>
            {/if}
          </div>
          {#if canManage}
            {#if session.me?.username !== run.user}
              <button class="base-btn base-btn--primary base-btn--sm" disabled={!!busy} onclick={() => decide(true)}>{#if busy === 'approve'}<Loader2 size={13} class="spin" />{:else}<Check size={13} />{/if} Approve & run</button>
            {/if}
            <button class="base-btn base-btn--ghost base-btn--sm" disabled={!!busy} onclick={() => decide(false)}><X size={13} /> {session.me?.username === run.user ? 'Withdraw' : 'Reject'}</button>
          {/if}
        </div>
      {/if}

      {#if error}<div class="notice notice--{error === 'Reconnecting…' ? 'warning' : 'error'}">{error}</div>{/if}

      <section class="rv-log-card">
        <div class="rv-log" bind:this={logEl} onscroll={onScroll}>
          {#if lines.length > SHOWN_LINES}<div class="rv-line warn">… {lines.length - SHOWN_LINES} earlier lines — use “Output” to download everything</div>{/if}
          {#each shown as l, i (i)}<div class="rv-line {cls(l)}">{l || ' '}</div>{/each}
          {#if run && run.status === 'approval'}<div class="rv-line rv-cursor"><Hourglass size={12} /> waiting for approval — nothing has run yet</div>{/if}
          {#if run && isActive(run)}<div class="rv-line rv-cursor"><Loader2 size={12} class="spin" /> {run.status === 'queued' ? 'starting…' : 'running…'}</div>{/if}
          {#if !run && !error}<div class="rv-line"><Loader2 size={12} class="spin" /></div>{/if}
        </div>
        {#if !follow && run && isActive(run)}<button class="rv-follow" onclick={() => { follow = true; scrollDown() }}>↓ Follow output</button>{/if}
      </section>
    </div>
  </div>
</div>

<style>
  .rv-title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .rv-title .badge { font-size: 11px; }
  .rv-meta { display: flex; gap: 14px; flex-wrap: wrap; font-size: 12px; color: var(--text-secondary); margin-top: 6px; }
  .rv-meta :global(svg) { vertical-align: -2px; color: var(--text-muted); }
  .rv-meta .mono { color: var(--brand); }
  .rv-result { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; margin-top: 10px; }
  .rv-summary { font-size: 14px; font-weight: 700; color: var(--text-primary); padding: 3px 10px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .rv-error { color: var(--danger); font-size: 12.5px; }
  .rv-opts { color: var(--warning) !important; }
  .rv-approval { display: flex; align-items: center; gap: 12px; padding: 12px 16px; border-radius: var(--r-lg); background: var(--warning-bg); border: 1px solid color-mix(in srgb, var(--warning) 40%, transparent); color: var(--text-primary); }
  .rv-approval > :global(svg) { color: var(--warning); flex: 0 0 auto; }
  .rv-approval__text { flex: 1; font-size: 13px; line-height: 1.5; }
  .rv-approval__plan { margin-top: 4px; font-size: 12.5px; }
  .rv-summary--sm { font-size: 12px; padding: 1px 7px; }
  .linkish { border: 0; background: none; padding: 0; color: var(--brand); cursor: pointer; font: inherit; font-size: 12.5px; }
  .rv-log-card { position: relative; border-radius: var(--r-lg); border: 1px solid var(--border); overflow: hidden; background: #0d0f12; }
  .rv-log { height: calc(100vh - 330px); min-height: 320px; overflow: auto; padding: 12px 0; font-family: var(--mono); font-size: 12px; line-height: 1.55; color: #d4d4d8; }
  .rv-line { padding: 0 16px; white-space: pre-wrap; word-break: break-word; }
  .rv-line.step { color: #5eead4; font-weight: 700; margin-top: 4px; }
  .rv-line.ok { color: #4ade80; font-weight: 700; }
  .rv-line.bad { color: #f87171; }
  .rv-line.warn { color: #fbbf24; }
  .rv-line.add { color: #86efac; }
  .rv-line.del { color: #fca5a5; }
  .rv-line.chg { color: #fcd34d; }
  .rv-line.sum { color: #fff; font-weight: 700; }
  .rv-cursor { color: #71717a; display: flex; align-items: center; gap: 6px; }
  .rv-follow { position: absolute; right: 14px; bottom: 12px; border: 1px solid #3f3f46; background: #18181b; color: #e4e4e7; border-radius: 14px; padding: 4px 12px; font-size: 12px; cursor: pointer; }
</style>
