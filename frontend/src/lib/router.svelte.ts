// Minimal hash router — the app has a handful of top-level pages, so a
// dependency isn't worth it. `#/servers/web-1/files` → path `/servers/web-1/files`.

function current(): string {
  return decodeURI(location.hash.replace(/^#/, '')) || '/'
}

export const router = $state({ path: current() })

window.addEventListener('hashchange', () => { router.path = current() })

export function navigate(path: string) {
  location.hash = encodeURI(path)
}

export function isActive(to: string): boolean {
  return to === '/' ? router.path === '/' : router.path === to || router.path.startsWith(to + '/')
}
