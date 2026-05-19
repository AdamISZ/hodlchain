# hodl-desktop

Tauri v2 + Svelte 5 + TypeScript desktop wallet for the hodlchain POC.

The Rust side is a thin Tauri wrapper around `hodl_wallet::ops::*`:
`src/lib.rs` registers the commands; `src/commands.rs` is one
3-5-line wrapper per `ops::*` function; `src/state.rs` holds the
resolved wallet-file path. No business logic lives here.

## Prerequisites

### Linux (tested target: Ubuntu 24.04+)

Ubuntu 22.04 ships webkit2gtk-4.0; Tauri v2 needs 4.1, which is
available on 24.04+ or via backports.

```bash
sudo apt install -y \
    libwebkit2gtk-4.1-dev \
    libsoup-3.0-dev \
    libssl-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    pkg-config \
    build-essential
```

### Frontend toolchain

```bash
# Node 20+ and pnpm 9+
nvm install 20      # or your package manager of choice
npm install -g pnpm
```

### Tauri CLI

```bash
cargo install tauri-cli --version "^2" --locked
```

## Development

```bash
# First-time only: install JS deps.
cd crates/hodl-desktop/ui && pnpm install

# Run dev server + Tauri shell with HMR.
cd crates/hodl-desktop && cargo tauri dev
```

The first build downloads webview tooling and compiles a chunky
Rust dep tree — give it 5 minutes. Subsequent runs are seconds.

## Production build

```bash
cd crates/hodl-desktop && cargo tauri build
```

`bundle.targets` in `tauri.conf.json` is set to `"all"`, which means
"every bundle type supported on the current platform". Cross-builds
aren't supported — you need to run the build *on* the OS you're
targeting.

| Host    | Bundles produced                            |
|---------|---------------------------------------------|
| Linux   | `.AppImage`, `.deb` (and `.rpm` if rpmbuild is installed) |
| macOS   | `.app`, `.dmg`                              |
| Windows | `.exe` (NSIS), `.msi`                       |

Outputs land under `target/release/bundle/<target>/`.

### macOS notes

- Requires the Apple developer toolchain — install Xcode Command
  Line Tools (`xcode-select --install`).
- Tauri auto-generates an `.icns` from the PNGs in `icons/` if you
  don't provide one explicitly. For a polished release, run
  `cargo tauri icon path/to/logo.png` first; it produces a proper
  `icon.icns` (plus `.ico` for Windows) and updates the icon list.
- `bundle.macOS.minimumSystemVersion` is set to 10.15. Bump if the
  embedded webview (WKWebView) ever needs a newer floor.
- The first time you launch the unsigned `.app` or `.dmg` macOS
  Gatekeeper will block it; right-click → Open, or run
  `xattr -dr com.apple.quarantine /Applications/hodlchain.app`.
  Signing + notarisation are out of scope for the POC.

### Windows notes

Out of scope for now. The Tauri config doesn't preclude it
(`targets: "all"` will pick up NSIS + MSI on a Windows host) but
nobody has built it yet.

### Cross-platform CI

A GitHub Actions workflow with `macos-latest` and `windows-latest`
matrix entries is the natural way to produce releases without
needing a Mac on hand. Not set up yet; happy to add when needed.

## Wallet location

Linux: `$XDG_CONFIG_HOME/hodlchain/wallet.json`
(default `~/.config/hodlchain/wallet.json`).
macOS: `~/Library/Application Support/hodlchain/wallet.json`.
Windows: `%APPDATA%/hodlchain/wallet.json`.

CLI users have `hodl-wallet --wallet <path>` to point elsewhere.
The desktop app deliberately doesn't expose this — its wallet
location is part of its stable contract with the user.

## Layout

```
crates/hodl-desktop/
  Cargo.toml            # Rust crate (lib + bin)
  build.rs              # tauri-build hook
  tauri.conf.json       # Tauri v2 app config
  capabilities/         # Tauri v2 permissions
  icons/                # bundle icons (placeholder PNGs for now;
                        # run `cargo tauri icon path/to/logo.png`
                        # to regenerate from a source image)
  src/
    main.rs             # thin entrypoint, calls run()
    lib.rs              # Tauri Builder setup + invoke_handler
    state.rs            # AppState (resolved wallet path)
    commands.rs         # #[tauri::command] wrappers over ops::*
  ui/                   # Svelte + Vite frontend
    package.json
    vite.config.ts
    svelte.config.js
    tsconfig.json
    index.html
    src/
      main.ts           # Svelte 5 mount
      App.svelte        # root component
```
