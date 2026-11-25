//! Changing what opens the account keys while holding them
//! (docs/CRYPTO_CONTRACT.md, "Phase 2a-2"): a new password, or a new recovery
//! code. The account keys themselves never change, so every store envelope
//! sealed to the account stays valid; only their wraps are made again.
//!
//! Each answer is the whole of what the accounts service is sent, made in one
//! step, so a caller has nothing to assemble from parts: the service replaces
//! the password's material (or the recovery code's) in one update, and the
//! account opens with exactly one password (or code) at every moment.

use super::*;

/// What a new password derives to, ready for `POST /api/v1/me/password`:
/// fresh Argon2id parameters, the `auth_key` the service will hash, and the
/// account keys wrapped under the new password's KEK. The KEK itself is not
/// kept: it was only needed for the wrap.
pub struct NewPasswordMaterial {
    pub kdf: KdfParams,
    /// base64url, as [`encode_auth_key`] gives it. A credential: never logged.
    pub auth_key: String,
    pub account_key_blob: AccountKeyBlob,
}

impl std::fmt::Debug for NewPasswordMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewPasswordMaterial").field("kdf", &self.kdf).field("auth_key", &"<redacted>").finish_non_exhaustive()
    }
}

/// Derive `new_password` under fresh parameters and wrap `keys` under it.
pub fn new_password_material(keys: &AccountKeys, new_password: &str) -> Result<NewPasswordMaterial> {
    let kdf = KdfParams::generate();
    let derived = derive_password_keys(new_password, &kdf)?;
    let account_key_blob = wrap_account_keys(keys, &derived.kek)?;
    Ok(NewPasswordMaterial { kdf, auth_key: encode_auth_key(&derived.auth_key), account_key_blob })
}

/// A new recovery code and the account keys wrapped under it, ready for
/// `POST /api/v1/me/recovery-code` (`recovery_salt`, `recovery_key_blob`).
/// The code is the person's to keep: shown once, never stored.
pub struct NewRecoveryMaterial {
    pub code: String,
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

impl std::fmt::Debug for NewRecoveryMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewRecoveryMaterial").field("code", &"<redacted>").field("recovery_salt", &self.recovery_salt).finish_non_exhaustive()
    }
}

impl Drop for NewRecoveryMaterial {
    fn drop(&mut self) {
        self.code.zeroize();
    }
}

/// Make a recovery code and wrap `keys` under the KEK it derives to, with a
/// fresh salt and the contract's fixed costs (the costs the accounts service
/// serves back with the recovery material).
pub fn new_recovery_material(keys: &AccountKeys) -> Result<NewRecoveryMaterial> {
    let code = generate_recovery_code();
    let params = KdfParams::generate();
    let kek = derive_recovery_kek(&code, &params)?;
    let recovery_key_blob = wrap_account_keys(keys, &kek)?;
    Ok(NewRecoveryMaterial { code, recovery_salt: params.salt, recovery_key_blob })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same_keys(a: &AccountKeys, b: &AccountKeys) -> bool {
        a.encryption_secret == b.encryption_secret && a.signing_secret == b.signing_secret
    }

    /// The new password opens the same keys; the old password's KEK does not
    /// open the new wrap; the auth key is the one the new password derives.
    #[test]
    fn a_new_password_wraps_the_same_keys() {
        let keys = AccountKeys::generate();
        let old_kdf = KdfParams::generate();
        let old = derive_password_keys("old password one", &old_kdf).unwrap();

        let material = new_password_material(&keys, "new password two").unwrap();
        assert_ne!(material.kdf.salt, old_kdf.salt);
        let again = derive_password_keys("new password two", &material.kdf).unwrap();
        assert_eq!(material.auth_key, encode_auth_key(&again.auth_key));
        assert!(same_keys(&unwrap_account_keys(&material.account_key_blob, &again.kek).unwrap(), &keys));
        assert!(unwrap_account_keys(&material.account_key_blob, &old.kek).is_err());
        assert!(!format!("{material:?}").contains(&material.auth_key));
    }

    /// The code, typed back in any case and with its dashes or without,
    /// opens the same keys under the salt sent with it.
    #[test]
    fn a_new_recovery_code_opens_the_same_keys() {
        let keys = AccountKeys::generate();
        let material = new_recovery_material(&keys).unwrap();
        let params = KdfParams { salt: material.recovery_salt.clone(), m_cost: KDF_M_COST_KIB, t_cost: KDF_T_COST, p_cost: KDF_P_COST };
        let typed = material.code.replace('-', "").to_lowercase();
        let kek = derive_recovery_kek(&typed, &params).unwrap();
        assert!(same_keys(&unwrap_account_keys(&material.recovery_key_blob, &kek).unwrap(), &keys));
        assert!(!format!("{material:?}").contains(&material.code));
        let other = new_recovery_material(&keys).unwrap();
        assert_ne!(other.code, material.code);
    }
}
