<script lang="ts">
  // Recording player: streams the asciicast v2 recording and replays it into a
  // read-only terminal with its original timing (long pauses shortened).
  import { onMount, onDestroy } from 'svelte'
  import { fade, scale } from 'svelte/transition'
  import { X, Play, Pause, RotateCcw, Loader2 } from '@lucide/svelte'
  import type { Terminal } from '@xterm/xterm'
  import { bastion, errMsg } from '../lib/api'
  import { loadXterm } from '../lib/terminals.svelte'
  import type { Session, Command } from '../gen/timika/v1/bastion_pb'

  /** `startAt`: seconds into the session (e.g. when a command ran) to start from. */
  let { session, onclose, startAt }: { session: Session; onclose: () => void; startAt?: number } = $props()

  /** t = position on the (pause-compressed) timeline, real = seconds since start. */
  type Ev = { t: number; real: number; kind: string; data: string }
  const IDLE_CAP = 2 // seconds: compress longer pauses

  let host = $state<HTMLDivElement>()
  let term: Terminal | null = null
  let events: Ev[] = []
  let loading = $state(true)
  let error = $state('')
  let playing = $state(false)
  let speed = $state(1)
  let pos = $state(0) // seconds into the (compressed) timeline
  let total = $state(0)
  let idx = 0
  let raf = 0
  let last = 0

  onMount(async () => {
    const x = await loadXterm()
    term = new x.Terminal({
      theme: { background: '#0e0f11', foreground: '#e4e4e7' },
      fontFamily: "'JetBrains Mono', 'SF Mono', Menlo, Consolas, monospace",
      fontSize: 13,
      cols: Math.max(20, session.cols),
      rows: Math.max(5, session.rows),
      disableStdin: true,
      cursorBlink: false,
    })
    term.open(host!)
    try {
      let text = ''
      const dec = new TextDecoder()
      for await (const c of bastion.getRecording({ id: session.id })) text += dec.decode(c.data, { stream: true })
      const [, ...lines] = text.split('\n').filter(Boolean)
      let prev = 0
      let shifted = 0
      for (const l of lines) {
        const [t, kind, data] = JSON.parse(l) as [number, string, string]
        shifted += Math.min(t - prev, IDLE_CAP)
        prev = t
        events.push({ t: shifted, real: t, kind, data })
      }
      total = events.at(-1)?.t ?? 0
      loading = false
      if (startAt !== undefined) seek(Math.max(0, timelineAt(startAt) - 1.5))
      play()
    } catch (e) {
      error = errMsg(e)
      loading = false
    }
  })

  onDestroy(() => {
    cancelAnimationFrame(raf)
    term?.dispose()
  })

  function step(now: number) {
    if (!playing || !term) return
    pos = Math.min(total, pos + ((now - last) / 1000) * speed)
    last = now
    while (idx < events.length && events[idx].t <= pos) {
      const e = events[idx++]
      if (e.kind === 'o') term.write(e.data)
      else if (e.kind === 'r') {
        const [c, r] = e.data.split('x').map(Number)
        if (c && r) term.resize(c, r)
      }
    }
    if (idx >= events.length) { playing = false; return }
    raf = requestAnimationFrame(step)
  }

  function play() {
    if (idx >= events.length) restart()
    playing = true
    last = performance.now()
    raf = requestAnimationFrame(step)
  }
  function pause() { playing = false; cancelAnimationFrame(raf) }
  function restart() {
    pause()
    term?.reset()
    idx = 0
    pos = 0
  }
  function seek(to: number) {
    const was = playing
    restart()
    pos = to
    while (idx < events.length && events[idx].t <= pos) {
      const e = events[idx++]
      if (e.kind === 'o') term?.write(e.data)
    }
    if (was) play()
  }
  /** Real session time → position on the compressed timeline. */
  function timelineAt(real: number) {
    const e = events.find((x) => x.real >= real)
    return e ? e.t : total
  }

  // The session's commands, to jump between them.
  let commands = $state<Command[]>([])
  let current = $derived.by(() => {
    let idx = -1
    for (let i = 0; i < commands.length; i++) if (timelineAt(commands[i].offset) <= pos + 0.05) idx = i
    return idx
  })
  onMount(() => {
    if (session.commands > 0) {
      bastion.listCommands({ session: session.id, limit: 1000 })
        .then((r) => (commands = r.commands.sort((a, b) => a.offset - b.offset)))
        .catch(() => {})
    }
  })
  const jump = (c: Command) => seek(Math.max(0, timelineAt(c.offset) - 0.5))

  const fmt = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`
</script>

<svelte:window onkeydown={(e) => {
  if (e.key === 'Escape') onclose()
  if (e.key === ' ' && !loading) { e.preventDefault(); if (playing) pause(); else play() }
}} />

<div class="rp-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="rp" role="dialog" aria-modal="true" transition:scale={{ duration: 150, start: 0.97 }}>
    <header class="rp__head">
      <div>
        <div class="rp__title"><span class="mono">{session.account}@{session.assetName}</span> · {session.user}</div>
        <div class="muted rp__sub">{new Date(session.startedAt).toLocaleString()} · {session.status}{session.clientIp ? ` · from ${session.clientIp}` : ''}</div>
      </div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>
    <div class="rp__main">
      <div class="rp__screen">
        <div bind:this={host}></div>
        {#if loading}<div class="rp__overlay"><Loader2 size={20} class="spin" /></div>{/if}
        {#if error}<div class="rp__overlay"><div class="notice notice--error">{error}</div></div>{/if}
      </div>
      {#if commands.length}
        <aside class="rp__cmds">
          <div class="rp__cmds-head">Commands <span class="muted">{commands.length}</span></div>
          {#each commands as c, i (i)}
            <button class="rp__cmd" class:is-current={i === current} class:rp__cmd--high={c.risk === 'high'} class:rp__cmd--med={c.risk === 'medium'} onclick={() => jump(c)}>
              <span class="rp__cmd-t">{fmt(c.offset)}</span>
              <span class="mono rp__cmd-text">{c.command}</span>
            </button>
          {/each}
        </aside>
      {/if}
    </div>
    <footer class="rp__controls">
      <button class="icon-btn" title={playing ? 'Pause (space)' : 'Play (space)'} disabled={loading} onclick={() => (playing ? pause() : play())}>
        {#if playing}<Pause size={15} />{:else}<Play size={15} />{/if}
      </button>
      <button class="icon-btn" title="Restart" disabled={loading} onclick={() => { restart(); play() }}><RotateCcw size={14} /></button>
      <span class="mono rp__time">{fmt(pos)} / {fmt(total)}</span>
      <input class="rp__bar" type="range" min="0" max={total || 1} step="0.1" value={pos} disabled={loading}
             oninput={(e) => seek(Number((e.target as HTMLInputElement).value))} />
      <div class="rp__speed">
        {#each [1, 2, 4, 8] as s (s)}
          <button class:is-on={speed === s} onclick={() => (speed = s)}>{s}×</button>
        {/each}
      </div>
    </footer>
  </div>
</div>

<style>
  .rp-mask { position: fixed; inset: 0; z-index: 900; background: rgba(0, 0, 0, 0.55); display: grid; place-items: center; padding: 24px; }
  .rp { width: min(1100px, 100%); max-height: 100%; display: flex; flex-direction: column; background: var(--bg-surface); border: 1px solid var(--border); border-radius: var(--r-lg); box-shadow: var(--shadow-lg); overflow: hidden; }
  .rp__head { display: flex; justify-content: space-between; align-items: flex-start; padding: 14px 16px; border-bottom: 1px solid var(--border); }
  .rp__title { font-weight: 700; color: var(--text-primary); }
  .rp__sub { font-size: 12px; margin-top: 2px; }
  .rp__main { display: flex; min-height: 0; }
  .rp__screen { position: relative; flex: 1; min-width: 0; background: #0e0f11; padding: 10px; overflow: auto; min-height: 240px; }
  .rp__cmds { width: 260px; flex: 0 0 auto; border-left: 1px solid var(--border); overflow-y: auto; max-height: 70vh; background: var(--bg-surface); }
  .rp__cmds-head { position: sticky; top: 0; padding: 10px 12px; font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: .05em; color: var(--text-secondary); background: var(--bg-surface); border-bottom: 1px solid var(--border); }
  .rp__cmd { display: flex; gap: 8px; width: 100%; padding: 6px 12px; border: 0; border-left: 2px solid transparent; background: none; text-align: left; cursor: pointer; color: var(--text-secondary); font-size: 12px; }
  .rp__cmd:hover { background: var(--bg-hover); color: var(--text-primary); }
  .rp__cmd.is-current { border-left-color: var(--brand); background: var(--brand-dim); color: var(--text-primary); }
  .rp__cmd--med .rp__cmd-text { color: var(--warning); }
  .rp__cmd--high .rp__cmd-text { color: var(--danger); font-weight: 600; }
  .rp__cmd-t { font-family: var(--mono); font-size: 11px; color: var(--text-muted); flex: 0 0 auto; padding-top: 1px; }
  .rp__cmd-text { word-break: break-all; }
  @media (max-width: 800px) { .rp__cmds { display: none; } }
  .rp__overlay { position: absolute; inset: 0; display: grid; place-items: center; }
  .rp__controls { display: flex; align-items: center; gap: 10px; padding: 10px 14px; border-top: 1px solid var(--border); }
  .rp__time { font-size: 12px; color: var(--text-muted); white-space: nowrap; }
  .rp__bar { flex: 1; accent-color: var(--brand); }
  .rp__speed { display: inline-flex; padding: 2px; border-radius: var(--r); background: var(--bg-elevated); border: 1px solid var(--border); }
  .rp__speed button { padding: 3px 8px; border: 0; border-radius: var(--r-sm); background: none; color: var(--text-muted); font-size: 12px; cursor: pointer; }
  .rp__speed button.is-on { background: var(--brand-soft); color: var(--brand); }
</style>
