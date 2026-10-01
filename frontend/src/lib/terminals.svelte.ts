// Open web-terminal tabs. They live here, not in a page, so a session keeps
// running while you browse elsewhere in the app.
//
// Wire protocol (backend/src/bastion/session.rs): binary frames are terminal
// bytes both ways; text frames are JSON — {"type":"resize",cols,rows} from us,
// {"type":"status"|"ready"|"error"|"closed", …} from the server.

import type { Terminal } from '@xterm/xterm'
import type { FitAddon } from '@xterm/addon-fit'
import { session } from './session.svelte'
import { navigate } from './router.svelte'

export type TabStatus = 'connecting' | 'live' | 'closed' | 'error'

export type Tab = {
  id: string
  assetId: string
  assetName: string
  account: string
  status: TabStatus
  message: string
  sessionId: string | null
  connectMs: number | null
}

/** The live objects behind a tab (not reactive). */
type Runtime = { term: Terminal; fit: FitAddon; ws: WebSocket | null; el: HTMLElement | null; observer: ResizeObserver | null }

export const terminals = $state({ tabs: [] as Tab[], active: null as string | null })
const runtime = new Map<string, Runtime>()
const ready = new Map<string, Promise<Runtime | null>>()
let seq = 0

// xterm is ~300 KB: it is loaded on demand (and prefetched by the Servers and
// Sessions pages), so the rest of the app starts fast.
let lib: ReturnType<typeof importXterm> | null = null
function importXterm() {
  return Promise.all([
    import('@xterm/xterm'),
    import('@xterm/addon-fit'),
    import('@xterm/addon-webgl'),
    import('@xterm/addon-web-links'),
    import('@xterm/xterm/css/xterm.css'),
  ]).then(([x, f, w, l]) => ({ Terminal: x.Terminal, FitAddon: f.FitAddon, WebglAddon: w.WebglAddon, WebLinksAddon: l.WebLinksAddon }))
}
export const loadXterm = () => (lib ??= importXterm())

const THEME = {
  background: '#0e0f11',
  foreground: '#e4e4e7',
  cursor: '#5cb8a5',
  cursorAccent: '#0e0f11',
  selectionBackground: 'rgba(92, 184, 165, 0.35)',
  black: '#1c1d21', red: '#e88080', green: '#5cb8a5', yellow: '#f2c97d',
  blue: '#6c9eff', magenta: '#c79bf2', cyan: '#6ecbb8', white: '#e4e4e7',
  brightBlack: '#71717a', brightRed: '#f0a0a0', brightGreen: '#7fd3c1', brightYellow: '#f7dba4',
  brightBlue: '#94b8ff', brightMagenta: '#dcbcf7', brightCyan: '#9fe0d3', brightWhite: '#ffffff',
}

const tabOf = (id: string) => terminals.tabs.find((t) => t.id === id)

/** Open a terminal to `account@asset` and show it. */
export function openTerminal(assetId: string, assetName: string, account: string) {
  const id = `t${++seq}`
  terminals.tabs.push({ id, assetId, assetName, account, status: 'connecting', message: 'Connecting…', sessionId: null, connectMs: null })
  terminals.active = id
  ready.set(id, loadXterm().then((x) => {
    if (!tabOf(id)) return null // closed while loading
    const term = new x.Terminal({
      theme: THEME,
      fontFamily: "'JetBrains Mono', 'SF Mono', Menlo, Consolas, monospace",
      fontSize: 13,
      lineHeight: 1.15,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: true,
    })
    const fit = new x.FitAddon()
    term.loadAddon(fit)
    term.loadAddon(new x.WebLinksAddon())
    const rt: Runtime = { term, fit, ws: null, el: null, observer: null }
    runtime.set(id, rt)
    return rt
  }))
  navigate('/terminal')
}

/** Svelte action: attach a tab's terminal to its element, then connect. */
export function mountTerminal(el: HTMLElement, id: string) {
  ready.get(id)?.then((rt) => rt && attach(el, id, rt))
}

async function attach(el: HTMLElement, id: string, rt: Runtime) {
  const x = await loadXterm()
  rt.el = el
  rt.term.open(el)
  try {
    const gl = new x.WebglAddon()
    gl.onContextLoss(() => gl.dispose())
    rt.term.loadAddon(gl)
  } catch { /* no WebGL: the DOM renderer is fine */ }
  rt.fit.fit()
  rt.observer = new ResizeObserver(() => {
    // Hidden tabs have no size; fitting them would shrink the remote terminal.
    if (el.offsetWidth > 0 && el.offsetHeight > 0) rt.fit.fit()
  })
  rt.observer.observe(el)
  rt.term.onData((d) => send(id, new TextEncoder().encode(d)))
  rt.term.onBinary((d) => send(id, Uint8Array.from(d, (c) => c.charCodeAt(0))))
  rt.term.onResize(({ cols, rows }) => send(id, JSON.stringify({ type: 'resize', cols, rows })))
  connect(id)
}

function send(id: string, data: string | Uint8Array) {
  const ws = runtime.get(id)?.ws
  if (ws?.readyState === WebSocket.OPEN) ws.send(data)
}

function connect(id: string) {
  const tab = tabOf(id)
  const rt = runtime.get(id)
  if (!tab || !rt || !session.token) return
  tab.status = 'connecting'
  tab.message = 'Connecting…'
  tab.sessionId = null
  const q = new URLSearchParams({ asset: tab.assetId, account: tab.account, cols: String(rt.term.cols), rows: String(rt.term.rows) })
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/v1/bastion/connect?${q}`
  // Browsers can't set headers on a WebSocket: the token rides in the subprotocol list.
  const ws = new WebSocket(url, ['timika', session.token])
  ws.binaryType = 'arraybuffer'
  rt.ws = ws
  let opened = false
  ws.onopen = () => { opened = true }
  ws.onmessage = (m) => {
    if (typeof m.data !== 'string') {
      rt.term.write(new Uint8Array(m.data as ArrayBuffer))
      return
    }
    const ev = JSON.parse(m.data) as { type: string; message?: string; session?: string; connect_ms?: number; first_seen?: boolean; host_key?: string }
    const t = tabOf(id)
    if (!t) return
    if (ev.type === 'status') {
      t.message = ev.message ?? ''
    } else if (ev.type === 'ready') {
      t.status = 'live'
      t.sessionId = ev.session ?? null
      t.connectMs = ev.connect_ms ?? null
      t.message = ev.first_seen ? `Connected — host key ${ev.host_key} trusted on first use` : 'Connected · recorded'
      rt.term.focus()
    } else if (ev.type === 'error') {
      t.status = 'error'
      t.message = ev.message ?? 'error'
      rt.term.writeln(`\r\n\x1b[31m✖ ${ev.message}\x1b[0m`)
    } else if (ev.type === 'closed') {
      if (t.status !== 'error') t.status = 'closed'
      t.message = ev.message ?? 'closed'
      rt.term.writeln(`\r\n\x1b[90m— ${ev.message} —\x1b[0m`)
    }
  }
  ws.onclose = () => {
    const t = tabOf(id)
    if (!t || rt.ws !== ws) return
    rt.ws = null
    if (t.status === 'connecting' || t.status === 'live') {
      t.status = opened ? 'closed' : 'error'
      t.message = opened ? 'Connection lost' : 'Could not open the terminal — access was refused or your session expired'
      rt.term.writeln(`\r\n\x1b[90m— ${t.message} —\x1b[0m`)
    }
  }
}

export function reconnect(id: string) {
  const rt = runtime.get(id)
  if (!rt) return
  rt.ws?.close()
  rt.term.writeln('\r\n\x1b[90m— reconnecting —\x1b[0m')
  connect(id)
}

export function closeTab(id: string) {
  const rt = runtime.get(id)
  rt?.ws?.close()
  rt?.observer?.disconnect()
  rt?.term.dispose()
  runtime.delete(id)
  ready.delete(id)
  const i = terminals.tabs.findIndex((t) => t.id === id)
  if (i >= 0) terminals.tabs.splice(i, 1)
  if (terminals.active === id) terminals.active = terminals.tabs[Math.min(i, terminals.tabs.length - 1)]?.id ?? null
}

export function focusTab(id: string) {
  terminals.active = id
  // After the tab becomes visible: size it to its pane and focus.
  requestAnimationFrame(() => {
    const rt = runtime.get(id)
    if (rt?.el && rt.el.offsetWidth > 0) rt.fit.fit()
    rt?.term.focus()
  })
}

/** Signing out closes every terminal. */
export function closeAll() {
  for (const t of [...terminals.tabs]) closeTab(t.id)
}

export const liveCount = () => terminals.tabs.filter((t) => t.status === 'live' || t.status === 'connecting').length
