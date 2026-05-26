<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "./lib/api";
  import { session, go } from "./lib/state.svelte";
  import Setup from "./views/Setup.svelte";
  import WalletPicker from "./views/WalletPicker.svelte";
  import Dashboard from "./views/Dashboard.svelte";
  import Mint from "./views/Mint.svelte";
  import Transfer from "./views/Transfer.svelte";
  import Reclaim from "./views/Reclaim.svelte";
  import BlockchainOverview from "./views/BlockchainOverview.svelte";
  import History from "./views/History.svelte";

  let bootErr = $state<string | null>(null);

  onMount(async () => {
    try {
      // Honour any pre-existing backend selection (e.g. from a
      // previous run that lingered in process memory) — but the
      // backend resets `current_wallet` to None on each app start,
      // so in practice this is null and we land on the picker.
      session.currentWallet = await api.currentWallet();
      const wallets = await api.listWallets();
      if (session.currentWallet) {
        go("dashboard");
      } else if (wallets.length === 0) {
        go("setup");
      } else {
        go("picker");
      }
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
{:else if session.view === "picker"}
  <WalletPicker />
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
{:else if session.view === "overview"}
  <BlockchainOverview />
{:else if session.view === "history"}
  <History />
{/if}

<style>
  .boot-error,
  .loading {
    max-width: 600px;
    margin: 4rem auto;
    padding: 0 var(--space-4);
  }
</style>
