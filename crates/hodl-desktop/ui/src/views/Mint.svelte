<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import * as api from "../lib/api";
  import { go, session, type ActiveMint } from "../lib/state.svelte";
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
  //
  // Flow state (stage, utxo, funding poll result, mint receipt,
  // soft→hard conf tracking) lives in `session.activeMint` so it
  // survives navigation. When the component remounts after the user
  // popped out to the Dashboard, we restore from session and restart
  // any polls that were in flight.

  // Component-local transient state — form input, busy/error flags,
  // and the timer handles themselves (runtime objects, not data).
  let formLockBlocks = $state(10000);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let confTimer: ReturnType<typeof setInterval> | null = null;

  const FUNDING_POLL_INTERVAL_MS = 5_000;
  const CONF_POLL_INTERVAL_MS = 5_000;

  // The current stage, derived from session. "form" when no active
  // mint exists; otherwise tracked in session.activeMint.stage.
  let stage = $derived(session.activeMint?.stage ?? "form");

  onMount(() => {
    // Resume any in-flight polling that was happening when the user
    // navigated away.
    if (session.activeMint?.stage === "funding") {
      void poll();
      pollTimer = setInterval(() => void poll(), FUNDING_POLL_INTERVAL_MS);
    }
    if (
      session.activeMint?.stage === "done" &&
      session.activeMint.confStage === "soft" &&
      session.activeMint.msg?.accepted &&
      session.activeMint.msg?.soft_conf
    ) {
      void pollForHardConf();
      confTimer = setInterval(() => void pollForHardConf(), CONF_POLL_INTERVAL_MS);
    }
  });

  onDestroy(() => {
    if (pollTimer !== null) clearInterval(pollTimer);
    if (confTimer !== null) clearInterval(confTimer);
  });

  async function pollForHardConf() {
    const mint = session.activeMint;
    if (mint === null || mint.msg === null || !mint.msg.soft_conf) return;
    try {
      const head = await api.lightBalance({ addr: null });
      if (head.l2_height >= mint.msg.soft_conf.target_l2_height) {
        mint.confStage = "hard";
        mint.confirmedAtUnix = Math.floor(Date.now() / 1000);
        if (confTimer !== null) { clearInterval(confTimer); confTimer = null; }
      }
    } catch (e) {
      console.warn("hard-conf poll failed:", e);
    }
  }

  function fmtUnix(ts: number): string {
    return new Date(ts * 1000).toLocaleString();
  }

  async function deriveAddress() {
    err = null;
    busy = true;
    try {
      const utxo = await api.mintUtxo({ lock_blocks: formLockBlocks });
      const fresh: ActiveMint = {
        stage: "funding",
        lockBlocks: formLockBlocks,
        utxo,
        funding: null,
        msg: null,
        confStage: "soft",
        confirmedAtUnix: null,
      };
      session.activeMint = fresh;
      // Kick off a poll right away, then every FUNDING_POLL_INTERVAL_MS.
      void poll();
      pollTimer = setInterval(() => void poll(), FUNDING_POLL_INTERVAL_MS);
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function poll() {
    const mint = session.activeMint;
    if (mint === null || mint.utxo === null) return;
    try {
      const funding = await api.checkMintFunding({ bip32_index: mint.utxo.bip32_index });
      mint.funding = funding;
      if (funding.state === "confirmed") {
        if (pollTimer !== null) {
          clearInterval(pollTimer);
          pollTimer = null;
        }
        mint.stage = "mint";
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
    const mint = session.activeMint;
    if (mint === null || mint.utxo === null) return;
    err = null;
    busy = true;
    try {
      const out = await api.mintMessage({ bip32_index: mint.utxo.bip32_index, to: null });
      mint.msg = out;
      mint.stage = "done";
      mint.confStage = "soft";
      mint.confirmedAtUnix = null;
      if (out.accepted && out.soft_conf) {
        // Fire one immediately (in case enough L1 blocks have
        // already elapsed) plus a timer for follow-ups. The poll
        // clears its own timer once it flips to hard.
        void pollForHardConf();
        confTimer = setInterval(() => void pollForHardConf(), CONF_POLL_INTERVAL_MS);
      }
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  // Clear the in-flight state when leaving from the "done" screen so
  // the next visit to Mint starts a fresh form. Mid-flow back-clicks
  // intentionally preserve activeMint so a Dashboard detour is safe.
  function back() {
    if (session.activeMint?.stage === "done") {
      session.activeMint = null;
    }
    go("dashboard");
  }
</script>

<header class="topbar">
  <button onclick={back}>← back</button>
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
          bind:value={formLockBlocks}
        />
        <small class="muted">BIP112 range: 1 .. 65535 blocks</small>
      </div>
      <div>
        <button class="primary" disabled={busy} onclick={deriveAddress}>
          {busy ? "deriving…" : "derive deposit address"}
        </button>
      </div>
    </div>
  {:else if stage === "funding" && session.activeMint?.utxo}
    <div class="card stack">
      <p>
        Send any BTC amount to this address from your normal wallet:
      </p>
      <AddressBox value={session.activeMint.utxo.mint_address} />
      <dl>
        <dt>bip32_index</dt>
        <dd>{session.activeMint.utxo.bip32_index}</dd>
        <dt>lock</dt>
        <dd>{session.activeMint.utxo.lock_blocks} L1 blocks</dd>
        <dt>funding status</dt>
        <dd>
          {#if !session.activeMint.funding || session.activeMint.funding.state === "unfunded"}
            no UTXO observed yet
          {:else if session.activeMint.funding.state === "pending"}
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
  {:else if stage === "mint" && session.activeMint?.utxo && session.activeMint?.funding}
    <div class="card stack">
      <p>
        <span class="success">✓ deposit confirmed</span> at L1 height
        {session.activeMint.funding.funded_at_height}. Submit the mint
        message to credit your L2 tokens.
      </p>
      <dl>
        <dt>outpoint</dt>
        <dd class="mono small">{session.activeMint.funding.outpoint}</dd>
        <dt>value</dt>
        <dd>{session.activeMint.funding.value_sat} sat</dd>
      </dl>
      <div>
        <button class="primary" disabled={busy} onclick={submitMessage}>
          {busy ? "submitting…" : "submit mint message"}
        </button>
      </div>
    </div>
  {:else if stage === "done" && session.activeMint?.msg}
    <div class="card stack">
      {#if session.activeMint.msg.accepted}
        <p>
          <span class="success">✓ accepted</span>
          {#if session.activeMint.confStage === "soft"}
            <span class="tag soft">soft</span>
          {:else}
            <span class="tag hard">L1-confirmed</span>
          {/if}
        </p>
        <p>
          <strong>{session.activeMint.msg.mint_amount ?? "?"}</strong> L2 atoms
          {#if session.activeMint.confStage === "hard"}
            credited.
          {:else}
            will be credited.
          {/if}
        </p>
        <dl>
          <dt>nullifier</dt>
          <dd class="mono small">{session.activeMint.msg.nullifier_hex}</dd>
          {#if session.activeMint.msg.soft_conf}
            <dt>target L2 height</dt>
            <dd>{session.activeMint.msg.soft_conf.target_l2_height}</dd>
            {#if session.activeMint.confirmedAtUnix !== null}
              <dt>L1-confirmed at</dt>
              <dd class="small">{fmtUnix(session.activeMint.confirmedAtUnix)}</dd>
            {/if}
          {/if}
        </dl>
        {#if session.activeMint.confStage === "soft"}
          <p class="muted small">
            Sequencer has soft-confirmed your mint. Watching the L1
            attestation chain for L2 height
            {session.activeMint.msg.soft_conf?.target_l2_height}; the
            pill above will flip to "L1-confirmed" automatically.
          </p>
        {:else}
          <p class="muted small">
            L2 block {session.activeMint.msg.soft_conf?.target_l2_height}
            is L1-attested. The mint is final.
          </p>
        {/if}
      {:else}
        <p>
          <span class="error">rejected:</span>
          {session.activeMint.msg.error ?? "(no error message)"}
        </p>
      {/if}
      <div>
        <button class="primary" onclick={back}>
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
