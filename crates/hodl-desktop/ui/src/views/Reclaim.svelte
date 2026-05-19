<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../lib/api";
  import type { ReclaimableMint, ReclaimMintOutput } from "../lib/types";
  import { go } from "../lib/state.svelte";

  let mints = $state<ReclaimableMint[]>([]);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let result = $state<ReclaimMintOutput | null>(null);

  // Active reclaim dialog state. null when no dialog open.
  let dialog = $state<{ outpoint: string; dest: string; feeSat: number } | null>(
    null,
  );

  async function refresh() {
    err = null;
    busy = true;
    try {
      mints = await api.listReclaimableMints();
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  onMount(refresh);

  function openDialog(m: ReclaimableMint) {
    dialog = { outpoint: m.outpoint, dest: "", feeSat: 1000 };
  }

  function closeDialog() {
    dialog = null;
  }

  async function submitReclaim() {
    if (!dialog) return;
    err = null;
    busy = true;
    try {
      result = await api.reclaimMint({
        outpoint: dialog.outpoint,
        dest_address: dialog.dest,
        fee_sat: dialog.feeSat,
      });
      dialog = null;
      await refresh();
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  function statusLabel(m: ReclaimableMint): string {
    switch (m.status) {
      case "pending":
        return "pending confirmation";
      case "locked":
        return `locked, ${m.blocks_remaining ?? "?"} block(s) remaining`;
      case "ready":
        return "ready to reclaim";
      case "reclaimed":
        return "reclaimed";
    }
  }
</script>

<header class="topbar">
  <button onclick={() => go("dashboard")}>← back</button>
  <h2>reclaim deposits</h2>
  <span></span>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if result}
    <div class="card success-card">
      <p>
        <span class="success">✓ reclaim broadcast.</span>
        txid <code class="mono small">{result.txid}</code>
      </p>
      <p class="muted">
        {result.value_sat_in} sat → {result.value_sat_out} sat
        (fee {result.fee_sat} sat). Settles in 1 L1 confirmation.
      </p>
    </div>
  {/if}

  <div class="row">
    <button onclick={refresh} disabled={busy}>refresh</button>
  </div>

  {#if mints.length === 0}
    <p class="muted">no mint UTXOs recorded.</p>
  {:else}
    <ul class="mints">
      {#each mints as m (m.outpoint)}
        <li class="card">
          <div class="spread">
            <div>
              <div>
                <strong>{m.value_sat} sat</strong>
                <span class="muted">T={m.lock_blocks} blocks</span>
              </div>
              <div class="muted small mono">{m.outpoint}</div>
              <div class="small status-{m.status}">{statusLabel(m)}</div>
            </div>
            <div>
              {#if m.status === "ready"}
                <button class="primary" onclick={() => openDialog(m)}>
                  reclaim
                </button>
              {/if}
            </div>
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if dialog}
    <div
      class="overlay"
      onclick={closeDialog}
      onkeydown={(e) => e.key === "Escape" && closeDialog()}
      role="presentation"
    >
      <div
        class="modal card"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
        role="dialog"
        tabindex="-1"
      >
        <h3>reclaim {dialog.outpoint}</h3>
        <div class="field">
          <label for="dest">destination L1 address</label>
          <input
            id="dest"
            class="mono"
            type="text"
            bind:value={dialog.dest}
          />
        </div>
        <div class="field">
          <label for="fee">fee (sat)</label>
          <input id="fee" type="number" min="0" bind:value={dialog.feeSat} />
        </div>
        <div class="row">
          <button onclick={closeDialog}>cancel</button>
          <button
            class="primary"
            disabled={busy || !dialog.dest}
            onclick={submitReclaim}
          >
            {busy ? "broadcasting…" : "broadcast reclaim"}
          </button>
        </div>
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
    max-width: 720px;
    margin: var(--space-5) auto;
    padding: 0 var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  ul.mints {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .small {
    font-size: 0.85rem;
  }
  .status-ready {
    color: var(--color-success);
    font-weight: 600;
  }
  .status-locked {
    color: var(--color-warning);
  }
  .status-pending {
    color: var(--color-text-muted);
  }
  .status-reclaimed {
    color: var(--color-text-muted);
    text-decoration: line-through;
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
    width: min(500px, 90vw);
    background: var(--color-surface);
  }
  .modal h3 {
    margin: 0 0 var(--space-4);
    font-size: 1rem;
    word-break: break-all;
  }
  .success-card {
    background: #f0fdf4;
    border-color: #bbf7d0;
  }
</style>
