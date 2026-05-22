<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../lib/api";
  import type { LightBalanceOutput } from "../lib/types";
  import { go, session } from "../lib/state.svelte";
  import AddressBox from "../lib/components/AddressBox.svelte";

  let head = $state<LightBalanceOutput | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);

  async function refresh() {
    err = null;
    busy = true;
    try {
      head = await api.lightBalance({ addr: null });
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function switchWallet() {
    await api.deselectWallet();
    session.currentWallet = null;
    go("picker");
  }

  onMount(refresh);

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
  </nav>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if head === null}
    <p class="muted">verifying chain…</p>
  {:else}
    <div class="card balance">
      <div class="muted">balance</div>
      <div class="big mono">{fmtAtoms(head.balance)}</div>
      <div class="muted small">atoms</div>
    </div>

    <div class="card details">
      <h3>verified head</h3>
      <dl>
        <dt>verification</dt>
        <dd>
          <span class="success">✓</span>
          {modeLabel(head.mode)}, {head.blocks_verified}
          new block{head.blocks_verified === 1 ? "" : "s"}
        </dd>
        <dt>L2 height</dt>
        <dd>{head.l2_height}</dd>
        <dt>L1 height</dt>
        <dd>{head.l1_height}</dd>
        <dt>nonce</dt>
        <dd>{head.nonce}</dd>
        <dt>state_root</dt>
        <dd class="mono small">{head.state_root}</dd>
      </dl>
      <div class="address-row">
        <AddressBox value={head.address} label="L2 address" size="compact" />
      </div>
    </div>

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
