<script lang="ts">
  // "Run with options…": pick the action and the per-run choices, then start.
  import { onMount } from 'svelte'
  import { fade, scale } from 'svelte/transition'
  import { X, Loader2, Play, TriangleAlert } from '@lucide/svelte'
  import { automation, bastion, errMsg } from '../lib/api'
  import { ACTIONS, ACTION_LABEL, KINDS, emptyOpts, optsInput, type Opts } from '../lib/automation'
  import type { Project, Repo, Run } from '../gen/timika/v1/automation_pb'
  import RunOptionsForm from './RunOptionsForm.svelte'

  let { repo, project, onclose, onstarted }: { repo: Repo; project: Project; onclose: () => void; onstarted: (r: Run) => void } = $props()

  // Apply always takes a reviewed plan: not offered here.
  // svelte-ignore state_referenced_locally
  const choices = (ACTIONS[project.kind] ?? []).filter((a) => a !== 'apply')
  // svelte-ignore state_referenced_locally
  let action = $state(choices[0])
  let opts = $state<Opts>(emptyOpts())
  let tags = $state<string[]>([])
  let busy = $state(false)
  let error = $state('')
  // svelte-ignore state_referenced_locally
  const K = KINDS[project.kind]
  const changing = $derived(action === ACTIONS[project.kind]?.[1])

  onMount(async () => {
    if (project.kind !== 'ansible') return
    try { tags = [...new Set((await bastion.listAssets({})).assets.flatMap((a) => a.tags))].sort() } catch { /* optional */ }
  })

  async function start() {
    busy = true
    error = ''
    try {
      onstarted(await automation.startRun({ repo: repo.id, project: project.id, action, options: optsInput(opts) }))
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="md-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="md" role="dialog" aria-modal="true" transition:scale={{ duration: 140, start: 0.96 }}>
    <header class="md__head">
      <div>
        <div class="page-kicker">{K?.label ?? project.kind} · {repo.name}</div>
        <div class="md__title mono">{project.name}</div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>
    <div class="md__body">
      {#if choices.length > 1}
        <div class="seg">
          {#each choices as a (a)}<button class:is-on={action === a} onclick={() => (action = a)}>{ACTION_LABEL[a]}</button>{/each}
        </div>
      {/if}
      <RunOptionsForm kind={project.kind} bind:value={opts} {tags} />
      {#if changing}
        <div class="notice notice--warning"><TriangleAlert size={13} /> {ACTION_LABEL[action]} makes real changes.{repo.requireApproval ? ' It waits for another admin to approve.' : ''}</div>
      {/if}
      {#if error}<div class="notice notice--error">{error}</div>{/if}
    </div>
    <footer class="md__foot">
      <button class="base-btn base-btn--ghost" onclick={onclose}>Cancel</button>
      <button class="base-btn" class:base-btn--danger={changing || opts.destroy} class:base-btn--primary={!changing && !opts.destroy} disabled={busy} onclick={start}>
        {#if busy}<Loader2 size={14} class="spin" />{:else}<Play size={13} />{/if} {opts.destroy ? 'Plan destroy' : ACTION_LABEL[action]}
      </button>
    </footer>
  </div>
</div>

<style>
  .md-mask { position: fixed; inset: 0; z-index: 950; display: flex; align-items: center; justify-content: center; background: rgba(0, 0, 0, 0.5); backdrop-filter: blur(3px); }
  .md { width: 560px; max-width: calc(100vw - 32px); max-height: calc(100vh - 48px); display: flex; flex-direction: column; background: var(--bg-surface); border: 1px solid var(--border); border-radius: 14px; box-shadow: var(--shadow-lg); }
  .md__head { display: flex; justify-content: space-between; align-items: flex-start; padding: 18px 20px 8px; }
  .md__title { font-size: 16px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .md__body { padding: 8px 20px 16px; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; }
  .md__foot { display: flex; justify-content: flex-end; gap: 8px; padding: 12px 20px; border-top: 1px solid var(--border); }
  .seg { display: inline-flex; align-self: flex-start; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); }
  .seg button { padding: 5px 14px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12.5px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .notice :global(svg) { vertical-align: -2px; }
</style>
