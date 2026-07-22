//! Combined-secret derivation for Locking's multi-factor unlock.
//!
//! Per `workplan/LOCK-SPEC.md` §2: the password and the keypair-file bytes
//! are combined at the app layer into a single secret *before* anything
//! else (BitLocker included) ever sees it — this is what gives true AND
//! semantics, since BitLocker's own multi-protector model is OR-based
//! (ticket 041) and can't express "both factors required."
//!
//! Parameters are the ones measured for real on target hardware in
//! ticket 048 (1 GiB / t=3 / p=4, ~1.8s) — not published/assumed defaults.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::VaultError;

/// Argon2id parameters tuned in ticket 048. A once-per-launch desktop
/// unlock, not a per-request server auth path — deliberately heavier than
/// typical server-interactive guidance.
pub const ARGON2_MEM_COST_KIB: u32 = 1024 * 1024; // 1 GiB
pub const ARGON2_TIME_COST: u32 = 3;
pub const ARGON2_PARALLELISM: u32 = 4;

/// Random per-vault salt length. Not secret — persisted in the vault-root
/// status marker (ticket 044) alongside `encrypted`/`format_version`; only
/// needs to be unique per vault, not hidden.
pub const KDF_SALT_LEN: usize = 16;

pub const COMBINED_SECRET_LEN: usize = 32;

/// The derived secret. Zeroized on drop — this is the one value that
/// stands in for both unlock factors at once, so it's handled like other
/// key material, not like an ordinary byte buffer.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct CombinedSecret([u8; COMBINED_SECRET_LEN]);

impl CombinedSecret {
    /// Hex-encodes the secret for use as BitLocker's password protector
    /// string (ticket 040) — hex rather than base64 specifically so the
    /// value is alphanumeric-only and safe to pass as a PowerShell/
    /// `manage-bde` argument with no shell-escaping ambiguity.
    pub fn to_bitlocker_password(&self) -> String {
        let mut s = String::with_capacity(COMBINED_SECRET_LEN * 2);
        for byte in &self.0 {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }
}

/// Derives the combined secret from the user's password and the raw bytes
/// of the per-vault ed25519 keypair file (ticket 038/043).
///
/// `salt` is the per-vault KDF salt from the vault's status marker — not
/// secret, but must be the same salt used at vault-creation time, or every
/// subsequent unlock attempt will derive a different secret than the one
/// BitLocker was actually configured with.
pub fn derive_combined_secret(
    password: &str,
    keypair_bytes: &[u8],
    salt: &[u8; KDF_SALT_LEN],
) -> Result<CombinedSecret, VaultError> {
    let params = Params::new(
        ARGON2_MEM_COST_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(COMBINED_SECRET_LEN),
    )
    .map_err(|e| VaultError::KeyDerivation(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    // Password is treated as an opaque UTF-8 byte string end to end
    // (ticket 043) — no charset restriction, no normalization.
    let mut stretched = [0u8; COMBINED_SECRET_LEN];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut stretched)
        .map_err(|e| VaultError::KeyDerivation(e.to_string()))?;

    // HKDF-combine the stretched password with the keypair-file bytes so
    // both factors are structurally required to reconstruct the secret —
    // this is the AND-combination, done ourselves rather than through
    // age's OR-based recipient model (ticket 041).
    let hk = Hkdf::<Sha256>::new(Some(&stretched), keypair_bytes);
    let mut out = [0u8; COMBINED_SECRET_LEN];
    hk.expand(b"lenslocker-vault-combined-secret-v1", &mut out)
        .map_err(|e| VaultError::KeyDerivation(e.to_string()))?;

    stretched.zeroize();
    Ok(CombinedSecret(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real production Argon2id parameters (~1.8s per ticket 048's
    // benchmark) — kept as a single `#[ignore]`d test so `cargo test`
    // stays fast by default; run explicitly with `--ignored` to verify
    // against the real parameters.
    #[test]
    #[ignore = "slow: real Argon2id params take ~1-2s, run with --ignored"]
    fn same_inputs_produce_same_secret() {
        let salt = *b"0123456789abcdef";
        let a = derive_combined_secret("correct horse battery staple", b"keybytes", &salt).unwrap();
        let b = derive_combined_secret("correct horse battery staple", b"keybytes", &salt).unwrap();
        assert_eq!(a.to_bitlocker_password(), b.to_bitlocker_password());
    }

    #[test]
    #[ignore = "slow: real Argon2id params take ~1-2s, run with --ignored"]
    fn different_password_produces_different_secret() {
        let salt = *b"0123456789abcdef";
        let a = derive_combined_secret("correct horse battery staple", b"keybytes", &salt).unwrap();
        let b = derive_combined_secret("wrong horse battery staple", b"keybytes", &salt).unwrap();
        assert_ne!(a.to_bitlocker_password(), b.to_bitlocker_password());
    }

    #[test]
    #[ignore = "slow: real Argon2id params take ~1-2s, run with --ignored"]
    fn different_keypair_produces_different_secret() {
        let salt = *b"0123456789abcdef";
        let a = derive_combined_secret("correct horse battery staple", b"keybytes-one", &salt).unwrap();
        let b = derive_combined_secret("correct horse battery staple", b"keybytes-two", &salt).unwrap();
        assert_ne!(a.to_bitlocker_password(), b.to_bitlocker_password());
    }

    #[test]
    #[ignore = "slow: real Argon2id params take ~1-2s, run with --ignored"]
    fn unicode_password_with_spaces_and_emoji_works() {
        let salt = *b"0123456789abcdef";
        let secret = derive_combined_secret("correct 马 horse 🔒 battery staple", b"keybytes", &salt);
        assert!(secret.is_ok());
    }

    // Fast smoke test using deliberately tiny (insecure) parameters, just
    // to exercise the Argon2id -> HKDF wiring itself without waiting on
    // the real 1 GiB cost — the four tests above are the ones that
    // validate production parameters and behavior.
    #[test]
    fn wiring_smoke_test_with_tiny_params() {
        let params = Params::new(8, 1, 1, Some(COMBINED_SECRET_LEN)).unwrap();
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt = b"0123456789abcdef";
        let mut stretched = [0u8; COMBINED_SECRET_LEN];
        argon2
            .hash_password_into(b"pw", salt, &mut stretched)
            .unwrap();
        let hk = Hkdf::<Sha256>::new(Some(&stretched), b"keybytes");
        let mut out = [0u8; COMBINED_SECRET_LEN];
        hk.expand(b"lenslocker-vault-combined-secret-v1", &mut out)
            .unwrap();
        assert_ne!(out, [0u8; COMBINED_SECRET_LEN]);
    }
}
