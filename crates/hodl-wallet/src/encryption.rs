//! Optional wallet-mnemonic encryption.
//!
//! Wraps the BIP39 mnemonic of a `WalletFile` in an Argon2id-derived
//! key and XChaCha20-Poly1305 AEAD. Encryption is per-wallet and
//! opt-in at creation time (see plan H3). Wallet files without an
//! `encrypted_mnemonic` block are v1 plaintext and still load
//! unchanged; encrypted files carry `version: 2` plus the blob
//! defined here.
//!
//! ## Key model
//!
//! - **Salt** (16 bytes): random at wallet creation, **stable** across
//!   saves. It scopes the KDF to this specific wallet — there's no
//!   benefit to rotating it on every save (the threat model is
//!   offline brute-force; a stable salt + good KDF params already
//!   defeat that).
//! - **Nonce** (24 bytes, XChaCha20): rotated **every save**. AEAD
//!   nonce reuse with the same key is catastrophic, so this matters.
//! - **Key** (32 bytes): derived once from `(passphrase, salt)` via
//!   Argon2id. We cache it in [`UnlockContext`] so subsequent saves
//!   can re-encrypt without re-prompting; the cache is held in
//!   process memory (desktop `AppState`) and cleared on wallet
//!   deselect.
//!
//! Passphrase rotation isn't supported in this iteration — it would
//! require asking for both old and new passphrases and re-running the
//! KDF, but the wire format above (with `kdf_params` + `salt` stored
//! per-wallet) already accommodates it.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    AeadCore, XChaCha20Poly1305, XNonce,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;

/// Argon2id parameters used for KDF, stored alongside the ciphertext
/// so future changes don't break existing wallet files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Time cost (iterations).
    pub t_cost: u32,
    /// Parallelism degree.
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        // argon2 crate's Params::DEFAULT — m=19 MiB, t=2, p=1. Matches
        // OWASP's interactive-use minimum recommendation. Fast enough
        // to feel instant on a laptop (~50ms) while still pricey for
        // an offline attacker.
        let p = argon2::Params::DEFAULT;
        Self {
            m_cost: p.m_cost(),
            t_cost: p.t_cost(),
            p_cost: p.p_cost(),
        }
    }
}

/// On-disk encrypted-mnemonic blob. Lives inside the v2 wallet file
/// under `encrypted_mnemonic`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedMnemonic {
    /// Algorithm tag. Only `"argon2id"` is recognised today.
    pub kdf: String,
    pub kdf_params: KdfParams,
    /// 16 bytes, base64 (no padding).
    pub salt_b64: String,
    /// 24 bytes, base64 (no padding). Rotated every save.
    pub nonce_b64: String,
    /// AEAD ciphertext (mnemonic bytes + 16-byte tag), base64 (no padding).
    pub ciphertext_b64: String,
}

/// In-memory unlock context. Holds the KDF-derived key plus the salt
/// and params it was derived from, so a subsequent save can produce a
/// fresh ciphertext without re-prompting for the passphrase. The
/// `Drop` impl zeroes the key bytes.
#[derive(Clone)]
pub struct UnlockContext {
    key: [u8; KEY_LEN],
    salt: [u8; SALT_LEN],
    kdf_params: KdfParams,
}

impl UnlockContext {
    /// Decrypt a blob using this context's cached key. Used by the
    /// desktop side to load an encrypted wallet without re-prompting
    /// for the passphrase. Errors if the blob's salt + KDF params
    /// don't match the cached ones — a mismatch would mean the
    /// wallet was re-encrypted out of band (e.g. via the CLI with a
    /// different passphrase), which we deliberately refuse rather
    /// than silently overwrite later on save.
    pub fn decrypt(&self, blob: &EncryptedMnemonic) -> Result<String> {
        if blob.kdf != "argon2id" {
            bail!("unsupported KDF: {}", blob.kdf);
        }
        let salt_bytes = STANDARD_NO_PAD
            .decode(&blob.salt_b64)
            .context("decode salt_b64")?;
        if salt_bytes != self.salt {
            bail!(
                "wallet salt has changed since this unlock — re-enter the passphrase"
            );
        }
        if blob.kdf_params.m_cost != self.kdf_params.m_cost
            || blob.kdf_params.t_cost != self.kdf_params.t_cost
            || blob.kdf_params.p_cost != self.kdf_params.p_cost
        {
            bail!(
                "wallet KDF params have changed since this unlock — re-enter the passphrase"
            );
        }
        let nonce_bytes = STANDARD_NO_PAD
            .decode(&blob.nonce_b64)
            .context("decode nonce_b64")?;
        let ct = STANDARD_NO_PAD
            .decode(&blob.ciphertext_b64)
            .context("decode ciphertext_b64")?;
        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let nonce = XNonce::from_slice(&nonce_bytes);
        let pt = cipher
            .decrypt(nonce, ct.as_slice())
            .map_err(|_| anyhow!("AEAD decrypt failed with cached key"))?;
        String::from_utf8(pt).context("decrypted bytes are not valid UTF-8")
    }
}

impl Drop for UnlockContext {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl std::fmt::Debug for UnlockContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockContext")
            .field("key", &"<redacted>")
            .field("salt", &self.salt)
            .field("kdf_params", &self.kdf_params)
            .finish()
    }
}

fn derive_key(
    passphrase: &str,
    salt: &[u8; SALT_LEN],
    params: &KdfParams,
) -> Result<[u8; KEY_LEN]> {
    let argon_params = argon2::Params::new(
        params.m_cost,
        params.t_cost,
        params.p_cost,
        Some(KEY_LEN),
    )
    .map_err(|e| anyhow!("invalid Argon2 parameters: {e}"))?;
    let argon = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon_params,
    );
    let mut key = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("Argon2id derivation failed: {e}"))?;
    Ok(key)
}

fn encrypt_with_key(plaintext: &str, key: &[u8; KEY_LEN]) -> Result<(String, String)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("AEAD encrypt failed: {e}"))?;
    Ok((
        STANDARD_NO_PAD.encode(nonce),
        STANDARD_NO_PAD.encode(ct),
    ))
}

/// First-time encryption: generates a fresh salt, derives the KEK,
/// produces both the on-disk blob and the cacheable unlock context.
pub fn encrypt_new(mnemonic: &str, passphrase: &str) -> Result<(EncryptedMnemonic, UnlockContext)> {
    if passphrase.is_empty() {
        bail!("passphrase must not be empty");
    }
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let params = KdfParams::default();
    let key = derive_key(passphrase, &salt, &params)?;
    let (nonce_b64, ciphertext_b64) = encrypt_with_key(mnemonic, &key)?;
    let blob = EncryptedMnemonic {
        kdf: "argon2id".to_string(),
        kdf_params: params.clone(),
        salt_b64: STANDARD_NO_PAD.encode(salt),
        nonce_b64,
        ciphertext_b64,
    };
    let ctx = UnlockContext {
        key,
        salt,
        kdf_params: params,
    };
    Ok((blob, ctx))
}

/// Re-encrypt with the cached KEK on save. Rotates the nonce; reuses
/// the stored salt and KDF params.
pub fn reencrypt(mnemonic: &str, ctx: &UnlockContext) -> Result<EncryptedMnemonic> {
    let (nonce_b64, ciphertext_b64) = encrypt_with_key(mnemonic, &ctx.key)?;
    Ok(EncryptedMnemonic {
        kdf: "argon2id".to_string(),
        kdf_params: ctx.kdf_params.clone(),
        salt_b64: STANDARD_NO_PAD.encode(ctx.salt),
        nonce_b64,
        ciphertext_b64,
    })
}

/// Decrypt with a passphrase. Returns the plaintext mnemonic alongside
/// an unlock context for caching.
pub fn decrypt(
    blob: &EncryptedMnemonic,
    passphrase: &str,
) -> Result<(String, UnlockContext)> {
    if blob.kdf != "argon2id" {
        bail!("unsupported KDF: {}", blob.kdf);
    }
    let salt_bytes = STANDARD_NO_PAD
        .decode(&blob.salt_b64)
        .context("decode salt_b64")?;
    let salt: [u8; SALT_LEN] = salt_bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("salt has {} bytes, expected {SALT_LEN}", v.len()))?;
    let nonce_bytes = STANDARD_NO_PAD
        .decode(&blob.nonce_b64)
        .context("decode nonce_b64")?;
    let ct = STANDARD_NO_PAD
        .decode(&blob.ciphertext_b64)
        .context("decode ciphertext_b64")?;
    let key = derive_key(passphrase, &salt, &blob.kdf_params)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let nonce = XNonce::from_slice(&nonce_bytes);
    let pt = cipher.decrypt(nonce, ct.as_slice()).map_err(|_| {
        anyhow!("decryption failed — wrong passphrase or corrupted ciphertext")
    })?;
    let mnemonic =
        String::from_utf8(pt).context("decrypted bytes are not valid UTF-8")?;
    let ctx = UnlockContext {
        key,
        salt,
        kdf_params: blob.kdf_params.clone(),
    };
    Ok((mnemonic, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn roundtrip_with_correct_passphrase() {
        let (blob, _) = encrypt_new(PHRASE, "correct horse battery staple").unwrap();
        let (back, _) = decrypt(&blob, "correct horse battery staple").unwrap();
        assert_eq!(back, PHRASE);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let (blob, _) = encrypt_new(PHRASE, "right").unwrap();
        let err = decrypt(&blob, "wrong").unwrap_err();
        assert!(format!("{err}").contains("decryption failed"));
    }

    #[test]
    fn empty_passphrase_rejected_at_encrypt() {
        assert!(encrypt_new(PHRASE, "").is_err());
    }

    #[test]
    fn reencrypt_rotates_nonce_keeps_salt() {
        let (blob1, ctx) = encrypt_new(PHRASE, "p").unwrap();
        let blob2 = reencrypt(PHRASE, &ctx).unwrap();
        assert_eq!(blob1.salt_b64, blob2.salt_b64);
        assert_ne!(blob1.nonce_b64, blob2.nonce_b64);
        assert_ne!(blob1.ciphertext_b64, blob2.ciphertext_b64);
        let (back, _) = decrypt(&blob2, "p").unwrap();
        assert_eq!(back, PHRASE);
    }

    #[test]
    fn unknown_kdf_rejected() {
        let (mut blob, _) = encrypt_new(PHRASE, "p").unwrap();
        blob.kdf = "scrypt".to_string();
        assert!(decrypt(&blob, "p").is_err());
    }

    #[test]
    fn unlock_context_debug_redacts_key() {
        let (_, ctx) = encrypt_new(PHRASE, "p").unwrap();
        let s = format!("{ctx:?}");
        assert!(s.contains("<redacted>"));
        assert!(!s.contains(&format!("{:?}", ctx.key)));
    }

    #[test]
    fn unlock_context_decrypts_freshly_rotated_blob() {
        // The cached context survives a nonce rotation: we encrypt
        // once, get the ctx, reencrypt to produce a new blob with
        // the same salt/key but a fresh nonce, then decrypt the new
        // blob via the cached ctx. This is exactly the desktop path:
        // load_unlocked once → cache → save rotates nonce → load
        // again uses the cache.
        let (_, ctx) = encrypt_new(PHRASE, "p").unwrap();
        let blob2 = reencrypt(PHRASE, &ctx).unwrap();
        let back = ctx.decrypt(&blob2).unwrap();
        assert_eq!(back, PHRASE);
    }

    #[test]
    fn unlock_context_rejects_blob_with_different_salt() {
        let (_, ctx1) = encrypt_new(PHRASE, "p").unwrap();
        let (blob2, _) = encrypt_new(PHRASE, "p").unwrap(); // different salt
        assert!(ctx1.decrypt(&blob2).is_err());
    }
}
