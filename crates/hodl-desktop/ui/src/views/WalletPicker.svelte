<script lang="ts">
  import * as api from "../lib/api";
  import { go, session } from "../lib/state.svelte";

  let wallets = $state<string[] | null>(null);
  let err = $state<string | null>(null);
  let busy = $state(false);

  async function refresh() {
    try {
      wallets = await api.listWallets();
    } catch (e) {
      err = String(e);
    }
  }

  $effect(() => {
    void refresh();
  });

  async function pick(name: string) {
    err = null;
    busy = true;
    try {
      await api.selectWallet(name);
      session.currentWallet = name;
      go("dashboard");
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  function createNew() {
    go("setup");
  }
</script>

<main>
  <h1>hodlchain</h1>
  <p class="muted">pick a wallet</p>

  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if wallets === null}
    <p class="muted">loading…</p>
  {:else if wallets.length === 0}
    <p class="muted">no wallets yet.</p>
    <button class="primary" onclick={createNew}>create one</button>
  {:else}
    <ul class="wallets">
      {#each wallets as name (name)}
        <li>
          <button class="row-button" disabled={busy} onclick={() => pick(name)}>
            <span class="name">{name}</span>
            <span class="arrow">→</span>
          </button>
        </li>
      {/each}
    </ul>
    <button onclick={createNew}>+ create new wallet</button>
  {/if}
</main>

<style>
  main {
    max-width: 480px;
    margin: 4rem auto;
    padding: 0 var(--space-4);
  }
  h1 {
    margin: 0 0 var(--space-1);
  }
  ul.wallets {
    list-style: none;
    margin: var(--space-4) 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .row-button {
    width: 100%;
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-3) var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    text-align: left;
    cursor: pointer;
  }
  .row-button:hover:not(:disabled) {
    border-color: var(--color-accent);
  }
  .row-button .name {
    font-weight: 600;
  }
  .row-button .arrow {
    color: var(--color-text-muted);
  }
</style>
