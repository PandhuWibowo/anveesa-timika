<script lang="ts">
  // #/containers → every container · #/containers/<server>/<name>[/files/<path…>] → one container.
  import { router } from '../lib/router.svelte'
  import Containers from './Containers.svelte'
  import ContainerDetail from './ContainerDetail.svelte'

  const parts = $derived(router.path.split('/').map(decodeURIComponent))
</script>

{#if parts[2] && parts[3]}
  {#key `${parts[2]}/${parts[3]}`}<ContainerDetail asset={parts[2]} name={parts[3]} tab={parts[4] ?? ''} path={'/' + parts.slice(5).join('/')} />{/key}
{:else}
  <Containers />
{/if}
