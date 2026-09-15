//! HTTP-edge auth (docs/history/HARDENING_CONTRACT.md decisions 1-3).
//!
//! Two independent checks run on every HTTP request the server receives —
//! including the WebSocket upgrade, which is a plain HTTP request until the
//! `101` response — via a tower layer wired onto jsonrpsee with
//! `Server::builder().set_http_middleware(...)` (pattern:
//! `jsonrpsee-server`'s own `middleware/http/host_filter.rs`):
//!
//! - Any request carrying an `Origin` header is refused with `403`.
//!   Browsers always send `Origin` on a WebSocket handshake (and on
//!   fetch/XHR); jsonrpsee's own ws client, the app, the CLI, and a sync
//!   link never do. This holds whether or not a token is configured, so a
//!   page in a browser can never reach this server even with a correct
//!   token pasted into it.
//! - When the server has a token, a request must carry it as
//!   `Authorization: Bearer <token>` or `X-Api-Key: <token>`, compared in
//!   constant time; anything else is `401`.
//!
//! Also home to the server token file (decision 3): 32 random bytes,
//! base64url without padding, used by `pimble-cli server`/`token` and by
//! [`crate::server::PimbleServer`] callers that want one.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use base64::Engine;
use bytes::Bytes;
use jsonrpsee::core::BoxError;
use jsonrpsee::server::{HttpBody, HttpRequest, HttpResponse};
use subtle::ConstantTimeEq;
use tower::{Layer, Service};

// ── HTTP-edge tower layer ────────────────────────────────────────────────

/// Tower layer applying [`AuthMiddleware`]. `token: None` means the second
/// check never rejects anything (the `Origin` check still runs);
/// [`crate::server::PimbleServer::start`] is what refuses to even bind a
/// non-loopback address without a token in the first place.
#[derive(Debug, Clone)]
pub struct AuthLayer {
    token: Option<Arc<str>>,
}

impl AuthLayer {
    pub fn new(token: Option<String>) -> Self {
        Self { token: token.map(Arc::from) }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthMiddleware { inner, token: self.token.clone() }
    }
}

/// The service [`AuthLayer`] produces: checks a request against `token`
/// before letting it reach `inner` (the JSON-RPC service).
#[derive(Debug, Clone)]
pub struct AuthMiddleware<S> {
    inner: S,
    token: Option<Arc<str>>,
}

impl<S, B> Service<HttpRequest<B>> for AuthMiddleware<S>
where
    S: Service<HttpRequest<B>, Response = HttpResponse>,
    S::Response: 'static,
    S::Error: Into<BoxError> + 'static,
    S::Future: Send + 'static,
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: HttpRequest<B>) -> Self::Future {
        if let Some(rejection) = reject(request.headers(), self.token.as_deref()) {
            return Box::pin(async move { Ok(rejection) });
        }

        let fut = self.inner.call(request);
        Box::pin(async move { fut.await.map_err(Into::into) })
    }
}

/// The rejection response for `headers`, if any: an `Origin` header present
/// is always `403`; otherwise, with a `token` configured, a missing or
/// wrong credential is `401`. `None` means the request passes through.
fn reject(headers: &http::HeaderMap, token: Option<&str>) -> Option<HttpResponse> {
    if headers.contains_key(http::header::ORIGIN) {
        return Some(text_response(http::StatusCode::FORBIDDEN, "Origin header is not allowed\n"));
    }

    let token = token?;
    if has_valid_credential(headers, token) {
        None
    } else {
        Some(text_response(http::StatusCode::UNAUTHORIZED, "Missing or invalid credentials\n"))
    }
}

/// Whether `headers` carries `token` as either `Authorization: Bearer
/// <token>` or `X-Api-Key: <token>`. Both candidates are compared in
/// constant time (`subtle::ConstantTimeEq`, which itself short-circuits
/// only on a length mismatch, never on content) so a wrong guess can't be
/// narrowed down by response timing.
fn has_valid_credential(headers: &http::HeaderMap, token: &str) -> bool {
    let bearer = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(bearer) = bearer {
        if bool::from(bearer.as_bytes().ct_eq(token.as_bytes())) {
            return true;
        }
    }

    if let Some(api_key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        if bool::from(api_key.as_bytes().ct_eq(token.as_bytes())) {
            return true;
        }
    }

    false
}

fn text_response(status: http::StatusCode, body: &'static str) -> HttpResponse {
    HttpResponse::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .body(HttpBody::from(body))
        .expect("static status/header/body always build")
}

// ── Server token file (decision 3) ───────────────────────────────────────

/// `<dirs::config_dir()>/pimble/server-token`, used by `pimble-cli server`
/// when `--token-file` isn't given and `--addr` isn't loopback, and by
/// `pimble-cli token`.
pub fn default_token_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("server-token")
}

/// Read `path`'s token, creating it (32 random bytes, base64url without
/// padding, one line, mode `0600`) if the file doesn't exist yet. An
/// existing but empty or whitespace-only file is an error rather than a
/// usable empty token: `AuthMiddleware` would otherwise admit `Authorization:
/// Bearer ` (nothing after it) and an empty `X-Api-Key`.
pub fn load_or_create_token(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.trim().to_string();
            if token.is_empty() {
                Err(io::Error::new(io::ErrorKind::InvalidData, format!("token file {} is empty", path.display())))
            } else {
                Ok(token)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => regenerate_token(path),
        Err(e) => Err(e),
    }
}

/// Replace `path`'s token with a freshly generated one and return it. A
/// server never logs the token; callers that print it (`pimble-cli token`)
/// do so deliberately.
pub fn regenerate_token(path: &Path) -> io::Result<String> {
    let token = generate_token();
    crate::fs_util::write_atomic_0600(path, format!("{token}\n").as_bytes())?;
    Ok(token)
}

fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ── Unit tests (header rules only, no network) ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        for (k, v) in pairs {
            map.insert(http::HeaderName::from_bytes(k.as_bytes()).unwrap(), http::HeaderValue::from_str(v).unwrap());
        }
        map
    }

    #[test]
    fn origin_is_refused_even_without_a_token() {
        let h = headers(&[("origin", "http://evil.example")]);
        let rejection = reject(&h, None).expect("Origin must always be refused");
        assert_eq!(rejection.status(), http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn origin_is_refused_ahead_of_a_correct_token() {
        let h = headers(&[("origin", "http://evil.example"), ("authorization", "Bearer secret")]);
        let rejection = reject(&h, Some("secret")).expect("Origin wins over a correct token");
        assert_eq!(rejection.status(), http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn no_token_configured_admits_any_non_browser_request() {
        let h = headers(&[]);
        assert!(reject(&h, None).is_none());
    }

    #[test]
    fn missing_credential_is_refused_with_a_token_configured() {
        let h = headers(&[]);
        let rejection = reject(&h, Some("secret")).expect("no credential must be refused");
        assert_eq!(rejection.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn wrong_bearer_is_refused() {
        let h = headers(&[("authorization", "Bearer wrong")]);
        let rejection = reject(&h, Some("secret")).expect("wrong bearer must be refused");
        assert_eq!(rejection.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn correct_bearer_is_admitted() {
        let h = headers(&[("authorization", "Bearer secret")]);
        assert!(reject(&h, Some("secret")).is_none());
    }

    #[test]
    fn correct_api_key_is_admitted() {
        let h = headers(&[("x-api-key", "secret")]);
        assert!(reject(&h, Some("secret")).is_none());
    }

    #[test]
    fn wrong_api_key_is_refused() {
        let h = headers(&[("x-api-key", "wrong")]);
        let rejection = reject(&h, Some("secret")).expect("wrong api key must be refused");
        assert_eq!(rejection.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn load_or_create_then_reload_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server-token");

        let created = load_or_create_token(&path).unwrap();
        assert!(!created.is_empty());
        let reloaded = load_or_create_token(&path).unwrap();
        assert_eq!(created, reloaded, "a second load must not regenerate the token");

        let regenerated = regenerate_token(&path).unwrap();
        assert_ne!(created, regenerated, "--new must produce a different token");
        assert_eq!(regenerated, load_or_create_token(&path).unwrap());
    }

    #[test]
    fn an_existing_empty_or_whitespace_only_token_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();

        let empty_path = dir.path().join("empty-token");
        std::fs::write(&empty_path, "").unwrap();
        let err = load_or_create_token(&empty_path).expect_err("an empty token file must be an error");
        assert!(err.to_string().contains("is empty"), "got: {}", err);

        let whitespace_path = dir.path().join("whitespace-token");
        std::fs::write(&whitespace_path, "  \n").unwrap();
        load_or_create_token(&whitespace_path).expect_err("a whitespace-only token file must be an error too");
    }
}
