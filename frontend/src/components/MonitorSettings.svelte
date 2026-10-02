<script lang="ts">
  // Monitoring settings: the default alert rules and where alerts are sent.
  import { onMount } from 'svelte'
  import { fade, fly } from 'svelte/transition'
  import { X, Loader2, Plus, Trash2, Send, Check, Bell, TriangleAlert } from '@lucide/svelte'
  import { monitor, errMsg } from '../lib/api'
  import type { RuleRow } from '../lib/monitor'
  import RulesEditor from './RulesEditor.svelte'

  let { onclose }: { onclose: () => void } = $props()

  const KINDS = [
    { id: 'slack', label: 'Slack', hint: 'Incoming webhook URL' },
    { id: 'teams', label: 'Microsoft Teams', hint: 'Incoming webhook URL' },
    { id: 'discord', label: 'Discord', hint: 'Channel webhook URL' },
    { id: 'telegram', label: 'Telegram', hint: 'Bot token from @BotFather' },
    { id: 'webhook', label: 'Webhook (JSON)', hint: 'Any URL — gets {event, text}' },
  ]
  type N = { id: string; kind: string; label: string; url: string; chatId: string }

  let rules = $state<RuleRow[]>([])
  let notifiers = $state<N[]>([])
  let interval = $state(60)
  let loading = $state(true)
  let busy = $state('')
  let error = $state('')
  let saved = $state(false)
  let kind = $state('slack')
  let url = $state('')
  let chatId = $state('')
  let tested = $state('')

  onMount(async () => {
    try {
      const s = await monitor.getSettings({})
      rules = s.defaults.map((r) => ({ metric: r.metric, threshold: r.threshold, minutes: r.minutes }))
      notifiers = s.notifiers.map((n) => ({ id: n.id, kind: n.kind, label: n.label, url: '', chatId: '' }))
      interval = s.interval
    } catch (e) { error = errMsg(e) } finally { loading = false }
  })

  async function test() {
    busy = 'test'; error = ''; tested = ''
    try {
      const r = await monitor.testNotifier({ notifier: { kind, url, chatId } })
      if (r.ok) tested = 'Sent — check the channel.'
      else error = `Test failed: ${r.error}`
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  function add() {
    notifiers = [...notifiers, { id: '', kind, label: KINDS.find((k) => k.id === kind)!.label + ' (new)', url, chatId }]
    url = ''; chatId = ''; tested = ''
  }

  async function save() {
    busy = 'save'; error = ''; saved = false
    try {
      const s = await monitor.saveSettings({ defaults: rules, notifiers: notifiers.map((n) => ({ id: n.id, kind: n.kind, url: n.url, chatId: n.chatId })) })
      notifiers = s.notifiers.map((n) => ({ id: n.id, kind: n.kind, label: n.label, url: '', chatId: '' }))
      saved = true
      setTimeout(() => (saved = false), 1800)
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="drawer-mask" role="presentation" transition:fade={{ duration: 120 }} onclick={(e) => { if (e.target === e.currentTarget) onclose() }}>
  <div class="drawer" role="dialog" aria-modal="true" transition:fly={{ x: 40, duration: 180 }}>
    <header class="drawer__head">
      <div><div class="page-kicker">Monitoring</div><div class="drawer__title">Alerts & notifications</div></div>
      <button class="icon-btn" title="Close (esc)" onclick={onclose}><X size={16} /></button>
    </header>
    <div class="drawer__body">
      {#if loading}
        <div class="empty-state"><Loader2 size={18} class="spin" /></div>
      {:else}
        <div class="blk">
          <div class="blk__title"><TriangleAlert size={14} /> Alert when…</div>
          <p class="muted">For every monitored server (a server can have its own rules on its page). Values are averaged over the minutes; servers are read every {interval} s.</p>
          <RulesEditor bind:value={rules} />
        </div>

        <div class="blk">
          <div class="blk__title"><Bell size={14} /> Send alerts to</div>
          {#each notifiers as n, i (i)}
            <div class="nt"><span class="badge badge--info">{KINDS.find((k) => k.id === n.kind)?.label ?? n.kind}</span><span class="mono nt__label">{n.label}</span>
              <button class="icon-btn" title="Remove" onclick={() => (notifiers = notifiers.filter((_, j) => j !== i))}><Trash2 size={13} /></button></div>
          {:else}
            <p class="muted">Nowhere yet — alerts only show in timika. Add Slack, Teams, Discord, Telegram or a webhook.</p>
          {/each}
          <div class="nt-form">
            <div class="nt-row">
              <select class="base-select" bind:value={kind}>{#each KINDS as k (k.id)}<option value={k.id}>{k.label}</option>{/each}</select>
              <input class="base-input mono" type="password" bind:value={url} placeholder={KINDS.find((k) => k.id === kind)?.hint} autocomplete="off" />
            </div>
            {#if kind === 'telegram'}<input class="base-input mono" bind:value={chatId} placeholder="chat id (e.g. -1001234567890)" />{/if}
            <div class="nt-actions">
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={!url || !!busy} onclick={test}>{#if busy === 'test'}<Loader2 size={13} class="spin" />{:else}<Send size={12} />{/if} Send a test</button>
              {#if tested}<span class="ok"><Check size={13} /> {tested}</span>{/if}
              <span class="spacer"></span>
              <button class="base-btn base-btn--ghost base-btn--sm" disabled={!url} onclick={add}><Plus size={13} /> Add</button>
            </div>
          </div>
        </div>
        {#if error}<div class="notice notice--error">{error}</div>{/if}
      {/if}
    </div>
    <footer class="drawer__foot">
      {#if saved}<span class="ok"><Check size={13} /> Saved</span>{/if}
      <button class="base-btn base-btn--ghost" onclick={onclose}>Close</button>
      <button class="base-btn base-btn--primary" disabled={!!busy || loading} onclick={save}>{#if busy === 'save'}<Loader2 size={14} class="spin" />{/if} Save</button>
    </footer>
  </div>
</div>

<style>
  .drawer-mask { position: fixed; inset: 0; z-index: 900; background: rgba(0, 0, 0, 0.45); backdrop-filter: blur(2px); display: flex; justify-content: flex-end; }
  .drawer { width: min(540px, 100vw); height: 100%; display: flex; flex-direction: column; background: var(--bg-surface); border-left: 1px solid var(--border); box-shadow: var(--shadow-lg); }
  .drawer__head { display: flex; align-items: flex-start; justify-content: space-between; padding: 20px 22px 12px; }
  .drawer__title { font-size: 18px; font-weight: 700; color: var(--text-primary); margin-top: 2px; }
  .drawer__body { flex: 1; overflow-y: auto; padding: 8px 22px 22px; display: flex; flex-direction: column; gap: 14px; }
  .drawer__foot { display: flex; justify-content: flex-end; align-items: center; gap: 8px; padding: 14px 22px; border-top: 1px solid var(--border); }
  .blk { display: flex; flex-direction: column; gap: 8px; padding: 14px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .blk p { margin: 0; font-size: 12px; line-height: 1.5; }
  .blk__title { display: flex; align-items: center; gap: 7px; font-weight: 700; color: var(--text-primary); font-size: 13px; }
  .nt { display: flex; align-items: center; gap: 10px; font-size: 12.5px; }
  .nt__label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .nt-form { display: flex; flex-direction: column; gap: 8px; padding-top: 8px; border-top: 1px solid var(--border); }
  .nt-row { display: flex; gap: 8px; }
  .nt-row select { flex: 0 0 160px; }
  .nt-row input { flex: 1; min-width: 0; }
  .nt-actions { display: flex; gap: 8px; align-items: center; }
  .spacer { flex: 1; }
  .ok { color: var(--success); font-size: 12px; display: inline-flex; gap: 4px; align-items: center; }
</style>
