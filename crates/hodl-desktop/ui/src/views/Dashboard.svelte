<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import * as api from "../lib/api";
  import type { BalanceOutput, LightBalanceOutput } from "../lib/types";
  import { go, session } from "../lib/state.svelte";
  import AddressBox from "../lib/components/AddressBox.svelte";

  // Two parallel reads on each poll:
  //   - `soft` is the sequencer's current view (committed at the
  //     sequencer, includes L2 blocks not yet L1-attested). This is
  //     what the user sees as the headline balance — they care
  //     about "what the chain just said happened", not "what's been
  //     anchored to Bitcoin".
  //   - `verified` walks the L1 attestation chain and re-verifies
  //     every L2 block. Slower; the L1-confirmed footnote and the
  //     "verified head" panel are sourced from this.
  // For the POC we keep the lighter-trust default. Power-users can
  // compare the two to see how much value is awaiting L1 finality.
  let soft = $state<BalanceOutput | null>(null);
  let verified = $state<LightBalanceOutput | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // Auto-poll cadence. The L2 produces a fresh block every 30s by
  // default, so 10s polling reliably picks up new blocks within
  // one full block interval — fast enough to feel live, slow enough
  // not to pin the sequencer's HTTP API.
  const POLL_INTERVAL_MS = 10_000;

  async function refresh() {
    err = null;
    busy = true;
    try {
      // Soft balance first — it's the headline, runs cheaply. Then
      // verify in the background.
      soft = await api.balance({ addr: null });
      verified = await api.lightBalance({ addr: null });
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  // How much of the current (soft) balance is still pending L1
  // confirmation. Negative would mean L1-confirmed *exceeds* soft
  // (e.g. an incoming credit that landed after our soft read). We
  // clamp at zero — the panel reads "all confirmed" in that case
  // since negative pending is just race noise.
  let pendingL1 = $derived.by(() => {
    if (soft === null || verified === null) return 0;
    return Math.max(0, soft.balance - verified.balance);
  });

  async function switchWallet() {
    await api.deselectWallet();
    session.currentWallet = null;
    go("picker");
  }

  onMount(() => {
    void refresh();
    pollTimer = setInterval(() => void refresh(), POLL_INTERVAL_MS);
  });

  onDestroy(() => {
    if (pollTimer !== null) clearInterval(pollTimer);
  });

  // Format atom amounts with an underscore every three digits, just
  // for legibility. Atoms are the smallest L2 unit; no decimal point.
  function fmtAtoms(n: number): string {
    return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, "_");
  }

  function modeLabel(m: "cold_start" | "warm_start"): string {
    return m === "cold_start" ? "cold-start" : "warm-start";
  }
</script>

<header class="topbar">
  <div class="left">
    <h1>hodlchain</h1>
    {#if session.currentWallet}
      <span class="wallet">
        <span class="muted small">wallet:</span>
        <strong>{session.currentWallet}</strong>
        <button class="switch" onclick={switchWallet} title="switch wallet">
          switch
        </button>
      </span>
    {/if}
  </div>
  <nav>
    <button onclick={() => go("mint")}>deposit (mint)</button>
    <button onclick={() => go("transfer")}>send</button>
    <button onclick={() => go("reclaim")}>reclaim</button>
    <button onclick={() => go("overview")}>overview</button>
  </nav>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if soft === null}
    <p class="muted">loading…</p>
  {:else}
    <div class="card balance">
      <div class="muted">balance</div>
      <div class="big mono">{fmtAtoms(soft.balance)}</div>
      <div class="muted small">atoms <span class="tag soft" title="Sequencer-acknowledged; not yet L1-final">soft</span></div>
      {#if verified !== null}
        <div class="hard-line small">
          {#if pendingL1 > 0}
            <span class="muted">L1-confirmed:</span>
            <span class="mono">{fmtAtoms(verified.balance)}</span>
            <span class="muted">
              · <strong class="mono">{fmtAtoms(pendingL1)}</strong> atoms pending L1
            </span>
          {:else}
            <span class="success">✓</span>
            <span class="muted">all L1-confirmed</span>
          {/if}
        </div>
      {/if}
    </div>

    {#if verified !== null}
      <div class="card details">
        <h3>verified head</h3>
        <dl>
          <dt>verification</dt>
          <dd>
            <span class="success">✓</span>
            {modeLabel(verified.mode)}, {verified.blocks_verified}
            new block{verified.blocks_verified === 1 ? "" : "s"}
          </dd>
          <dt>L2 height</dt>
          <dd>{verified.l2_height}</dd>
          <dt>L1 height</dt>
          <dd>{verified.l1_height}</dd>
          <dt>nonce</dt>
          <dd>{verified.nonce}</dd>
          <dt>state_root</dt>
          <dd class="mono small">{verified.state_root}</dd>
        </dl>
        <div class="address-row">
          <AddressBox value={verified.address} label="L2 address" size="compact" />
        </div>
      </div>
    {/if}

    <div class="row">
      <button onclick={refresh} disabled={busy}>
        {busy ? "verifying…" : "refresh"}
      </button>
    </div>
  {/if}
</main>

<style>
  .topbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4) var(--space-5);
    background: var(--color-surface);
    border-bottom: 1px solid var(--color-border);
  }
  .topbar h1 {
    margin: 0;
    font-size: 1.1rem;
  }
  .left {
    display: flex;
    align-items: baseline;
    gap: var(--space-4);
  }
  .wallet {
    display: inline-flex;
    align-items: baseline;
    gap: var(--space-2);
  }
  .switch {
    padding: 0.1rem 0.5rem;
    font-size: 0.8rem;
  }
  .small {
    font-size: 0.85rem;
  }
  nav {
    display: flex;
    gap: var(--space-2);
  }
  main {
    max-width: 700px;
    margin: var(--space-5) auto;
    padding: 0 var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .balance {
    text-align: center;
    padding: var(--space-6);
  }
  .big {
    font-size: 2.5rem;
    font-weight: 700;
    margin: var(--space-2) 0;
  }
  .hard-line {
    margin-top: var(--space-3);
    padding-top: var(--space-2);
    border-top: 1px solid var(--color-border);
  }
  .small {
    font-size: 0.85rem;
  }
  .details h3 {
    margin: 0 0 var(--space-3);
    font-size: 0.95rem;
    color: var(--color-text-muted);
  }
  .address-row {
    margin-top: var(--space-3);
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
</style>
