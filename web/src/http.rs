//! The one way this page talks to the accounts service.
//!
//! Every call is same-origin with the session cookie attached, and every
//! failure comes back as an [`ApiError`] carrying the service's own
//! `{ error, message }` shape (`crates/pimble-cloud/src/error.rs`), so a page
//! can branch on a code like `email_unverified` instead of matching strings.
//!
//! Nothing here knows about keys. Key material is derived and unwrapped in
//! `crate::session`, and only ever the *wrapped* form travels through these
//! calls.

use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestCredentials, RequestInit, Response};

/// A failed call: the HTTP status, the service's error code (empty when the
/// body was not the service's error shape) and a message fit to show someone.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// A failure that never reached the service (no window, a dead network, a
    /// body that would not parse).
    pub fn local(message: impl Into<String>) -> Self {
        Self { status: 0, code: String::new(), message: message.into() }
    }

    /// Whether this is "no session, or one the server no longer accepts".
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// `GET path`, decoded as `R`.
pub async fn get<R: DeserializeOwned>(path: &str) -> ApiResult<R> {
    call::<(), R>("GET", path, None).await
}

/// `POST path` with a JSON body, decoded as `R`.
pub async fn post<B: Serialize, R: DeserializeOwned>(path: &str, body: &B) -> ApiResult<R> {
    call("POST", path, Some(body)).await
}

/// `POST path` with no body at all (the token and logout endpoints).
pub async fn post_empty<R: DeserializeOwned>(path: &str) -> ApiResult<R> {
    call::<(), R>("POST", path, None).await
}

/// `PUT path` with a JSON body, decoded as `R`.
pub async fn put<B: Serialize, R: DeserializeOwned>(path: &str, body: &B) -> ApiResult<R> {
    call("PUT", path, Some(body)).await
}

/// `DELETE path`, decoded as `R`.
pub async fn delete<R: DeserializeOwned>(path: &str) -> ApiResult<R> {
    call::<(), R>("DELETE", path, None).await
}

/// One request, start to finish.
///
/// `credentials: include` is what makes the browser attach the `HttpOnly`
/// session cookie; script cannot read it, which is the point.
async fn call<B: Serialize, R: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<&B>,
) -> ApiResult<R> {
    let window = web_sys::window().ok_or_else(|| ApiError::local("no window"))?;

    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(RequestCredentials::Include);

    if let Some(body) = body {
        let json = serde_json::to_string(body)
            .map_err(|e| ApiError::local(format!("could not encode the request: {e}")))?;
        init.set_body(&JsValue::from_str(&json));
        let headers = Headers::new().map_err(|e| ApiError::local(describe(&e)))?;
        headers
            .set("content-type", "application/json")
            .map_err(|e| ApiError::local(describe(&e)))?;
        init.set_headers(&headers);
    }

    let request = Request::new_with_str_and_init(path, &init)
        .map_err(|e| ApiError::local(describe(&e)))?;

    let response: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| ApiError::local(describe(&e)))?
        .dyn_into()
        .map_err(|_| ApiError::local("fetch did not answer with a Response"))?;

    let status = response.status();
    let text = response_text(&response).await?;

    if !response.ok() {
        return Err(parse_error(status, &text, path));
    }

    // A 204 or an endpoint that answers with nothing still has to produce an
    // `R`; `null` is what serde turns into `()` or an `Option`.
    let text = if text.trim().is_empty() { "null".to_string() } else { text };
    serde_json::from_str(&text).map_err(|e| {
        ApiError::local(format!("{path} answered with something unexpected: {e}"))
    })
}

async fn response_text(response: &Response) -> ApiResult<String> {
    let promise = response.text().map_err(|e| ApiError::local(describe(&e)))?;
    JsFuture::from(promise)
        .await
        .map_err(|e| ApiError::local(describe(&e)))?
        .as_string()
        .ok_or_else(|| ApiError::local("the response body was not text"))
}

/// The service's `{ "error": code, "message": "..." }`, or a plain description
/// of the status when the body is something else (a proxy's 502 page, say).
fn parse_error(status: u16, body: &str, path: &str) -> ApiError {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(default)]
        error: String,
        #[serde(default)]
        message: String,
    }
    match serde_json::from_str::<Body>(body) {
        Ok(b) if !b.message.is_empty() || !b.error.is_empty() => ApiError {
            status,
            code: b.error,
            message: if b.message.is_empty() { format!("{path} failed ({status})") } else { b.message },
        },
        _ => ApiError { status, code: String::new(), message: format!("{path} answered {status}") },
    }
}

/// A `JsValue` error as something worth putting in a message.
pub fn describe(err: &JsValue) -> String {
    err.as_string()
        .or_else(|| err.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())))
        .unwrap_or_else(|| format!("{err:?}"))
}
