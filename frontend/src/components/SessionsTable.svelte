<script lang="ts">
  // Recorded terminal sessions as a table: replay, download, end (admins).
  import { Play, Square, Download, Loader2 } from '@lucide/svelte'
  import { bastion, errMsg } from '../lib/api'
  import { isAdmin } from '../lib/session.svelte'
  import { confirm } from '../lib/ui.svelte'
  import { navigate } from '../lib/router.svelte'
  import type { Session } from '../gen/timika/v1/bastion_pb'
  import Replay from './Replay.svelte'

  let { sessions, showServer = true, empty = 'No sessions yet.', onchanged }: {
    sessions: Session[]
    showServer?: boolean
    empty?: string
    onchanged?: () => void
  } = $props()

  let replaying = $state<Session | null>(null)
  let downloading = $state('')
  let error = $state('')

  const badge = (s: string) =>
    s === 'live' ? 'success' : s === 'connecting' ? 'info' : s === 'killed' || s === 'failed' ? 'danger' : s === 'lost' ? 'warning' : 'default'

  function duration(s: Session) {
    const end = s.endedAt ? new Date(s.endedAt).getTime() : Date.now()
    const secs = Math.max(0, Math.round((end - new Date(s.startedAt).getTime()) / 1000))
    if (secs < 60) return `${secs}s`
    if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`
    return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`
  }
  const size = (b: bigint) => {
    const n = Number(b)
    return n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`
  }
  const when = (t: string) => {
    const d = new Date(t)
    return d.toDateString() === new Date().toDateString() ? d.toLocaleTimeString() : d.toLocaleString()
  }

  async function kill(s: Session) {
    const ok = await confirm({
      title: 'End this session?',
      message: `${s.user}'s terminal on ${s.assetName} closes within a few seconds. The recording is kept.`,
      subject: `${s.account}@${s.host}`,
      confirmText: 'End session',
      variant: 'danger',
    })
    if (!ok) return
    try { await bastion.killSession({ id: s.id }); setTimeout(() => onchanged?.(), 2500) } catch (e) { error = errMsg(e) }
  }

  async function download(s: Session) {
    downloading = s.id
    try {
      const parts: Uint8Array[] = []
      for await (const c of bastion.getRecording({ id: s.id })) parts.push(c.data)
      const a = document.createElement('a')
      a.href = URL.createObjectURL(new Blob(parts as BlobPart[], { type: 'application/x-asciicast' }))
      a.download = `${s.assetName}-${s.id}.cast`
      a.click()
      URL.revokeObjectURL(a.href)
    } catch (e) { error = errMsg(e) } finally { downloading = '' }
  }
</script>

{#if error}<div class="notice notice--error">{error}</div>{/if}
<div class="data-table-wrap">
  <table class="data-table">
    <thead><tr><th>Status</th><th>User</th>{#if showServer}<th>Server</th>{/if}<th>Account</th><th>From</th><th>Started</th><th>Duration</th><th>Commands</th><th>Output</th><th></th></tr></thead>
    <tbody>
      {#each sessions as s (s.id)}
        <tr>
          <td><span class="badge badge--{badge(s.status)}">{#if s.status === 'live'}<span class="pill-live"></span>{/if}{s.status}</span></td>
          <td class="strong">{s.user}</td>
          {#if showServer}
            <td><button class="st-link" onclick={() => navigate(`/servers/${s.asset}`)}>{s.assetName}</button><div class="mono muted st-sub">{s.host}</div></td>
          {/if}
          <td class="mono">{s.account}</td>
          <td class="mono muted">{s.clientIp ?? '—'}</td>
          <td title={new Date(s.startedAt).toLocaleString()}>{when(s.startedAt)}</td>
          <td>{duration(s)}</td>
          <td>{s.commands || '—'}</td>
          <td class="muted" title={s.truncated ? 'recording hit its size limit' : ''}>{size(s.bytes)}{s.truncated ? ' ✂' : ''}</td>
          <td class="st-actions">
            <button class="icon-btn" title="Replay" disabled={s.chunks === 0 && s.status !== 'live'} onclick={() => (replaying = s)}><Play size={14} /></button>
            <button class="icon-btn" title="Download .cast (asciinema)" onclick={() => download(s)}>
              {#if downloading === s.id}<Loader2 size={14} class="spin" />{:else}<Download size={14} />{/if}
            </button>
            {#if isAdmin() && s.status === 'live'}
              <button class="icon-btn st-kill" title="End session" onclick={() => kill(s)}><Square size={13} /></button>
            {/if}
          </td>
        </tr>
      {:else}
        <tr><td colspan={showServer ? 10 : 9}><div class="empty-state">{empty}</div></td></tr>
      {/each}
    </tbody>
  </table>
</div>

{#if replaying}
  <Replay session={replaying} onclose={() => (replaying = null)} />
{/if}

<style>
  .st-link { background: none; border: 0; padding: 0; color: var(--text-primary); cursor: pointer; font: inherit; }
  .st-link:hover { color: var(--brand); text-decoration: underline; }
  .st-sub { font-size: 11.5px; }
  .st-actions { white-space: nowrap; text-align: right; width: 100px; }
  .st-kill:hover { color: var(--danger); }
</style>
