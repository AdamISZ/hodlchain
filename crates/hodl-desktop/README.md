# hodl-desktop

Tauri v2 + Svelte 5 + TypeScript desktop wallet for the hodlcoin POC.

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

Outputs (with `bundle.targets = ["appimage", "deb"]` in
`tauri.conf.json`):

```
target/release/bundle/appimage/hodlcoin_0.1.0_amd64.AppImage
target/release/bundle/deb/hodlcoin_0.1.0_amd64.deb
```

## Wallet location

Linux: `$XDG_CONFIG_HOME/hodlcoin/wallet.json`
(default `~/.config/hodlcoin/wallet.json`).
macOS: `~/Library/Application Support/hodlcoin/wallet.json`.
Windows: `%APPDATA%/hodlcoin/wallet.json`.

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
