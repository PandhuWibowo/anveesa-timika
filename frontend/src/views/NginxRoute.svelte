<script lang="ts">
  // #/nginx → every nginx found (one: straight into it) · #/nginx/<server>/<container | -> → one nginx.
  import { Loader2, Globe, Container as Box, Server } from '@lucide/svelte'
  import { createQuery } from '@tanstack/svelte-query'
  import { keys } from '../lib/query'
  import { nginxApi, errMsg } from '../lib/api'
  import { router, navigate } from '../lib/router.svelte'
  import NginxInstance from './NginxInstance.svelte'

  const parts = $derived(router.path.split('/').map(decodeURIComponent))
  const picked = $derived(!!parts[2])
  // From the last monitoring reading: no SSH, so this is instant.
  const list = createQuery(() => ({ queryKey: keys.nginxList, queryFn: () => nginxApi.listInstances({}), refetchInterval: 30000 }))
  const all = $derived(list.data?.instances ?? [])
  const only = $derived(!picked && all.length === 1 ? all[0] : null)
  const open = (asset: string, container: string) => navigate(`/nginx/${encodeURIComponent(asset)}/${container || '-'}`)
</script>

{#if picked}
  {#key `${parts[2]}/${parts[3]}`}<NginxInstance asset={parts[2]} container={parts[3] && parts[3] !== '-' ? parts[3] : ''} back={all.length !== 1} />{/key}
{:else if only}
  {#key `${only.asset}/${only.container}`}<NginxInstance asset={only.asset} container={only.container} back={false} />{/key}
{:else}
  <div class="page-shell">
    <div class="page-scroll">
      <div class="page-stack">
        <section class="page-hero">
          <div class="page-hero__content">
            <div class="page-kicker">Web</div>
            <h1 class="page-title">Nginx</h1>
            <p class="page-subtitle">Every nginx on your monitored servers — installed on the server or running in a container. Open one for its sites, config, certificates and logs.</p>
          </div>
          <div class="page-metrics">
            <div class="page-metric"><span class="page-metric__value">{all.length}</span><span class="page-metric__label">Found</span></div>
            <div class="page-metric"><span class="page-metric__value">{all.filter((i) => i.running).length}</span><span class="page-metric__label">Running</span></div>
          </div>
        </section>
        {#if list.error && !list.data}<div class="notice notice--error">{errMsg(list.error)}</div>{/if}
        {#if list.isPending}
          <div class="empty-state"><Loader2 size={20} class="spin" /></div>
        {:else if !all.length}
          <section class="page-card"><div class="empty-state nx-empty">
            <Globe size={28} />
            <div class="nx-empty__t">No nginx found yet</div>
            {#if list.data?.monitored}
              <div class="muted">None of your {list.data.monitored} monitored server{list.data.monitored === 1 ? '' : 's'} has nginx installed or runs it in a container. It shows up here by itself within a minute of being installed.</div>
            {:else}
              <div class="muted">nginx is found on <b>monitored</b> servers. Turn monitoring on for a server and it appears here by itself.</div>
              <button class="base-btn base-btn--primary" onclick={() => navigate('/monitoring')}>Monitoring → Systems</button>
            {/if}
          </div></section>
        {:else}
          <div class="nx-grid">
            {#each all as i (i.asset + '/' + i.container)}
              <button class="nx-card" onclick={() => open(i.asset, i.container)}>
                <span class="nx-card__ico" class:is-on={i.running}><Globe size={17} /></span>
                <span class="nx-card__id">
                  <span class="nx-card__name">{i.system}</span>
                  <span class="nx-card__sub">{#if i.container}<Box size={11} /> in container <span class="mono">{i.container}</span>{:else}<Server size={11} /> on the server{/if}</span>
                </span>
                <span class="nx-card__meta">
                  <span class="badge badge--{i.running ? 'success' : 'default'}">{i.running ? 'running' : 'stopped'}</span>
                  <span class="mono muted">{i.version || i.image}</span>
                </span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .nx-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 12px; }
  .nx-card { display: flex; align-items: center; gap: 12px; padding: 14px; border-radius: var(--r-lg); background: var(--bg-surface); border: 1px solid var(--border); box-shadow: var(--shadow-sm); text-align: left; color: inherit; font: inherit; cursor: pointer; }
  .nx-card:hover { background: var(--bg-hover); }
  .nx-card__ico { width: 32px; height: 32px; border-radius: var(--r); display: grid; place-items: center; background: var(--bg-elevated); color: var(--text-muted); flex: 0 0 auto; }
  .nx-card__ico.is-on { background: var(--brand-soft); color: var(--brand); }
  .nx-card__id { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
  .nx-card__name { font-weight: 700; color: var(--text-primary); }
  .nx-card__sub { font-size: 11.5px; color: var(--text-muted); }
  .nx-card__sub :global(svg) { vertical-align: -1px; }
  .nx-card__meta { display: flex; flex-direction: column; align-items: flex-end; gap: 4px; font-size: 11px; max-width: 45%; overflow: hidden; white-space: nowrap; }
  .nx-empty { gap: 10px; padding: 36px 20px; text-align: center; }
  .nx-empty__t { font-size: 15px; font-weight: 700; color: var(--text-primary); }
</style>
