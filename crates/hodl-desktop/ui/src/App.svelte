<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "./lib/api";
  import { session, go } from "./lib/state.svelte";
  import Setup from "./views/Setup.svelte";
  import Dashboard from "./views/Dashboard.svelte";
  import Mint from "./views/Mint.svelte";
  import Transfer from "./views/Transfer.svelte";
  import Reclaim from "./views/Reclaim.svelte";

  let bootErr = $state<string | null>(null);

  onMount(async () => {
    try {
      session.walletPath = await api.walletPath();
      session.walletExists = await api.walletExists();
      go(session.walletExists ? "dashboard" : "setup");
    } catch (e) {
      bootErr = String(e);
    }
  });
</script>

{#if bootErr}
  <main class="boot-error">
    <h1>could not start</h1>
    <pre class="error">{bootErr}</pre>
  </main>
{:else if session.view === "loading"}
  <main class="loading">
    <p class="muted">loading…</p>
  </main>
{:else if session.view === "setup"}
  <Setup />
{:else if session.view === "dashboard"}
  <Dashboard />
{:else if session.view === "mint"}
  <Mint />
{:else if session.view === "transfer"}
  <Transfer />
{:else if session.view === "reclaim"}
  <Reclaim />
{/if}

<style>
  .boot-error,
  .loading {
    max-width: 600px;
    margin: 4rem auto;
    padding: 0 var(--space-4);
  }
</style>
