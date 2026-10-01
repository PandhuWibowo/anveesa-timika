<script lang="ts">
  // Where run alerts go: Slack, Teams, Discord, Telegram or any webhook.
  import { Bell, Plus, Trash2, Loader2, Send, Check } from '@lucide/svelte'
  import { automation, errMsg } from '../lib/api'
  import type { Repo } from '../gen/timika/v1/automation_pb'

  let { repo, onsaved }: { repo: Repo; onsaved: (r: Repo) => void } = $props()

  const KINDS = [
    { id: 'slack', label: 'Slack', hint: 'Incoming webhook URL (https://hooks.slack.com/services/…)' },
    { id: 'teams', label: 'Microsoft Teams', hint: 'Incoming webhook URL' },
    { id: 'discord', label: 'Discord', hint: 'Channel webhook URL (https://discord.com/api/webhooks/…)' },
    { id: 'telegram', label: 'Telegram', hint: 'Bot token from @BotFather' },
    { id: 'webhook', label: 'Webhook (JSON)', hint: 'Any URL — gets {event, text, run}' },
  ]
  const EVENTS = [
    { id: 'failed', label: 'Failed' }, { id: 'drift', label: 'Drift found' }, { id: 'approval', label: 'Needs approval' }, { id: 'succeeded', label: 'Succeeded' },
  ]

  let adding = $state(false)
  let kind = $state('slack')
  let url = $state('')
  let chatId = $state('')
  let on = $state<string[]>(['failed', 'drift', 'approval'])
  let busy = $state('')
  let error = $state('')
  let tested = $state('')

  const existing = () => repo.notifiers.map((n) => ({ id: n.id, kind: n.kind, on: n.on, url: '', chatId: '' }))

  async function save(list: ReturnType<typeof existing>) {
    busy = 'save'
    error = ''
    try {
      onsaved(await automation.saveNotifiers({ repo: repo.id, notifiers: list }))
      adding = false
      url = ''
      chatId = ''
      tested = ''
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  async function test() {
    busy = 'test'
    error = ''
    tested = ''
    try {
      const r = await automation.testNotifier({ repo: repo.id, notifier: { kind, url, chatId, on } })
      if (r.ok) tested = 'Sent — check the channel.'
      else error = `Test failed: ${r.error}`
    } catch (e) { error = errMsg(e) } finally { busy = '' }
  }

  const toggle = (e: string) => (on = on.includes(e) ? on.filter((x) => x !== e) : [...on, e])
</script>

<div class="setup__block">
  <div class="setup__title"><Bell size={14} /> Notifications</div>
  {#each repo.notifiers as n (n.id)}
    <div class="nt">
      <span class="badge badge--info">{KINDS.find((k) => k.id === n.kind)?.label ?? n.kind}</span>
      <span class="mono nt__label">{n.label}</span>
      <span class="muted nt__on">{n.on.map((e) => EVENTS.find((x) => x.id === e)?.label ?? e).join(' · ')}</span>
      <button class="icon-btn" title="Remove" disabled={!!busy} onclick={() => save(existing().filter((x) => x.id !== n.id))}><Trash2 size={13} /></button>
    </div>
  {:else}
    {#if !adding}<p class="muted">Get told in chat when a run fails, a scheduled plan finds drift, or an apply waits for approval.</p>{/if}
  {/each}

  {#if adding}
    <div class="nt-form">
      <div class="nt-row">
        <select class="base-select" bind:value={kind}>{#each KINDS as k (k.id)}<option value={k.id}>{k.label}</option>{/each}</select>
        <input class="base-input mono" type="password" bind:value={url} placeholder={KINDS.find((k) => k.id === kind)?.hint} autocomplete="off" />
        {#if kind === 'telegram'}<input class="base-input mono nt-chat" bind:value={chatId} placeholder="chat id" />{/if}
      </div>
      <div class="nt-events">
        <span class="muted">When:</span>
        {#each EVENTS as e (e.id)}<button class="chip" class:is-on={on.includes(e.id)} onclick={() => toggle(e.id)}>{e.label}</button>{/each}
      </div>
      <div class="nt-actions">
        <button class="base-btn base-btn--ghost base-btn--sm" disabled={!url || !!busy} onclick={test}>{#if busy === 'test'}<Loader2 size={13} class="spin" />{:else}<Send size={12} />{/if} Send a test</button>
        {#if tested}<span class="nt-ok"><Check size={13} /> {tested}</span>{/if}
        <span class="nt-spacer"></span>
        <button class="base-btn base-btn--ghost base-btn--sm" onclick={() => { adding = false; error = '' }}>Cancel</button>
        <button class="base-btn base-btn--primary base-btn--sm" disabled={!url || !on.length || !!busy} onclick={() => save([...existing(), { id: '', kind, on, url, chatId }])}>{#if busy === 'save'}<Loader2 size={13} class="spin" />{/if} Add</button>
      </div>
    </div>
  {:else}
    <div><button class="base-btn base-btn--ghost base-btn--sm" onclick={() => (adding = true)}><Plus size={13} /> Add a notification</button></div>
  {/if}
  {#if error}<div class="notice notice--error">{error}</div>{/if}
</div>

<style>
  .setup__block { display: flex; flex-direction: column; gap: 8px; padding: 14px; border-radius: var(--r); background: var(--bg-body); border: 1px solid var(--border); }
  .setup__block p { margin: 0; font-size: 12.5px; }
  .setup__title { display: flex; align-items: center; gap: 7px; font-weight: 700; color: var(--text-primary); font-size: 13px; }
  .nt { display: flex; align-items: center; gap: 10px; font-size: 12.5px; padding: 6px 0; border-top: 1px solid var(--border); }
  .nt__label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .nt__on { font-size: 11.5px; }
  .nt-form { display: flex; flex-direction: column; gap: 8px; padding: 10px; border-radius: var(--r); background: var(--bg-surface); border: 1px solid var(--border); }
  .nt-row { display: flex; gap: 8px; }
  .nt-row select { flex: 0 0 170px; }
  .nt-row input { flex: 1; min-width: 0; }
  .nt-row .nt-chat { flex: 0 0 150px; }
  .nt-events { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; font-size: 12px; }
  .nt-actions { display: flex; gap: 8px; align-items: center; }
  .nt-spacer { flex: 1; }
  .nt-ok { color: var(--success); font-size: 12px; display: inline-flex; gap: 4px; align-items: center; }
  .chip { padding: 3px 10px; border-radius: 12px; border: 1px solid var(--border); background: none; color: var(--text-secondary); cursor: pointer; font-size: 12px; }
  .chip.is-on { border-color: var(--brand-ring); background: var(--brand-dim); color: var(--brand); }
</style>
