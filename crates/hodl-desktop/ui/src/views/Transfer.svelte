<script lang="ts">
  import { onDestroy } from "svelte";
  import * as api from "../lib/api";
  import type { TransferOutput } from "../lib/types";
  import { go } from "../lib/state.svelte";
  import { transferFee } from "../lib/consensus";

  let to = $state("");
  let amount = $state(0);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let result = $state<TransferOutput | null>(null);
  // 'soft' = sequencer-acked, awaiting L1 attestation.
  // 'hard' = verified head has covered target_l2_height (tx is
  //          inside an L1-attested L2 block).
  let confStage = $state<"soft" | "hard">("soft");
  let confirmedAtUnix = $state<number | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // Poll the verified head every 5s while a soft conf is
  // outstanding. The dashboard polls at 10s; we use a tighter
  // cadence here because the user is actively watching this
  // specific tx land.
  const POLL_INTERVAL_MS = 5_000;

  async function submit() {
    err = null;
    busy = true;
    result = null;
    confStage = "soft";
    confirmedAtUnix = null;
    if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null; }
    try {
      result = await api.transfer({ to, amount });
      if (result.accepted && result.soft_conf) {
        // Fire a first poll immediately (in case the L1
        // attestation already landed in the seconds between
        // submission and now), and a timer for follow-ups.
        // pollForHardConf clears its own timer once it flips
        // confStage to "hard".
        void pollForHardConf();
        pollTimer = setInterval(() => void pollForHardConf(), POLL_INTERVAL_MS);
      }
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function pollForHardConf() {
    if (result === null || !result.soft_conf) return;
    try {
      const head = await api.lightBalance({ addr: null });
      if (head.l2_height >= result.soft_conf.target_l2_height) {
        confStage = "hard";
        confirmedAtUnix = Math.floor(Date.now() / 1000);
        if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null; }
      }
    } catch (e) {
      // Non-fatal: leave the pill as 'soft' and try again on
      // the next tick. Don't surface as an error since the tx
      // itself was accepted.
      console.warn("hard-conf poll failed:", e);
    }
  }

  onDestroy(() => {
    if (pollTimer !== null) clearInterval(pollTimer);
  });

  // Loose validation. The backend re-validates; we just nudge the
  // user away from obviously-wrong inputs in the form.
  let canSubmit = $derived(
    !busy && /^[0-9a-fA-F]{64}$/.test(to.trim()) && amount > 0,
  );

  // Live fee preview as the user types. Mirrors apply_transfer's
  // formula so the UI doesn't lie about what the chain will deduct.
  let previewFee = $derived(amount > 0 ? transferFee(amount) : 0);
  let previewTotal = $derived(amount + previewFee);

  function fmtAtoms(n: number): string {
    return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, "_");
  }

  function fmtUnix(ts: number): string {
    return new Date(ts * 1000).toLocaleString();
  }
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
    {#if amount > 0}
      <dl class="fee-preview">
        <dt>amount</dt><dd class="mono">{fmtAtoms(amount)}</dd>
        <dt>+ fee (auto)</dt><dd class="mono">{fmtAtoms(previewFee)}</dd>
        <dt>= total</dt><dd class="mono"><strong>{fmtAtoms(previewTotal)}</strong></dd>
      </dl>
    {/if}
    <div>
      <button class="primary" disabled={!canSubmit} onclick={submit}>
        {busy ? "submitting…" : "send"}
      </button>
    </div>
  </div>

  {#if result !== null}
    <div class="card">
      {#if result.accepted}
        <p>
          <span class="success">✓ accepted</span>
          {#if confStage === "soft"}
            <span class="tag soft">soft</span>
          {:else}
            <span class="tag hard">L1-confirmed</span>
          {/if}
        </p>
        <dl class="receipt">
          <dt>amount</dt><dd class="mono">{fmtAtoms(amount)}</dd>
          <dt>fee</dt><dd class="mono">{fmtAtoms(result.fee)}</dd>
          <dt>total</dt><dd class="mono">{fmtAtoms(result.total)}</dd>
          {#if result.soft_conf}
            <dt>target L2 height</dt>
            <dd>{result.soft_conf.target_l2_height}</dd>
            <dt>sequencer ack</dt>
            <dd class="small">{fmtUnix(result.soft_conf.accepted_at_unix)}</dd>
            {#if confirmedAtUnix !== null}
              <dt>L1-confirmed at</dt>
              <dd class="small">{fmtUnix(confirmedAtUnix)}</dd>
            {/if}
          {/if}
        </dl>
        {#if confStage === "soft"}
          <p class="muted small">
            Sequencer has soft-confirmed inclusion. Watching the L1
            attestation chain for L2 height
            {result.soft_conf?.target_l2_height}; the pill above
            will flip to "L1-confirmed" automatically when that
            block is attested (typically within a couple of L1
            blocks).
          </p>
        {:else}
          <p class="muted small">
            L2 block {result.soft_conf?.target_l2_height} is
            L1-attested and contains your transfer. Balance changes
            are now backed by Bitcoin.
          </p>
        {/if}
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
  dl.fee-preview, dl.receipt {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-1) var(--space-3);
    margin: 0;
    padding: var(--space-2) var(--space-3);
    background: var(--color-bg);
    border-radius: var(--radius);
    border: 1px dashed var(--color-border);
  }
  dl.fee-preview dt, dl.receipt dt {
    color: var(--color-text-muted);
  }
  dl.fee-preview dd, dl.receipt dd {
    margin: 0;
  }
  .small {
    font-size: 0.85rem;
  }
</style>
