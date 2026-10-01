// Shell-level UI state: theme, sidebar collapse, and the confirm modal.
// Mirrors mikko's useTheme / useSidebar / useConfirm composables as runes.

type Theme = 'dark' | 'light'

function load(key: string): string | null {
  try { return localStorage.getItem(key) } catch { return null }
}
function save(key: string, v: string) {
  try { localStorage.setItem(key, v) } catch { /* private mode */ }
}

export const ui = $state({
  theme: 'dark' as Theme,
  collapsed: load('timika-sidebar') === '1',
})

function applyTheme() {
  document.documentElement.setAttribute('data-theme', ui.theme)
  save('timika-theme', ui.theme)
}

export function syncTheme() {
  const saved = load('timika-theme')
  ui.theme = saved === 'light' || saved === 'dark'
    ? saved
    : window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
  applyTheme()
}

export function toggleTheme() {
  ui.theme = ui.theme === 'dark' ? 'light' : 'dark'
  applyTheme()
}

export function toggleSidebar() {
  ui.collapsed = !ui.collapsed
  save('timika-sidebar', ui.collapsed ? '1' : '0')
}

// ─── Confirm modal (promise-based, one at a time) ───────────────────────────
export type ConfirmOpts = {
  title: string
  message: string
  subject?: string
  confirmText?: string
  cancelText?: string
  variant?: 'default' | 'danger' | 'warning'
}

export const confirmState = $state({ open: false, opts: { title: '', message: '' } as ConfirmOpts })
let resolver: ((ok: boolean) => void) | null = null

export function confirm(opts: ConfirmOpts): Promise<boolean> {
  resolver?.(false)
  confirmState.opts = opts
  confirmState.open = true
  return new Promise((r) => { resolver = r })
}

export function respond(ok: boolean) {
  confirmState.open = false
  resolver?.(ok)
  resolver = null
}
