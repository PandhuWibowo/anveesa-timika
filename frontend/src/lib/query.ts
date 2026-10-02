// Server-state cache (TanStack Query): pages show what they last had at once
// and refresh in the background; identical requests are shared; polling stops
// while the tab is hidden and resumes on focus / reconnect.
//
// Memory only — nothing is written to browser storage (this is a secrets
// manager) — and emptied whenever the identity changes: sign-in, sign-out,
// seal.
import { QueryClient } from '@tanstack/svelte-query'
import { ConnectError } from '@connectrpc/connect'

const network = (e: unknown) =>
  (e instanceof ConnectError && /failed to fetch|networkerror|load failed|network connection/i.test(e.rawMessage)) ||
  (e instanceof TypeError && /fetch|network/i.test(e.message))

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Fresh for a moment (tabs of one page share a request), kept 10 minutes after the last use.
      staleTime: 2_000,
      gcTime: 10 * 60_000,
      refetchOnWindowFocus: true,
      refetchOnReconnect: true,
      refetchIntervalInBackground: false,
      // Only a network blip is worth retrying; "permission denied" is an answer.
      retry: (failures, error) => network(error) && failures < 2,
    },
  },
})

const alsoClear: (() => void)[] = []
/** Other in-memory state to drop with the cache. */
export const onClear = (f: () => void) => { alsoClear.push(f) }

/** Forget everything (another person may use this browser next). */
export function clearCache() {
  queryClient.cancelQueries()
  queryClient.clear()
  for (const f of alsoClear) f()
}

/** Query keys, in one place so invalidation can't drift from the queries. */
export const keys = {
  assets: ['assets'] as const,
  sessions: ['sessions'] as const,
  users: ['users'] as const,
  grants: ['grants'] as const,
  systems: ['monitor', 'systems'] as const,
  containers: ['monitor', 'containers'] as const,
  container: (asset: string, name: string) => ['docker', 'container', asset, name] as const,
  containerFiles: (asset: string, name: string, path: string) => ['docker', 'files', asset, name, path] as const,
  images: (asset: string) => ['docker', 'images', asset] as const,
  volumes: (asset: string) => ['docker', 'volumes', asset] as const,
  containerUsage: (asset: string, name: string, range: string) => ['monitor', 'container', asset, name, range] as const,
  system: (id: string, range: string) => ['monitor', 'system', id, range] as const,
  repos: ['automation', 'repos'] as const,
  runs: (repo = '') => ['automation', 'runs', repo] as const,
  repo: (id: string) => ['automation', 'repo', id] as const,
  schedules: (repo: string) => ['automation', 'schedules', repo] as const,
}
