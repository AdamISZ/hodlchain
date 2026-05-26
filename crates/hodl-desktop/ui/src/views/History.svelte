<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../lib/api";
  import { go } from "../lib/state.svelte";
  import type { TxRecord, TxKind, TxStatus } from "../lib/types";

  let txs = $state<TxRecord[] | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);

  async function refresh() {
    err = null;
    busy = true;
    try {
      txs = await api.listTransactions();
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  onMount(refresh);

  // ---------- display helpers ----------

  function fmtNum(n: number): string {
    return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, "_");
  }

  function fmtTime(unix_secs: number): string {
    if (unix_secs === 0) return "—";
    const d = new Date(unix_secs * 1000);
    return d.toISOString().replace("T", " ").replace(/\.\d+Z$/, "");
  }

  function kindLabel(k: TxKind): string {
    switch (k) {
      case "l1_deposit": return "L1 deposit";
      case "l1_reclaim": return "L1 reclaim";
      case "l2_mint_apply": return "L2 mint";
      case "l2_transfer_sent": return "L2 send";
      case "l2_transfer_received": return "L2 recv";
    }
  }

  function isL1(k: TxKind): boolean {
    return k === "l1_deposit" || k === "l1_reclaim";
  }

  function amountUnit(k: TxKind): string {
    return isL1(k) ? "sat" : "atoms";
  }

  function statusLabel(s: TxStatus): string {
    switch (s.kind) {
      case "soft":       return "soft";
      case "l1_mempool": return "in mempool";
      case "in_block":   return "in block";
      case "finalized":  return "finalized";
      case "failed":     return "failed";
    }
  }

  function statusClass(s: TxStatus): string {
    return `status-${s.kind}`;
  }

  function statusTitle(s: TxStatus): string {
    switch (s.kind) {
      case "soft":
        return `Sequencer-accepted at ${fmtTime(s.since_ts)}; awaiting next L2 block.`;
      case "l1_mempool":
        return `Broadcast at ${fmtTime(s.since_ts)}; awaiting L1 confirmation.`;
      case "in_block":
        return `Observed in a block at L2 height ${s.l2_height} / L1 height ${s.l1_height}.`;
      case "finalized":
        return `L1-anchored past reorg-finality depth. L2 height ${s.l2_height} / L1 height ${s.l1_height}.`;
      case "failed":
        return `Failed at ${fmtTime(s.ts)}: ${s.reason}`;
    }
  }

  function truncate(s: string | null | undefined, head = 8, tail = 4): string {
    if (!s) return "—";
    if (s.length <= head + tail + 3) return s;
    return `${s.slice(0, head)}…${s.slice(-tail)}`;
  }
</script>

<header class="topbar">
  <button onclick={() => go("dashboard")}>← back</button>
  <h2>transaction history</h2>
  <button onclick={refresh} disabled={busy} class="refresh">
    {busy ? "…" : "refresh"}
  </button>
</header>

<main>
  {#if err}
    <div class="error">{err}</div>
  {/if}

  {#if txs === null}
    <p class="muted">loading…</p>
  {:else if txs.length === 0}
    <p class="muted">no transactions yet. mint or send something to see entries appear here.</p>
  {:else}
    <table>
      <thead>
        <tr>
          <th>when</th>
          <th>kind</th>
          <th>status</th>
          <th class="num">amount</th>
          <th>counterparty</th>
          <th>ref</th>
        </tr>
      </thead>
      <tbody>
        {#each txs as tx (tx.id)}
          <tr>
            <td class="mono small">{fmtTime(tx.created_ts)}</td>
            <td>{kindLabel(tx.kind)}</td>
            <td>
              <span class="pill {statusClass(tx.status)}" title={statusTitle(tx.status)}>
                {statusLabel(tx.status)}
              </span>
            </td>
            <td class="num mono">
              {fmtNum(tx.amount)} <span class="muted small">{amountUnit(tx.kind)}</span>
            </td>
            <td class="mono small" title={tx.counterparty ?? ""}>
              {truncate(tx.counterparty)}
            </td>
            <td class="mono small">
              {#if tx.l1_txid}
                <span title={`L1 txid: ${tx.l1_txid}`}>{truncate(tx.l1_txid)}</span>
              {:else if tx.l2_sighash}
                <span title={`L2 sighash / nullifier: ${tx.l2_sighash}`}>
                  {truncate(tx.l2_sighash)}
                </span>
              {:else}
                <span class="muted">—</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
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
  .refresh {
    justify-self: end;
  }
  main {
    max-width: 980px;
    margin: var(--space-5) auto;
    padding: 0 var(--space-4);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.9rem;
  }
  thead th {
    text-align: left;
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-border);
    color: var(--color-text-muted);
    font-weight: 600;
  }
  th.num,
  td.num {
    text-align: right;
  }
  tbody td {
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-border);
    vertical-align: middle;
  }
  tbody tr:hover {
    background: var(--color-surface);
  }
  .small {
    font-size: 0.82rem;
  }
  .pill {
    display: inline-block;
    padding: 0.1rem 0.5rem;
    font-size: 0.78rem;
    border-radius: 999px;
    background: var(--color-border);
    color: var(--color-text);
  }
  .pill.status-soft {
    background: var(--color-warning, #fef3c7);
    color: #92400e;
  }
  .pill.status-l1_mempool {
    background: var(--color-warning, #fef3c7);
    color: #92400e;
  }
  .pill.status-in_block {
    background: #dbeafe;
    color: #1e40af;
  }
  .pill.status-finalized {
    background: #d1fae5;
    color: #065f46;
  }
  .pill.status-failed {
    background: #fee2e2;
    color: #991b1b;
  }
</style>
