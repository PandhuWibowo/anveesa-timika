<script lang="ts">
  // #/automation → repositories · #/automation/run/<id> → a run ·
  // #/automation/<repo>[/<tab>] → a repository.
  import { router } from '../lib/router.svelte'
  import Automation from './Automation.svelte'
  import AutomationRepo from './AutomationRepo.svelte'
  import RunView from './RunView.svelte'

  const parts = $derived(router.path.split('/').map(decodeURIComponent))
</script>

{#if parts[2] === 'run' && parts[3]}
  {#key parts[3]}<RunView id={parts[3]} />{/key}
{:else if parts[2]}
  {#key parts[2]}<AutomationRepo id={parts[2]} tab={parts[3] ?? ''} path={parts.slice(4).join('/')} />{/key}
{:else}
  <Automation />
{/if}
