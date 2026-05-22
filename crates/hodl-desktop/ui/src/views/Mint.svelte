<script lang="ts">
  import { onDestroy } from "svelte";
  import * as api from "../lib/api";
  import type {
    MintUtxoOutput,
    CheckMintFundingOutput,
    MintMessageOutput,
  } from "../lib/types";
  import { go } from "../lib/state.svelte";
  import AddressBox from "../lib/components/AddressBox.svelte";

  // Three-stage flow:
  //
  //   1. form     — user picks lock_blocks, clicks "derive deposit
  //                 address". hodl-wallet derives a fresh BIP32 key
  //                 and shows the resulting bech32m address.
  //   2. funding  — user is asked to send BTC from their normal
  //                 wallet. We poll Esplora periodically until a
  //                 confirmed UTXO appears.
  //   3. mint     — once funded, user clicks "submit mint message"
  //                 to credit the L2 tokens.

  type Stage = "form" | "funding" | "mint" | "done";

  let stage = $state<Stage>("form");
  let lockBlocks = $state(10000);

  let busy = $state(false);
  let err = $state<string | null>(null);
  let utxo = $state<MintUtxoOutput | null>(null);
  let funding = $state<CheckMintFundingOutput | null>(null);
  let msg = $state<MintMessageOutput | null>(null);

  let pollTimer: ReturnType<typeof setInterval> | null = null;

  onDestroy(() => {
    if (pollTimer !== null) clearInterval(pollTimer);
  });

  async function deriveAddress() {
    err = null;
    busy = true;
    try {
      utxo = await api.mintUtxo({ lock_blocks: lockBlocks });
      stage = "funding";
      // Kick off a poll right away, then every 5s.
      void poll();
      pollTimer = setInterval(() => void poll(), 5000);
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function poll() {
    if (!utxo) return;
    try {
      funding = await api.checkMintFunding({ bip32_index: utxo.bip32_index });
      if (funding.state === "confirmed") {
        if (pollTimer !== null) {
          clearInterval(pollTimer);
          pollTimer = null;
        }
        stage = "mint";
      }
    } catch (e) {
      err = String(e);
    }
  }

  async function pollNow() {
    busy = true;
    try {
      await poll();
    } finally {
      busy = false;
    }
  }

  async function submitMessage() {
    if (!utxo) return;
    err = null;
    busy = true;
    try {
      msg = await api.mintMessage({ bip32_index: utxo.bip32_index, to: null });
      stage = "done";
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
      <div>
        <button class="primary" disabled={busy} onclick={deriveAddress}>
          {busy ? "deriving…" : "derive deposit address"}
        </button>
      </div>
    </div>
  {:else if stage === "funding" && utxo}
    <div class="card stack">
      <p>
        Send any BTC amount to this address from your normal wallet:
      </p>
      <AddressBox value={utxo.mint_address} />
      <dl>
        <dt>bip32_index</dt>
        <dd>{utxo.bip32_index}</dd>
        <dt>lock</dt>
        <dd>{utxo.lock_blocks} L1 blocks</dd>
        <dt>funding status</dt>
        <dd>
          {#if !funding || funding.state === "unfunded"}
            no UTXO observed yet
          {:else if funding.state === "pending"}
            UTXO seen in mempool, waiting for 1 confirmation
          {:else}
            confirmed
          {/if}
        </dd>
      </dl>
      <p class="muted">
        The app polls the configured Esplora endpoint every 5 seconds.
        You can also re-check manually.
      </p>
      <div class="row">
        <button onclick={pollNow} disabled={busy}>
          {busy ? "checking…" : "check now"}
        </button>
      </div>
    </div>
  {:else if stage === "mint" && utxo && funding}
    <div class="card stack">
      <p>
        <span class="success">✓ deposit confirmed</span> at L1 height
        {funding.funded_at_height}. Submit the mint message to credit
        your L2 tokens.
      </p>
      <dl>
        <dt>outpoint</dt>
        <dd class="mono small">{funding.outpoint}</dd>
        <dt>value</dt>
        <dd>{funding.value_sat} sat</dd>
      </dl>
      <div>
        <button class="primary" disabled={busy} onclick={submitMessage}>
          {busy ? "submitting…" : "submit mint message"}
        </button>
      </div>
    </div>
  {:else if stage === "done" && msg}
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
