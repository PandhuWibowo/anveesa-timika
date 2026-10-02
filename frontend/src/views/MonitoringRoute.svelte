<script lang="ts">
  // #/monitoring → all systems · #/monitoring/<server> → one system.
  import { router } from '../lib/router.svelte'
  import Monitoring from './Monitoring.svelte'
  import MonitorSystem from './MonitorSystem.svelte'

  const id = $derived(decodeURIComponent(router.path.split('/')[2] ?? ''))
</script>

{#if id}
  {#key id}<MonitorSystem {id} />{/key}
{:else}
  <Monitoring />
{/if}
