// Shared app state. Uses Svelte 5 runes; the `.svelte.ts` extension
// is what unlocks rune usage in a non-component module.

import type {
  MintUtxoOutput,
  CheckMintFundingOutput,
  MintMessageOutput,
} from "./types";

export type View =
  | "loading"
  | "picker"
  | "setup"
  | "dashboard"
  | "mint"
  | "transfer"
  | "reclaim"
  | "overview"
  | "history";

/**
 * In-flight mint stage. Mirrors the linear flow the Mint view drives
 * users through. Held in `session` so the in-progress mint *survives
 * navigation* — leaving Mint and returning lands the user back where
 * they were, with the same UTXO record and funding poll cadence. (The
 * old behaviour was to lose all of this on unmount; combined with
 * Mint being the only place to submit the mint message, that meant a
 * mid-flow detour could silently forfeit L2 credit. Dashboard's
 * pending-mints surface plus this resumable state are the two halves
 * of that fix.)
 *
 * `null` when there's no active mint — the form is fresh and the
 * user hasn't yet derived an address.
 */
export interface ActiveMint {
  stage: "form" | "funding" | "mint" | "done";
  lockBlocks: number;
  utxo: MintUtxoOutput | null;
  funding: CheckMintFundingOutput | null;
  msg: MintMessageOutput | null;
  confStage: "soft" | "hard";
  confirmedAtUnix: number | null;
}

export const session = $state({
  view: "loading" as View,
  /** Name of the currently-active wallet, or null when not selected. */
  currentWallet: null as string | null,
  /** In-flight Mint flow state, or null. See [[ActiveMint]]. */
  activeMint: null as ActiveMint | null,
});

export function go(v: View) {
  session.view = v;
}
