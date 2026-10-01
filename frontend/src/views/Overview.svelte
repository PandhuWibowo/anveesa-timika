<script lang="ts">
  import { onMount } from 'svelte'
  import { TriangleAlert } from '@lucide/svelte'
  import { toJson } from '@bufbuild/protobuf'
  import { StructSchema } from '@bufbuild/protobuf/wkt'
  import { sys, errMsg } from '../lib/api'
  import { session } from '../lib/session.svelte'
  import type { KeyStatus } from '../gen/timika/v1/sys_pb'
  import { fmtTime, type RaftInfo, type RedisInfo, type StorageInfo } from '../lib/types'

  let keys = $state<KeyStatus | null>(null)
  let storage = $state<StorageInfo | null>(null)
  let error = $state('')

  onMount(async () => {
    try {
      const [k, st] = await Promise.all([sys.getKeyStatus({}), sys.storage({})])
      keys = k
      // Free-form report (a top-level google.protobuf.Struct → plain JSON).
      storage = toJson(StructSchema, st) as unknown as StorageInfo
    } catch (e) { error = errMsg(e) }
  })

  const redis = $derived(storage?.engine === 'redis' ? (storage as RedisInfo) : null)
  const raft = $derived(storage?.engine === 'raft' ? (storage as RaftInfo) : null)
  const isRaft = $derived(session.status?.storage === 'raft')

  // The write path, top to bottom — the answer to "where does a secret go?".
  const common = [
    { step: 'Auth', body: 'SHA-256(token) → read sys/token/<hash> through the barrier; unknown token → UNAUTHENTICATED.' },
    { step: 'Encrypt', body: 'Barrier seals the JSON with the active AES-256-GCM data key. The storage path is AEAD-bound, so a blob copied to another key will not decrypt.' },
  ]
  const flow = $derived(isRaft ? [
    { step: 'Request', body: 'KvService.Write(app/db) over gRPC to any unsealed node; a follower forwards the write to the Raft leader.' },
    ...common,
    { step: 'Replicate', body: 'The leader appends one log entry { SET kv/ver/app/db@N, SET kv/meta/app/db } and sends it to every peer.' },
    { step: 'Commit', body: 'Once a majority has it fsynced to their redb file, it is committed and applied on every node. Then the client gets OK.' },
  ] : [
    { step: 'Request', body: 'KvService.Write(app/db) over gRPC, with x-timika-token metadata.' },
    ...common,
    { step: 'Commit', body: 'One atomic Lua script: SET kv/ver/app/db@N + SET kv/meta/app/db (+ DEL of pruned versions) and update the path index.' },
    { step: 'Persist', body: 'Redis appends to its AOF and fsyncs (everysec); with REDIS_WAIT_REPLICAS the write is also confirmed by replicas before the client hears back.' },
  ])
</script>

<div class="page-shell">
  <div class="page-scroll">
    <div class="page-stack">
      <section class="page-hero">
        <div class="page-hero__content">
          <div class="page-kicker">Vault</div>
          <h1 class="page-title">Overview</h1>
          <p class="page-subtitle">
            Secrets are encrypted in this process and stored as ciphertext in {isRaft ? 'every Raft node' : 'Redis'}.
            The key that decrypts them lives only in memory while the vault is unsealed.
          </p>
        </div>
        <div class="page-metrics">
          <div class="page-metric"><span class="page-metric__value">Unsealed</span><span class="page-metric__label">State</span></div>
          <div class="page-metric"><span class="page-metric__value">{session.status?.t}/{session.status?.n}</span><span class="page-metric__label">Shamir</span></div>
          <div class="page-metric"><span class="page-metric__value">{keys?.term ?? '—'}</span><span class="page-metric__label">Key term</span></div>
          <div class="page-metric"><span class="page-metric__value">{storage?.keys ?? '—'}</span><span class="page-metric__label">Stored keys</span></div>
        </div>
      </section>

      {#if error}<div class="notice notice--error">{error}</div>{/if}
      {#each storage?.warnings ?? [] as w (w)}
        <div class="notice notice--warning"><TriangleAlert size={14} /> {w}</div>
      {/each}

      <div class="page-grid page-grid--2">
        <section class="page-card">
          <div class="page-card__head">
            <div>
              <div class="page-card__title">Persistence flow</div>
              <div class="page-card__sub">What happens to a secret between the API and the disk</div>
            </div>
          </div>
          <div class="page-card__body">
            <ol class="flow">
              {#each flow as f, i (f.step)}
                <li class="flow__step">
                  <span class="flow__n">{i + 1}</span>
                  <div><div class="flow__name">{f.step}</div><div class="flow__body">{f.body}</div></div>
                </li>
              {/each}
            </ol>
          </div>
        </section>

        <section class="page-card">
          <div class="page-card__head">
            <div>
              <div class="page-card__title">Engine</div>
              <div class="page-card__sub">Live readings from the barrier and {isRaft ? 'Raft' : 'Redis'}</div>
            </div>
          </div>
          <div class="page-card__body">
            <div class="data-table-wrap">
              <table class="data-table">
                <tbody>
                  <tr><td>Encryption</td><td class="strong mono">{keys?.encryption ?? '—'}</td></tr>
                  <tr><td>Active term installed</td><td class="strong">{fmtTime(keys?.installTime)}</td></tr>
                  <tr><td>Terms in keyring</td><td class="strong">{keys?.terms ?? '—'}</td></tr>
                  {#if raft}
                    <tr><td>Storage</td><td class="strong">Integrated Raft (redb per node)</td></tr>
                    <tr><td>Leader</td><td class="strong mono">{raft.leader?.name ?? 'none'}</td></tr>
                    <tr>
                      <td>Voters</td>
                      <td><span class="badge {raft.voters >= 3 && raft.voters % 2 === 1 ? 'badge--success' : 'badge--warning'}">{raft.voters}</span></td>
                    </tr>
                    <tr><td>Raft term</td><td class="strong">{raft.term ?? '—'}</td></tr>
                  {:else if redis}
                    <tr><td>Storage</td><td class="strong">Redis {redis.redis_version ?? ''} · {redis.mode}</td></tr>
                    <tr><td>Key prefix</td><td class="strong mono">{redis.prefix}:</td></tr>
                    <tr>
                      <td>AOF</td>
                      <td>
                        <span class="badge {redis.aof_enabled ? 'badge--success' : 'badge--danger'}">
                          {redis.aof_enabled ? `on · fsync ${redis.appendfsync ?? '?'}` : 'off'}
                        </span>
                      </td>
                    </tr>
                    <tr>
                      <td>Eviction policy</td>
                      <td>
                        <span class="badge {redis.maxmemory_policy === 'noeviction' ? 'badge--success' : 'badge--warning'}">
                          {redis.maxmemory_policy ?? 'unknown'}
                        </span>
                      </td>
                    </tr>
                    <tr><td>Memory used</td><td class="strong">{redis.used_memory_human ?? '—'}</td></tr>
                  {/if}
                </tbody>
              </table>
            </div>
          </div>
        </section>
      </div>
    </div>
  </div>
</div>

<style>
  .flow { list-style: none; display: flex; flex-direction: column; gap: 14px; }
  .flow__step { display: flex; gap: 12px; align-items: flex-start; }
  .flow__n {
    flex-shrink: 0; width: 24px; height: 24px; border-radius: 50%; display: grid; place-items: center;
    font-size: 11px; font-weight: 700; background: var(--brand-dim); color: var(--brand); border: 1px solid var(--brand-ring);
  }
  .flow__name { font-size: 13px; font-weight: 600; color: var(--text-primary); }
  .flow__body { font-size: 12.5px; color: var(--text-secondary); line-height: 1.55; }
</style>
