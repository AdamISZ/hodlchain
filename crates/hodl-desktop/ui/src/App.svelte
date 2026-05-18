<script lang="ts">
  // Proof-of-life Svelte 5 component. Calls the `wallet_path` Tauri
  // command to verify the bridge works. The full MVP UI is built in
  // a follow-up pass; this file exists so `pnpm dev` / `cargo tauri
  // dev` boots into a real window.

  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  let walletPath = $state<string | null>(null);
  let walletExists = $state<boolean | null>(null);
  let err = $state<string | null>(null);

  onMount(async () => {
    try {
      walletPath = await invoke<string>("wallet_path");
      walletExists = await invoke<boolean>("wallet_exists");
    } catch (e) {
      err = String(e);
    }
  });
</script>

<main>
  <h1>hodlcoin</h1>
  <p class="muted">desktop wallet — scaffolding placeholder</p>

  {#if err}
    <pre class="error">{err}</pre>
  {:else}
    <dl>
      <dt>wallet path</dt>
      <dd><code>{walletPath ?? "(loading…)"}</code></dd>
      <dt>wallet file</dt>
      <dd>
        {#if walletExists === null}
          (loading…)
        {:else if walletExists}
          exists
        {:else}
          not yet created — run setup
        {/if}
      </dd>
    </dl>
  {/if}
</main>

<style>
  main {
    font-family:
      -apple-system,
      BlinkMacSystemFont,
      "Segoe UI",
      sans-serif;
    max-width: 600px;
    margin: 4rem auto;
    padding: 0 1.5rem;
    color: #222;
  }
  h1 {
    margin: 0 0 0.25rem;
  }
  .muted {
    color: #888;
    margin-top: 0;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.5rem 1rem;
    margin-top: 2rem;
  }
  dt {
    font-weight: 600;
  }
  code {
    background: #f4f4f4;
    padding: 0.1rem 0.4rem;
    border-radius: 3px;
    font-size: 0.9em;
  }
  .error {
    color: #c33;
    background: #fee;
    padding: 0.5rem 0.75rem;
    border-radius: 4px;
    overflow-x: auto;
  }
</style>
