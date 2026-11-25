//! Outbound HTTP to the Pimble Cloud accounts service
//! (docs/ONE_CODEBASE_CONTRACT.md seam D): sign-in, tokens, stores, keys,
//! members, the personal document, diagnostics. JSON in (diagnostic records
//! NDJSON), JSON out, a session bearer.
//!
//! - The desktop: [`ReqwestHttp`] (`reqwest` with rustls, a 15 s timeout), as
//!   before this seam.
//! - The browser's worker: [`FetchHttp`] (the global `fetch`), with the
//!   session cookie the page already has, so the bearer may be left out.
//! - A phone app: its platform's client, or `reqwest` as on the desktop.
//!
//! A caller that must not wait longer than it says wraps any of them in
//! [`Bounded`], which ends a call on the server's runtime clock alike on
//! every host (diagnostics' sends, which an off waits for).
//!
//! Links to Pimble servers do not come through here: they connect with
//! `PimbleClient`, which builds for every host already. One WebSocket does:
//! asking Pimble Cloud's relay how it answers a member's connection
//! ([`Http::websocket_close_code`]), because the JSON-RPC client does not
//! pass a close frame's code on (docs/RELAY_CONTRACT.md: `owner offline` is
//! close code 4404).

use std::fmt;
use std::time::Duration;

use crate::runtime::{Elapsed, Runtime};

/// The methods the accounts service is called with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

/// One call.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    /// `Authorization: Bearer <bearer>`.
    pub bearer: Option<String>,
    /// The body, sent with its `Content-Type`.
    pub body: Option<Body>,
}

/// A request's body and its media type.
#[derive(Debug, Clone)]
pub struct Body {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

impl Body {
    /// JSON (`application/json`): what the accounts service takes.
    pub fn json(bytes: Vec<u8>) -> Self {
        Body { content_type: "application/json", bytes }
    }

    /// Newline-delimited JSON (`application/x-ndjson`): diagnostic records.
    pub fn ndjson(bytes: Vec<u8>) -> Self {
        Body { content_type: "application/x-ndjson", bytes }
    }
}

/// What the service answered, whatever its status.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The service could not be reached (no answer at all): a sentence.
#[derive(Debug, Clone)]
pub struct HttpError(pub String);

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

/// See the module documentation.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Http: Send + Sync + fmt::Debug {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;

    /// Open a WebSocket to `url` (`ws://` or `wss://`) with `token` as its
    /// credential, and answer the close code it is closed with as the first
    /// thing it says, if it says that within [`FIRST_WORD_WAIT`] of opening.
    /// `None` for anything else: it does not open within
    /// [`SOCKET_CONNECT_TIMEOUT`] (refused before the upgrade, no network),
    /// it says something else first, or it stays quiet. The socket is closed
    /// before this answers.
    ///
    /// The desktop sends the token as `Authorization: Bearer`; a browser's
    /// WebSocket cannot set a header, so there it is the `access_token`
    /// query parameter, which every Pimble endpoint takes too.
    async fn websocket_close_code(&self, url: &str, token: &str) -> Option<u16>;
}

/// Another [`Http`] whose every call ends within `limit` on `runtime`'s clock:
/// a call that has had no answer by then answers [`HttpError`] (`timed
/// out`), and the caller stops waiting for it. The same on every host, where
/// the hosts' own clients are not: the desktop's has a 15 s timeout, a
/// browser's `fetch` none at all. A request already on the wire is not taken
/// back; only the waiting for its answer ends.
#[derive(Debug, Clone, Copy)]
pub struct Bounded<'a> {
    inner: &'a dyn Http,
    runtime: &'a Runtime,
    limit: Duration,
}

impl<'a> Bounded<'a> {
    pub fn new(inner: &'a dyn Http, runtime: &'a Runtime, limit: Duration) -> Self {
        Bounded { inner, runtime, limit }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Http for Bounded<'_> {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let url = request.url.clone();
        match self.runtime.timeout(self.limit, self.inner.send(request)).await {
            Ok(answer) => answer,
            Err(Elapsed) => Err(HttpError(format!("{url} timed out after {} ms", self.limit.as_millis()))),
        }
    }

    /// The inner client's own: it is bounded already
    /// ([`SOCKET_CONNECT_TIMEOUT`], [`FIRST_WORD_WAIT`]).
    async fn websocket_close_code(&self, url: &str, token: &str) -> Option<u16> {
        self.inner.websocket_close_code(url, token).await
    }
}

/// How long [`Http::websocket_close_code`] waits for the socket to open.
pub const SOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long, once open, it waits for the first thing the socket says.
pub const FIRST_WORD_WAIT: Duration = Duration::from_secs(2);

/// The desktop's: one `reqwest` client (rustls, a 15 s timeout per call).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct ReqwestHttp(reqwest::Client);

#[cfg(not(target_arch = "wasm32"))]
impl Default for ReqwestHttp {
    fn default() -> Self {
        Self(
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("a reqwest client with only a timeout set always builds"),
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl Http for ReqwestHttp {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut builder = match request.method {
            Method::Get => self.0.get(&request.url),
            Method::Post => self.0.post(&request.url),
            Method::Put => self.0.put(&request.url),
            Method::Delete => self.0.delete(&request.url),
        };
        if let Some(bearer) = &request.bearer {
            builder = builder.bearer_auth(bearer);
        }
        if let Some(body) = request.body {
            builder = builder.header(reqwest::header::CONTENT_TYPE, body.content_type).body(body.bytes);
        }
        let response = builder.send().await.map_err(|e| HttpError(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|e| HttpError(e.to_string()))?.to_vec();
        Ok(HttpResponse { status, body })
    }

    async fn websocket_close_code(&self, url: &str, token: &str) -> Option<u16> {
        socket::close_code(url, token).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[path = "http/native_socket.rs"]
mod socket;

#[cfg(target_arch = "wasm32")]
#[path = "http/browser_socket.rs"]
mod socket;

/// A browser's worker (and the page, and Node): the global `fetch`, with
/// `credentials: "include"` so the accounts service's session cookie goes
/// along whether or not a bearer does. No timeout of its own: the browser's
/// (a caller that needs one wraps it in [`Bounded`]).
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy, Default)]
pub struct FetchHttp;

#[cfg(target_arch = "wasm32")]
mod fetch {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        /// `globalThis.fetch`: a window's, a worker's or Node's alike, where
        /// web-sys only offers `Window::fetch` and `WorkerGlobalScope::fetch`.
        #[wasm_bindgen(js_name = fetch)]
        pub fn fetch_with_request(request: &web_sys::Request) -> js_sys::Promise;
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl Http for FetchHttp {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        use wasm_bindgen::JsCast;

        let failed = |what: &str, e: wasm_bindgen::JsValue| HttpError(format!("{what}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}"))));
        let init = web_sys::RequestInit::new();
        init.set_method(match request.method {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        });
        init.set_credentials(web_sys::RequestCredentials::Include);
        let headers = web_sys::Headers::new().map_err(|e| failed("headers", e))?;
        if let Some(bearer) = &request.bearer {
            headers.set("Authorization", &format!("Bearer {bearer}")).map_err(|e| failed("headers", e))?;
        }
        if let Some(body) = &request.body {
            headers.set("Content-Type", body.content_type).map_err(|e| failed("headers", e))?;
            init.set_body(&js_sys::Uint8Array::from(body.bytes.as_slice()).into());
        }
        init.set_headers(&headers.into());
        let req = web_sys::Request::new_with_str_and_init(&request.url, &init).map_err(|e| failed("request", e))?;
        let answer = wasm_bindgen_futures::JsFuture::from(fetch::fetch_with_request(&req)).await.map_err(|e| failed("fetch", e))?;
        let response: web_sys::Response = answer.dyn_into().map_err(|e| failed("fetch answered something else", e))?;
        let status = response.status();
        let buffer = wasm_bindgen_futures::JsFuture::from(response.array_buffer().map_err(|e| failed("body", e))?).await.map_err(|e| failed("body", e))?;
        let body = js_sys::Uint8Array::new(&buffer).to_vec();
        Ok(HttpResponse { status, body })
    }

    async fn websocket_close_code(&self, url: &str, token: &str) -> Option<u16> {
        socket::close_code(url, token).await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// An endpoint that answers 200 after `delay`.
    #[derive(Debug)]
    struct After(Duration);

    #[async_trait::async_trait]
    impl Http for After {
        async fn send(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
            tokio::time::sleep(self.0).await;
            Ok(HttpResponse { status: 200, body: Vec::new() })
        }

        async fn websocket_close_code(&self, _url: &str, _token: &str) -> Option<u16> {
            Some(4404)
        }
    }

    fn post() -> HttpRequest {
        HttpRequest { method: Method::Post, url: "https://cloud.test/x".into(), bearer: None, body: None }
    }

    #[tokio::test]
    async fn a_call_with_no_answer_by_its_limit_ends_with_an_error() {
        let runtime = Runtime::default();
        let stalled = After(Duration::from_secs(3600));
        let bounded = Bounded::new(&stalled, &runtime, Duration::from_millis(50));
        let started = std::time::Instant::now();
        let answer = tokio::time::timeout(Duration::from_secs(10), bounded.send(post())).await.expect("the bound ends the call");
        let error = answer.expect_err("no answer in time");
        assert!(error.0.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));

        let quick = After(Duration::ZERO);
        let answer = Bounded::new(&quick, &runtime, Duration::from_millis(50)).send(post()).await.expect("answered in time");
        assert_eq!(answer.status, 200);
        assert_eq!(Bounded::new(&quick, &runtime, Duration::ZERO).websocket_close_code("wss://x", "t").await, Some(4404));
    }
}
