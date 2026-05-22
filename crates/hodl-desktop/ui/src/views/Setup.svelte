<script lang="ts">
  import * as api from "../lib/api";
  import { go, session } from "../lib/state.svelte";
  import type { Network } from "../lib/types";
  import AddressBox from "../lib/components/AddressBox.svelte";

  // Two modes:
  //   "create"  → backend generates a fresh BIP39 phrase; we display
  //               it once for the user to back up.
  //   "restore" → user pastes an existing 24-word phrase; backend
  //               validates it (BIP39 checksum) and rebuilds the
  //               same wallet keys deterministically.
  type Mode = "create" | "restore";
  let mode = $state<Mode>("create");

  let name = $state("");
  let network = $state<Network>("regtest");
  let sequencerUrl = $state("http://127.0.0.1:28080");
  let nodeUrl = $state("http://127.0.0.1:28081");
  let esploraUrl = $state("http://127.0.0.1:28081");
  let restorePhrase = $state("");

  let busy = $state(false);
  let err = $state<string | null>(null);
  let mnemonic = $state<string | null>(null);
  let wasFresh = $state(true);
  let l2Address = $state<string | null>(null);

  // Lightweight client-side check — mirrors hodl_wallet::wallets::validate_name.
  // Server re-validates so this just keeps the button gated.
  let nameOk = $derived(
    /^[A-Za-z0-9_-]+$/.test(name) && name.length >= 1 && name.length <= 32,
  );

  // Loose phrase check: count whitespace-separated tokens. The
  // backend does the real BIP39 checksum validation. We just gate
  // the submit button on a plausible-looking count so users don't
  // hit the backend with empty/half-typed input.
  let phraseWordCount = $derived(
    restorePhrase.trim().split(/\s+/).filter((w) => w.length > 0).length,
  );
  let phraseOk = $derived(
    mode === "create" || [12, 15, 18, 21, 24].includes(phraseWordCount),
  );

  let canSubmit = $derived(!busy && nameOk && phraseOk);

  async function submit() {
    err = null;
    busy = true;
    try {
      const out = await api.keygen({
        name,
        network,
        sequencer_url: sequencerUrl,
        node_url: nodeUrl || null,
        esplora_url: esploraUrl,
        mnemonic: mode === "restore" ? restorePhrase.trim() : null,
        force: false,
      });
      mnemonic = out.mnemonic;
      wasFresh = out.was_fresh;
      l2Address = out.l2_address;
      // Backend made this wallet the active one as part of keygen.
      session.currentWallet = name;
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  function done() {
    go("dashboard");
  }

  function backToPicker() {
    go("picker");
  }
</script>

<main>
  <h1>welcome to hodlchain</h1>

  {#if mnemonic === null}
    <p class="muted">
      Set up a wallet — either create a new one (we'll generate a
      BIP39 phrase to back up) or restore from an existing phrase.
      Your L1 BTC stays in whatever Bitcoin wallet you already use;
      this app only needs an Esplora endpoint to watch addresses on
      chain.
    </p>

    {#if err}
      <div class="error">{err}</div>
    {/if}

    <div class="card stack">
      <fieldset class="field mode-group">
        <legend>mode</legend>
        <div class="row">
          <label class="radio">
            <input type="radio" bind:group={mode} value="create" />
            create a new wallet
          </label>
          <label class="radio">
            <input type="radio" bind:group={mode} value="restore" />
            restore from existing phrase
          </label>
        </div>
      </fieldset>

      <div class="field">
        <label for="name">wallet name</label>
        <input
          id="name"
          type="text"
          placeholder="e.g. alice, mainnet-cold"
          bind:value={name}
        />
        <small class="muted">
          a–z, A–Z, 0–9, hyphen, underscore. 1–32 chars.
          Stored as <code>~/.config/hodlchain/wallets/&lt;name&gt;.json</code>.
        </small>
      </div>

      {#if mode === "restore"}
        <div class="field">
          <label for="phrase">recovery phrase</label>
          <textarea
            id="phrase"
            class="mono"
            rows="3"
            placeholder="paste your 12 / 15 / 18 / 21 / 24-word BIP39 phrase here, space-separated"
            bind:value={restorePhrase}
          ></textarea>
          <small class="muted">
            {#if phraseWordCount === 0}
              no words yet
            {:else if phraseOk}
              ✓ {phraseWordCount} words (full BIP39 checksum check runs server-side)
            {:else}
              {phraseWordCount} words — BIP39 requires 12, 15, 18, 21, or 24
            {/if}
          </small>
        </div>
      {/if}

      <div class="field">
        <label for="network">network</label>
        <select id="network" bind:value={network}>
          <option value="regtest">regtest</option>
          <option value="signet">signet</option>
          <option value="testnet">testnet</option>
          <option value="bitcoin">bitcoin (mainnet)</option>
        </select>
      </div>

      <div class="field">
        <label for="seq-url">sequencer URL</label>
        <input id="seq-url" type="url" bind:value={sequencerUrl} />
      </div>

      <div class="field">
        <label for="node-url">node URL <span class="muted">(optional)</span></label>
        <input id="node-url" type="url" bind:value={nodeUrl} />
      </div>

      <div class="field">
        <label for="esplora-url">esplora URL</label>
        <input id="esplora-url" type="url" bind:value={esploraUrl} />
        <small class="muted">
          mempool.space / electrs / hodl-node — anything that speaks
          the standard Esplora HTTP API. Required.
        </small>
      </div>

      <div class="row">
        <button onclick={backToPicker} disabled={busy}>← back</button>
        <button class="primary" disabled={!canSubmit} onclick={submit}>
          {#if busy}
            {mode === "create" ? "generating…" : "restoring…"}
          {:else}
            {mode === "create" ? "create wallet" : "restore wallet"}
          {/if}
        </button>
      </div>
    </div>
  {:else}
    {#if wasFresh}
      <h2>back up your recovery phrase</h2>
      <p class="muted">
        These words are the only way to recover this wallet if you
        lose the wallet file. Write them down somewhere safe and
        offline. They are also stored in plain text in your config
        directory.
      </p>
    {:else}
      <h2>wallet restored</h2>
      <p class="muted">
        Your wallet has been re-created from the supplied phrase.
        The L2 address below should match what you previously had on
        the original device — if it doesn't, the phrase you typed
        differs from what generated that wallet.
      </p>
    {/if}

    <div class="card">
      <div class="mnemonic mono">{mnemonic}</div>
    </div>

    {#if l2Address}
      <AddressBox value={l2Address} label="L2 address" />
    {/if}

    <div class="row">
      <button class="primary" onclick={done}>
        I've backed up my phrase — continue
      </button>
    </div>
  {/if}
</main>

<style>
  main {
    max-width: 640px;
    margin: 3rem auto;
    padding: 0 var(--space-4);
  }
  h1 {
    margin: 0 0 var(--space-2);
  }
  h2 {
    margin-top: var(--space-6);
  }
  .mnemonic {
    font-size: 1.05rem;
    word-spacing: 0.4rem;
    line-height: 1.8;
    user-select: all;
  }
  /* Inline radio rows — the global `label { display: block }` would
     otherwise stack the two options vertically. */
  label.radio {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    margin-bottom: 0;
    font-weight: 400;
    cursor: pointer;
  }
  label.radio input[type="radio"] {
    width: auto;
    margin: 0;
  }
  textarea {
    resize: vertical;
    min-height: 4rem;
    font-size: 0.95rem;
    line-height: 1.4;
  }
  /* Strip default browser fieldset/legend chrome so it matches the
     other `.field` blocks visually — we only used the element for
     a11y reasons (legend → radio group). */
  fieldset.mode-group {
    border: 0;
    padding: 0;
    margin-bottom: var(--space-4);
  }
  fieldset.mode-group legend {
    padding: 0;
    margin-bottom: var(--space-1);
    font-weight: 600;
    font-size: 0.9rem;
  }
</style>
