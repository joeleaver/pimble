//! The accounts service, from the browser.
//!
//! One call matters here: `POST /api/v1/token` exchanges the `pimble_session`
//! cookie for a short-lived JWT and tells us where the Pimble server is. The
//! app never sees a password and never stores a token anywhere the page
//! outlives; when the session is not good, the answer is the login page.

use serde::Deserialize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCredentials, RequestInit, Response};

/// Where the account API lives, relative to this page's origin. The site, the
/// app, the API and the Pimble server are one origin in production, which is
/// what makes the session cookie work with no CORS anywhere.
const TOKEN_PATH: &str = "/api/v1/token";

/// Where an unauthenticated visitor is sent.
const LOGIN_PATH: &str = "/login.html";

/// A minted credential and the server it opens.
#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    /// The JWT, `aud` of `pimble`, carrying this user's grants.
    pub token: String,
    /// Unix seconds at which `token` stops being accepted.
    pub exp: i64,
    /// The WebSocket URL of the Pimble server that honours it.
    pub rpc_url: String,
}

#[derive(Debug)]
pub enum TokenError {
    /// No session, or one the server no longer accepts. The only answer is to
    /// sign in again.
    Unauthorized,
    /// Anything else: the network, a 500, a body that is not a session.
    Failed(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Unauthorized => write!(f, "not signed in"),
            TokenError::Failed(msg) => write!(f, "{}", msg),
        }
    }
}

/// Mint a fresh token for the session cookie this page carries.
///
/// The cookie is `HttpOnly`, so script cannot read it; `credentials: include`
/// is what makes the browser attach it. A 401 means the session is gone.
pub async fn fetch_token() -> Result<Session, TokenError> {
    let window = web_sys::window().ok_or_else(|| TokenError::Failed("no window".into()))?;

    let init = RequestInit::new();
    init.set_method("POST");
    init.set_credentials(RequestCredentials::Include);

    let request = Request::new_with_str_and_init(TOKEN_PATH, &init)
        .map_err(|e| TokenError::Failed(describe(&e)))?;

    let response: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| TokenError::Failed(describe(&e)))?
        .dyn_into()
        .map_err(|_| TokenError::Failed("fetch did not answer with a Response".into()))?;

    if response.status() == 401 {
        return Err(TokenError::Unauthorized);
    }
    if !response.ok() {
        return Err(TokenError::Failed(format!(
            "{} answered {}",
            TOKEN_PATH,
            response.status()
        )));
    }

    let text = JsFuture::from(
        response
            .text()
            .map_err(|e| TokenError::Failed(describe(&e)))?,
    )
    .await
    .map_err(|e| TokenError::Failed(describe(&e)))?
    .as_string()
    .ok_or_else(|| TokenError::Failed("token response was not text".into()))?;

    serde_json::from_str::<Session>(&text)
        .map_err(|e| TokenError::Failed(format!("token response did not parse: {}", e)))
}

/// Leave for the login page. Does not return in practice.
pub fn go_to_login() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href(LOGIN_PATH);
    }
}

/// A `JsValue` error as something worth putting in a log line.
fn describe(err: &JsValue) -> String {
    err.as_string()
        .or_else(|| {
            err.dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{:?}", err))
}
