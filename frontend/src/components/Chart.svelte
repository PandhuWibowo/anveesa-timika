<script lang="ts">
  // A small time-series chart (SVG, no dependencies): filled lines, gaps where
  // nothing was read, a hover line with the values.
  type S = { label: string; values: number[]; color: string }
  let { times, series, max, format, step = 60, height = 150 }: { times: number[]; series: S[]; max?: number; format: (v: number) => string; step?: number; height?: number } = $props()

  let width = $state(600)
  let hover = $state<number | null>(null)
  const padL = 62, padR = 8, padT = 8, padB = 18

  const t0 = $derived(times[0] ?? 0)
  const t1 = $derived(Math.max(times[times.length - 1] ?? 1, t0 + 1))
  const top = $derived.by(() => {
    if (max) return max
    let m = 0
    for (const s of series) for (const v of s.values) if (!Number.isNaN(v) && v > m) m = v
    if (m <= 0) return 1
    const p = Math.pow(10, Math.floor(Math.log10(m)))
    return Math.ceil((m * 1.1) / p) * p
  })
  const x = (t: number) => padL + ((t - t0) / (t1 - t0)) * (width - padL - padR)
  const y = (v: number) => padT + (1 - Math.min(v, top) / top) * (height - padT - padB)

  /** Runs of consecutive readings (a gap = NaN or a missed interval). */
  function runs(values: number[]): [number, number][][] {
    const out: [number, number][][] = []
    let cur: [number, number][] = []
    for (let i = 0; i < times.length; i++) {
      const v = values[i]
      const gap = i > 0 && times[i] - times[i - 1] > step * 2.5
      if (Number.isNaN(v) || v === undefined || gap) { if (cur.length) out.push(cur); cur = [] }
      if (!Number.isNaN(v) && v !== undefined) cur.push([x(times[i]), y(v)])
    }
    if (cur.length) out.push(cur)
    return out
  }
  const line = (r: [number, number][]) => r.map(([a, b], i) => `${i ? 'L' : 'M'}${a.toFixed(1)},${b.toFixed(1)}`).join('')
  const area = (r: [number, number][]) => `${line(r)}L${r[r.length - 1][0].toFixed(1)},${height - padB}L${r[0][0].toFixed(1)},${height - padB}Z`

  const long = $derived(t1 - t0 > 2 * 86400)
  const tick = (t: number) => new Date(t * 1000).toLocaleString([], long ? { month: 'short', day: 'numeric' } : { hour: '2-digit', minute: '2-digit' })
  const full = (t: number) => new Date(t * 1000).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })

  function move(e: MouseEvent) {
    if (!times.length) return
    const r = (e.currentTarget as SVGElement).getBoundingClientRect()
    const t = t0 + ((e.clientX - r.left - padL) / (width - padL - padR)) * (t1 - t0)
    let best = 0
    for (let i = 1; i < times.length; i++) if (Math.abs(times[i] - t) < Math.abs(times[best] - t)) best = i
    hover = best
  }
  const any = $derived(series.some((s) => s.values.some((v) => !Number.isNaN(v))))
</script>

<div class="ch" bind:clientWidth={width}>
  {#if !any}
    <div class="ch-empty" style="height:{height}px">No readings in this range yet</div>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <svg {width} {height} onmousemove={move} onmouseleave={() => (hover = null)}>
      {#each [0, 0.5, 1] as f (f)}
        <line x1={padL} x2={width - padR} y1={y(top * f)} y2={y(top * f)} class="ch-grid" />
        <text x={padL - 6} y={y(top * f) + 3} class="ch-axis" text-anchor="end">{format(top * f)}</text>
      {/each}
      {#each series as s (s.label)}
        {#each runs(s.values) as r, i (i)}
          {#if r.length > 1}
            <path d={area(r)} fill={s.color} opacity="0.12" />
            <path d={line(r)} fill="none" stroke={s.color} stroke-width="1.6" stroke-linejoin="round" />
          {:else}
            <circle cx={r[0][0]} cy={r[0][1]} r="1.8" fill={s.color} />
          {/if}
        {/each}
      {/each}
      <text x={padL} y={height - 4} class="ch-axis">{tick(t0)}</text>
      <text x={width - padR} y={height - 4} class="ch-axis" text-anchor="end">{tick(t1)}</text>
      {#if hover !== null}
        <line x1={x(times[hover])} x2={x(times[hover])} y1={padT} y2={height - padB} class="ch-hover" />
        {#each series as s (s.label)}
          {#if !Number.isNaN(s.values[hover])}<circle cx={x(times[hover])} cy={y(s.values[hover])} r="3" fill={s.color} stroke="var(--bg-surface)" stroke-width="1.5" />{/if}
        {/each}
      {/if}
    </svg>
    {#if hover !== null}
      <div class="ch-tip" style="left:{Math.min(Math.max(x(times[hover]) + 10, 0), width - 150)}px">
        <div class="ch-tip__t">{full(times[hover])}</div>
        {#each series as s (s.label)}
          <div class="ch-tip__r"><i style="background:{s.color}"></i>{s.label}<b>{Number.isNaN(s.values[hover]) ? '—' : format(s.values[hover])}</b></div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .ch { position: relative; width: 100%; }
  svg { display: block; cursor: crosshair; }
  .ch-grid { stroke: var(--border); stroke-width: 1; stroke-dasharray: 2 3; }
  .ch-axis { fill: var(--text-muted); font-size: 10px; font-family: var(--mono); }
  .ch-hover { stroke: var(--text-muted); stroke-width: 1; }
  .ch-empty { display: grid; place-items: center; color: var(--text-muted); font-size: 12px; }
  .ch-tip { position: absolute; top: 6px; pointer-events: none; min-width: 130px; padding: 6px 9px; border-radius: var(--r-sm); background: var(--bg-surface); border: 1px solid var(--border); box-shadow: var(--shadow-md); font-size: 11.5px; z-index: 2; }
  .ch-tip__t { color: var(--text-muted); margin-bottom: 3px; }
  .ch-tip__r { display: flex; align-items: center; gap: 6px; color: var(--text-secondary); }
  .ch-tip__r i { width: 8px; height: 8px; border-radius: 2px; }
  .ch-tip__r b { margin-left: auto; color: var(--text-primary); font-family: var(--mono); font-weight: 600; }
</style>
