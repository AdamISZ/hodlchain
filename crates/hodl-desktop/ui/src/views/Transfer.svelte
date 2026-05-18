<script lang="ts">
  import * as api from "../lib/api";
  import type { TransferOutput } from "../lib/types";
  import { go } from "../lib/state.svelte";

  let to = $state("");
  let amount = $state(0);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let result = $state<TransferOutput | null>(null);

  async function submit() {
    err = null;
    busy = true;
    result = null;
    try {
      result = await api.transfer({ to, amount });
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  // Loose validation. The backend re-validates; we just nudge the
  // user away from obviously-wrong inputs in the form.
  let canSubmit = $derived(
    !busy && /^[0-9a-fA-F]{64}$/.test(to.trim()) && amount > 0,
  );
</script>

<header class="topbar">
  <button onclick={() => go("dashboard")}>← back</button>
  <h2>send L2 tokens</h2>
  <span></span>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  <div class="card stack">
    <div class="field">
      <label for="to">recipient (x-only pubkey, 64 hex chars)</label>
      <input id="to" class="mono" type="text" bind:value={to} />
    </div>
    <div class="field">
      <label for="amount">amount (atoms)</label>
      <input id="amount" type="number" min="1" bind:value={amount} />
    </div>
    <div>
      <button class="primary" disabled={!canSubmit} onclick={submit}>
        {busy ? "submitting…" : "send"}
      </button>
    </div>
  </div>

  {#if result !== null}
    <div class="card">
      {#if result.accepted}
        <p><span class="success">✓ accepted.</span></p>
        <p class="muted">
          The transfer will appear in your verified balance after the
          next L2 block confirms and you refresh the dashboard.
        </p>
      {:else}
        <p>
          <span class="error">rejected:</span>
          {result.error ?? "(no error message)"}
        </p>
      {/if}
    </div>
  {/if}
</main>

<style>
  .topbar {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding: var(--space-4) var(--space-5);
    background: var(--color-surface);
    border-bottom: 1px solid var(--color-border);
  }
  .topbar h2 {
    margin: 0;
    text-align: center;
    font-size: 1rem;
  }
  main {
    max-width: 640px;
    margin: var(--space-5) auto;
    padding: 0 var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
</style>
