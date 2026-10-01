<script lang="ts">
  // #/servers → the list · #/servers/<id>[/<tab>[/<account>]] → that server's page.
  import { router } from '../lib/router.svelte'
  import Servers from './Servers.svelte'
  import ServerDetail from './ServerDetail.svelte'

  const parts = $derived(router.path.split('/').map(decodeURIComponent))
  const id = $derived(parts[2] ?? '')
  const tab = $derived(parts[3] ?? '')
  const account = $derived(parts[4] ?? '')
</script>

{#if id}
  {#key id}<ServerDetail {id} {tab} {account} />{/key}
{:else}
  <Servers />
{/if}
