<script lang="ts">
  import { fade, scale } from 'svelte/transition'
  import { TriangleAlert, CircleAlert, CircleHelp } from '@lucide/svelte'
  import { confirmState, respond } from '../lib/ui.svelte'

  const variant = $derived(confirmState.opts.variant ?? 'default')

  function onKeydown(e: KeyboardEvent) {
    if (!confirmState.open) return
    if (e.key === 'Escape') respond(false)
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if confirmState.open}
  <div class="modal-backdrop" role="presentation" transition:fade={{ duration: 150 }}
       onclick={(e) => { if (e.target === e.currentTarget) respond(false) }}>
    <div class="modal-card modal-card--{variant}" role="dialog" aria-modal="true" transition:scale={{ duration: 150, start: 0.95 }}>
      <div class="modal-icon modal-icon--{variant}">
        {#if variant === 'danger'}<TriangleAlert size={22} />
        {:else if variant === 'warning'}<CircleAlert size={22} />
        {:else}<CircleHelp size={22} />{/if}
      </div>
      <div class="modal-body">
        <h3 class="modal-title">{confirmState.opts.title}</h3>
        <p class="modal-msg">{confirmState.opts.message}</p>
        {#if confirmState.opts.subject}<div class="modal-subject">{confirmState.opts.subject}</div>{/if}
      </div>
      <div class="modal-actions">
        <button class="base-btn base-btn--ghost" onclick={() => respond(false)}>{confirmState.opts.cancelText ?? 'Cancel'}</button>
        <!-- svelte-ignore a11y_autofocus -->
        <button
          class="base-btn"
          class:base-btn--danger={variant === 'danger'}
          class:base-btn--warning={variant === 'warning'}
          class:base-btn--primary={variant === 'default'}
          autofocus
          onclick={() => respond(true)}
        >{confirmState.opts.confirmText ?? 'Confirm'}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed; inset: 0; z-index: 9999;
    display: flex; align-items: center; justify-content: center;
    background: rgba(0, 0, 0, 0.55); backdrop-filter: blur(4px); -webkit-backdrop-filter: blur(4px);
  }
  .modal-card {
    background: var(--bg-surface, var(--bg-elevated)); border: 1px solid var(--border-2); border-radius: 14px;
    box-shadow: 0 24px 48px rgba(0, 0, 0, 0.40), 0 8px 16px rgba(0, 0, 0, 0.25);
    width: 420px; max-width: calc(100vw - 32px); padding: 28px 28px 24px;
    display: flex; flex-direction: column; gap: 18px;
  }
  .modal-card--danger  { border-color: var(--danger); }
  .modal-card--warning { border-color: var(--warning); }
  .modal-icon { width: 48px; height: 48px; border-radius: 50%; display: flex; align-items: center; justify-content: center; }
  .modal-icon--danger  { background: var(--danger-bg);  color: var(--danger); }
  .modal-icon--warning { background: var(--warning-bg); color: var(--warning); }
  .modal-icon--default { background: var(--brand-soft); color: var(--brand); }
  .modal-body { display: flex; flex-direction: column; gap: 6px; }
  .modal-title { font-size: 16px; font-weight: 700; color: var(--text-primary); line-height: 1.3; }
  .modal-msg { font-size: 13px; color: var(--text-muted); line-height: 1.6; }
  .modal-subject {
    margin-top: 4px; background: var(--bg-body); border: 1px solid var(--border-2); border-radius: 8px;
    padding: 9px 13px; font-size: 13px; font-weight: 600; font-family: var(--mono); color: var(--text-primary); word-break: break-all;
  }
  .modal-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .base-btn--warning { background: var(--warning-bg); color: var(--warning); border-color: var(--warning); }
  .base-btn--warning:hover { filter: brightness(1.1); }
</style>
