<script lang="ts">
  // #/usage → what every container uses · #/usage/<server>/<name> → one container over time.
  import { router } from '../lib/router.svelte'
  import ContainerUsage from './ContainerUsage.svelte'
  import ContainerUsageDetail from './ContainerUsageDetail.svelte'

  const parts = $derived(router.path.split('/').map(decodeURIComponent))
</script>

{#if parts[2] && parts[3]}
  {#key `${parts[2]}/${parts[3]}`}<ContainerUsageDetail asset={parts[2]} name={parts[3]} />{/key}
{:else}
  <ContainerUsage />
{/if}
