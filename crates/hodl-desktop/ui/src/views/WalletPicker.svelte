<script lang="ts">
  import * as api from "../lib/api";
  import { go, session } from "../lib/state.svelte";

  // Each wallet row carries an `encrypted` flag so we can show a lock
  // badge in the list. `null` while we're still resolving the status
  // (the encrypted-check is cheap — file read + JSON parse — so we
  // just fan it out at render time).
  type WalletRow = { name: string; encrypted: boolean | null };

  let wallets = $state<WalletRow[] | null>(null);
  let err = $state<string | null>(null);
  let busy = $state(false);

  // Passphrase prompt state. `target` is the wallet being unlocked;
  // `null` means the prompt is closed.
  let prompt = $state<{ target: string; passphrase: string; error: string | null } | null>(null);

  async function refresh() {
    try {
      const names = await api.listWallets();
      wallets = names.map((name) => ({ name, encrypted: null }));
      // Fan out the per-wallet is-encrypted probe in parallel.
      await Promise.all(
        wallets.map(async (row) => {
          try {
            row.encrypted = await api.isWalletEncrypted(row.name);
          } catch {
            // Treat probe failure as "unknown" — show no badge rather
            // than blocking the picker. The actual select will surface
            // any real error.
            row.encrypted = false;
          }
        }),
      );
    } catch (e) {
      err = String(e);
    }
  }

  $effect(() => {
    void refresh();
  });

  async function pick(row: WalletRow) {
    err = null;
    if (row.encrypted) {
      prompt = { target: row.name, passphrase: "", error: null };
      return;
    }
    busy = true;
    try {
      await api.selectWallet(row.name);
      session.currentWallet = row.name;
      go("dashboard");
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function submitPassphrase() {
    // Capture into a local so TS narrowing survives the await.
    const p = prompt;
    if (!p) return;
    busy = true;
    p.error = null;
    try {
      await api.selectWallet(p.target, p.passphrase);
      prompt = null;
      session.currentWallet = p.target;
      go("dashboard");
    } catch (e) {
      p.error = String(e);
    } finally {
      busy = false;
    }
  }

  function cancelPrompt() {
    prompt = null;
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
      {#each wallets as row (row.name)}
        <li>
          <button class="row-button" disabled={busy} onclick={() => pick(row)}>
            <span class="left">
              <span class="name">{row.name}</span>
              {#if row.encrypted}
                <span class="badge" title="this wallet is encrypted; a passphrase is required to open it">encrypted</span>
              {/if}
            </span>
            <span class="arrow">→</span>
          </button>
        </li>
      {/each}
    </ul>
    <button onclick={createNew}>+ create new wallet</button>
  {/if}

  {#if prompt}
    <div
      class="overlay"
      onclick={cancelPrompt}
      onkeydown={(e) => e.key === "Escape" && cancelPrompt()}
      role="presentation"
    >
      <div
        class="modal card"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
        role="dialog"
        tabindex="-1"
      >
        <h3>unlock {prompt.target}</h3>
        <p class="muted small">
          Enter the passphrase you set when creating this wallet. The
          derived key stays in memory until you switch wallets.
        </p>
        <form
          onsubmit={(e) => {
            e.preventDefault();
            void submitPassphrase();
          }}
        >
          <div class="field">
            <label for="pp">passphrase</label>
            <!-- svelte-ignore a11y_autofocus — autofocusing the input is correct UX
                 for a passphrase dialog the user explicitly opened. -->
            <input
              id="pp"
              type="password"
              autocomplete="current-password"
              bind:value={prompt.passphrase}
              disabled={busy}
              autofocus
            />
          </div>
          {#if prompt.error}
            <div class="error">{prompt.error}</div>
          {/if}
          <div class="row">
            <button type="button" onclick={cancelPrompt} disabled={busy}>cancel</button>
            <button
              type="submit"
              class="primary"
              disabled={busy || prompt.passphrase.length === 0}
            >
              {busy ? "unlocking…" : "unlock"}
            </button>
          </div>
        </form>
      </div>
    </div>
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
  .row-button .left {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .row-button .name {
    font-weight: 600;
  }
  .row-button .arrow {
    color: var(--color-text-muted);
  }
  .badge {
    display: inline-block;
    padding: 0.1rem 0.5rem;
    border-radius: 999px;
    font-size: 0.7rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    color: #1e40af;
    background: #dbeafe;
    border: 1px solid currentColor;
  }
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .modal {
    width: min(420px, 90vw);
    background: var(--color-surface);
  }
  .modal h3 {
    margin: 0 0 var(--space-2);
    font-size: 1rem;
    word-break: break-all;
  }
  .small {
    font-size: 0.85rem;
  }
</style>
