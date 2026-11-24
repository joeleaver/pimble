//! Deriving a password's keys in this page, timed: what signing up and
//! recovering an account do before they wrap the account keys
//! (docs/CRYPTO_CONTRACT.md, "Web app"). The derivation itself is
//! `pimble_crypto`'s; this only times it.

use pimble_crypto::{derive_password_keys, KdfParams, PasswordKeys};

/// Argon2id over the password, timed. The cost is the contract's (32 MiB, 3
/// passes) and a browser feels it, so the duration goes to the console once per
/// derivation: it is the number that decides whether the cost is tolerable
/// here. The error is a sentence.
pub fn derive_timed(password: &str, params: &KdfParams) -> Result<PasswordKeys, String> {
    let started = now_ms();
    let derived = derive_password_keys(password, params).map_err(|e| format!("Deriving your keys failed: {e}"))?;
    tracing::info!("Argon2id ({} MiB, {} passes) took {:.0} ms in this browser", params.m_cost / 1024, params.t_cost, now_ms() - started);
    Ok(derived)
}

/// The page's monotonic-enough clock, in milliseconds.
fn now_ms() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map(|p| p.now()).unwrap_or_else(js_sys::Date::now)
}
