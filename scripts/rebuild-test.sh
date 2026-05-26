#!/usr/bin/env bash
#
# Rebuild every artifact you might launch during a manual test session:
# the standalone backend binaries (hodl-sequencer, hodl-node, hodl-regtest,
# hodl-wallet CLI) AND the desktop GUI AppImage (frontend bundle +
# embedded hodl-desktop binary).
#
# `cargo build --workspace --release` covers the standalone binaries but
# does NOT refresh the AppImage at target/release/bundle/appimage/.
# `cargo tauri build` covers the AppImage but only touches the desktop
# crate. Both are needed; this script runs them in sequence and dumps
# timestamps at the end so you can confirm everything actually updated.
#
# Pass `--debug` to skip release-mode LTO and bundle a debug AppImage —
# typically 3-5× faster, fine for behavioural smoke-testing.
#
# Usage:
#   scripts/rebuild-test.sh            # full release rebuild
#   scripts/rebuild-test.sh --debug    # fast debug rebuild

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# pnpm + node live outside the default PATH on this dev machine; the
# Tauri beforeBuildCommand needs both. Harmless if they're already on
# PATH or if these paths don't exist on a different machine.
export PATH="$HOME/.local/bin:$HOME/.local/node/bin:$PATH"

# Belt-and-braces: prevent Corepack from auto-fetching a newer pnpm if
# the user's Node ships a Corepack with stale signing keys (Node 22.11.0
# hit this; later patch versions don't). The standalone pnpm in
# ~/.local/bin should win because of the PATH order above, but this
# stops Corepack hijacking even if it gets ahead.
export COREPACK_INTEGRITY_KEYS=0

mode_flag=""
profile_label="release"
if [[ "${1:-}" == "--debug" ]]; then
    mode_flag="--debug"
    profile_label="debug"
fi

echo "[..] building standalone backend binaries ($profile_label)..."
if [[ "$profile_label" == "release" ]]; then
    cargo build --workspace --release
else
    cargo build --workspace
fi

echo
echo "[..] building desktop GUI (frontend + hodl-desktop + AppImage)..."
( cd crates/hodl-desktop && cargo tauri build $mode_flag --bundles appimage )

# Resolve the binary directory the AppImage lands under. Tauri puts
# release artifacts under target/release/... and debug ones under
# target/debug/... regardless of --bundles.
bin_dir="target/release"
if [[ "$profile_label" == "debug" ]]; then
    bin_dir="target/debug"
fi

echo
echo "[ok] timestamps after rebuild ($profile_label):"
for p in \
    "$bin_dir"/hodl-sequencer \
    "$bin_dir"/hodl-node \
    "$bin_dir"/hodl-regtest \
    "$bin_dir"/hodl-wallet \
    "$bin_dir"/hodl-desktop \
    "$bin_dir"/bundle/appimage/*.AppImage; do
    [ -e "$p" ] && printf "  %s  %s\n" "$(date -r "$p" '+%Y-%m-%d %H:%M:%S')" "$p"
done

echo
echo "Reminder: if a hodl-regtest is already running, it may still be"
echo "using daemons spawned from older binaries. Run:"
echo "  $bin_dir/hodl-regtest stop"
echo "  $bin_dir/hodl-regtest reset --yes"
echo "  $bin_dir/hodl-regtest start"
echo "to pick up the freshly-built sequencer/node."
