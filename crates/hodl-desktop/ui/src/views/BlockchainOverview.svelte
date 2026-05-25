<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../lib/api";
  import { go } from "../lib/state.svelte";
  import type { LightBalanceOutput } from "../lib/types";
  import { mintFn, BLOCKS_PER_YEAR } from "../lib/consensus";

  // Stats sourced from light-verification of the wallet's own address.
  // We don't need the balance for this screen — but the same call gives
  // us the verified head and total_minted_atoms.
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

  onMount(refresh);

  // ---------- mint calculator ----------
  let calcValueSat = $state<number | null>(null);
  let calcLockBlocks = $state<number | null>(null);

  let calcAtoms = $derived.by(() => {
    if (calcValueSat === null || calcLockBlocks === null) return null;
    if (calcValueSat <= 0 || calcLockBlocks <= 0) return null;
    return mintFn(calcValueSat, calcLockBlocks);
  });

  function fmtAtoms(n: number): string {
    return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, "_");
  }
</script>

<header class="topbar">
  <button onclick={() => go("dashboard")}>← back</button>
  <h2>blockchain overview</h2>
  <span></span>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if head === null}
    <p class="muted">verifying chain…</p>
  {:else}
    <div class="card">
      <h3>chain head</h3>
      <dl>
        <dt>L2 block height</dt>
        <dd>{head.l2_height}</dd>
        <dt>L1 anchor height</dt>
        <dd>{head.l1_height}</dd>
        <dt>verification</dt>
        <dd>
          <span class="success">✓</span>
          {head.mode === "cold_start" ? "cold-start" : "warm-start"},
          {head.blocks_verified} new block{head.blocks_verified === 1
            ? ""
            : "s"}
        </dd>
      </dl>
    </div>

    <div class="card">
      <h3>supply</h3>
      <dl>
        <dt>total minted atoms</dt>
        <dd class="big mono">{fmtAtoms(head.total_minted_atoms)}</dd>
      </dl>
      {#if head.mode === "cold_start"}
        <p class="muted small">
          On cold-start this value is seeded from the sequencer's
          snapshot and trusted. Subsequent walks are
          light-verified — refresh after any new block to upgrade
          trust.
        </p>
      {/if}
    </div>

    <div class="card">
      <h3>mint calculator</h3>
      <p class="muted small">
        The mint function's inflection point is at one year of locking
        ({fmtAtoms(BLOCKS_PER_YEAR)} L1 blocks); locks shorter than
        that are in the convex (anti-splitting) regime, and longer
        locks are in the concave (diminishing-return) regime. The
        rate parameter <em>r</em> is a fixed consensus constant in
        this design — there is no retargeting, so the figure below
        is exactly what the sequencer will credit.
      </p>
      <div class="calc">
        <div class="field">
          <label for="calc-v">deposit (sat)</label>
          <input
            id="calc-v"
            type="number"
            min="1"
            bind:value={calcValueSat}
            placeholder="e.g. 100000"
          />
        </div>
        <div class="field">
          <label for="calc-t">lock duration (L1 blocks)</label>
          <input
            id="calc-t"
            type="number"
            min="1"
            max="65535"
            bind:value={calcLockBlocks}
            placeholder="e.g. 10000"
          />
        </div>
      </div>
      <div class="result">
        {#if calcAtoms === null}
          <span class="muted">enter both fields above…</span>
        {:else}
          you would receive
          <span class="big mono">{fmtAtoms(calcAtoms)}</span>
          atoms
        {/if}
      </div>
    </div>

    <div class="row">
      <button onclick={refresh} disabled={busy}>
        {busy ? "refreshing…" : "refresh"}
      </button>
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
    max-width: 720px;
    margin: var(--space-5) auto;
    padding: 0 var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  h3 {
    margin: 0 0 var(--space-3);
    font-size: 0.95rem;
    color: var(--color-text-muted);
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
  .big {
    font-size: 1.4rem;
    font-weight: 700;
  }
  .small {
    font-size: 0.85rem;
  }
  .calc {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
  }
  .result {
    margin-top: var(--space-3);
    padding: var(--space-3);
    background: var(--color-bg);
    border-radius: var(--radius);
    border: 1px dashed var(--color-border);
  }
  .result .big {
    margin: 0 var(--space-1);
  }
</style>
