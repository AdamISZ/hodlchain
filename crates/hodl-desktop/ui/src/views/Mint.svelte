<script lang="ts">
  import * as api from "../lib/api";
  import type { MintUtxoOutput, MintMessageOutput } from "../lib/types";
  import { go } from "../lib/state.svelte";

  // The mint flow has two on-chain steps: (1) create the funded
  // CSV-locked UTXO, (2) once it's confirmed, submit the mint message
  // to the sequencer. We split the UI into clear stages so the user
  // can see what's happening and resume later if they leave.

  type Stage = "form" | "broadcasted" | "submitted";

  let stage = $state<Stage>("form");
  let lockBlocks = $state(10000);
  let valueBtc = $state(0.1);

  let busy = $state(false);
  let err = $state<string | null>(null);
  let utxo = $state<MintUtxoOutput | null>(null);
  let msg = $state<MintMessageOutput | null>(null);

  async function createUtxo() {
    err = null;
    busy = true;
    try {
      utxo = await api.mintUtxo({ lock_blocks: lockBlocks, value_btc: valueBtc });
      stage = "broadcasted";
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function submitMessage() {
    if (!utxo) return;
    err = null;
    busy = true;
    try {
      msg = await api.mintMessage({
        outpoint: `${utxo.txid}:${utxo.vout}`,
        to: null,
      });
      stage = "submitted";
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<header class="topbar">
  <button onclick={() => go("dashboard")}>← back</button>
  <h2>deposit BTC → L2</h2>
  <span></span>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if stage === "form"}
    <p class="muted">
      Lock some BTC under a relative-locktime taproot output. You'll
      receive L2 tokens (the mint amount) proportional to value × time.
      After the lock expires you can reclaim the BTC.
    </p>
    <div class="card stack">
      <div class="field">
        <label for="lock">lock duration (L1 blocks)</label>
        <input
          id="lock"
          type="number"
          min="1"
          max="65535"
          bind:value={lockBlocks}
        />
        <small class="muted">BIP112 range: 1 .. 65535 blocks</small>
      </div>
      <div class="field">
        <label for="value">amount (BTC)</label>
        <input
          id="value"
          type="number"
          min="0.00000001"
          step="0.00000001"
          bind:value={valueBtc}
        />
      </div>
      <div>
        <button class="primary" disabled={busy} onclick={createUtxo}>
          {busy ? "broadcasting…" : "create deposit UTXO"}
        </button>
      </div>
    </div>
  {:else if stage === "broadcasted" && utxo}
    <div class="card stack">
      <p>
        ✓ deposit transaction broadcast. Wait for it to confirm before
        submitting the mint message.
      </p>
      <dl>
        <dt>txid</dt>
        <dd class="mono small">{utxo.txid}</dd>
        <dt>outpoint</dt>
        <dd class="mono small">{utxo.txid}:{utxo.vout}</dd>
        <dt>mint address (informational)</dt>
        <dd class="mono small">{utxo.mint_address}</dd>
        <dt>value</dt>
        <dd>{utxo.value_sat} sat</dd>
        <dt>lock</dt>
        <dd>{utxo.lock_blocks} L1 blocks</dd>
      </dl>
      <p class="muted">
        Once the funding tx has 1 confirmation, click below to submit
        the mint message and credit the L2 tokens.
      </p>
      <div>
        <button class="primary" disabled={busy} onclick={submitMessage}>
          {busy ? "submitting…" : "submit mint message"}
        </button>
      </div>
    </div>
  {:else if stage === "submitted" && msg}
    <div class="card stack">
      {#if msg.accepted}
        <p>
          <span class="success">✓ accepted.</span>
          <strong>{msg.mint_amount ?? "?"}</strong> L2 atoms credited.
        </p>
        <dl>
          <dt>nullifier</dt>
          <dd class="mono small">{msg.nullifier_hex}</dd>
        </dl>
      {:else}
        <p>
          <span class="error">rejected:</span>
          {msg.error ?? "(no error message)"}
        </p>
      {/if}
      <div>
        <button class="primary" onclick={() => go("dashboard")}>
          back to dashboard
        </button>
      </div>
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
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-2) var(--space-4);
    margin: 0;
  }
  dt {
    font-weight: 600;
    color: var(--color-text-muted);
  }
  dd {
    margin: 0;
    word-break: break-all;
  }
  .small {
    font-size: 0.85rem;
  }
</style>
