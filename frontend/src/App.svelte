<script lang="ts">
  import { onMount } from 'svelte'
  import { Search, Moon, Sun, LockOpen, LogOut } from '@lucide/svelte'
  import { sys, authApi } from './lib/api'
  import { session, refreshStatus, clearToken, isAdmin, canReadVault } from './lib/session.svelte'
  import { idle, startIdleWatch, stopIdleWatch, stayActive } from './lib/idle.svelte'
  import { router, isActive, navigate } from './lib/router.svelte'
  import { navGroups, titleFor } from './lib/nav'
  import { ui, syncTheme, toggleTheme, toggleSidebar, confirm } from './lib/ui.svelte'
  import CommandPalette from './components/CommandPalette.svelte'
  import ConfirmModal from './components/ConfirmModal.svelte'
  import Gate from './views/Gate.svelte'
  import Overview from './views/Overview.svelte'
  import Servers from './views/ServersRoute.svelte'
  import Sessions from './views/Sessions.svelte'
  import Terminals from './views/Terminals.svelte'
  import People from './views/People.svelte'
  import Audit from './views/Audit.svelte'
  import Account from './views/Account.svelte'
  import Automation from './views/AutomationRoute.svelte'
  import Monitoring from './views/MonitoringRoute.svelte'
  import Containers from './views/ContainersRoute.svelte'
  import Usage from './views/UsageRoute.svelte'
  import Images from './views/Images.svelte'
  import Volumes from './views/Volumes.svelte'
  import { closeAll } from './lib/terminals.svelte'

  const views = { '/': Overview, '/servers': Servers, '/sessions': Sessions, '/people': People, '/audit': Audit, '/account': Account, '/automation': Automation, '/monitoring': Monitoring, '/containers': Containers, '/usage': Usage, '/images': Images, '/volumes': Volumes } as const
  const onTerminal = $derived(router.path === '/terminal')
  const View = $derived(views[('/' + (router.path.split('/')[1] ?? '')) as keyof typeof views] ?? Overview)
  const title = $derived(titleFor(router.path))
  $effect(() => { document.title = `${title} · Anveesa Timika` })

  // Uninitialized, sealed, or no token → the bare gate screen (like mikko's Login).
  const gated = $derived(!session.status?.initialized || session.status.sealed || !session.token)


  const isMac = /Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent)
  const kbdHint = isMac ? '⌘K' : 'Ctrl K'
  let paletteOpen = $state(false)

  async function sealNow() {
    const ok = await confirm({
      title: 'Seal the vault?',
      message: 'The root key and keyring are dropped from memory. Data stays in Redis, but nothing can be read until the threshold of unseal keys is entered again.',
      confirmText: 'Seal now',
      variant: 'danger',
    })
    if (!ok) return
    await sys.seal({}).catch(() => {})
    await refreshStatus()
  }

  async function logout() {
    // End the session server-side too, not just in this tab.
    closeAll()
    await authApi.revokeSelf({}).catch(() => {})
    clearToken('You have signed out.')
  }

  // SSH-only users have no vault pages: land them on their servers.
  $effect(() => {
    if (!gated && session.me && !canReadVault() && router.path === '/') navigate('/servers')
  })

  // Idle watch only while signed in with an expiring session.
  $effect(() => {
    if (!gated && session.me?.expiresAt) startIdleWatch()
    else stopIdleWatch()
  })

  let lastLoginDismissed = $state(false)
  const fmt = (t: string) => new Date(t).toLocaleString()

  async function probe() {
    await refreshStatus().catch(() => {})
  }

  function onKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k' && !gated) {
      e.preventDefault()
      paletteOpen = true
    }
  }

  onMount(() => {
    syncTheme()
    probe()
    // Another operator may seal the vault — poll so this tab notices.
    const timer = setInterval(probe, 15000)
    return () => clearInterval(timer)
  })
</script>

<svelte:window onkeydown={onKeydown} />

{#if !session.loaded}
  <div class="app-loading">Connecting to vault…</div>
{:else if gated}
  <Gate />
{:else}
  <div class="app-shell">
    <div class="app-body">
      <aside class="sidebar" class:sidebar--collapsed={ui.collapsed}>
        <div class="sidebar__brand">
          <div class="brand-icon">t</div>
          <div class="brand-name">Anveesa <span>Timika</span></div>
        </div>

        <button class="sidebar__search" title="Search ({kbdHint})" onclick={() => (paletteOpen = true)}>
          <Search size={16} />
          <span class="sidebar__search-label">Search…</span>
          <kbd class="sidebar__search-kbd">{kbdHint}</kbd>
        </button>

        <nav class="sidebar__body">
          {#each navGroups as group (group.label)}
            {@const items = group.items.filter((i) => (!i.admin || isAdmin()) && (!i.vault || canReadVault()))}
            {#if items.length}<div class="sidebar__section-label">{group.label}</div>{/if}
            {#each items as item (item.to)}
              <a href="#{item.to}" class="nav-item" class:is-active={isActive(item.to)} title={ui.collapsed ? item.label : undefined}>
                <span class="ico"><item.ico size={16} /></span>
                <span class="nav-item__label">{item.label}</span>
                {#if item.badge?.()}<span class="nav-item__badge">{item.badge()}</span>{/if}
              </a>
            {/each}
          {/each}
        </nav>

        <div class="sidebar__footer">
          <button class="sidebar__collapse-btn" title={ui.collapsed ? 'Expand sidebar' : 'Collapse sidebar'} onclick={toggleSidebar}>
            {ui.collapsed ? '»' : '«'}
          </button>
        </div>
      </aside>

      <main class="main-area">
        <header class="topbar">
          <div class="topbar__title">{title}</div>
          <div class="topbar__actions">
            <button class="topbar__search" title="Search ({kbdHint})" onclick={() => (paletteOpen = true)}>
              <Search size={14} /> Search <kbd class="cmdk-kbd">{kbdHint}</kbd>
            </button>
            <button class="topbar__help" title={ui.theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'} onclick={toggleTheme}>
              {#if ui.theme === 'dark'}<Moon size={16} />{:else}<Sun size={16} />{/if}
            </button>
            {#if isAdmin()}
              <button class="ar-pill ar-pill--dry seal-pill" title="Vault is unsealed — click to seal" onclick={sealNow}>
                <LockOpen size={12} /> unsealed
              </button>
            {/if}
            <div class="user-chip" title={session.me?.username ? `Signed in as ${session.me.username}` : 'Signed in with the root token'}>
              <a class="user-chip__name" href="#/account" title="My account — password and two-factor">{session.me?.username ?? 'root'}</a>
              <span class="user-chip__role" class:user-chip__role--admin={isAdmin()}>
                {session.me?.username ? (session.me.policies[0] ?? 'user') : 'token'}
              </span>
              <button class="user-chip__logout" title="Forget token" onclick={logout}><LogOut size={14} /></button>
            </div>
          </div>
        </header>
        {#if idle.secondsLeft !== null && idle.secondsLeft <= 120}
          <div class="notice notice--warning session-warn" role="alert">
            Your session ends in {Math.max(0, idle.secondsLeft)}s due to inactivity.
            <button class="base-btn base-btn--ghost base-btn--xs" onclick={stayActive}>Stay signed in</button>
          </div>
        {/if}
        {#if session.me?.username && !lastLoginDismissed && (session.me.lastLogin || session.me.lastFailedLogin)}
          <div class="notice notice--info session-warn">
            {#if session.me.lastLogin}Last sign-in: {fmt(session.me.lastLogin.time)} from {session.me.lastLogin.ip ?? 'unknown'}.{/if}
            {#if session.me.lastFailedLogin}&nbsp;Last failed attempt: {fmt(session.me.lastFailedLogin.time)} from {session.me.lastFailedLogin.ip ?? 'unknown'}.{/if}
            &nbsp;Not you? Change your password and tell an administrator.
            <button class="icon-btn" title="Dismiss" onclick={() => (lastLoginDismissed = true)}>×</button>
          </div>
        {/if}
        {#if !onTerminal}
          {#key View}
            <View />
          {/key}
        {/if}
        <!-- Always mounted so open sessions survive navigation. -->
        <Terminals visible={onTerminal} />
      </main>
    </div>


    <CommandPalette bind:open={paletteOpen} onseal={sealNow} />
  </div>
{/if}

<ConfirmModal />

<style>
  .session-warn { flex: 0 0 auto !important; margin: 10px 18px 0; justify-content: space-between; }
  .nav-item__badge { margin-left: auto; min-width: 18px; padding: 0 6px; height: 18px; border-radius: 9px; display: grid; place-items: center; font-size: 10.5px; font-weight: 700; background: var(--brand-soft); color: var(--brand); }
</style>
