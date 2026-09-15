//! The signed-in account, in memory and nowhere else.
//!
//! The unwrapped account keys live in this module's thread-local for as long
//! as the page does. They are never written to `localStorage`, `sessionStorage`
//! or a cookie, never sent anywhere, and never logged — so a reload asks for
//! the password again (docs/CRYPTO_CONTRACT.md, "Web app"). The session cookie
//! survives a reload; the keys deliberately do not.
//!
//! Two ways in:
//!
//! * [`sign_in`] — no session yet. Derive from the password, prove it to the
//!   server with `auth_key`, then unwrap the account keys the server hands back.
//! * [`unlock`] — the session cookie is still good but the keys are gone (a
//!   reload). One request for the wrapped blob, then the password either opens
//!   it or does not. A wrong password fails **here**, with nothing sent.

use std::cell::RefCell;
use std::rc::Rc;

use pimble_crypto::{
    derive_password_keys, encode_auth_key, unwrap_account_keys, AccountKeys, AccountPublicKeys,
    KdfParams,
};

use crate::accounts;
use crate::http::ApiError;

thread_local! {
    static SESSION: RefCell<Option<Account>> = const { RefCell::new(None) };
}

/// Everything this page knows about the person using it.
pub struct Account {
    pub user_id: String,
    pub email: String,
    /// The unwrapped private keys. `Rc` because [`AccountKeys`] zeroizes on
    /// drop and must therefore exist exactly once.
    pub keys: Rc<AccountKeys>,
    pub public_keys: AccountPublicKeys,
}

/// Why signing in or unlocking did not work.
#[derive(Debug, Clone)]
pub enum SignInError {
    /// The password does not open this account. Decided locally by the unwrap,
    /// or by the server refusing `auth_key`.
    WrongPassword,
    /// The account exists but its email has not been verified yet.
    Unverified,
    /// Anything else: the network, a 500, a malformed answer.
    Failed(String),
}

impl std::fmt::Display for SignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignInError::WrongPassword => write!(f, "That email and password do not match."),
            SignInError::Unverified => {
                write!(f, "Check your inbox: this address has not been verified yet.")
            }
            SignInError::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl From<ApiError> for SignInError {
    fn from(e: ApiError) -> Self {
        match (e.status, e.code.as_str()) {
            (403, "email_unverified") => SignInError::Unverified,
            (401, _) => SignInError::WrongPassword,
            _ => SignInError::Failed(e.message),
        }
    }
}

/// Whether the account keys are in memory right now.
pub fn is_unlocked() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

pub fn email() -> Option<String> {
    SESSION.with(|s| s.borrow().as_ref().map(|a| a.email.clone()))
}

pub fn user_id() -> Option<String> {
    SESSION.with(|s| s.borrow().as_ref().map(|a| a.user_id.clone()))
}

/// The unwrapped private keys, for unwrapping a store key or signing an
/// envelope. `None` before sign-in and after sign-out.
pub fn keys() -> Option<Rc<AccountKeys>> {
    SESSION.with(|s| s.borrow().as_ref().map(|a| a.keys.clone()))
}

pub fn public_keys() -> Option<AccountPublicKeys> {
    SESSION.with(|s| s.borrow().as_ref().map(|a| a.public_keys.clone()))
}

/// Forget everything. Called on sign-out, before the cookie is cleared.
pub fn clear() {
    SESSION.with(|s| *s.borrow_mut() = None);
}

fn store(account: Account) {
    SESSION.with(|s| *s.borrow_mut() = Some(account));
}

/// Argon2id over the password, timed. The cost is the contract's (32 MiB, 3
/// passes) and a browser feels it, so the duration goes to the console once per
/// derivation — it is the number that decides whether the cost is tolerable
/// here.
pub fn derive_timed(password: &str, params: &KdfParams) -> Result<pimble_crypto::PasswordKeys, SignInError> {
    let started = now_ms();
    let derived = derive_password_keys(password, params)
        .map_err(|e| SignInError::Failed(format!("Deriving your keys failed: {e}")))?;
    tracing::info!(
        "Argon2id ({} MiB, {} passes) took {:.0} ms in this browser",
        params.m_cost / 1024,
        params.t_cost,
        now_ms() - started
    );
    Ok(derived)
}

/// Sign in from nothing: derive, prove, fetch, unwrap.
///
/// The password itself never leaves the page. `auth_key` is what the server
/// sees and what it hashes again at rest.
pub async fn sign_in(email: &str, password: &str) -> Result<(), SignInError> {
    let params = accounts::kdf(email).await?;
    let derived = derive_timed(password, &params)?;

    let login = accounts::login(email, &encode_auth_key(&derived.auth_key)).await?;

    let my_keys = accounts::my_keys().await?;
    let keys = unwrap_account_keys(&my_keys.account_key_blob, &derived.kek)
        .map_err(|_| SignInError::WrongPassword)?;

    store(Account {
        user_id: login.user.id,
        email: login.user.email,
        keys: Rc::new(keys),
        public_keys: my_keys.public_keys,
    });
    Ok(())
}

/// Unlock an account whose session cookie is still valid but whose keys were
/// lost to a reload.
///
/// One request (`GET /me/keys`), then the password either opens the blob or it
/// does not: a wrong password is decided here, in this browser, and nothing
/// further is sent.
pub async fn unlock(password: &str) -> Result<(), SignInError> {
    let who = accounts::me().await?;
    let my_keys = accounts::my_keys().await?;

    let derived = derive_timed(password, &my_keys.kdf)?;
    let keys = unwrap_account_keys(&my_keys.account_key_blob, &derived.kek)
        .map_err(|_| SignInError::WrongPassword)?;

    store(Account {
        user_id: who.id,
        email: who.email,
        keys: Rc::new(keys),
        public_keys: my_keys.public_keys,
    });
    Ok(())
}

/// The page's monotonic-enough clock, in milliseconds.
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or_else(js_sys::Date::now)
}
