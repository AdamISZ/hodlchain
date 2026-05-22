# Cutting a release

Maintainer notes. Users want
[`run-the-gui.md`](./run-the-gui.md) instead.

## The full flow

```bash
# 1. Bump the version. One file — workspace.package.version is the
#    single source of truth (Tauri reads it from there too).
$EDITOR Cargo.toml      # change e.g. 0.1.0 → 0.2.0
cargo check             # forces Cargo.lock to update
git add Cargo.toml Cargo.lock
git commit -m "Bump to 0.2.0"
git push

# 2. Tag and push the tag. This triggers .github/workflows/release.yml.
git tag v0.2.0
git push --tags

# 3. Wait ~10 minutes. The workflow:
#    - validates that the tag matches workspace.package.version
#    - builds backend tarballs on Linux + macOS (in parallel)
#    - builds GUI bundles on Linux (.AppImage + .deb) + macOS (.dmg)
#    - creates a DRAFT GitHub Release with all six artifacts attached
#
#    Watch progress at:
#        https://github.com/AdamISZ/hodlchain/actions

# 4. Pull the artifacts locally so you can GPG-sign them. The signing
#    key never touches CI — that's the whole point.
mkdir -p /tmp/release-v0.2.0 && cd /tmp/release-v0.2.0
gh release download v0.2.0

# 5. Sign every file. ASCII-armored detached signatures (the .asc form
#    most reproducible-builds-style verifications expect).
for f in *.AppImage *.deb *.dmg *.tar.gz; do
    [ -f "$f" ] && gpg --detach-sign --armor "$f"
done

# 6. Attach signatures to the same draft release.
gh release upload v0.2.0 *.asc

# 7. Fill in release notes (Releases UI on GitHub), then publish.
gh release edit v0.2.0 --draft=false
#   …or click "Publish release" in the web UI after you've reviewed
#   the notes and the artifact list.
```

## Release artifact inventory

A complete release has **six** files (plus their `.asc` sidecars):

```
hodlchain-backend-linux-x86_64-<version>.tar.gz
hodlchain-backend-macos-aarch64-<version>.tar.gz
hodlchain_<version>_amd64.AppImage
hodlchain_<version>_amd64.deb
hodlchain_<version>_aarch64.dmg
```

…plus matching `.asc` files. (Tauri picks the version + arch suffix
itself from the bundle config + the host arch; the backend tarball
gets versioned by the workflow.)

## Exercising the workflow without cutting a release

`workflow_dispatch` lets you push the Run button on a branch. The
build jobs run; the `release` job is skipped, so nothing ends up in
the Releases tab. Useful if you've touched the workflow file or the
Tauri config and want to confirm it still builds before tagging.

## Signing key

The maintainer key the project signs releases with should be linked
from the README and ideally the GitHub profile. For a POC the chain
of trust is "follow Adam on Twitter / read the paper"; for a real
launch we'd want the key in a more durable location and probably
some independent verification path.

## What's NOT automated (yet)

- **Apple Developer code-signing + notarisation.** Requires a paid
  Apple Developer account ($99/yr) plus secret plumbing. Until then
  macOS users hit Gatekeeper on first launch — the
  `xattr -dr com.apple.quarantine` workaround is in `run-the-gui.md`.
- **Windows builds.** No `nsis` / `msi` job today. The Tauri config
  would produce them on a Windows runner; nobody has asked yet.
- **macOS x86_64.** Apple Silicon only, by design.
- **Release notes.** The draft release ships with an empty body —
  hand-write notes before publishing.
- **`CHANGELOG.md`.** None yet. Worth adding once we have more than
  one release.
