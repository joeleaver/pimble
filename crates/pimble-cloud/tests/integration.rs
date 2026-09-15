//! End-to-end tests for the accounts service (docs/CLOUD_CONTRACT.md,
//! section C). Every test drives the real axum app (`pimble_cloud::build_router`)
//! over real HTTP, backed by a real `rhypedb-server` process and a real
//! in-process `pimble_server::PimbleServer` — no RhypeDB or Pimble server
//! double stands in for either.
//!
//! `rhypedb-server` isn't a workspace member here (rhypedb is a sibling repo,
//! see CLAUDE.md), so there is no library entry point this crate can start
//! in-process on an ephemeral port: `rhypedb_server::run()` parses this test
//! binary's own argv via `clap` and calls `std::process::exit` on any
//! problem, which is exactly wrong for a test. Every test therefore spawns
//! the real `rhypedb-server` binary as a subprocess (found at
//! `~/dev/rhypedb/target/{release,debug}/rhypedb-server` or on `PATH`) and
//! skips cleanly, printing why, when it can't be found.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};

const SCHEMA: &str = include_str!("../schema.rhype");

// ── Harness ──────────────────────────────────────────────────────────────

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn find_rhypedb_server_binary() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("RHYPEDB_SERVER_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        for rel in ["dev/rhypedb/target/release/rhypedb-server", "dev/rhypedb/target/debug/rhypedb-server"] {
            let p = PathBuf::from(&home).join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in ["rhypedb-server", "rhypedb"] {
                let p = dir.join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

struct RhypeDbGuard {
    child: Child,
    addr: String,
    /// stdout+stderr, drained continuously by a background thread so a
    /// long-running server can't block on a full pipe. Only read back out
    /// on the startup-failure paths in `spawn_rhypedb` (a live `Stack`'s
    /// guard just needs to keep the drainer threads' `Arc` alive); kept here
    /// rather than dropped so a future test that wants a mid-run crash's
    /// output has it available.
    _output: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Drop for RhypeDbGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Continuously copy `reader`'s bytes, line by line, into `into` — run on a
/// plain OS thread (not tokio) since it blocks on synchronous I/O for the
/// subprocess's whole lifetime.
fn drain_into(reader: impl std::io::Read + Send + 'static, into: std::sync::Arc<std::sync::Mutex<String>>) {
    use std::io::BufRead;
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(reader).lines().map_while(Result::ok) {
            if let Ok(mut buf) = into.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    });
}

/// Spawn the real `rhypedb-server` binary at `binary` and wait for its
/// binary-protocol port to accept connections. `Err` (never `None`) on any
/// failure — a binary that was found but wouldn't start, crashed during
/// startup, or never opened its port is a real test failure, not something
/// to skip past; only "no binary exists anywhere" (checked by the caller
/// before this is called at all) is a skip.
async fn spawn_rhypedb(binary: &Path, data_dir: &Path, schema_path: &Path) -> Result<RhypeDbGuard, String> {
    let http_port = free_port();
    let tcp_port = free_port();
    let mut child = Command::new(binary)
        .arg("--schema")
        .arg(schema_path)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--listen")
        .arg(format!("127.0.0.1:{http_port}"))
        .arg("--tcp-listen")
        .arg(format!("127.0.0.1:{tcp_port}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", binary.display()))?;

    let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    drain_into(child.stdout.take().unwrap(), output.clone());
    drain_into(child.stderr.take().unwrap(), output.clone());

    let addr = format!("127.0.0.1:{tcp_port}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let log = output.lock().unwrap().clone();
            return Err(format!("{} exited early with {status}; output:\n{log}", binary.display()));
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let log = output.lock().unwrap().clone();
            return Err(format!(
                "{} never opened its TCP port ({addr}) within 15s; output so far:\n{log}",
                binary.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(RhypeDbGuard { child, addr, _output: output })
}

/// Everything a test needs: the two real backing servers plus the cloud
/// service's own axum app, all bound to ephemeral loopback ports. Dropping
/// it tears the stack down (kills the rhypedb-server subprocess, aborts the
/// cloud app's serve task; the in-process Pimble server and its temp dirs go
/// with the struct).
pub struct Stack {
    _rhypedb: RhypeDbGuard,
    _rhypedb_data_dir: tempfile::TempDir,
    _rhypedb_schema_dir: tempfile::TempDir,
    pub pimble_server: pimble_server::PimbleServer,
    pub service_token: String,
    _pimble_creds_dir: tempfile::TempDir,
    _pimble_replicas_dir: tempfile::TempDir,
    _stores_dir: tempfile::TempDir,
    pub base_url: String,
    pub http: reqwest::Client,
    /// Kept so tests can reach the [`pimble_cloud::mail::LogMailer`] through
    /// `app_state.mailer` (Phase 1b: every test runs with no `RESEND_API_KEY`,
    /// so this is always a `LogMailer`) and the DB directly (e.g. to force a
    /// verify token's expiry in the past).
    pub app_state: pimble_cloud::state::AppState,
    cloud_server: tokio::task::JoinHandle<()>,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.cloud_server.abort();
    }
}

pub async fn spawn_stack() -> Option<Stack> {
    spawn_stack_with_releases_base_url(None).await
}

pub async fn spawn_stack_with_releases_base_url(releases_base_url: Option<String>) -> Option<Stack> {
    // The ONLY skip condition: no rhypedb-server binary exists anywhere we
    // know to look. A binary that exists but fails to start is a real test
    // failure (`spawn_rhypedb`'s `expect` below panics with its stderr/stdout).
    let binary = find_rhypedb_server_binary()?;

    let rhypedb_schema_dir = tempfile::tempdir().unwrap();
    let schema_path = rhypedb_schema_dir.path().join("schema.rhype");
    std::fs::write(&schema_path, SCHEMA).unwrap();
    let rhypedb_data_dir = tempfile::tempdir().unwrap();

    let rhypedb = spawn_rhypedb(&binary, rhypedb_data_dir.path(), &schema_path)
        .await
        .expect("rhypedb-server binary was found but did not become ready");

    let service_token = "pimble-cloud-test-service-token".to_string();
    let pimble_creds_dir = tempfile::tempdir().unwrap();
    let pimble_replicas_dir = tempfile::tempdir().unwrap();
    let mut pimble_server = pimble_server::PimbleServer::with_config(pimble_server::ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(service_token.clone()),
        credentials_path: Some(pimble_creds_dir.path().join("credentials.json")),
        replicas_dir: Some(pimble_replicas_dir.path().to_path_buf()),
        // Forward-compatible with fields agent B is concurrently adding to
        // `ServerConfig` (JWT verification, origin allowlist): this service
        // only exercises the pre-existing static-token mode.
        ..Default::default()
    });
    pimble_server.start().await.expect("pimble server starts");
    let pimble_addr = pimble_server.addr();

    let stores_dir = tempfile::tempdir().unwrap();

    // A fixed, non-secret development seed: deterministic within a test run
    // so a test can verify a minted token's signature against the JWKS this
    // same process serves.
    let dev_seed = "aa".repeat(32);

    let config = pimble_cloud::config::Config {
        port: 0,
        rhypedb_addr: rhypedb.addr.clone(),
        pimble_server_url: format!("http://{pimble_addr}"),
        pimble_server_token: Some(service_token.clone()),
        pimble_stores_dir: stores_dir.path().to_path_buf(),
        jkbase_auth_issuer_url: None,
        jkbase_auth_key: None,
        dev_signing_seed: Some(dev_seed),
        // A fake host: not actually resolvable. Tests never dereference a
        // full verify link against this address — they pull the `token`
        // query parameter out of it and hit `stack.base_url` (the real
        // bound address) directly. See `extract_verify_token`.
        public_url: "http://cloud.test".to_string(),
        github_repo: "joeleaver/pimble".to_string(),
        releases_base_url,
        // No RESEND_API_KEY: every test runs against `LogMailer`, which is
        // exactly what `extract_verify_token` and friends rely on.
        resend_api_key: None,
        mail_from: "Pimble <no-reply@m.pimble.app>".to_string(),
    };

    let app_state = pimble_cloud::build_state(config).await.expect("build_state");
    let router = pimble_cloud::router_from_state(app_state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cloud_server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    Some(Stack {
        _rhypedb: rhypedb,
        _rhypedb_data_dir: rhypedb_data_dir,
        _rhypedb_schema_dir: rhypedb_schema_dir,
        pimble_server,
        service_token,
        _pimble_creds_dir: pimble_creds_dir,
        _pimble_replicas_dir: pimble_replicas_dir,
        _stores_dir: stores_dir,
        base_url: format!("http://{addr}/api/v1"),
        http: reqwest::Client::new(),
        app_state,
        cloud_server,
    })
}

macro_rules! skip_without_rhypedb {
    () => {
        match spawn_stack().await {
            Some(s) => s,
            None => {
                eprintln!(
                    "SKIP: no rhypedb-server binary found (checked $RHYPEDB_SERVER_BIN, \
                     ~/dev/rhypedb/target/{{release,debug}}/rhypedb-server, and $PATH)"
                );
                return;
            }
        }
    };
}

/// `POST /signup` (Phase 1b: 202, no session — see docs/CLOUD_CONTRACT.md
/// "Phase 1b: email verification").
async fn signup(stack: &Stack, email: &str, password: &str) -> Value {
    let resp = stack
        .http
        .post(format!("{}/signup", stack.base_url))
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202, "signup for {email} should be accepted (verification pending)");
    resp.json().await.unwrap()
}

fn first_cookie_pair(resp: &reqwest::Response) -> String {
    resp.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_string()
}

async fn login(stack: &Stack, email: &str, password: &str) -> (Value, String) {
    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "password": password })).send().await.unwrap();
    assert_eq!(resp.status(), 200, "login for {email} should succeed");
    let cookie = first_cookie_pair(&resp);
    let body: Value = resp.json().await.unwrap();
    (body, cookie)
}

/// A `reqwest::Client` that does not follow redirects — `GET /verify` always
/// answers 303, and a normal client would try to follow it to a `/login.html`
/// this test stack never serves.
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

/// Pulls the verify token last sent (via `LogMailer`) to `email` out of
/// `stack.app_state`. Panics with a clear message if no mail was sent —
/// every test that calls this expects one to be waiting.
fn extract_verify_token(stack: &Stack, email: &str) -> String {
    let log_mailer = stack.app_state.mailer.as_log_mailer().expect("test stack must run with LogMailer (no RESEND_API_KEY)");
    let body = log_mailer.last_message(email).unwrap_or_else(|| panic!("no verification email was sent to {email}"));
    let link = pimble_cloud::mail::first_url(&body).unwrap_or_else(|| panic!("verification email body had no link:\n{body}"));
    // `link` is `<fake public_url>/api/v1/verify?token=<hex>`; only the
    // token is usable against this test stack's real bound address (see
    // `spawn_stack_with_releases_base_url`'s comment on `public_url`).
    link.rsplit("token=").next().unwrap().to_string()
}

async fn visit_verify_link(stack: &Stack, token: &str) -> reqwest::Response {
    no_redirect_client().get(format!("{}/verify?token={token}", stack.base_url)).send().await.unwrap()
}

fn redirect_location(resp: &reqwest::Response) -> String {
    resp.headers().get("location").unwrap().to_str().unwrap().to_string()
}

/// Consumes the verify token last sent to `email`, asserts it redirects to
/// `verified=1`, and logs in. Does not sign up — call [`signup`] first (or
/// use [`signup_verify_login`], which does both).
async fn verify_then_login(stack: &Stack, email: &str, password: &str) -> (Value, String) {
    let token = extract_verify_token(stack, email);
    let resp = visit_verify_link(stack, &token).await;
    assert_eq!(resp.status(), 303, "verify should redirect");
    let location = redirect_location(&resp);
    assert!(location.contains("verified=1"), "unexpected redirect: {location}");
    login(stack, email, password).await
}

async fn signup_verify_login(stack: &Stack, email: &str, password: &str) -> (Value, String) {
    signup(stack, email, password).await;
    verify_then_login(stack, email, password).await
}

// ── Accounts: signup / login / logout / me ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn signup_login_logout_me_round_trip() {
    let stack = skip_without_rhypedb!();
    let email = "alice@example.com";
    let password = "correct horse battery staple";

    let signup_body = signup(&stack, email, password).await;
    assert_eq!(signup_body["status"], "verification_sent");
    assert_eq!(signup_body["email"], email);
    assert!(signup_body.get("session").is_none(), "signup must not start a session");
    assert!(signup_body.get("token").is_none(), "signup must not mint a token");

    let (body, cookie) = verify_then_login(&stack, email, password).await;
    assert_eq!(body["user"]["email"], email);
    assert!(body["user"]["id"].as_str().is_some(), "user id (the sub UUID) should be present");
    let session_token = body["session"].as_str().unwrap().to_string();
    assert!(body["token"].as_str().is_some());
    assert!(body["exp"].as_i64().is_some());

    // /me via the cookie.
    let resp = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let me: Value = resp.json().await.unwrap();
    assert_eq!(me["email"], email);

    // /me via `Authorization: Bearer <session>` instead of the cookie.
    let resp = stack.http.get(format!("{}/me", stack.base_url)).bearer_auth(&session_token).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // No credential at all.
    let resp = stack.http.get(format!("{}/me", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 401);

    // logout, then the same cookie is dead.
    let resp = stack.http.post(format!("{}/logout", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let resp = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 401);

    // login re-establishes a session.
    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "password": password })).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_and_unknown_email_are_both_401() {
    let stack = skip_without_rhypedb!();
    signup(&stack, "carol@example.com", "the right password").await;

    let resp = stack
        .http
        .post(format!("{}/login", stack.base_url))
        .json(&json!({ "email": "carol@example.com", "password": "the wrong password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = stack
        .http
        .post(format!("{}/login", stack.base_url))
        .json(&json!({ "email": "nobody-signed-up-with-this@example.com", "password": "anything" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_email_is_409_once_verified_but_202_while_unverified() {
    let stack = skip_without_rhypedb!();
    signup(&stack, "dupe@example.com", "first password!").await;

    // Still unverified: a duplicate signup just re-sends the link — 202,
    // no enumeration (docs/CLOUD_CONTRACT.md "Phase 1b").
    let resp = stack
        .http
        .post(format!("{}/signup", stack.base_url))
        .json(&json!({ "email": "dupe@example.com", "password": "second password!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    // Verify (with the ORIGINAL password — a re-send never changes it), then
    // a duplicate signup is a real 409.
    verify_then_login(&stack, "dupe@example.com", "first password!").await;
    let resp = stack
        .http
        .post(format!("{}/signup", stack.base_url))
        .json(&json!({ "email": "dupe@example.com", "password": "third password!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    // Case-insensitivity: the same address differently-cased also conflicts.
    let resp = stack
        .http
        .post(format!("{}/signup", stack.base_url))
        .json(&json!({ "email": "DUPE@example.com", "password": "fourth password!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

// ── Phase 1b: email verification ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn login_before_verification_is_403_email_unverified() {
    let stack = skip_without_rhypedb!();
    let email = "oscar@example.com";
    let password = "oscar's unverified password";
    signup(&stack, email, password).await;

    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "password": password })).send().await.unwrap();
    assert_eq!(resp.status(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "email_unverified");

    // The password check still runs first: a wrong password is 401, not
    // "email_unverified" (which would leak that the email exists yet the
    // password was never even checked).
    let resp =
        stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "password": "not oscar's password" })).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_verify_token_redirects_with_expired() {
    let stack = skip_without_rhypedb!();
    let email = "nina@example.com";
    signup(&stack, email, "nina's password!!").await;
    let token = extract_verify_token(&stack, email);

    // Force the already-issued token into the past (no way to wait out the
    // real 24h TTL in a test) by re-setting the SAME hash with an expired
    // timestamp, reaching the DB directly through `stack.app_state`.
    let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
    let past_ms = chrono::Utc::now().timestamp_millis() - 1_000;
    stack.app_state.db.set_verify_token(user.rid, &user.verify_token_hash, past_ms).await.unwrap();

    let resp = visit_verify_link(&stack, &token).await;
    assert_eq!(resp.status(), 303);
    assert!(redirect_location(&resp).contains("verify_error=expired"));

    // An unknown token (never issued, or already consumed) redirects with
    // "invalid" rather than "expired".
    let resp = visit_verify_link(&stack, &"0".repeat(64)).await;
    assert!(redirect_location(&resp).contains("verify_error=invalid"));
}

#[tokio::test(flavor = "multi_thread")]
async fn second_signup_for_unverified_address_resends_and_invalidates_the_old_token() {
    let stack = skip_without_rhypedb!();
    let email = "judy@example.com";
    signup(&stack, email, "judy's real password!!").await;
    let old_token = extract_verify_token(&stack, email);

    let resp = stack
        .http
        .post(format!("{}/signup", stack.base_url))
        .json(&json!({ "email": email, "password": "ignored on a resend" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let new_token = extract_verify_token(&stack, email);
    assert_ne!(old_token, new_token, "a re-sent signup should issue a fresh token");

    let resp = visit_verify_link(&stack, &old_token).await;
    assert!(redirect_location(&resp).contains("verify_error=invalid"), "the superseded token must no longer verify");

    let resp = visit_verify_link(&stack, &new_token).await;
    assert!(redirect_location(&resp).contains("verified=1"));

    // The original password (not the ignored resend one) still works.
    login(&stack, email, "judy's real password!!").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resend_verification_issues_a_new_token_and_invalidates_the_old() {
    let stack = skip_without_rhypedb!();
    let email = "kevin@example.com";
    signup(&stack, email, "kevin's password!!").await;
    let old_token = extract_verify_token(&stack, email);

    let resp =
        stack.http.post(format!("{}/resend-verification", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let new_token = extract_verify_token(&stack, email);
    assert_ne!(old_token, new_token);

    let resp = visit_verify_link(&stack, &old_token).await;
    assert!(redirect_location(&resp).contains("verify_error=invalid"));
    let resp = visit_verify_link(&stack, &new_token).await;
    assert!(redirect_location(&resp).contains("verified=1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn resend_verification_is_202_for_unknown_and_already_verified_addresses_too() {
    let stack = skip_without_rhypedb!();

    // Unknown address: 202, no enumeration.
    let resp = stack
        .http
        .post(format!("{}/resend-verification", stack.base_url))
        .json(&json!({ "email": "nobody-ever-signed-up@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    // Already verified: also 202, and it's a no-op (login keeps working).
    let email = "laura@example.com";
    signup_verify_login(&stack, email, "laura's password!!").await;
    let resp = stack.http.post(format!("{}/resend-verification", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    login(&stack, email, "laura's password!!").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resend_verification_is_rate_limited_to_once_per_minute() {
    let stack = skip_without_rhypedb!();
    let email = "mallory@example.com";
    signup(&stack, email, "mallory's password!!").await;

    let resp = stack.http.post(format!("{}/resend-verification", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token_after_first = extract_verify_token(&stack, email);

    // A second resend within the same minute is accepted (still 202) but
    // does nothing — the token in flight does not change.
    let resp = stack.http.post(format!("{}/resend-verification", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token_after_second = extract_verify_token(&stack, email);
    assert_eq!(token_after_first, token_after_second, "a resend within the same minute must be rate-limited");

    let resp = visit_verify_link(&stack, &token_after_second).await;
    assert!(redirect_location(&resp).contains("verified=1"), "the token that survived the rate limit should still verify");
}

// ── Tokens: claims + JWKS verification ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn token_claims_and_jwks_verify() {
    let stack = skip_without_rhypedb!();
    let (login_body, cookie) = signup_verify_login(&stack, "erin@example.com", "a fine password").await;
    let user_id = login_body["user"]["id"].as_str().unwrap().to_string();

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Erin's Notes" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let token_resp: Value =
        stack.http.post(format!("{}/token", stack.base_url)).header("Cookie", &cookie).send().await.unwrap().json().await.unwrap();
    let token = token_resp["token"].as_str().unwrap();
    assert!(token_resp["rpc_url"].as_str().unwrap().ends_with("/rpc"));

    let jwks: Value = stack.http.get(format!("{}/.well-known/jwks.json", stack.base_url)).send().await.unwrap().json().await.unwrap();
    let payload = verify_and_decode(token, &jwks);

    assert_eq!(payload["sub"], user_id);
    assert_eq!(payload["aud"], "pimble");
    assert_eq!(payload["claims"]["email"], "erin@example.com");
    assert_eq!(payload["claims"]["stores"][&store_id], "owner");
}

/// Verify `token`'s Ed25519 signature against `jwks` (matching by `kid`) and
/// return its decoded payload.
fn verify_and_decode(token: &str, jwks: &Value) -> Value {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "a JWT has three segments");

    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    let payload: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    let signature_bytes = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();

    let kid = header["kid"].as_str().expect("header carries a kid");
    let key = jwks["keys"].as_array().unwrap().iter().find(|k| k["kid"] == kid).expect("jwks has a matching kid");
    let x = key["x"].as_str().unwrap();
    let public_key_bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(x).unwrap().try_into().unwrap();
    let verifying_key = VerifyingKey::from_bytes(&public_key_bytes).unwrap();

    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let signature = Signature::from_bytes(&signature_bytes.try_into().unwrap());
    verifying_key.verify(signing_input.as_bytes(), &signature).expect("JWT signature verifies against the served JWKS");

    payload
}

// ── Stores + members ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn create_store_creates_a_real_store_and_an_owner_grant() {
    let stack = skip_without_rhypedb!();
    let (_login_body, cookie) = signup_verify_login(&stack, "frank@example.com", "another fine password").await;

    let resp = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Frank's Store" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let store: Value = resp.json().await.unwrap();
    assert_eq!(store["name"], "Frank's Store");
    assert_eq!(store["role"], "owner");
    let store_id = store["store_id"].as_str().unwrap().to_string();

    // It's a real store on the Pimble server, not just a RhypeDB row: connect
    // as the service principal and check the server itself has it open.
    let pimble_client = pimble_client::PimbleClient::connect_with_auth(
        format!("http://{}", stack.pimble_server.addr()),
        &pimble_core::AuthMethod::Bearer { token: stack.service_token.clone() },
    )
    .await
    .unwrap();
    let open_stores = pimble_client.list_stores().await.unwrap();
    assert!(
        open_stores.iter().any(|s| s.id.as_uuid().to_string() == store_id),
        "the store POST /stores created should be open on the Pimble server"
    );

    // GET /stores lists it for the owner.
    let stores: Value = stack.http.get(format!("{}/stores", stack.base_url)).header("Cookie", &cookie).send().await.unwrap().json().await.unwrap();
    assert!(stores.as_array().unwrap().iter().any(|s| s["store_id"] == store_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn member_lifecycle_and_last_owner_refusal() {
    let stack = skip_without_rhypedb!();
    let (owner_body, owner_cookie) = signup_verify_login(&stack, "grace@example.com", "owner password!!").await;
    let (_member_body, member_cookie) = signup_verify_login(&stack, "heidi@example.com", "member password!!").await;
    let member_user_id = stack
        .http
        .get(format!("{}/me", stack.base_url))
        .header("Cookie", &member_cookie)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = owner_body;

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "name": "Shared Store" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    // A non-member can't see the members list.
    let resp = stack.http.get(format!("{}/stores/{store_id}/members", stack.base_url)).header("Cookie", &member_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 403);

    // The owner adds heidi as a reader.
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "email": "heidi@example.com", "role": "reader" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let members: Value =
        stack.http.get(format!("{}/stores/{store_id}/members", stack.base_url)).header("Cookie", &owner_cookie).send().await.unwrap().json().await.unwrap();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.iter().any(|m| m["email"] == "heidi@example.com" && m["role"] == "reader"));

    // A non-owner (reader) can't change roles.
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", &member_cookie)
        .json(&json!({ "email": "heidi@example.com", "role": "editor" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // A non-owner (reader) can't delete the store either.
    let resp = stack.http.delete(format!("{}/stores/{store_id}", stack.base_url)).header("Cookie", &member_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 403);

    // The owner changes heidi to editor.
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "email": "heidi@example.com", "role": "editor" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["role"], "editor");

    // Demoting the sole owner (via PUT) is refused exactly like removing
    // them: either way the store would end up with zero owners.
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "email": "grace@example.com", "role": "editor" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    // Removing the sole owner is refused.
    let owner_user_id = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &owner_cookie).send().await.unwrap().json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/members/{owner_user_id}", stack.base_url))
        .header("Cookie", &owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    // The owner removes heidi (an editor, not the last owner: allowed).
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/members/{member_user_id}", stack.base_url))
        .header("Cookie", &owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let members: Value =
        stack.http.get(format!("{}/stores/{store_id}/members", stack.base_url)).header("Cookie", &owner_cookie).send().await.unwrap().json().await.unwrap();
    assert_eq!(members.as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_store_hides_it() {
    let stack = skip_without_rhypedb!();
    let (_body, cookie) = signup_verify_login(&stack, "ivan@example.com", "yet another password").await;

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Disposable" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let resp = stack.http.delete(format!("{}/stores/{store_id}", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let stores: Value = stack.http.get(format!("{}/stores", stack.base_url)).header("Cookie", &cookie).send().await.unwrap().json().await.unwrap();
    assert!(!stores.as_array().unwrap().iter().any(|s| s["store_id"] == store_id), "a deleted store must not be listed");

    let resp = stack.http.get(format!("{}/stores/{store_id}/members", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 404, "a deleted store's members endpoint should look like it never existed");
}

// ── Releases ────────────────────────────────────────────────────────────

async fn spawn_github_stub(body: Value, status: axum::http::StatusCode) -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route(
        "/repos/:owner/:repo/releases/latest",
        axum::routing::get(move || {
            let body = body.clone();
            async move { (status, axum::Json(body)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), task)
}

#[tokio::test(flavor = "multi_thread")]
async fn releases_endpoint_uses_a_stub_and_infers_os() {
    let (stub_url, _stub) = spawn_github_stub(
        json!({
            "tag_name": "v1.2.3",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                { "name": "pimble-1.2.3-linux-x86_64.tar.gz", "browser_download_url": "https://example.com/linux.tar.gz", "size": 111 },
                { "name": "pimble-1.2.3-windows-x86_64.zip", "browser_download_url": "https://example.com/windows.zip", "size": 222 },
            ],
        }),
        axum::http::StatusCode::OK,
    )
    .await;

    let stack = match spawn_stack_with_releases_base_url(Some(stub_url)).await {
        Some(s) => s,
        None => {
            eprintln!("SKIP: no rhypedb-server binary found");
            return;
        }
    };

    let resp = stack.http.get(format!("{}/releases", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["version"], "v1.2.3");
    let assets = body["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[0]["os"], "linux");
    assert_eq!(assets[1]["os"], "windows");
}

#[tokio::test(flavor = "multi_thread")]
async fn releases_endpoint_tolerates_no_releases_yet() {
    let (stub_url, _stub) = spawn_github_stub(json!({ "message": "Not Found" }), axum::http::StatusCode::NOT_FOUND).await;

    let stack = match spawn_stack_with_releases_base_url(Some(stub_url)).await {
        Some(s) => s,
        None => {
            eprintln!("SKIP: no rhypedb-server binary found");
            return;
        }
    };

    let resp = stack.http.get(format!("{}/releases", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["version"], "");
    assert_eq!(body["assets"], json!([]));
}

// ── Health ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn health_is_ok() {
    let stack = skip_without_rhypedb!();
    let resp = stack.http.get(format!("{}/health", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}
