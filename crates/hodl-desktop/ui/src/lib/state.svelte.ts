// Shared app state. Uses Svelte 5 runes; the `.svelte.ts` extension
// is what unlocks rune usage in a non-component module.

export type View =
  | "loading"
  | "setup"
  | "dashboard"
  | "mint"
  | "transfer"
  | "reclaim";

export const session = $state({
  view: "loading" as View,
  walletPath: null as string | null,
  walletExists: null as boolean | null,
});

export function go(v: View) {
  session.view = v;
}
