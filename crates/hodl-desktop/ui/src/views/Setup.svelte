<script lang="ts">
  import * as api from "../lib/api";
  import { go } from "../lib/state.svelte";
  import type { Network } from "../lib/types";

  // Form fields. Pre-filled with the regtest demo's endpoints so a
  // user running `scripts/regtest-demo.sh --keep-running` can hit
  // Create immediately. For production use, the user would point
  // these at their own sequencer, node, and Esplora (or mempool.space).
  let network = $state<Network>("regtest");
  let sequencerUrl = $state("http://127.0.0.1:28080");
  let nodeUrl = $state("http://127.0.0.1:28081");
  let esploraUrl = $state("http://127.0.0.1:28081");

  let busy = $state(false);
  let err = $state<string | null>(null);
  let mnemonic = $state<string | null>(null);
  let l2Address = $state<string | null>(null);

  async function submit() {
    err = null;
    busy = true;
    try {
      const out = await api.keygen({
        network,
        sequencer_url: sequencerUrl,
        node_url: nodeUrl || null,
        esplora_url: esploraUrl,
        force: false,
      });
      mnemonic = out.mnemonic;
      l2Address = out.l2_address;
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  function done() {
    go("dashboard");
  }
</script>

<main>
  <h1>welcome to hodlcoin</h1>

  {#if mnemonic === null}
    <p class="muted">
      Set up a fresh wallet. A BIP39 mnemonic will be generated and
      stored in your config directory. Your L1 BTC stays in whatever
      Bitcoin wallet you already use — this app only needs an Esplora
      endpoint to watch addresses on chain.
    </p>

    {#if err}
      <div class="error">{err}</div>
    {/if}

    <div class="card stack">
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
        <button class="primary" disabled={busy} onclick={submit}>
          {busy ? "generating…" : "create wallet"}
        </button>
      </div>
    </div>
  {:else}
    <h2>back up your recovery phrase</h2>
    <p class="muted">
      These 24 words are the only way to recover this wallet if you
      lose the wallet file. Write them down somewhere safe and offline.
      They are also stored in plain text in your config directory.
    </p>

    <div class="card">
      <div class="mnemonic mono">{mnemonic}</div>
    </div>

    <p>
      <strong>L2 address:</strong>
      <code class="mono">{l2Address}</code>
    </p>

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
</style>
