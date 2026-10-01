<script lang="ts">
  // Runs, newest first. Click a row to open its output.
  import { Loader2, GitCommitHorizontal, Webhook, CalendarClock, Rocket } from '@lucide/svelte'
  import { navigate } from '../lib/router.svelte'
  import { KINDS, ACTION_LABEL, STATUS_BADGE, STATUS_TEXT, ago, duration, short } from '../lib/automation'
  import type { Run } from '../gen/timika/v1/automation_pb'

  let { runs, showRepo = false, empty = 'No runs yet.' }: { runs: Run[]; showRepo?: boolean; empty?: string } = $props()
</script>

<div class="data-table-wrap">
  <table class="data-table runs">
    <thead>
      <tr><th>Status</th>{#if showRepo}<th>Repository</th>{/if}<th>Project</th><th>Action</th><th>Result</th><th>Commit</th><th>By</th><th>When</th><th>Took</th></tr>
    </thead>
    <tbody>
      {#each runs as r (r.id)}
        {@const K = KINDS[r.kind]}
        <tr class="runs__row" onclick={() => navigate(`/automation/run/${r.id}`)}>
          <td>
            <span class="badge badge--{STATUS_BADGE[r.status] ?? 'default'}">
              {#if r.status === 'running' || r.status === 'queued'}<Loader2 size={11} class="spin" />{/if}{STATUS_TEXT[r.status] ?? r.status}
            </span>
          </td>
          {#if showRepo}<td class="strong">{r.repoName}</td>{/if}
          <td><span class="runs__proj">{#if K}<K.ico size={13} />{/if}<span>{r.projectName}</span></span></td>
          <td>{ACTION_LABEL[r.action] ?? r.action}{#if r.optionsText}<div class="runs__opts" title={r.optionsText}>{r.optionsText}</div>{/if}</td>
          <td class="runs__result">
            {#if r.summary}<span class="mono">{r.summary}</span>{:else if r.error}<span class="runs__err">{r.error}</span>{:else}<span class="muted">—</span>{/if}
          </td>
          <td><span class="mono muted" title={r.commitMessage}><GitCommitHorizontal size={12} /> {short(r.sha)}</span></td>
          <td>{#if r.trigger === 'push'}<span title="Started by a push"><Webhook size={12} /> {r.user}</span>{:else if r.trigger === 'schedule'}<span title="Scheduled"><CalendarClock size={12} /> {r.user.replace(/^schedule /, '')}</span>{:else if r.trigger === 'ci'}<span title="Started from CI"><Rocket size={12} /> {r.user}</span>{:else}{r.user}{/if}</td>
          <td title={new Date(r.createdAt).toLocaleString()}>{ago(r.createdAt)}</td>
          <td class="muted">{duration(r)}</td>
        </tr>
      {:else}
        <tr><td colspan={showRepo ? 9 : 8}><div class="empty-state">{empty}</div></td></tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .runs__row { cursor: pointer; }
  .runs__proj { display: inline-flex; align-items: center; gap: 6px; color: var(--text-primary); }
  .runs__proj :global(svg) { color: var(--text-muted); flex: 0 0 auto; }
  .runs__result { max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .runs__err { color: var(--danger); font-size: 12px; }
  .runs__opts { font-size: 11px; color: var(--warning); max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .runs td :global(svg) { vertical-align: -2px; }
</style>
