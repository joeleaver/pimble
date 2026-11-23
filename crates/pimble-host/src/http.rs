//! Outbound HTTP to the Pimble Cloud accounts service
//! (docs/ONE_CODEBASE_CONTRACT.md seam D): sign-in, tokens, stores, keys,
//! members, the personal document. JSON in, JSON out, a session bearer.
//!
//! - The desktop: [`ReqwestHttp`] (`reqwest` with rustls, a 15 s timeout), as
//!   before this seam.
//! - The browser's worker: [`FetchHttp`] (the global `fetch`), with the
//!   session cookie the page already has, so the bearer may be left out.
//! - A phone app: its platform's client, or `reqwest` as on the desktop.
//!
//! Links to Pimble servers do not come through here: they connect with
//! `PimbleClient`, which builds for every host already.

use std::fmt;

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
    /// A JSON body, sent with `Content-Type: application/json`.
    pub json: Option<Vec<u8>>,
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
}

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
        if let Some(json) = request.json {
            builder = builder.header(reqwest::header::CONTENT_TYPE, "application/json").body(json);
        }
        let response = builder.send().await.map_err(|e| HttpError(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|e| HttpError(e.to_string()))?.to_vec();
        Ok(HttpResponse { status, body })
    }
}

/// A browser's worker (and the page, and Node): the global `fetch`, with
/// `credentials: "include"` so the accounts service's session cookie goes
/// along whether or not a bearer does. No timeout of its own: the browser's.
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
        if let Some(json) = &request.json {
            headers.set("Content-Type", "application/json").map_err(|e| failed("headers", e))?;
            init.set_body(&js_sys::Uint8Array::from(json.as_slice()).into());
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
}
