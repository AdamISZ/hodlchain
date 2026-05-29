<script lang="ts">
  import QRCode from "qrcode";

  // Re-usable display for any bech32m / hex address-like string.
  // - shows the value in a monospace box
  // - "copy" button writes it to clipboard
  // - "QR" button toggles an inline SVG QR code below
  //
  // Used for the L1 deposit address, the bech32m L2 address shown
  // after wallet creation / on the dashboard, and the reclaim outpoint
  // address in the reclaim list.

  type Props = {
    value: string;
    /** Optional label rendered above the box (e.g. "deposit address"). */
    label?: string;
    /**
     * Visual density: "full" for hero-style boxes (deposit address),
     * "compact" for inline list items where vertical space matters.
     */
    size?: "full" | "compact";
  };
  let { value, label, size = "full" }: Props = $props();

  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | null = null;
  let showQr = $state(false);
  let qrSvg = $state<string | null>(null);
  let qrError = $state<string | null>(null);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      copied = true;
      if (copyTimer) clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        copied = false;
      }, 1500);
    } catch (e) {
      // Surface in the same place the copy chip lives so users notice.
      qrError = `copy failed: ${e}`;
    }
  }

  async function toggleQr() {
    if (showQr) {
      showQr = false;
      return;
    }
    if (qrSvg === null) {
      try {
        // Bitcoin addresses are case-sensitive for taproot bech32m
        // anyway; uppercasing for "alphanumeric mode" compaction is a
        // micro-optimization wallets sometimes do, but it would lose
        // case for non-address content. Keep value verbatim.
        qrSvg = await QRCode.toString(value, {
          type: "svg",
          errorCorrectionLevel: "M",
          margin: 1,
          width: 240,
        });
        qrError = null;
      } catch (e) {
        qrError = `QR render failed: ${e}`;
        return;
      }
    }
    showQr = true;
  }
</script>

<div class="address-box" class:compact={size === "compact"}>
  {#if label}
    <div class="label">{label}</div>
  {/if}
  <div class="row-wrap">
    <code class="value mono">{value}</code>
    <div class="actions">
      <button type="button" onclick={copy} title="copy to clipboard">
        {copied ? "✓ copied" : "copy"}
      </button>
      <button type="button" onclick={toggleQr} title="show QR code">
        {showQr ? "hide QR" : "QR"}
      </button>
    </div>
  </div>
  {#if qrError}
    <div class="error small">{qrError}</div>
  {/if}
  {#if showQr && qrSvg}
    <div class="qr">
      {@html qrSvg}
    </div>
  {/if}
</div>

<style>
  .address-box {
    width: 100%;
  }
  .label {
    font-weight: 600;
    font-size: 0.9rem;
    margin-bottom: var(--space-1);
  }
  .row-wrap {
    display: flex;
    align-items: stretch;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .value {
    flex: 1 1 16rem;
    min-width: 0;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
    background: var(--color-surface);
    word-break: break-all;
    user-select: all;
    /* Make sure long bech32m strings wrap rather than overflow. */
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    align-items: stretch;
    flex-shrink: 0;
  }
  .actions button {
    /* Override the global `width: 100%` button style (if any) so the
       action buttons sit compactly next to the value. */
    width: auto;
    white-space: nowrap;
  }
  .compact .value {
    padding: var(--space-1) var(--space-2);
    font-size: 0.85rem;
  }
  .compact .actions button {
    padding: var(--space-1) var(--space-2);
    font-size: 0.85rem;
  }
  .qr {
    margin-top: var(--space-3);
    display: inline-block;
    padding: var(--space-2);
    background: white;
    border: 1px solid var(--color-border);
    border-radius: var(--radius);
  }
  .qr :global(svg) {
    display: block;
    width: 240px;
    height: 240px;
  }
  .small {
    font-size: 0.85rem;
  }
</style>
