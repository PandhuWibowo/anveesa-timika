<script lang="ts">
  // Alert rules: tick a metric, set its threshold and for how long.
  import { RULES, type RuleRow } from '../lib/monitor'

  let { value = $bindable() }: { value: RuleRow[] } = $props()

  const find = (id: string) => value.find((r) => r.metric === id)

  function toggle(id: string) {
    const d = RULES.find((r) => r.id === id)!
    value = find(id) ? value.filter((r) => r.metric !== id) : [...value, { metric: id, threshold: d.threshold, minutes: d.minutes }]
  }
  function set(id: string, field: 'threshold' | 'minutes', raw: string, scale = 1) {
    const n = Number(raw)
    if (!Number.isFinite(n)) return
    value = value.map((r) => (r.metric === id ? { ...r, [field]: field === 'threshold' ? n * scale : Math.round(n) } : r))
  }
</script>

<div class="re">
  {#each RULES as d (d.id)}
    {@const r = find(d.id)}
    <div class="re-row" class:re-row--off={!r}>
      <label class="re-name"><input type="checkbox" checked={!!r} onchange={() => toggle(d.id)} /> {d.label}</label>
      {#if r}
        {#if d.id !== 'status'}
          <input class="base-input mono re-num" type="number" min="0" step="any" value={r.threshold / d.scale} oninput={(e) => set(d.id, 'threshold', e.currentTarget.value, d.scale)} />
          <span class="re-unit">{d.unit}</span>
        {:else}
          <span class="re-hint muted">{d.hint}</span>
        {/if}
        <span class="muted">for</span>
        <input class="base-input mono re-num re-num--min" type="number" min="1" max="1440" value={r.minutes} oninput={(e) => set(d.id, 'minutes', e.currentTarget.value)} />
        <span class="muted">min</span>
      {/if}
    </div>
  {/each}
</div>

<style>
  .re { display: flex; flex-direction: column; }
  .re-row { display: flex; align-items: center; gap: 8px; min-height: 34px; font-size: 12.5px; border-top: 1px solid var(--border); padding: 3px 0; }
  .re-row:first-child { border-top: 0; }
  .re-row--off { color: var(--text-muted); }
  .re-name { flex: 1; display: flex; align-items: center; gap: 8px; cursor: pointer; color: inherit; }
  .re-row:not(.re-row--off) .re-name { color: var(--text-primary); }
  .re-num { width: 74px; padding: 4px 8px; text-align: right; }
  .re-num--min { width: 62px; }
  .re-unit { width: 34px; color: var(--text-muted); }
  .re-hint { font-size: 11.5px; }
</style>
