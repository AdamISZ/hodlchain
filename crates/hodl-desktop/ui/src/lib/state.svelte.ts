// Shared app state. Uses Svelte 5 runes; the `.svelte.ts` extension
// is what unlocks rune usage in a non-component module.

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

export const session = $state({
  view: "loading" as View,
  /** Name of the currently-active wallet, or null when not selected. */
  currentWallet: null as string | null,
});

export function go(v: View) {
  session.view = v;
}
