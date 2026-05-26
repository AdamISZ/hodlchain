<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../lib/api";
  import { go } from "../lib/state.svelte";
  import type { TxRecord, TxKind, TxStatus } from "../lib/types";

  let txs = $state<TxRecord[] | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);

  // "all" = unfiltered; otherwise an exact match against the field.
  let kindFilter = $state<"all" | TxKind>("all");
  let statusFilter = $state<"all" | TxStatus["kind"]>("all");

  // One expanded row at a time; click again to collapse.
  let expandedId = $state<string | null>(null);

  // Per-copy-button transient "copied" tick.
  let copiedKey = $state<string | null>(null);

  async function refresh() {
    err = null;
    busy = true;
    try {
      // listTransactions() just re-reads wallet.json; it doesn't move
      // statuses. The driver for status transitions (Soft → InBlock
      // → Finalized, L1Mempool → InBlock) is `light_balance`, which
      // walks new L2 attestations, projects events into TxRecords,
      // and polls esplora for L1 confirmations on in-flight records.
      // Run it first so the wallet file we then read is up-to-date.
      // We don't care about the balance return value here.
      await api.lightBalance({ addr: null });
      txs = await api.listTransactions();
    } catch (e) {
      err = String(e);
    } finally {
      busy = false;
    }
  }

  async function copy(value: string, key: string) {
    try {
      await navigator.clipboard.writeText(value);
      copiedKey = key;
      setTimeout(() => {
        if (copiedKey === key) copiedKey = null;
      }, 1500);
    } catch {
      // clipboard may be blocked; full value is in the title attribute
    }
  }

  function toggleExpand(id: string) {
    expandedId = expandedId === id ? null : id;
  }

  onMount(refresh);

  let visible = $derived.by(() => {
    if (txs === null) return [];
    return txs.filter((t) => {
      if (kindFilter !== "all" && t.kind !== kindFilter) return false;
      if (statusFilter !== "all" && t.status.kind !== statusFilter) return false;
      return true;
    });
  });

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

  function feeDisplay(tx: TxRecord): string {
    if (tx.fee_atoms != null) return `${fmtNum(tx.fee_atoms)} atoms`;
    if (tx.fee_sat != null) return `${fmtNum(tx.fee_sat)} sat`;
    return "—";
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
  {:else}
    <div class="filters">
      <label>
        <span class="muted small">kind</span>
        <select bind:value={kindFilter}>
          <option value="all">all</option>
          <option value="l1_deposit">L1 deposit</option>
          <option value="l1_reclaim">L1 reclaim</option>
          <option value="l2_mint_apply">L2 mint</option>
          <option value="l2_transfer_sent">L2 send</option>
          <option value="l2_transfer_received">L2 recv</option>
        </select>
      </label>
      <label>
        <span class="muted small">status</span>
        <select bind:value={statusFilter}>
          <option value="all">all</option>
          <option value="soft">soft</option>
          <option value="l1_mempool">in mempool</option>
          <option value="in_block">in block</option>
          <option value="finalized">finalized</option>
          <option value="failed">failed</option>
        </select>
      </label>
      <span class="count muted small">
        {visible.length} of {txs.length}
      </span>
    </div>

    {#if visible.length === 0}
      <p class="muted">
        {txs.length === 0
          ? "no transactions yet. mint or send something to see entries appear here."
          : "no transactions match the current filters."}
      </p>
    {:else}
      <!--
        Layout uses CSS Grid (not <table>) so the conditional
        expand-panel doesn't cause browser table-fixup quirks. Each
        row is itself a subgrid that inherits the outer grid's
        column tracks, which is what keeps headers and data
        perfectly column-aligned.
      -->
      <div class="tx-grid">
        <div class="row header">
          <div></div>
          <div>when</div>
          <div>kind</div>
          <div>status</div>
          <div class="num">amount</div>
          <div>counterparty</div>
          <div>ref</div>
        </div>

        {#each visible as tx (tx.id)}
          <div
            class="row data"
            onclick={() => toggleExpand(tx.id)}
            role="button"
            tabindex="0"
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                toggleExpand(tx.id);
              }
            }}
          >
            <div class="caret-cell">
              <span class="caret" class:open={expandedId === tx.id}>▸</span>
            </div>
            <div class="mono small">{fmtTime(tx.created_ts)}</div>
            <div>{kindLabel(tx.kind)}</div>
            <div>
              <span
                class="pill {statusClass(tx.status)}"
                title={statusTitle(tx.status)}
              >
                {statusLabel(tx.status)}
              </span>
            </div>
            <div class="num mono">
              {fmtNum(tx.amount)} <span class="muted small">{amountUnit(tx.kind)}</span>
            </div>
            <div class="mono small truncate" title={tx.counterparty ?? ""}>
              {truncate(tx.counterparty)}
            </div>
            <div class="mono small truncate">
              {#if tx.l1_txid}
                <span title={`L1 txid: ${tx.l1_txid}`}>{truncate(tx.l1_txid)}</span>
              {:else if tx.l2_sighash}
                <span title={`L2 sighash / nullifier: ${tx.l2_sighash}`}>
                  {truncate(tx.l2_sighash)}
                </span>
              {:else}
                <span class="muted">—</span>
              {/if}
            </div>
          </div>

          <div class="expand-panel" class:hidden={expandedId !== tx.id}>
            {#if expandedId === tx.id}
              <dl>
                <dt>id</dt>
                <dd class="mono">{tx.id}</dd>

                <dt>created</dt>
                <dd class="mono">{fmtTime(tx.created_ts)}</dd>

                <dt>amount</dt>
                <dd class="mono">
                  {fmtNum(tx.amount)} {amountUnit(tx.kind)}
                </dd>

                {#if tx.fee_atoms != null || tx.fee_sat != null}
                  <dt>fee</dt>
                  <dd class="mono">{feeDisplay(tx)}</dd>
                {/if}

                {#if tx.counterparty}
                  <dt>counterparty</dt>
                  <dd>
                    <span class="mono">{tx.counterparty}</span>
                    <button
                      class="copy"
                      onclick={(e) => {
                        e.stopPropagation();
                        copy(tx.counterparty!, `${tx.id}:cp`);
                      }}
                    >
                      {copiedKey === `${tx.id}:cp` ? "✓ copied" : "copy"}
                    </button>
                  </dd>
                {/if}

                {#if tx.l1_txid}
                  <dt>L1 txid</dt>
                  <dd>
                    <span class="mono">{tx.l1_txid}</span>
                    <button
                      class="copy"
                      onclick={(e) => {
                        e.stopPropagation();
                        copy(tx.l1_txid!, `${tx.id}:l1`);
                      }}
                    >
                      {copiedKey === `${tx.id}:l1` ? "✓ copied" : "copy"}
                    </button>
                  </dd>
                {/if}

                {#if tx.l2_sighash}
                  <dt>
                    {tx.kind === "l2_mint_apply" ? "nullifier" : "L2 sighash"}
                  </dt>
                  <dd>
                    <span class="mono">{tx.l2_sighash}</span>
                    <button
                      class="copy"
                      onclick={(e) => {
                        e.stopPropagation();
                        copy(tx.l2_sighash!, `${tx.id}:sh`);
                      }}
                    >
                      {copiedKey === `${tx.id}:sh` ? "✓ copied" : "copy"}
                    </button>
                  </dd>
                {/if}

                {#if tx.bip32_index != null}
                  <dt>bip32 index</dt>
                  <dd class="mono">{tx.bip32_index}</dd>
                {/if}

                <dt>status</dt>
                <dd>
                  <span
                    class="pill {statusClass(tx.status)}"
                    title={statusTitle(tx.status)}
                  >
                    {statusLabel(tx.status)}
                  </span>
                  <span class="muted small">
                    {#if tx.status.kind === "soft"}
                      since {fmtTime(tx.status.since_ts)}
                    {:else if tx.status.kind === "l1_mempool"}
                      since {fmtTime(tx.status.since_ts)}
                    {:else if tx.status.kind === "in_block"}
                      L2 {tx.status.l2_height} / L1 {tx.status.l1_height} at
                      {fmtTime(tx.status.included_ts)}
                    {:else if tx.status.kind === "finalized"}
                      L2 {tx.status.l2_height} / L1 {tx.status.l1_height}
                    {:else if tx.status.kind === "failed"}
                      at {fmtTime(tx.status.ts)}
                    {/if}
                  </span>
                  {#if tx.status.kind === "failed"}
                    <div class="failed-reason small">
                      <span class="muted">reason:</span>
                      <span class="mono">{tx.status.reason}</span>
                    </div>
                  {/if}
                </dd>
              </dl>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
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
    max-width: 1100px;
    margin: var(--space-5) auto;
    padding: 0 var(--space-4);
  }
  .filters {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    margin-bottom: var(--space-3);
  }
  .filters label {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
  .filters select {
    font: inherit;
    padding: 0.15rem 0.4rem;
  }
  .count {
    margin-left: auto;
  }

  /* ---- Grid layout (not <table>) ----
   *
   * Seven column tracks defined once on the outer container; each
   * row is its own subgrid that inherits these tracks, so headers
   * and data cells align exactly. The conditional expand-panel
   * spans all columns via `grid-column: 1 / -1`.
   */
  .tx-grid {
    display: grid;
    grid-template-columns:
      1.5rem               /* caret */
      max-content          /* when (timestamp) */
      max-content          /* kind */
      max-content          /* status pill */
      max-content          /* amount */
      minmax(8rem, 1fr)    /* counterparty (flexes) */
      max-content;         /* ref */
    font-size: 0.9rem;
    align-items: stretch;
  }
  .row {
    display: grid;
    grid-column: 1 / -1;
    grid-template-columns: subgrid;
    align-items: center;
  }
  .row > div {
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--color-border);
    min-width: 0; /* allow truncation in the flex track */
  }
  .row.header > div {
    color: var(--color-text-muted);
    font-weight: 600;
  }
  .row.data {
    cursor: pointer;
  }
  .row.data:hover > div {
    background: var(--color-surface);
  }
  .row .num {
    text-align: right;
  }
  .caret-cell {
    color: var(--color-text-muted);
  }
  .caret {
    display: inline-block;
    transition: transform 0.12s ease;
    font-size: 0.75rem;
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .truncate {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .expand-panel {
    grid-column: 1 / -1;
    background: var(--color-bg);
    padding: var(--space-3) var(--space-5);
    border-bottom: 1px solid var(--color-border);
  }
  .expand-panel.hidden {
    display: none;
  }
  .expand-panel dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-2) var(--space-4);
    margin: 0;
  }
  .expand-panel dt {
    font-weight: 600;
    color: var(--color-text-muted);
  }
  .expand-panel dd {
    margin: 0;
    word-break: break-all;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .copy {
    font-size: 0.75rem;
    padding: 0.05rem 0.4rem;
  }
  .failed-reason {
    flex-basis: 100%;
    margin-top: var(--space-1);
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
