<script lang="ts">
  // New / edit schedule: what to run, when (presets or cron), in which time zone.
  import { onMount } from 'svelte'
  import { fade, scale } from 'svelte/transition'
  import { X, Loader2, CalendarClock } from '@lucide/svelte'
  import { automation, bastion, errMsg } from '../lib/api'
  import { ACTIONS, ACTION_LABEL, PRESETS, cronFor, presetOf, localOffset, optsFrom, optsInput, type Opts, type Preset } from '../lib/automation'
  import type { Repo, Schedule } from '../gen/timika/v1/automation_pb'
  import RunOptionsForm from './RunOptionsForm.svelte'

  let { repo, schedule = null, onclose, onsaved }: { repo: Repo; schedule?: Schedule | null; onclose: () => void; onsaved: (s: Schedule) => void } = $props()
  // svelte-ignore state_referenced_locally
  const editing = schedule
  // svelte-ignore state_referenced_locally
  const start = editing ? presetOf(editing.cron) : { preset: 'daily' as Preset, time: '02:00' }

  // svelte-ignore state_referenced_locally
  let project = $state(editing?.project ?? (repo.projects.find((p) => p.kind === 'terraform') ?? repo.projects[0])?.id ?? '')
  let action = $state(editing?.action ?? '')
  let preset = $state<Preset>(start.preset)
  let time = $state(start.time)
  let custom = $state(editing?.cron ?? '0 2 * * *')
  let timezone = $state(editing?.timezone ?? localOffset())
  let name = $state(editing?.name ?? '')
  let enabled = $state(editing?.enabled ?? true)
  let opts = $state<Opts>(optsFrom(editing?.options))
  let tags = $state<string[]>([])
  let busy = $state(false)
  let error = $state('')

  // svelte-ignore state_referenced_locally
  const kind = $derived(repo.projects.find((p) => p.id === project)?.kind ?? '')
  const choices = $derived((ACTIONS[kind] ?? []).filter((a) => a !== 'apply'))
  $effect(() => { if (!choices.includes(action)) action = choices[0] ?? '' })
  const cron = $derived(cronFor(preset, time, custom))

  onMount(async () => {
    try { tags = [...new Set((await bastion.listAssets({})).assets.flatMap((a) => a.tags))].sort() } catch { /* optional */ }
  })

  async function save() {
    busy = true
    error = ''
    try {
      onsaved(await automation.saveSchedule({ id: editing?.id ?? '', repo: repo.id, project, action, name: name.trim(), cron, timezone: timezone.trim(), enabled, options: optsInput(opts) }))
    } catch (e) { error = errMsg(e) } finally { busy = false }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="md-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="md" role="dialog" aria-modal="true" transition:scale={{ duration: 140, start: 0.96 }}>
    <header class="md__head">
      <div>
        <div class="page-kicker">Schedule · {repo.name}</div>
        <div class="md__title">{editing ? editing.name : 'New schedule'}</div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>
    <div class="md__body">
      <div class="g2">
        <label class="f"><span>Project</span>
          <select class="base-select mono" bind:value={project}>{#each repo.projects as p (p.id)}<option value={p.id}>{p.name}</option>{/each}</select>
        </label>
        <label class="f"><span>Action</span>
          <select class="base-select" bind:value={action}>{#each choices as a (a)}<option value={a}>{ACTION_LABEL[a]}</option>{/each}</select>
        </label>
      </div>
      {#if kind === 'terraform'}<small class="muted">A scheduled Plan catches drift: changes found are reported (and can be applied after review).</small>{/if}

      <div class="g3">
        <label class="f"><span>When</span>
          <select class="base-select" bind:value={preset}>{#each PRESETS as p (p.id)}<option value={p.id}>{p.label}</option>{/each}</select>
        </label>
        {#if preset === 'custom'}
          <label class="f"><span>Cron</span><input class="base-input mono" bind:value={custom} placeholder="m h dom mon dow" /></label>
        {:else}
          <label class="f"><span>{preset === 'hourly' ? 'Minute' : 'Time'}</span><input class="base-input mono" type="time" bind:value={time} /></label>
        {/if}
        <label class="f"><span>Time zone</span><input class="base-input mono" bind:value={timezone} placeholder="UTC or +07:00" /></label>
      </div>
      <div class="cron"><CalendarClock size={13} /> <code class="mono">{cron}</code> <span class="muted">({timezone || 'UTC'})</span></div>

      <details class="more" open={!!editing?.options && Object.values(optsInput(opts)).some((v) => (Array.isArray(v) ? v.length : !!v))}>
        <summary>Run options</summary>
        <RunOptionsForm {kind} bind:value={opts} {tags} />
      </details>

      <div class="g2">
        <label class="f"><span>Name</span><input class="base-input" bind:value={name} placeholder={`${action} ${repo.projects.find((p) => p.id === project)?.name ?? ''}`} /></label>
        <label class="check"><input type="checkbox" bind:checked={enabled} /> Enabled</label>
      </div>
      {#if error}<div class="notice notice--error">{error}</div>{/if}
    </div>
    <footer class="md__foot">
      <button class="base-btn base-btn--ghost" onclick={onclose}>Cancel</button>
      <button class="base-btn base-btn--primary" disabled={busy || !project || !action || !cron} onclick={save}>{#if busy}<Loader2 size={14} class="spin" />{/if} {editing ? 'Save' : 'Create schedule'}</button>
    </footer>
  </div>
</div>

<style>
  .md-mask { position: fixed; inset: 0; z-index: 950; display: flex; align-items: center; justify-content: center; background: rgba(0, 0, 0, 0.5); backdrop-filter: blur(3px); }
  .md { width: 600px; max-width: calc(100vw - 32px); max-height: calc(100vh - 48px); display: flex; flex-direction: column; background: var(--bg-surface); border: 1px solid var(--border); border-radius: 14px; box-shadow: var(--shadow-lg); }
  .md__head { display: flex; justify-content: space-between; align-items: flex-start; padding: 18px 20px 8px; }
  .md__title { font-size: 16px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .md__body { padding: 8px 20px 16px; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; }
  .md__foot { display: flex; justify-content: flex-end; gap: 8px; padding: 12px 20px; border-top: 1px solid var(--border); }
  .f { display: flex; flex-direction: column; gap: 5px; }
  .f > span { font-size: 11px; color: var(--text-muted); letter-spacing: .03em; text-transform: uppercase; }
  .f input, .f select { width: 100%; }
  .g2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; align-items: end; }
  .g3 { display: grid; grid-template-columns: 1.3fr 1fr 1fr; gap: 10px; }
  .cron { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--text-secondary); }
  .check { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-secondary); cursor: pointer; padding-bottom: 8px; }
  .more summary { cursor: pointer; font-size: 12.5px; color: var(--text-secondary); margin-bottom: 8px; }
  small { font-size: 11.5px; margin-top: -6px; }
</style>
