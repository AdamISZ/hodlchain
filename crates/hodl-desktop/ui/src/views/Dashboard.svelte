<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import * as api from "../lib/api";
  import type { BalanceOutput, LightBalanceOutput, MintRecord } from "../lib/types";
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

  // Mints whose L1 deposit has confirmed but whose L2 mint-message has
  // not yet been submitted. These are at risk of *silent L2 credit
  // forfeit* — the BTC is recoverable via the reclaim path once the
  // CSV expires, but the L2 atoms only land if the user (re-)submits
  // the mint message. The Mint flow lives in component-local state, so
  // any navigation mid-flow loses the in-progress submit; surfacing the
  // funded-but-unminted records here gives the user a recovery path.
  let pendingMints = $state<MintRecord[]>([]);
  // Per-row busy state so submitting one doesn't grey out the others.
  let submitting = $state<Record<number, boolean>>({});
  let pendingErr = $state<string | null>(null);

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
      // Pending-mint refresh runs alongside balance; cheap (reads the
      // wallet file). If it fails we surface the error in its own slot
      // so a transient list_mints error doesn't blank the balance card.
      await refreshPendingMints();
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function refreshPendingMints() {
    try {
      const all = await api.listMints();
      // Funded-but-unminted: deposit landed on L1 (funded_at_height
      // populated), no mint message accepted yet, not reclaimed.
      pendingMints = all.filter(
        (m) => m.funded_at_height != null && !m.minted && !m.reclaimed,
      );
      pendingErr = null;
    } catch (e) {
      pendingErr = String(e);
    }
  }

  async function submitMintMessage(bip32_index: number) {
    pendingErr = null;
    submitting = { ...submitting, [bip32_index]: true };
    try {
      const out = await api.mintMessage({ bip32_index, to: null });
      if (!out.accepted) {
        pendingErr = `mint #${bip32_index} rejected: ${out.error ?? "(no error)"}`;
      }
      // Refresh both the pending list (the mint should drop off on
      // success) and the balance (atoms should land soft).
      await refreshPendingMints();
      soft = await api.balance({ addr: null });
    } catch (e) {
      pendingErr = `mint #${bip32_index} failed: ${e}`;
    } finally {
      submitting = { ...submitting, [bip32_index]: false };
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
    // Active-mint state is wallet-scoped (bip32_index lives on the
    // outgoing wallet). Drop it on switch so the next wallet doesn't
    // see a phantom "in-flight mint" banner pointing at someone else's
    // record.
    session.activeMint = null;
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
    <button onclick={() => go("history")}>history</button>
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
      <div class="muted small">
        atoms
        {#if pendingL1 > 0}
          <span
            class="tag soft"
            title="Sequencer-acknowledged; L1 attestation pending"
            >soft</span
          >
        {/if}
      </div>
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
            <span class="muted">L1-confirmed</span>
          {/if}
        </div>
      {/if}
    </div>

    {#if session.activeMint && session.activeMint.stage !== "done"}
      <button class="card inflight-banner" onclick={() => go("mint")}>
        <span class="inflight-pulse"></span>
        <span class="inflight-text">
          <strong>resume your in-flight mint</strong>
          <span class="muted small">
            {#if session.activeMint.stage === "funding"}
              waiting for L1 deposit to confirm
            {:else if session.activeMint.stage === "mint"}
              deposit confirmed — submit the mint message
            {:else}
              click to resume
            {/if}
          </span>
        </span>
        <span class="inflight-arrow">→</span>
      </button>
    {/if}

    {#if pendingMints.length > 0}
      <div class="card pending">
        <h3>pending mints — awaiting your mint message</h3>
        <p class="muted small">
          The deposit BTC has confirmed on L1, but the L2 credit hasn't
          been claimed yet. Submit the mint message to receive the L2
          atoms. (The BTC stays under your reclaim path either way; the
          atoms only land once the message is accepted.)
        </p>
        {#if pendingErr}
          <div class="error small">{pendingErr}</div>
        {/if}
        <ul class="pending-list">
          {#each pendingMints as m (m.bip32_index)}
            <li class="pending-row">
              <div class="meta">
                <div class="header">
                  <strong>#{m.bip32_index}</strong>
                  {#if m.value_sat != null}
                    <span class="muted small">· {fmtAtoms(m.value_sat)} sat</span>
                  {/if}
                  <span class="muted small">· T={m.lock_blocks} blocks</span>
                </div>
                <AddressBox value={m.mint_address} size="compact" />
              </div>
              <button
                class="primary"
                disabled={submitting[m.bip32_index] === true}
                onclick={() => void submitMintMessage(m.bip32_index)}
              >
                {submitting[m.bip32_index] === true ? "submitting…" : "submit mint message"}
              </button>
            </li>
          {/each}
        </ul>
      </div>
    {/if}

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
  .pending {
    /* Subtle accent so users notice this card without it screaming —
       the inline copy already explains the stakes. */
    border-color: var(--color-warning, #fcd34d);
    background: #fffbeb;
  }
  .pending h3 {
    margin: 0 0 var(--space-2);
    font-size: 0.95rem;
    color: #92400e;
  }
  .pending-list {
    list-style: none;
    margin: var(--space-3) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .pending-row {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }
  .pending-row .meta {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .pending-row .header {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .pending-row button {
    flex-shrink: 0;
    white-space: nowrap;
  }
  .inflight-banner {
    /* The whole card is a button — visual treatment is "informational
       row with affordance", not a primary CTA. */
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    width: 100%;
    background: var(--color-surface);
    border: 1px solid var(--color-accent, var(--color-border));
    border-radius: var(--radius);
    text-align: left;
    cursor: pointer;
  }
  .inflight-banner:hover {
    border-color: var(--color-accent);
    background: var(--color-bg);
  }
  .inflight-pulse {
    display: inline-block;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--color-accent);
    animation: inflight-pulse 1.6s ease-in-out infinite;
    flex-shrink: 0;
  }
  @keyframes inflight-pulse {
    0%, 100% { opacity: 0.4; transform: scale(1); }
    50%      { opacity: 1;   transform: scale(1.2); }
  }
  .inflight-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .inflight-arrow {
    flex-shrink: 0;
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
