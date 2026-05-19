//! Multi-wallet directory helpers.
//!
//! A "wallet directory" holds zero or more `<name>.json` files, each
//! a `WalletFile`. The desktop app's `AppState` resolves the current
//! wallet by name into a path here. The CLI keeps its
//! anywhere-on-disk `--wallet PATH` model and doesn't use this layer.
//!
//! Naming policy: ASCII alphanumerics plus `-` and `_`, length 1..=32,
//! deliberately strict so the name maps unambiguously to a single
//! file and to a label safely shown in the UI. No `.` `..` `/` etc.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Maximum wallet-name length. Long enough for `alice-mainnet-cold-2`,
/// short enough to stay on one line in a picker.
pub const MAX_NAME_LEN: usize = 32;

/// Reject names that aren't single-segment ASCII identifiers. Returns
/// the canonicalised name on success (we don't actually transform it
/// today, but routing the value through here means UIs see exactly
/// what gets written to disk).
pub fn validate_name(name: &str) -> Result<&str> {
    if name.is_empty() {
        return Err(anyhow!("wallet name must not be empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(anyhow!(
            "wallet name {} char(s) long; max is {MAX_NAME_LEN}",
            name.len()
        ));
    }
    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(anyhow!(
                "wallet name {name:?} contains disallowed char {c:?}; \
                 allowed: a-z A-Z 0-9 - _"
            ));
        }
    }
    Ok(name)
}

/// Resolve a wallet name into the path it lives at on disk. Does not
/// check whether the file exists.
pub fn wallet_path_for(wallets_dir: &Path, name: &str) -> Result<PathBuf> {
    let name = validate_name(name)?;
    Ok(wallets_dir.join(format!("{name}.json")))
}

/// Make sure `wallets_dir` exists (mkdir -p). Idempotent.
pub fn ensure_wallets_dir(wallets_dir: &Path) -> Result<()> {
    fs::create_dir_all(wallets_dir)
        .with_context(|| format!("create wallets dir at {}", wallets_dir.display()))?;
    Ok(())
}

/// List the wallet names visible in `wallets_dir` — every `*.json`
/// file's basename minus extension, sorted, names that fail
/// `validate_name` silently dropped (they can't be selected anyway,
/// and surfacing them in the picker would be confusing).
pub fn list_wallet_files(wallets_dir: &Path) -> Result<Vec<String>> {
    if !wallets_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(wallets_dir)
        .with_context(|| format!("read wallets dir {}", wallets_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        let Some(stem) = s.strip_suffix(".json") else { continue };
        if validate_name(stem).is_ok() {
            out.push(stem.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// One-shot migration for users on the pre-picker single-wallet
/// layout. If `legacy_path` exists and `wallets_dir` is empty, move
/// the legacy file to `wallets_dir/default.json` so it shows up in
/// the picker. No-op otherwise. Errors propagate; partial state isn't
/// possible because we use atomic rename.
pub fn migrate_legacy_wallet(legacy_path: &Path, wallets_dir: &Path) -> Result<()> {
    if !legacy_path.exists() {
        return Ok(());
    }
    ensure_wallets_dir(wallets_dir)?;
    if !list_wallet_files(wallets_dir)?.is_empty() {
        return Ok(()); // user already has wallets in the new layout
    }
    let target = wallet_path_for(wallets_dir, "default")?;
    fs::rename(legacy_path, &target)
        .with_context(|| format!("migrate {} -> {}", legacy_path.display(), target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn validate_name_accepts_simple_identifiers() {
        for ok in ["alice", "alice-mainnet", "bob_test", "x", "X", "1", "a-1_z"] {
            assert!(validate_name(ok).is_ok(), "expected {ok:?} ok");
        }
    }

    #[test]
    fn validate_name_rejects_bad_input() {
        for bad in [
            "",
            "alice.json",
            "../escape",
            "with/slash",
            "with\\slash",
            "with space",
            "emoji-🚀",
            "name?",
        ] {
            assert!(validate_name(bad).is_err(), "expected {bad:?} err");
        }
    }

    #[test]
    fn validate_name_enforces_length() {
        let just_long = "a".repeat(MAX_NAME_LEN);
        assert!(validate_name(&just_long).is_ok());
        let too_long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name(&too_long).is_err());
    }

    #[test]
    fn list_wallet_files_returns_sorted_names_only() {
        let dir = tempdir().unwrap();
        for f in [
            "bob.json",
            "alice.json",
            "charlie.json",
            "notes.txt",        // not .json — ignored
            "bad name.json",    // bad name — ignored
            "..hidden.json",    // bad name — ignored
        ] {
            File::create(dir.path().join(f)).unwrap().write_all(b"{}").unwrap();
        }
        let names = list_wallet_files(dir.path()).unwrap();
        assert_eq!(names, vec!["alice", "bob", "charlie"]);
    }

    #[test]
    fn list_wallet_files_missing_dir_is_empty() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(list_wallet_files(&missing).unwrap().is_empty());
    }

    #[test]
    fn migrate_legacy_moves_single_file() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("wallet.json");
        let wallets = dir.path().join("wallets");
        File::create(&legacy).unwrap().write_all(b"{}").unwrap();
        migrate_legacy_wallet(&legacy, &wallets).unwrap();
        assert!(!legacy.exists());
        assert_eq!(list_wallet_files(&wallets).unwrap(), vec!["default"]);
    }

    #[test]
    fn migrate_legacy_skips_when_wallets_already_exist() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("wallet.json");
        let wallets = dir.path().join("wallets");
        fs::create_dir_all(&wallets).unwrap();
        File::create(wallets.join("alice.json")).unwrap().write_all(b"{}").unwrap();
        File::create(&legacy).unwrap().write_all(b"{}").unwrap();
        migrate_legacy_wallet(&legacy, &wallets).unwrap();
        // legacy file untouched, alice still alone.
        assert!(legacy.exists());
        assert_eq!(list_wallet_files(&wallets).unwrap(), vec!["alice"]);
    }

    #[test]
    fn migrate_legacy_noop_when_no_legacy() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("wallet.json");
        let wallets = dir.path().join("wallets");
        migrate_legacy_wallet(&legacy, &wallets).unwrap();
        // Idempotent; no errors. wallets_dir may or may not exist.
        assert!(!legacy.exists());
    }

    #[test]
    fn wallet_path_for_validates_and_joins() {
        let dir = Path::new("/tmp/walletdir");
        let p = wallet_path_for(dir, "alice").unwrap();
        assert_eq!(p, Path::new("/tmp/walletdir/alice.json"));
        assert!(wallet_path_for(dir, "../escape").is_err());
    }
}
