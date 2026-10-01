<script lang="ts">
  // The per-run choices for a project's kind (Semaphore's "prompts").
  import { Server, FileText } from '@lucide/svelte'
  import type { Opts } from '../lib/automation'

  let { kind, value = $bindable(), tags = [] }: { kind: string; value: Opts; tags?: string[] } = $props()

  const useServers = $derived(value.servers !== '')
  const picked = $derived(value.servers.split(',').map((t) => t.trim()).filter(Boolean))

  function toggleTag(t: string) {
    const set = new Set(picked.filter((x) => x !== '*'))
    if (set.has(t)) set.delete(t)
    else set.add(t)
    value.servers = [...set].join(',') || '*'
  }
</script>

<div class="ro">
  {#if kind === 'terraform'}
    <label class="ro-check ro-check--danger"><input type="checkbox" bind:checked={value.destroy} /> Destroy plan <span class="muted">— plan the removal of everything (review, then Apply)</span></label>
    <div class="ro-grid">
      <label class="ro-f"><span>Workspace</span><input class="base-input mono" bind:value={value.workspace} placeholder="default" /></label>
    </div>
    <label class="ro-f"><span>Only these resources (-target), one per line</span><textarea class="base-input mono" rows="2" bind:value={value.targets} placeholder="aws_instance.web&#10;module.db"></textarea></label>
    <label class="ro-f"><span>Recreate (-replace), one per line</span><textarea class="base-input mono" rows="2" bind:value={value.replace} placeholder="aws_instance.web"></textarea></label>
  {:else if kind === 'ansible'}
    <div class="ro-f">
      <span>Hosts</span>
      <div class="seg">
        <button type="button" class:is-on={!useServers} onclick={() => (value.servers = '')}><FileText size={12} /> Repository inventory</button>
        <button type="button" class:is-on={useServers} onclick={() => (value.servers = value.servers || '*')}><Server size={12} /> Servers in timika</button>
      </div>
      {#if useServers}
        <div class="ro-tags">
          <button type="button" class="chip" class:is-on={picked.includes('*')} onclick={() => (value.servers = '*')}>all servers</button>
          {#each tags as t (t)}<button type="button" class="chip" class:is-on={picked.includes(t)} onclick={() => toggleTag(t)}>{t}</button>{/each}
        </div>
        <small class="muted">Each server's first account and its stored key / password; host keys must match timika's pins. Groups = tags.</small>
      {/if}
    </div>
    <div class="ro-grid ro-grid--3">
      <label class="ro-f"><span>Limit</span><input class="base-input mono" bind:value={value.limit} placeholder="web*:!web3" /></label>
      <label class="ro-f"><span>Tags</span><input class="base-input mono" bind:value={value.tags} placeholder="deploy,config" /></label>
      <label class="ro-f"><span>Skip tags</span><input class="base-input mono" bind:value={value.skipTags} placeholder="slow" /></label>
    </div>
    <label class="ro-f"><span>Extra vars — key=value per line, or JSON</span><textarea class="base-input mono" rows="3" bind:value={value.extraVars} placeholder="version=1.8.2&#10;maintenance=true"></textarea></label>
    <label class="ro-f ro-f--narrow"><span>Verbosity</span>
      <select class="base-select" bind:value={value.verbose}>
        <option value={0}>normal</option><option value={1}>-v</option><option value={2}>-vv</option><option value={3}>-vvv</option><option value={4}>-vvvv</option>
      </select>
    </label>
  {:else if kind === 'pulumi'}
    <label class="ro-check"><input type="checkbox" bind:checked={value.refresh} /> Refresh state first (--refresh)</label>
    <label class="ro-f"><span>Only these resources (--target URNs), one per line</span><textarea class="base-input mono" rows="2" bind:value={value.targets}></textarea></label>
  {/if}
</div>

<style>
  .ro { display: flex; flex-direction: column; gap: 10px; }
  .ro-f { display: flex; flex-direction: column; gap: 5px; }
  .ro-f > span { font-size: 11px; color: var(--text-muted); letter-spacing: .03em; text-transform: uppercase; }
  .ro-f input, .ro-f textarea, .ro-f select { width: 100%; }
  .ro-f textarea { resize: vertical; font-size: 12px; }
  .ro-f--narrow { max-width: 160px; }
  .ro-f small { font-size: 11.5px; line-height: 1.45; }
  .ro-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .ro-grid--3 { grid-template-columns: repeat(3, 1fr); }
  .ro-check { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-primary); cursor: pointer; }
  .ro-check--danger:has(input:checked) { color: var(--danger); font-weight: 600; }
  .seg { display: inline-flex; align-self: flex-start; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); }
  .seg button { display: flex; align-items: center; gap: 5px; padding: 5px 10px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .seg button.is-on { background: var(--brand-soft); color: var(--brand); }
  .ro-tags { display: flex; gap: 6px; flex-wrap: wrap; }
  .chip { padding: 3px 10px; border-radius: 12px; border: 1px solid var(--border); background: none; color: var(--text-secondary); cursor: pointer; font-size: 12px; }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
</style>
