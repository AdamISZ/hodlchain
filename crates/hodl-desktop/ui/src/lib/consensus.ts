// TypeScript mirror of hodl-core::consensus. Kept tiny on purpose —
// only the bits the UI needs (mint formula + retarget window size).
// MUST stay in sync with crates/hodl-core/src/consensus.rs.

/**
 * f_mint(V, T, r) = V * (1 - (1 + rT) * e^{-rT}).
 *
 * `valueSat` is the BTC value locked (sat). `lockBlocks` is T (the gap
 * between the funding L1 block and the CSV unlock). `r` is the live
 * rate parameter. Returns the L2 atom amount.
 *
 * Matches the Rust implementation: clamps the ratio into [0, 1) and
 * floors the result so the JS preview agrees with what the sequencer
 * will credit. (ATOMS_PER_SAT = 1 for the POC so the multiplication
 * is dropped here.)
 */
export function mintFn(valueSat: number, lockBlocks: number, r: number): number {
  if (lockBlocks === 0 || valueSat === 0) return 0;
  const rt = r * lockBlocks;
  let ratio = 1 - (1 + rt) * Math.exp(-rt);
  // Defensive clamp — matches the Rust `clamp(0.0, 1.0 - f64::EPSILON)`.
  if (ratio < 0) ratio = 0;
  if (ratio > 1 - Number.EPSILON) ratio = 1 - Number.EPSILON;
  return Math.floor(valueSat * ratio);
}

/** Cumulative atoms required to close one retarget window (paper §7 M_w). */
export const RETARGET_MINT_WINDOW_ATOMS = 216_000_000_000;

/** Target atom issuance per L1 block (paper §7 M*). */
export const TARGET_ATOMS_PER_BLOCK = 50_000_000;
