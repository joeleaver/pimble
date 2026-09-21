//! HTTP-edge auth (docs/history/HARDENING_CONTRACT.md decisions 1-3, extended by
//! docs/CLOUD_CONTRACT.md "B: pimble-server" items 1-4).
//!
//! Checks run on every HTTP request the server receives — including the
//! WebSocket upgrade, which is a plain HTTP request until the `101`
//! response — via a tower layer wired onto jsonrpsee with
//! `Server::builder().set_http_middleware(...)` (pattern:
//! `jsonrpsee-server`'s own `middleware/http/host_filter.rs`):
//!
//! - A request's `Origin` header, if any, must be in the configured
//!   allowlist (`--allow-origin`/`PIMBLE_ALLOW_ORIGINS`); otherwise `403`.
//!   Browsers always send `Origin` on a WebSocket handshake (and on
//!   fetch/XHR); jsonrpsee's own ws client, the app, the CLI, and a sync
//!   link never do. The embedded app server configures no allowlist, so it
//!   keeps refusing every `Origin` as before.
//! - With a static token and/or a JWT verifier configured, a request must
//!   carry a credential as `Authorization: Bearer <token-or-jwt>`,
//!   `X-Api-Key: <token-or-jwt>`, or the query parameter `access_token`
//!   (added for a browser `WebSocket`, which cannot set headers); the
//!   static token is checked first, in constant time, then the JWT.
//!   Anything else is `401`.
//!
//! **The `Principal`.** Whichever credential resolved (or `Principal::Service`
//! when neither verifier is configured) is inserted into the request's
//! `http::Extensions` before it reaches jsonrpsee's dispatch; see
//! `crate::principal`'s module doc comment for how that reaches a handler
//! method.
//!
//! Also home to the server token file (decision 3): 32 random bytes,
//! base64url without padding, used by `pimble-cli server`/`token` and by
//! [`crate::server::PimbleServer`] callers that want one.

use std::collections::HashSet;
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

use crate::jwt::JwtVerifier;
use crate::principal::Principal;

// ── HTTP-edge tower layer ────────────────────────────────────────────────

/// Tower layer applying [`AuthMiddleware`]. `token: None` and `jwt: None`
/// together mean the credential check never rejects anything (the origin
/// check still runs, and every request becomes `Principal::Service`);
/// [`crate::server::PimbleServer::start`] is what refuses to even bind a
/// non-loopback address with neither configured.
#[derive(Clone)]
pub struct AuthLayer {
    token: Option<Arc<str>>,
    jwt: Option<Arc<JwtVerifier>>,
    allowed_origins: Arc<HashSet<String>>,
}

impl AuthLayer {
    pub fn new(token: Option<String>, jwt: Option<Arc<JwtVerifier>>, allowed_origins: Vec<String>) -> Self {
        Self { token: token.map(Arc::from), jwt, allowed_origins: Arc::new(allowed_origins.into_iter().collect()) }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthMiddleware {
            inner,
            token: self.token.clone(),
            jwt: self.jwt.clone(),
            allowed_origins: Arc::clone(&self.allowed_origins),
        }
    }
}

/// The service [`AuthLayer`] produces: checks a request against `token`/`jwt`
/// before letting it reach `inner` (the JSON-RPC service), attaching the
/// resolved [`Principal`] to the request's extensions.
#[derive(Clone)]
pub struct AuthMiddleware<S> {
    inner: S,
    token: Option<Arc<str>>,
    jwt: Option<Arc<JwtVerifier>>,
    allowed_origins: Arc<HashSet<String>>,
}

impl<S, B> Service<HttpRequest<B>> for AuthMiddleware<S>
where
    S: Service<HttpRequest<B>, Response = HttpResponse> + Clone + Send + 'static,
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

    fn call(&mut self, mut request: HttpRequest<B>) -> Self::Future {
        if !origin_allowed(request.headers(), &self.allowed_origins) {
            return Box::pin(async move { Ok(text_response(http::StatusCode::FORBIDDEN, "Origin header is not allowed\n")) });
        }

        let credential = extract_credential(&request);
        let token = self.token.clone();
        let jwt = self.jwt.clone();
        // Cloning `inner` rather than calling it here lets the credential
        // check (which may need to await a JWKS refresh) run first and
        // decide whether `inner` is reached at all; jsonrpsee's own bound on
        // `Server::start` already requires this concrete service to be
        // `Clone` (every connection is dispatched through a fresh clone), so
        // this adds no new requirement.
        let mut inner = self.inner.clone();

        Box::pin(async move {
            match resolve_principal(credential.as_deref(), token.as_deref(), jwt.as_deref()).await {
                Some(principal) => {
                    request.extensions_mut().insert(principal);
                    inner.call(request).await.map_err(Into::into)
                }
                None => Ok(text_response(http::StatusCode::UNAUTHORIZED, "Missing or invalid credentials\n")),
            }
        })
    }
}

/// Whether `headers`' `Origin`, if present, passes `allowed_origins`
/// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 3): no `Origin` header
/// always passes (native clients never send one); an `Origin` present must
/// be in the (possibly empty) allowlist verbatim.
fn origin_allowed(headers: &http::HeaderMap, allowed_origins: &HashSet<String>) -> bool {
    match headers.get(http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(origin) => allowed_origins.contains(origin),
    }
}

/// The credential carried by this request, if any: `Authorization: Bearer`,
/// then `X-Api-Key`, then the `access_token` query parameter (decision 2) —
/// checked in that order, the first present one wins (a real client sends
/// exactly one).
fn extract_credential<B>(request: &HttpRequest<B>) -> Option<String> {
    let headers = request.headers();

    if let Some(bearer) = headers.get(http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")) {
        return Some(bearer.to_string());
    }
    if let Some(api_key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return Some(api_key.to_string());
    }
    if let Some(query) = request.uri().query() {
        if let Some((_, value)) = url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == "access_token") {
            return Some(value.into_owned());
        }
    }
    None
}

/// The [`Principal`] for `credential`, given the configured static `token`
/// and/or `jwt` verifier (docs/CLOUD_CONTRACT.md "B: pimble-server" item 2:
/// "Static token checked first in constant time, then JWT"). `None` means
/// the request is refused with `401`.
///
/// With neither `token` nor `jwt` configured, every request is
/// `Principal::Service` regardless of `credential` — matching the app's
/// embedded, tokenless, loopback-only server, where anything that reaches
/// the port at all is already trusted (`PimbleServer::start` is what keeps
/// this server loopback-only in that case).
async fn resolve_principal(credential: Option<&str>, token: Option<&str>, jwt: Option<&JwtVerifier>) -> Option<Principal> {
    if token.is_none() && jwt.is_none() {
        return Some(Principal::Service);
    }

    let credential = credential?;

    if let Some(token) = token {
        // `ConstantTimeEq` itself only short-circuits on a length mismatch,
        // never on content, so a wrong guess can't be narrowed down by
        // response timing.
        if bool::from(credential.as_bytes().ct_eq(token.as_bytes())) {
            return Some(Principal::Service);
        }
    }

    if let Some(jwt) = jwt {
        if let Ok(principal) = jwt.verify(credential).await {
            return Some(principal);
        }
    }

    None
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

pub(crate) fn generate_token() -> String {
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

    fn request_with(headers: &[(&str, &str)], uri: &str) -> HttpRequest<()> {
        let mut builder = http::Request::builder().uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(()).unwrap()
    }

    fn origins(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_origin_not_on_the_allowlist_is_refused() {
        let h = headers(&[("origin", "http://evil.example")]);
        assert!(!origin_allowed(&h, &origins(&[])), "an empty allowlist must refuse every Origin");
        assert!(!origin_allowed(&h, &origins(&["http://good.example"])));
    }

    #[test]
    fn an_allowlisted_origin_passes() {
        let h = headers(&[("origin", "http://good.example")]);
        assert!(origin_allowed(&h, &origins(&["http://good.example"])));
    }

    #[test]
    fn no_origin_header_always_passes() {
        let h = headers(&[]);
        assert!(origin_allowed(&h, &origins(&[])));
        assert!(origin_allowed(&h, &origins(&["http://good.example"])));
    }

    #[test]
    fn credential_is_read_from_bearer_then_api_key_then_query() {
        assert_eq!(extract_credential(&request_with(&[("authorization", "Bearer tok-1")], "/")), Some("tok-1".to_string()));
        assert_eq!(extract_credential(&request_with(&[("x-api-key", "tok-2")], "/")), Some("tok-2".to_string()));
        assert_eq!(extract_credential(&request_with(&[], "/rpc?access_token=tok-3")), Some("tok-3".to_string()));
        assert_eq!(extract_credential(&request_with(&[], "/rpc?other=1&access_token=tok-4&more=2")), Some("tok-4".to_string()));
        assert_eq!(extract_credential(&request_with(&[], "/")), None);
    }

    #[tokio::test]
    async fn no_verifier_configured_is_always_service() {
        assert!(matches!(resolve_principal(None, None, None).await, Some(Principal::Service)));
        assert!(matches!(resolve_principal(Some("anything"), None, None).await, Some(Principal::Service)));
    }

    #[tokio::test]
    async fn missing_credential_is_refused_with_a_token_configured() {
        assert!(resolve_principal(None, Some("secret"), None).await.is_none());
    }

    #[tokio::test]
    async fn wrong_bearer_is_refused() {
        assert!(resolve_principal(Some("wrong"), Some("secret"), None).await.is_none());
    }

    #[tokio::test]
    async fn correct_static_token_resolves_to_service() {
        assert!(matches!(resolve_principal(Some("secret"), Some("secret"), None).await, Some(Principal::Service)));
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
