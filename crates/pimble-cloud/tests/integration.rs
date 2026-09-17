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
use std::sync::OnceLock;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};

use pimble_crypto::{AccountKeyBlob, AccountKeys, KdfParams};

const SCHEMA: &str = include_str!("../schema.rhype");

/// A [`pimble_cloud::mail::Mailer`] that always fails, simulating Resend
/// rejecting a message (e.g. its real 422 on an `example.com` recipient) —
/// used to test that the raw provider response never reaches a caller.
struct FailingMailer;

#[async_trait::async_trait]
impl pimble_cloud::mail::Mailer for FailingMailer {
    async fn send(&self, _to: &str, _subject: &str, _text: &str, _html: &str) -> pimble_cloud::error::CloudResult<()> {
        Err(pimble_cloud::error::CloudError::Internal(
            "Resend rejected the message (422 Unprocessable Entity): {\"message\":\"invalid recipient domain\"}".to_string(),
        ))
    }
}

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

/// How many times a port-picking spawn (`spawn_rhypedb`, the in-process
/// Pimble server, this crate's own axum app, the GitHub stub) retries with a
/// fresh port before giving up for real. `free_port` (below) probes for a
/// free port by binding it and immediately releasing it — a TOCTOU race
/// against anything else (most likely another concurrently-starting test's
/// same probe) that grabs that exact port before the real server binds it a
/// moment later. One retry with a freshly-probed port almost always clears
/// it; five is generous headroom.
const MAX_PORT_ATTEMPTS: u32 = 5;
const PORT_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// Spawn the real `rhypedb-server` binary at `binary`, retrying with fresh
/// ports (see `MAX_PORT_ATTEMPTS`) if it exits early or its port never
/// opens. `Err` (never `None`) once attempts are exhausted — a binary that
/// exists but never starts even after retries is a real test failure, not
/// something to skip past; only "no binary exists anywhere" (checked by the
/// caller before this is called at all) is a skip.
async fn spawn_rhypedb(binary: &Path, data_dir: &Path, schema_path: &Path) -> Result<RhypeDbGuard, String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_PORT_ATTEMPTS {
        match try_spawn_rhypedb(binary, data_dir, schema_path).await {
            Ok(guard) => return Ok(guard),
            Err(e) => {
                last_err = format!("attempt {attempt}/{MAX_PORT_ATTEMPTS}: {e}");
                if attempt < MAX_PORT_ATTEMPTS {
                    tokio::time::sleep(PORT_RETRY_BACKOFF).await;
                }
            }
        }
    }
    Err(format!("giving up after {MAX_PORT_ATTEMPTS} attempts; {last_err}"))
}

/// One attempt: probe two fresh ports, spawn `rhypedb-server` on them, and
/// wait for its binary-protocol port to accept connections.
async fn try_spawn_rhypedb(binary: &Path, data_dir: &Path, schema_path: &Path) -> Result<RhypeDbGuard, String> {
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

/// Binds an ephemeral loopback port for this crate's own axum app or the
/// GitHub stub, retrying (see `MAX_PORT_ATTEMPTS`) on the off chance the OS
/// hands back a port another racing bind grabs first. `bind(..:0)` asks the
/// OS to assign whatever's free, so unlike `free_port` there's no
/// probe-then-release gap — this is defense in depth, not the fix for the
/// observed flake (that was `free_port`, in `try_spawn_rhypedb`).
async fn bind_ephemeral_loopback() -> tokio::net::TcpListener {
    let mut last_err = None;
    for attempt in 1..=MAX_PORT_ATTEMPTS {
        match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => return listener,
            Err(e) => {
                last_err = Some(e);
                if attempt < MAX_PORT_ATTEMPTS {
                    tokio::time::sleep(PORT_RETRY_BACKOFF).await;
                }
            }
        }
    }
    panic!("binding an ephemeral loopback port failed after {MAX_PORT_ATTEMPTS} attempts: {last_err:?}");
}

/// Caps how many `Stack`s run at once. `cargo test`'s default parallelism
/// (one thread per CPU) would otherwise try to start as many
/// `rhypedb-server` subprocesses simultaneously as there are tests, each
/// probing two "free" ports — the more of those racing at the same instant,
/// the more often one loses the race in `try_spawn_rhypedb`. Limiting
/// concurrency keeps that rare instead of routine; the retry loop above is
/// the other half of the fix, for whenever it still happens.
const MAX_CONCURRENT_STACKS: usize = 4;

fn stack_semaphore() -> &'static tokio::sync::Semaphore {
    static SEMAPHORE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_STACKS))
}

/// Starts the in-process Pimble server on an OS-assigned loopback port,
/// retrying (see `MAX_PORT_ATTEMPTS`) if `start` fails for any reason —
/// `addr: "127.0.0.1:0"` means the OS hands out whatever's free, so this is
/// defense in depth (see `bind_ephemeral_loopback`), not the fix for the
/// observed flake.
async fn start_pimble_server(creds_dir: &Path, replicas_dir: &Path, service_token: &str) -> pimble_server::PimbleServer {
    let mut last_err = None;
    for attempt in 1..=MAX_PORT_ATTEMPTS {
        let mut pimble_server = pimble_server::PimbleServer::with_config(pimble_server::ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            auth_token: Some(service_token.to_string()),
            credentials_path: Some(creds_dir.join("credentials.json")),
            replicas_dir: Some(replicas_dir.to_path_buf()),
            // Forward-compatible with fields agent B is concurrently adding
            // to `ServerConfig` (JWT verification, origin allowlist): this
            // service only exercises the pre-existing static-token mode.
            ..Default::default()
        });
        match pimble_server.start().await {
            Ok(()) => return pimble_server,
            Err(e) => {
                last_err = Some(e);
                if attempt < MAX_PORT_ATTEMPTS {
                    tokio::time::sleep(PORT_RETRY_BACKOFF).await;
                }
            }
        }
    }
    panic!("in-process Pimble server failed to start after {MAX_PORT_ATTEMPTS} attempts: {last_err:?}");
}

/// Everything a test needs: the two real backing servers plus the cloud
/// service's own axum app, all bound to ephemeral loopback ports. Dropping
/// it tears the stack down (kills the rhypedb-server subprocess, aborts the
/// cloud app's serve task; the in-process Pimble server and its temp dirs go
/// with the struct).
pub struct Stack {
    /// Held for this `Stack`'s whole lifetime, released on drop — see
    /// `MAX_CONCURRENT_STACKS`.
    _stack_permit: tokio::sync::SemaphorePermit<'static>,
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
    spawn_stack_inner(None, None, None).await
}

pub async fn spawn_stack_with_releases_base_url(releases_base_url: Option<String>) -> Option<Stack> {
    spawn_stack_inner(releases_base_url, None, None).await
}

/// Like [`spawn_stack`], but with `mailer` in place of the `LogMailer`
/// every other test gets — for covering how a mail-provider failure is
/// handled (`pimble_cloud::build_state_with_mailer`).
pub async fn spawn_stack_with_mailer(mailer: std::sync::Arc<dyn pimble_cloud::mail::Mailer>) -> Option<Stack> {
    spawn_stack_inner(None, Some(mailer), None).await
}

/// Like [`spawn_stack`], but with a lowered "members plus invitations per
/// store" cap (docs/SHARING_CONTRACT.md's fifty), so a test can reach it
/// without fifty accounts.
pub async fn spawn_stack_with_members_cap(cap: usize) -> Option<Stack> {
    spawn_stack_inner(None, None, Some(cap)).await
}

async fn spawn_stack_inner(
    releases_base_url: Option<String>,
    mailer_override: Option<std::sync::Arc<dyn pimble_cloud::mail::Mailer>>,
    members_cap: Option<usize>,
) -> Option<Stack> {
    // The ONLY skip condition: no rhypedb-server binary exists anywhere we
    // know to look. A binary that exists but fails to start (even after
    // `spawn_rhypedb`'s retries) is a real test failure, not a skip.
    let binary = find_rhypedb_server_binary()?;

    // Acquired before starting anything below, held for the whole `Stack`'s
    // lifetime: caps how many of these run at once (`MAX_CONCURRENT_STACKS`).
    let stack_permit = stack_semaphore().acquire().await.expect("stack semaphore is never closed");

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
    let pimble_server = start_pimble_server(pimble_creds_dir.path(), pimble_replicas_dir.path(), &service_token).await;
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
        // Fixed and non-secret, like `dev_seed` above: deterministic within
        // a test run so a `/kdf` test can assert the decoy salt is stable.
        kdf_decoy_secret: Some("test-kdf-decoy-secret".to_string()),
        // The real cap is fifty (docs/SHARING_CONTRACT.md); reaching it in a
        // test would mean fifty signups or invitations per run. The field
        // exists exactly so a test can lower it — see
        // `store_member_cap_refuses_one_too_many`.
        max_members_per_store: members_cap.unwrap_or(pimble_cloud::config::DEFAULT_MAX_MEMBERS_PER_STORE),
    };

    let app_state = match mailer_override {
        Some(mailer) => pimble_cloud::build_state_with_mailer(config, mailer).await.expect("build_state_with_mailer"),
        None => pimble_cloud::build_state(config).await.expect("build_state"),
    };
    let router = pimble_cloud::router_from_state(app_state.clone());
    let listener = bind_ephemeral_loopback().await;
    let addr = listener.local_addr().unwrap();
    let cloud_server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    Some(Stack {
        _stack_permit: stack_permit,
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

/// Builds a real, valid `POST /signup` body using `pimble_crypto` exactly as
/// a real client would (docs/CRYPTO_CONTRACT.md "Primitives" and "Client-
/// derived login"): derive `auth_key`/`kek` from `password`, generate a
/// fresh account keypair, wrap it under the password KEK and under a fresh
/// recovery code's KEK. Returns the JSON body (so a test can inspect what
/// was sent, e.g. the public keys), the plaintext `AccountKeys` (so a test
/// can later unwrap a returned blob or sign an envelope as this user), and
/// the plaintext recovery code (so a recovery-flow test can derive the same
/// recovery KEK a real client would from the salt `GET /recover/{token}`
/// serves).
fn build_signup_body(email: &str, password: &str) -> (Value, AccountKeys, String) {
    let kdf = KdfParams::generate();
    let password_keys = pimble_crypto::derive_password_keys(password, &kdf).expect("derive_password_keys");
    let auth_key = pimble_crypto::encode_auth_key(&password_keys.auth_key);

    let account_keys = AccountKeys::generate();
    let public_keys = account_keys.public_keys();
    let account_key_blob = pimble_crypto::wrap_account_keys(&account_keys, &password_keys.kek).expect("wrap_account_keys");

    let recovery_salt = KdfParams::generate().salt;
    let recovery_code = pimble_crypto::generate_recovery_code();
    let recovery_params = KdfParams { salt: recovery_salt.clone(), m_cost: kdf.m_cost, t_cost: kdf.t_cost, p_cost: kdf.p_cost };
    let recovery_kek = pimble_crypto::derive_recovery_kek(&recovery_code, &recovery_params).expect("derive_recovery_kek");
    let recovery_key_blob = pimble_crypto::wrap_account_keys(&account_keys, &recovery_kek).expect("wrap recovery blob");

    let body = json!({
        "email": email,
        "auth_key": auth_key,
        "kdf": kdf,
        "public_keys": public_keys,
        "account_key_blob": account_key_blob,
        "recovery_salt": recovery_salt,
        "recovery_key_blob": recovery_key_blob,
    });
    (body, account_keys, recovery_code)
}

/// `GET /api/v1/kdf?email=` — real params for a known email, a
/// (deterministic, same-shaped) decoy otherwise.
async fn fetch_kdf(stack: &Stack, email: &str) -> KdfParams {
    let resp = stack.http.get(format!("{}/kdf", stack.base_url)).query(&[("email", email)]).send().await.unwrap();
    assert_eq!(resp.status(), 200, "GET /kdf should always answer 200");
    resp.json().await.unwrap()
}

/// `POST /signup` (Phase 1b: 202, no session — see docs/CLOUD_CONTRACT.md
/// "Phase 1b: email verification"), with real key material
/// ([`build_signup_body`]). Returns the signup response body and the
/// account keys generated for it — use [`signup`] when the keys aren't
/// needed.
async fn signup_with_material(stack: &Stack, email: &str, password: &str) -> (Value, AccountKeys) {
    let (request_body, account_keys, _recovery_code) = build_signup_body(email, password);
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&request_body).send().await.unwrap();
    assert_eq!(resp.status(), 202, "signup for {email} should be accepted (verification pending)");
    (resp.json().await.unwrap(), account_keys)
}

async fn signup(stack: &Stack, email: &str, password: &str) -> Value {
    signup_with_material(stack, email, password).await.0
}

fn first_cookie_pair(resp: &reqwest::Response) -> String {
    resp.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_string()
}

/// `POST /login`: fetches the account's real KDF params first (as a real
/// client does) and derives `auth_key` from them, so this only succeeds
/// against a `password` that actually matches what `signup`/`signup_with_material`
/// used for `email`.
async fn login(stack: &Stack, email: &str, password: &str) -> (Value, String) {
    let kdf = fetch_kdf(stack, email).await;
    let auth_key = pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(password, &kdf).unwrap().auth_key);
    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": auth_key })).send().await.unwrap();
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

// ── Recovery test helpers (docs/CRYPTO_CONTRACT.md "Phase 2a-2") ─────────

/// Like [`signup_with_material`], but also returns the plaintext recovery
/// code — needed to derive the recovery KEK in a recovery-flow test the
/// same way a real client does from the salt `GET /recover/{token}` serves.
async fn signup_with_material_and_recovery_code(stack: &Stack, email: &str, password: &str) -> (Value, AccountKeys, String) {
    let (request_body, account_keys, recovery_code) = build_signup_body(email, password);
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&request_body).send().await.unwrap();
    assert_eq!(resp.status(), 202, "signup for {email} should be accepted (verification pending)");
    (resp.json().await.unwrap(), account_keys, recovery_code)
}

/// Pulls the recovery token last sent (via `LogMailer`) to `email` — same
/// mechanism as `extract_verify_token`, but the link is
/// `/app/recover?token=...`.
fn extract_recovery_token(stack: &Stack, email: &str) -> String {
    let log_mailer = stack.app_state.mailer.as_log_mailer().expect("test stack must run with LogMailer (no RESEND_API_KEY)");
    let body = log_mailer.last_message(email).unwrap_or_else(|| panic!("no recovery email was sent to {email}"));
    let link = pimble_cloud::mail::first_url(&body).unwrap_or_else(|| panic!("recovery email body had no link:\n{body}"));
    link.rsplit("token=").next().unwrap().to_string()
}

/// `GET /api/v1/recover/{token}`, then derives the recovery KEK from
/// `recovery_code` and the served salt/costs and unwraps the account keys —
/// exactly what `/app/recover` does client-side. Returns the raw response
/// body too, for a test that wants to inspect it further.
async fn fetch_and_unwrap_recovery_material(stack: &Stack, token: &str, recovery_code: &str) -> (Value, AccountKeys) {
    let resp = stack.http.get(format!("{}/recover/{token}", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200, "GET /recover/{{token}} should succeed for a fresh, unconsumed token");
    let body: Value = resp.json().await.unwrap();
    let recovery_key_blob: AccountKeyBlob = serde_json::from_value(body["recovery_key_blob"].clone()).unwrap();
    let recovery_params = KdfParams {
        salt: body["recovery_salt"].as_str().unwrap().to_string(),
        m_cost: body["recovery_kdf"]["m_cost"].as_u64().unwrap() as u32,
        t_cost: body["recovery_kdf"]["t_cost"].as_u64().unwrap() as u32,
        p_cost: body["recovery_kdf"]["p_cost"].as_u64().unwrap() as u32,
    };
    let recovery_kek = pimble_crypto::derive_recovery_kek(recovery_code, &recovery_params).expect("derive_recovery_kek");
    let unwrapped = pimble_crypto::unwrap_account_keys(&recovery_key_blob, &recovery_kek).expect("unwrap_account_keys");
    (body, unwrapped)
}

/// Derives brand new password material and a brand new recovery code for
/// `account_keys` (what `unwrap_account_keys` just produced) — a client
/// always rotates the code on recovery — then `POST`s
/// `/recover/{token}/complete`. Returns the response body.
async fn complete_recovery_with_new_material(stack: &Stack, token: &str, account_keys: &AccountKeys, new_password: &str) -> Value {
    let new_kdf = KdfParams::generate();
    let new_password_keys = pimble_crypto::derive_password_keys(new_password, &new_kdf).expect("derive_password_keys");
    let new_auth_key = pimble_crypto::encode_auth_key(&new_password_keys.auth_key);
    let new_account_key_blob = pimble_crypto::wrap_account_keys(account_keys, &new_password_keys.kek).expect("wrap_account_keys");

    let new_recovery_salt = KdfParams::generate().salt;
    let new_recovery_code = pimble_crypto::generate_recovery_code();
    let new_recovery_params =
        KdfParams { salt: new_recovery_salt.clone(), m_cost: new_kdf.m_cost, t_cost: new_kdf.t_cost, p_cost: new_kdf.p_cost };
    let new_recovery_kek = pimble_crypto::derive_recovery_kek(&new_recovery_code, &new_recovery_params).expect("derive_recovery_kek");
    let new_recovery_key_blob = pimble_crypto::wrap_account_keys(account_keys, &new_recovery_kek).expect("wrap recovery blob");

    let resp = stack
        .http
        .post(format!("{}/recover/{token}/complete", stack.base_url))
        .json(&json!({
            "auth_key": new_auth_key,
            "kdf": new_kdf,
            "account_key_blob": new_account_key_blob,
            "recovery_salt": new_recovery_salt,
            "recovery_key_blob": new_recovery_key_blob,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "recover complete should succeed");
    resp.json().await.unwrap()
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
    login(&stack, email, password).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_and_unknown_email_are_both_401() {
    let stack = skip_without_rhypedb!();
    signup(&stack, "carol@example.com", "the right password").await;

    // The server only ever sees `auth_key`, an opaque string as far as it's
    // concerned — any wrong value (not necessarily one really derived from
    // "the wrong password") must fail the same way.
    let resp = stack
        .http
        .post(format!("{}/login", stack.base_url))
        .json(&json!({ "email": "carol@example.com", "auth_key": "not-the-real-auth-key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = stack
        .http
        .post(format!("{}/login", stack.base_url))
        .json(&json!({ "email": "nobody-signed-up-with-this@example.com", "auth_key": "anything" }))
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
    let (second_body, _, _) = build_signup_body("dupe@example.com", "second password!");
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&second_body).send().await.unwrap();
    assert_eq!(resp.status(), 202);

    // Verify (with the ORIGINAL password — a re-send never changes it), then
    // a duplicate signup is a real 409.
    verify_then_login(&stack, "dupe@example.com", "first password!").await;
    let (third_body, _, _) = build_signup_body("dupe@example.com", "third password!");
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&third_body).send().await.unwrap();
    assert_eq!(resp.status(), 409);

    // Case-insensitivity: the same address differently-cased also conflicts.
    let (fourth_body, _, _) = build_signup_body("DUPE@example.com", "fourth password!");
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&fourth_body).send().await.unwrap();
    assert_eq!(resp.status(), 409);
}

// ── Phase 1b: email verification ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn login_before_verification_is_403_email_unverified() {
    let stack = skip_without_rhypedb!();
    let email = "oscar@example.com";
    let password = "oscar's unverified password";
    signup(&stack, email, password).await;

    // The correct auth_key (derived from the account's real kdf params, as
    // a real client would) still gets 403 email_unverified, not a session.
    let kdf = fetch_kdf(&stack, email).await;
    let auth_key = pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(password, &kdf).unwrap().auth_key);
    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": auth_key })).send().await.unwrap();
    assert_eq!(resp.status(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "email_unverified");

    // The password check still runs first: a wrong auth_key is 401, not
    // "email_unverified" (which would leak that the email exists yet the
    // password was never even checked).
    let resp = stack
        .http
        .post(format!("{}/login", stack.base_url))
        .json(&json!({ "email": email, "auth_key": "not oscar's auth_key" }))
        .send()
        .await
        .unwrap();
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

    let (resend_body, _, _) = build_signup_body(email, "ignored on a resend");
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&resend_body).send().await.unwrap();
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
    // An "old" token planted directly (bypassing signup, which would also
    // record a send against the rate limiter and make the resend below
    // rate-limited — see `resend_immediately_after_signup_is_rate_limited_too`
    // for that case) so this test's resend is this address's first send.
    let user = stack.app_state.db.create_user(email, "irrelevant-hash", &pimble_cloud::db::NewUserKeyMaterial::placeholder_for_tests()).await.unwrap().unwrap();
    let (old_token, old_hash) = pimble_cloud::auth::new_verify_token();
    let future_ms = chrono::Utc::now().timestamp_millis() + 24 * 60 * 60 * 1000;
    stack.app_state.db.set_verify_token(user.rid, &old_hash, future_ms).await.unwrap();

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
    // Created directly (bypassing signup, which is itself a send — see
    // `resend_immediately_after_signup_is_rate_limited_too`) so this test's
    // first resend is a genuinely fresh send for this address.
    stack.app_state.db.create_user(email, "irrelevant-hash", &pimble_cloud::db::NewUserKeyMaterial::placeholder_for_tests()).await.unwrap().unwrap();

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

#[tokio::test(flavor = "multi_thread")]
async fn resend_immediately_after_signup_is_rate_limited_too() {
    let stack = skip_without_rhypedb!();
    let email = "quentin@example.com";
    signup(&stack, email, "quentin's password!!").await;
    let token_from_signup = extract_verify_token(&stack, email);

    // Signup's own send must count against the same limiter a plain resend
    // uses — a resend moments later is silently rate-limited (202, no new
    // mail), not treated as the address's first send.
    let resp = stack.http.post(format!("{}/resend-verification", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token_after_resend = extract_verify_token(&stack, email);
    assert_eq!(token_from_signup, token_after_resend, "signup's send should count, so an immediate resend must not issue a new token");

    // The signup mail's original token is still the one that verifies.
    let resp = visit_verify_link(&stack, &token_from_signup).await;
    assert!(redirect_location(&resp).contains("verified=1"));
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
    let listener = bind_ephemeral_loopback().await;
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

// ── Phase 1b: mail-provider failure ────────────────────────────────────────

fn assert_mail_failed_response(body: &Value) {
    assert_eq!(body["error"], "mail_failed");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("Send it again"), "unexpected message: {message}");
    assert!(!message.contains("Resend") && !message.contains("422"), "provider detail leaked into the response: {message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn signup_maps_a_mail_failure_to_502_without_leaking_the_provider_response() {
    let stack = match spawn_stack_with_mailer(std::sync::Arc::new(FailingMailer)).await {
        Some(s) => s,
        None => {
            eprintln!("SKIP: no rhypedb-server binary found");
            return;
        }
    };
    let (signup_body, _, _) = build_signup_body("bounces@example.com", "whatever password");
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&signup_body).send().await.unwrap();
    assert_eq!(resp.status(), 502);
    assert_mail_failed_response(&resp.json().await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn resend_verification_maps_a_mail_failure_to_502_without_leaking_the_provider_response() {
    let stack = match spawn_stack_with_mailer(std::sync::Arc::new(FailingMailer)).await {
        Some(s) => s,
        None => {
            eprintln!("SKIP: no rhypedb-server binary found");
            return;
        }
    };
    // The user must exist (and be unverified) for resend to attempt a send
    // at all; signup itself already fails to mail, so create the row
    // directly rather than through the API.
    let user = stack.app_state.db.create_user("bounces2@example.com", "irrelevant-hash", &pimble_cloud::db::NewUserKeyMaterial::placeholder_for_tests()).await.unwrap().unwrap();
    let _ = user;

    let resp = stack
        .http
        .post(format!("{}/resend-verification", stack.base_url))
        .json(&json!({ "email": "bounces2@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
    assert_mail_failed_response(&resp.json().await.unwrap());
}

// ── Phase 2a: end-to-end encryption (docs/CRYPTO_CONTRACT.md) ─────────────

#[tokio::test(flavor = "multi_thread")]
async fn kdf_returns_real_params_for_a_known_email_and_a_stable_decoy_otherwise() {
    let stack = skip_without_rhypedb!();
    let email = "kdf-test@example.com";
    signup(&stack, email, "kdf test password!!").await;

    let real = fetch_kdf(&stack, email).await;
    assert!(!real.salt.is_empty());
    assert_eq!(real.m_cost, pimble_crypto::KDF_M_COST_KIB);
    assert_eq!(real.t_cost, pimble_crypto::KDF_T_COST);
    assert_eq!(real.p_cost, pimble_crypto::KDF_P_COST);

    // An unknown email gets the same shape, deterministically (not a fresh
    // random salt on every call), and it must not collide with a real one.
    let decoy1 = fetch_kdf(&stack, "nobody-has-signed-up@example.com").await;
    let decoy2 = fetch_kdf(&stack, "nobody-has-signed-up@example.com").await;
    assert_eq!(decoy1, decoy2, "the decoy salt must be stable for the same unknown email");
    assert_ne!(decoy1.salt, real.salt);
}

/// A legacy pre-Phase-2a account (docs/CRYPTO_CONTRACT.md's migration note:
/// Joe's own pre-encryption test accounts in production) has no key
/// material at all — every field simply absent, not an empty string. `GET
/// /kdf` must not 500 on it (the live bug reported), and both `/kdf` and
/// `/login` must treat it exactly as if the email were unknown.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_keyless_user_gets_the_decoy_kdf_and_a_401_on_login() {
    let stack = skip_without_rhypedb!();
    let email = "legacy@example.com";
    stack.app_state.db.create_legacy_user_without_keys_for_tests(email, "irrelevant-hash").await.unwrap();

    // Must not 500, and must be byte-for-byte the same decoy an unknown
    // email gets — not merely "some params came back".
    let legacy_kdf = fetch_kdf(&stack, email).await;
    let expected_decoy = pimble_cloud::kdf_decoy::decoy_kdf_params(b"test-kdf-decoy-secret", &email.to_lowercase());
    assert_eq!(legacy_kdf, expected_decoy);

    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": "anything" })).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}

/// The startup migration (`build_state`/`build_state_with_mailer`) must
/// delete a legacy row, its sessions, and its grants, so a redeploy cleans
/// production. Builds a second `AppState` against the same backing servers
/// — what a restart does — after seeding one directly.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_keyless_users_and_their_sessions_and_grants_are_deleted_at_startup() {
    let stack = skip_without_rhypedb!();
    let email = "legacy2@example.com";
    let legacy = stack.app_state.db.create_legacy_user_without_keys_for_tests(email, "irrelevant-hash").await.unwrap();

    let (_token, token_hash) = pimble_cloud::auth::new_session_token();
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + 60 * 60 * 1000;
    stack.app_state.db.create_session(legacy.rid, &token_hash, expires_at_ms).await.unwrap();

    let hosted =
        stack.app_state.db.create_hosted_store(&uuid::Uuid::new_v4().to_string(), "Legacy's Store", "legacy.pimble", "plain", false).await.unwrap();
    stack.app_state.db.create_grant(legacy.rid, hosted.rid, &hosted.store_id, "owner").await.unwrap();

    // A fresh `build_state` against the exact same rhypedb-server and
    // Pimble server — simulating the next deploy's restart.
    let second_state = pimble_cloud::build_state(stack.app_state.config.clone()).await.expect("build_state (second run, simulating a restart)");

    assert!(second_state.db.find_user_by_email(email).await.unwrap().is_none(), "the legacy user should be deleted at startup");
    assert!(second_state.db.find_session_by_token_hash(&token_hash).await.unwrap().is_none(), "its session should be deleted too");
    assert!(second_state.db.find_grant(legacy.rid, hosted.rid).await.unwrap().is_none(), "its grant should be deleted too");
}

/// A live crash: `delete_legacy_users_without_keys` used to deserialize
/// every row into a typed `UserRow` to decide what to delete, so a row from
/// before email verification existed — missing `verified`,
/// `verify_token_hash`, `verify_expires_at`, and every Phase 2a field, not
/// just the ones Phase 2a added — failed that parse and took the whole
/// startup down with it (`Error: 500 Internal Server Error internal:
/// User.verified: expected a Bool, got None`, crash-looping in production).
/// The cleanup must delete a row this bare by id, never deserializing it,
/// and normal lookups (`find_user_by_email`) must tolerate it too rather
/// than 500ing before the cleanup gets a chance to run.
#[tokio::test(flavor = "multi_thread")]
async fn phase1_shape_user_does_not_crash_startup_or_lookups() {
    let stack = skip_without_rhypedb!();
    let email = "phase1-shape@example.com";
    let user_rid = stack.app_state.db.create_phase1_shape_user_for_tests(email, "irrelevant-hash").await.unwrap();
    assert!(stack.app_state.db.user_exists_for_tests(user_rid).await.unwrap());

    // A normal lookup (e.g. what `GET /kdf`, `POST /login` do) must not 500
    // on it, even before any cleanup runs.
    assert!(stack.app_state.db.find_user_by_email(email).await.unwrap().is_some(), "a bare phase-1-shape row must still be readable");

    // The startup cleanup, run again as a fresh `build_state` would on a
    // restart, must delete it by id without ever deserializing it.
    let second_state = pimble_cloud::build_state(stack.app_state.config.clone()).await.expect("build_state must not fail on a phase-1-shape row");
    assert!(!second_state.db.user_exists_for_tests(user_rid).await.unwrap(), "a phase-1-shape row must be deleted at startup like any other keyless row");
}

#[tokio::test(flavor = "multi_thread")]
async fn me_keys_round_trips_through_real_crypto_and_never_returns_recovery() {
    let stack = skip_without_rhypedb!();
    let email = "keyholder@example.com";
    let password = "keyholder password!!";
    // `build_signup_body` directly (not `signup_with_material`, which only
    // hands back the 202 response — the `{status, email}` body, not the
    // request) so this test can compare against the public keys actually
    // sent at signup.
    let (request_body, account_keys, _recovery_code) = build_signup_body(email, password);
    let resp = stack.http.post(format!("{}/signup", stack.base_url)).json(&request_body).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let (_login_body, cookie) = verify_then_login(&stack, email, password).await;

    let resp = stack.http.get(format!("{}/me/keys", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["public_keys"], request_body["public_keys"]);
    assert!(body.get("recovery_key_blob").is_none(), "me/keys must never return the recovery blob");
    assert!(body.get("recovery_salt").is_none(), "me/keys must never return the recovery salt");

    // The returned `account_key_blob` really unwraps, with the
    // password-derived KEK, back to the exact keys generated at signup —
    // not just "some JSON came back that looks right".
    let kdf = fetch_kdf(&stack, email).await;
    let password_keys = pimble_crypto::derive_password_keys(password, &kdf).unwrap();
    let blob: pimble_crypto::AccountKeyBlob = serde_json::from_value(body["account_key_blob"].clone()).unwrap();
    let unwrapped = pimble_crypto::unwrap_account_keys(&blob, &password_keys.kek).unwrap();
    assert_eq!(unwrapped.encryption_secret, account_keys.encryption_secret);
    assert_eq!(unwrapped.signing_secret, account_keys.signing_secret);

    // No session: 401.
    let resp = stack.http.get(format!("{}/me/keys", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn users_lookup_finds_verified_users_and_404s_otherwise() {
    let stack = skip_without_rhypedb!();
    // A fresh, freshly-signed-in caller per lookup: `/users/lookup` is
    // rate-limited per caller (see `users_lookup_is_rate_limited_per_caller`),
    // and these local round trips are faster than that interval, so reusing
    // one caller across several lookups in the same test would spuriously
    // 429 the later ones.

    // An unverified target has no usable keys to share to yet: 404.
    signup(&stack, "lookup-unverified@example.com", "target password!!").await;
    let (_, looker1_cookie) = signup_verify_login(&stack, "lookup-caller1@example.com", "looker password!!").await;
    let resp = stack
        .http
        .get(format!("{}/users/lookup", stack.base_url))
        .header("Cookie", &looker1_cookie)
        .query(&[("email", "lookup-unverified@example.com")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // A verified target: 200 with its id and public keys.
    signup_verify_login(&stack, "lookup-target@example.com", "target password!!").await;
    let (_, looker2_cookie) = signup_verify_login(&stack, "lookup-caller2@example.com", "looker password!!").await;
    let resp = stack
        .http
        .get(format!("{}/users/lookup", stack.base_url))
        .header("Cookie", &looker2_cookie)
        .query(&[("email", "lookup-target@example.com")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["id"].as_str().is_some());
    assert!(body["public_keys"]["encryption"].as_str().is_some());

    // Unknown address: 404 too (same shape as unverified — no enumeration
    // signal either way).
    let (_, looker3_cookie) = signup_verify_login(&stack, "lookup-caller3@example.com", "looker password!!").await;
    let resp = stack
        .http
        .get(format!("{}/users/lookup", stack.base_url))
        .header("Cookie", &looker3_cookie)
        .query(&[("email", "no-such-address@example.com")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // No session: 401 (fails in the session extractor, before the rate
    // limiter is ever consulted — no fresh caller needed here).
    let resp = stack.http.get(format!("{}/users/lookup", stack.base_url)).query(&[("email", "lookup-target@example.com")]).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn users_lookup_is_rate_limited_per_caller() {
    let stack = skip_without_rhypedb!();
    let (_body, looker_cookie) = signup_verify_login(&stack, "rapid-looker@example.com", "looker password!!").await;
    signup_verify_login(&stack, "rapid-target@example.com", "target password!!").await;

    let resp = stack
        .http
        .get(format!("{}/users/lookup", stack.base_url))
        .header("Cookie", &looker_cookie)
        .query(&[("email", "rapid-target@example.com")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Immediately again: too fast, rate-limited.
    let resp = stack
        .http
        .get(format!("{}/users/lookup", stack.base_url))
        .header("Cookie", &looker_cookie)
        .query(&[("email", "rapid-target@example.com")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_store_with_chosen_kind_and_id() {
    let stack = skip_without_rhypedb!();
    let (_body, cookie) = signup_verify_login(&stack, "vault-owner@example.com", "vault owner password!!").await;

    let chosen_id = uuid::Uuid::new_v4().to_string();
    let resp = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Encrypted Notes", "kind": "vault", "store_id": chosen_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let store: Value = resp.json().await.unwrap();
    assert_eq!(store["kind"], "vault");
    assert_eq!(store["store_id"], chosen_id);

    // A plain store (the default `kind`) still works and is reported as such.
    let resp = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Plain Notes" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["kind"], "plain");
}

/// Wraps `key` to `recipient_keys` from `sender`, signing as `sender`, for
/// `store_id` — what a real client does before `PUT /stores/{id}/keys`.
fn wrap_store_key(key: &pimble_crypto::SymmetricKey, key_id: pimble_crypto::KeyId, store_id: &str, sender: &AccountKeys, recipient: &pimble_crypto::AccountPublicKeys) -> pimble_crypto::KeyEnvelope {
    pimble_crypto::wrap_key(key, key_id, recipient, sender, &format!("store:{store_id}")).expect("wrap_key")
}

#[tokio::test(flavor = "multi_thread")]
async fn store_keys_put_and_get_round_trip_with_signature_verification() {
    let stack = skip_without_rhypedb!();
    let email = "vault-owner2@example.com";
    let password = "owner password!!";
    let (_signup_body, owner_keys) = signup_with_material(&stack, email, password).await;
    let (login_body, cookie) = verify_then_login(&stack, email, password).await;
    let owner_id = login_body["user"]["id"].as_str().unwrap().to_string();
    let owner_public = owner_keys.public_keys();

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Vault Store", "kind": "vault" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let store_key = pimble_crypto::SymmetricKey::generate();
    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&store_key, key_id, &store_id, &owner_keys, &owner_public);

    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "envelopes": [ { "user_id": owner_id, "key_id": key_id.to_string(), "envelope": envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);

    let resp = stack.http.get(format!("{}/stores/{store_id}/keys", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let envelopes = body["envelopes"].as_array().unwrap();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0]["key_id"], key_id.to_string());

    // The returned envelope really does unwrap to the same store key.
    let returned_envelope: pimble_crypto::KeyEnvelope = serde_json::from_value(envelopes[0]["envelope"].clone()).unwrap();
    let unwrapped = pimble_crypto::unwrap_key(&returned_envelope, &owner_keys, &owner_public.signing).unwrap();
    assert_eq!(unwrapped.0, store_key.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn store_keys_put_rejects_a_tampered_signature() {
    let stack = skip_without_rhypedb!();
    let email = "vault-owner3@example.com";
    let password = "owner password!!";
    let (_signup_body, owner_keys) = signup_with_material(&stack, email, password).await;
    let (login_body, cookie) = verify_then_login(&stack, email, password).await;
    let owner_id = login_body["user"]["id"].as_str().unwrap().to_string();
    let owner_public = owner_keys.public_keys();

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Vault Store", "kind": "vault" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let store_key = pimble_crypto::SymmetricKey::generate();
    let key_id = uuid::Uuid::new_v4();
    let mut envelope = wrap_store_key(&store_key, key_id, &store_id, &owner_keys, &owner_public);
    let mut sig_bytes = URL_SAFE_NO_PAD.decode(&envelope.signature).unwrap();
    sig_bytes[0] ^= 0xff;
    envelope.signature = URL_SAFE_NO_PAD.encode(&sig_bytes);

    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "envelopes": [ { "user_id": owner_id, "key_id": key_id.to_string(), "envelope": envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "a tampered envelope signature must not verify");
}

#[tokio::test(flavor = "multi_thread")]
async fn store_keys_ownership_rules() {
    let stack = skip_without_rhypedb!();
    let (owner_login, owner_cookie) = signup_verify_login(&stack, "vault-owner4@example.com", "owner password!!").await;
    let owner_id = owner_login["user"]["id"].as_str().unwrap().to_string();
    let (_editor_signup, editor_keys) = signup_with_material(&stack, "vault-editor4@example.com", "editor password!!").await;
    let (editor_login, editor_cookie) = verify_then_login(&stack, "vault-editor4@example.com", "editor password!!").await;
    let editor_id = editor_login["user"]["id"].as_str().unwrap().to_string();
    let editor_public = editor_keys.public_keys();
    let (reader_login, reader_cookie) = signup_verify_login(&stack, "vault-reader4@example.com", "reader password!!").await;
    let reader_id = reader_login["user"]["id"].as_str().unwrap().to_string();

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "name": "Vault Store", "kind": "vault" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "email": "vault-editor4@example.com", "role": "editor" }))
        .send()
        .await
        .unwrap();
    stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "email": "vault-reader4@example.com", "role": "reader" }))
        .send()
        .await
        .unwrap();

    // A reader cannot set keys, even their own.
    let store_key = pimble_crypto::SymmetricKey::generate();
    let envelope_for_editor = wrap_store_key(&store_key, uuid::Uuid::new_v4(), &store_id, &editor_keys, &editor_public);
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &reader_cookie)
        .json(&json!({ "envelopes": [ { "user_id": reader_id, "key_id": uuid::Uuid::new_v4().to_string(), "envelope": envelope_for_editor } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // An editor CAN set their own key (signed by themselves)...
    let editor_key_id = uuid::Uuid::new_v4();
    let self_envelope = wrap_store_key(&store_key, editor_key_id, &store_id, &editor_keys, &editor_public);
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &editor_cookie)
        .json(&json!({ "envelopes": [ { "user_id": editor_id, "key_id": editor_key_id.to_string(), "envelope": self_envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // ...but not on behalf of another member.
    let other_envelope = wrap_store_key(&store_key, uuid::Uuid::new_v4(), &store_id, &editor_keys, &editor_public);
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &editor_cookie)
        .json(&json!({ "envelopes": [ { "user_id": owner_id, "key_id": uuid::Uuid::new_v4().to_string(), "envelope": other_envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

// ── Phase 2a-2: account recovery, password change, new recovery code ─────

#[tokio::test(flavor = "multi_thread")]
async fn recover_full_flow_rotates_keys_with_real_crypto_and_invalidates_old_password() {
    let stack = skip_without_rhypedb!();
    let email = "recoverable@example.com";
    let old_password = "old password for recovery!!";
    let (_signup_body, old_account_keys, recovery_code) = signup_with_material_and_recovery_code(&stack, email, old_password).await;
    let (_login_body, cookie) = verify_then_login(&stack, email, old_password).await;

    // A real hosted store + key envelope, so "store keys still unwrap"
    // (docs/CRYPTO_CONTRACT.md) is checked against real crypto, not assumed.
    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Recoverable Store", "kind": "vault" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();
    let me: Value = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &cookie).send().await.unwrap().json().await.unwrap();
    let user_id = me["id"].as_str().unwrap().to_string();
    let old_public_keys = old_account_keys.public_keys();
    let store_key = pimble_crypto::SymmetricKey::generate();
    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&store_key, key_id, &store_id, &old_account_keys, &old_public_keys);
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "envelopes": [ { "user_id": user_id, "key_id": key_id.to_string(), "envelope": envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);

    let (recover_body, unwrapped_keys) = fetch_and_unwrap_recovery_material(&stack, &token, &recovery_code).await;
    assert_eq!(recover_body["email"], email);
    assert_eq!(unwrapped_keys.encryption_secret, old_account_keys.encryption_secret);
    assert_eq!(unwrapped_keys.signing_secret, old_account_keys.signing_secret);

    let new_password = "new password after recovery!!";
    let complete_body = complete_recovery_with_new_material(&stack, &token, &unwrapped_keys, new_password).await;
    assert_eq!(complete_body["user"]["email"], email);

    // The old password no longer works...
    let old_kdf = fetch_kdf(&stack, email).await;
    let old_style_auth_key = pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(old_password, &old_kdf).unwrap().auth_key);
    let resp =
        stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": old_style_auth_key })).send().await.unwrap();
    assert_eq!(resp.status(), 401, "the old password must no longer work after recovery");

    // ...the new one does.
    let (_new_login_body, new_cookie) = login(&stack, email, new_password).await;

    // The session active before recovery is gone.
    let resp = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 401, "the session active before recovery must be gone");

    // Store keys still unwrap (public keys never changed).
    let resp = stack.http.get(format!("{}/stores/{store_id}/keys", stack.base_url)).header("Cookie", &new_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let keys_body: Value = resp.json().await.unwrap();
    let returned_envelope: pimble_crypto::KeyEnvelope = serde_json::from_value(keys_body["envelopes"][0]["envelope"].clone()).unwrap();
    let unwrapped_store_key = pimble_crypto::unwrap_key(&returned_envelope, &unwrapped_keys, &old_public_keys.signing).unwrap();
    assert_eq!(unwrapped_store_key.0, store_key.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_start_moments_after_signup_is_not_blocked_by_verifications_rate_limit() {
    let stack = skip_without_rhypedb!();
    let email = "recover-soon-after-signup@example.com";
    let password = "recover soon after signup!!";
    // Signup's own send records against `resend_rate_limit`; verifying and
    // logging in are both local and fast, so `recover/start` below lands
    // well within the same minute — recovery must not be blocked by it.
    signup(&stack, email, password).await;
    verify_then_login(&stack, email, password).await;

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);

    // A real recovery mail actually went out — proven by the token it
    // issued actually working, not just the always-202 response (which
    // looks identical whether or not `recover/start` silently skipped
    // sending because it thought the address was already rate-limited by
    // the unrelated verification send).
    let token = extract_recovery_token(&stack, email);
    let resp = stack.http.get(format!("{}/recover/{token}", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200, "recover/start must send even moments after signup's own verification email");
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_expired_token_is_invalid() {
    let stack = skip_without_rhypedb!();
    let email = "recover-expired@example.com";
    let password = "expiring password!!";
    signup_verify_login(&stack, email, password).await;

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);

    // Force the already-issued token into the past (no way to wait out the
    // real 1h TTL in a test) by re-setting the SAME hash with an expired
    // timestamp, reaching the DB directly through `stack.app_state`.
    let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
    let past_ms = chrono::Utc::now().timestamp_millis() - 1_000;
    stack.app_state.db.set_recovery_token(user.rid, &pimble_cloud::auth::hash_recovery_token(&token), past_ms).await.unwrap();

    let resp = stack.http.get(format!("{}/recover/{token}", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"], "recovery_invalid");
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_token_cannot_be_reused_after_complete() {
    let stack = skip_without_rhypedb!();
    let email = "recover-reuse@example.com";
    let password = "reuse password!!";
    let (_body, _account_keys, recovery_code) = signup_with_material_and_recovery_code(&stack, email, password).await;
    verify_then_login(&stack, email, password).await;

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);
    let (_recover_body, unwrapped_keys) = fetch_and_unwrap_recovery_material(&stack, &token, &recovery_code).await;
    complete_recovery_with_new_material(&stack, &token, &unwrapped_keys, "post-recovery password!!").await;

    // The same token must not verify, or complete, a second time.
    let resp = stack.http.get(format!("{}/recover/{token}", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 404, "a consumed token must not verify again");

    let junk_blob = pimble_crypto::AccountKeyBlob { v: 1, nonce: "x".to_string(), ciphertext: "y".to_string() };
    let resp = stack
        .http
        .post(format!("{}/recover/{token}/complete", stack.base_url))
        .json(&json!({
            "auth_key": "irrelevant",
            "kdf": pimble_crypto::KdfParams::generate(),
            "account_key_blob": junk_blob.clone(),
            "recovery_salt": "z",
            "recovery_key_blob": junk_blob,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "a consumed token must not be completable again");
}

#[tokio::test(flavor = "multi_thread")]
async fn change_password_refuses_wrong_current_key_and_accepts_right_one() {
    let stack = skip_without_rhypedb!();
    let email = "change-pw@example.com";
    let old_password = "original password!!";
    let (_body, account_keys) = signup_with_material(&stack, email, old_password).await;
    let (_login_body, cookie) = verify_then_login(&stack, email, old_password).await;

    let new_kdf = KdfParams::generate();
    let new_password = "changed password!!";
    let new_password_keys = pimble_crypto::derive_password_keys(new_password, &new_kdf).unwrap();
    let new_auth_key = pimble_crypto::encode_auth_key(&new_password_keys.auth_key);
    let new_account_key_blob = pimble_crypto::wrap_account_keys(&account_keys, &new_password_keys.kek).unwrap();

    // A wrong current_auth_key is refused.
    let resp = stack
        .http
        .post(format!("{}/me/password", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({
            "current_auth_key": "not the real current key",
            "auth_key": new_auth_key,
            "kdf": new_kdf,
            "account_key_blob": new_account_key_blob,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // The right one succeeds.
    let old_kdf = fetch_kdf(&stack, email).await;
    let current_auth_key = pimble_crypto::encode_auth_key(&pimble_crypto::derive_password_keys(old_password, &old_kdf).unwrap().auth_key);
    let resp = stack
        .http
        .post(format!("{}/me/password", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({
            "current_auth_key": current_auth_key,
            "auth_key": new_auth_key,
            "kdf": new_kdf,
            "account_key_blob": new_account_key_blob,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // The session used to change it is kept (docs/CRYPTO_CONTRACT.md:
    // "keeps other sessions").
    let resp = stack.http.get(format!("{}/me", stack.base_url)).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // The old password no longer works; the new one does.
    let resp =
        stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": current_auth_key })).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    login(&stack, email, new_password).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn change_recovery_code_replaces_material_for_future_recovery() {
    let stack = skip_without_rhypedb!();
    let email = "change-recovery@example.com";
    let password = "recovery code owner password!!";
    let (_body, account_keys) = signup_with_material(&stack, email, password).await;
    let (_login_body, cookie) = verify_then_login(&stack, email, password).await;

    let new_recovery_salt = KdfParams::generate().salt;
    let new_recovery_code = pimble_crypto::generate_recovery_code();
    let new_recovery_params = KdfParams {
        salt: new_recovery_salt.clone(),
        m_cost: pimble_crypto::KDF_M_COST_KIB,
        t_cost: pimble_crypto::KDF_T_COST,
        p_cost: pimble_crypto::KDF_P_COST,
    };
    let new_recovery_kek = pimble_crypto::derive_recovery_kek(&new_recovery_code, &new_recovery_params).unwrap();
    let new_recovery_key_blob = pimble_crypto::wrap_account_keys(&account_keys, &new_recovery_kek).unwrap();

    let resp = stack
        .http
        .post(format!("{}/me/recovery-code", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "recovery_salt": new_recovery_salt, "recovery_key_blob": new_recovery_key_blob }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // The new code really recovers the account now.
    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);
    let (_recover_body, unwrapped_keys) = fetch_and_unwrap_recovery_material(&stack, &token, &new_recovery_code).await;
    assert_eq!(unwrapped_keys.encryption_secret, account_keys.encryption_secret);
    assert_eq!(unwrapped_keys.signing_secret, account_keys.signing_secret);
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_delete_account_removes_everything_and_deletes_solely_owned_stores() {
    let stack = skip_without_rhypedb!();
    let email = "delete-me@example.com";
    let password = "delete me password!!";
    signup_verify_login(&stack, email, password).await;
    let (_login_body, cookie) = login(&stack, email, password).await;

    let store: Value = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Solely Owned" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let resp = stack.http.post(format!("{}/recover/start", stack.base_url)).json(&json!({ "email": email })).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let token = extract_recovery_token(&stack, email);

    let resp = stack.http.post(format!("{}/recover/{token}/delete-account", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // The account is gone: login fails like any unknown address, and the
    // store it solely owned is marked deleted (checked directly since no
    // session can reach it once the account is gone).
    let resp = stack.http.post(format!("{}/login", stack.base_url)).json(&json!({ "email": email, "auth_key": "anything" })).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    assert!(stack.app_state.db.find_user_by_email(email).await.unwrap().is_none());
    let hosted = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(hosted.deleted, "a solely-owned store must be marked deleted when its owner's account is deleted");

    // The token itself is dead too (its user row is gone).
    let resp = stack.http.get(format!("{}/recover/{token}", stack.base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Phase 2b: sharing (docs/SHARING_CONTRACT.md, "Accounts service") ──────

/// The plain-text body of the last mail sent to `to`, through `LogMailer`.
fn last_mail(stack: &Stack, to: &str) -> String {
    let log_mailer = stack.app_state.mailer.as_log_mailer().expect("test stack must run with LogMailer (no RESEND_API_KEY)");
    log_mailer.last_message(to).unwrap_or_else(|| panic!("no mail was sent to {to}"))
}

/// How many mails `to` has been sent — what a "the second one was rate
/// limited" assertion needs, since two sharing mails have identical bodies.
fn mail_count(stack: &Stack, to: &str) -> usize {
    let log_mailer = stack.app_state.mailer.as_log_mailer().expect("test stack must run with LogMailer (no RESEND_API_KEY)");
    log_mailer.message_count(to)
}

async fn create_store_with_body(stack: &Stack, cookie: &str, body: Value) -> Value {
    let resp = stack.http.post(format!("{}/stores", stack.base_url)).header("Cookie", cookie).json(&body).send().await.unwrap();
    assert_eq!(resp.status(), 200, "creating the store should succeed: {:?}", resp.text().await);
    resp.json().await.unwrap()
}

async fn put_member(stack: &Stack, cookie: &str, store_id: &str, email: &str, role: &str) -> reqwest::Response {
    stack
        .http
        .put(format!("{}/stores/{store_id}/members", stack.base_url))
        .header("Cookie", cookie)
        .json(&json!({ "email": email, "role": role }))
        .send()
        .await
        .unwrap()
}

async fn list_members(stack: &Stack, cookie: &str, store_id: &str) -> Vec<Value> {
    let resp = stack.http.get(format!("{}/stores/{store_id}/members", stack.base_url)).header("Cookie", cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.json::<Value>().await.unwrap().as_array().unwrap().clone()
}

async fn list_stores(stack: &Stack, cookie: &str) -> Vec<Value> {
    let resp = stack.http.get(format!("{}/stores", stack.base_url)).header("Cookie", cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.json::<Value>().await.unwrap().as_array().unwrap().clone()
}

fn find_member<'a>(members: &'a [Value], email: &str) -> &'a Value {
    members.iter().find(|m| m["email"] == email).unwrap_or_else(|| panic!("no member row for {email} in {members:?}"))
}

fn assert_says_encrypted_and_waits_for_the_sender(body: &str, inviter_email: &str) {
    assert!(body.contains("end-to-end encrypted"), "the mail must say the notes are end-to-end encrypted:\n{body}");
    assert!(
        body.contains(&format!("{inviter_email}'s Pimble has been online")),
        "the mail must say the share opens once the sender's Pimble has been online:\n{body}"
    );
}

/// An address with no account at all: `PUT members` invites it, and the mail
/// points at signup with the address prefilled. Signing up with that address
/// and verifying it turns the invitation into a grant and deletes it.
#[tokio::test(flavor = "multi_thread")]
async fn inviting_an_unknown_address_and_claiming_it_at_verification() {
    let stack = skip_without_rhypedb!();
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "share-owner@example.com", "share owner password!!").await;
    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Recipes", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let invitee = "not-a-user-yet@example.com";
    let resp = put_member(&stack, &owner_cookie, &store_id, invitee, "editor").await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "invited");
    assert_eq!(body["role"], "editor");
    assert_eq!(body["has_key"], false);
    assert!(body["user_id"].is_null(), "an invitation names no account: {body}");

    // The invitation mail carries a signup link with the address prefilled,
    // and says what a recipient needs to know about the encryption.
    let mail = last_mail(&stack, invitee);
    let link = pimble_cloud::mail::first_url(&mail).expect("the invitation mail has a link");
    assert!(link.contains("/app/signup?email="), "unexpected invitation link: {link}");
    assert!(link.contains("not-a-user-yet%40example.com"), "the address must be url-encoded into the link: {link}");
    assert!(mail.contains("Recipes"), "the mail names the share: {mail}");
    assert_says_encrypted_and_waits_for_the_sender(&mail, "share-owner@example.com");

    // The owner sees it listed as a pending invitation.
    let members = list_members(&stack, &owner_cookie, &store_id).await;
    assert_eq!(find_member(&members, invitee)["status"], "invited");

    // Signing up and verifying claims it.
    let (_body, invitee_cookie) = signup_verify_login(&stack, invitee, "invitee password!!").await;
    let stores = list_stores(&stack, &invitee_cookie).await;
    let row = stores.iter().find(|s| s["store_id"] == store_id.as_str()).expect("the claimed share should be listed");
    assert_eq!(row["role"], "editor");
    assert_eq!(row["share"], true);
    assert_eq!(row["shared_by"], "share-owner@example.com");

    // ...and the invitation is gone, replaced by a real grant.
    let hosted = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    assert!(stack.app_state.db.find_invitation(hosted.rid, invitee).await.unwrap().is_none(), "a claimed invitation must be deleted");
    let members = list_members(&stack, &owner_cookie, &store_id).await;
    assert_eq!(find_member(&members, invitee)["status"], "active");
}

/// The other order: the account was already verified when the invitation was
/// written (an owner inviting while a signup raced, or an invitation that
/// failed to claim at verification). Every login claims what is standing.
#[tokio::test(flavor = "multi_thread")]
async fn login_claims_an_invitation_for_an_already_verified_address() {
    let stack = skip_without_rhypedb!();
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "claim-owner@example.com", "claim owner password!!").await;
    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Claimed At Login", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    let hosted = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();

    let email = "claims-at-login@example.com";
    let password = "claims at login password!!";
    signup_verify_login(&stack, email, password).await;
    let user = stack.app_state.db.find_user_by_email(email).await.unwrap().unwrap();
    let owner = stack.app_state.db.find_user_by_email("claim-owner@example.com").await.unwrap().unwrap();

    // Written straight to the database: the shape a claim race leaves behind,
    // which no endpoint would produce for a verified address.
    stack.app_state.db.create_invitation(hosted.rid, &store_id, email, "reader", owner.rid).await.unwrap();
    assert!(stack.app_state.db.find_grant(user.rid, hosted.rid).await.unwrap().is_none());

    let (_body, cookie) = login(&stack, email, password).await;
    let grant = stack.app_state.db.find_grant(user.rid, hosted.rid).await.unwrap().expect("login should have claimed the invitation");
    assert_eq!(grant.role, "reader");
    assert!(stack.app_state.db.find_invitation(hosted.rid, email).await.unwrap().is_none(), "the claimed invitation must be deleted");
    assert!(list_stores(&stack, &cookie).await.iter().any(|s| s["store_id"] == store_id.as_str()));
}

/// An address that already has a verified account is granted outright: the
/// answer says `active`, carries the public keys the owner needs to wrap the
/// store key, and the mail points at the app rather than at signup.
#[tokio::test(flavor = "multi_thread")]
async fn inviting_a_known_verified_address_grants_at_once() {
    let stack = skip_without_rhypedb!();
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "known-owner@example.com", "known owner password!!").await;
    let member_email = "known-member@example.com";
    let (_signup_body, member_keys) = signup_with_material(&stack, member_email, "known member password!!").await;
    let (member_login, _member_cookie) = verify_then_login(&stack, member_email, "known member password!!").await;
    let member_id = member_login["user"]["id"].as_str().unwrap().to_string();

    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Known Share", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let resp = put_member(&stack, &owner_cookie, &store_id, member_email, "editor").await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "active");
    assert_eq!(body["user_id"], member_id);
    assert_eq!(body["has_key"], false, "nobody has wrapped the key to them yet");
    assert_eq!(body["public_keys"]["encryption"], member_keys.public_keys().encryption);
    assert_eq!(body["public_keys"]["signing"], member_keys.public_keys().signing);

    let mail = last_mail(&stack, member_email);
    let link = pimble_cloud::mail::first_url(&mail).expect("the shared-with-you mail has a link");
    assert!(link.ends_with("/app/"), "an existing account is pointed at the app, not signup: {link}");
    assert!(mail.contains("Known Share"));
    assert_says_encrypted_and_waits_for_the_sender(&mail, "known-owner@example.com");
}

/// `has_key` is false until somebody wraps the store key to that member, and
/// true once an envelope exists — what the owner's key sweep reads.
#[tokio::test(flavor = "multi_thread")]
async fn member_has_key_flips_once_an_envelope_is_uploaded() {
    let stack = skip_without_rhypedb!();
    let owner_email = "haskey-owner@example.com";
    let (_signup, owner_keys) = signup_with_material(&stack, owner_email, "haskey owner password!!").await;
    let (_owner_login, owner_cookie) = verify_then_login(&stack, owner_email, "haskey owner password!!").await;
    let member_email = "haskey-member@example.com";
    let (_member_signup, member_keys) = signup_with_material(&stack, member_email, "haskey member password!!").await;
    let (member_login, _member_cookie) = verify_then_login(&stack, member_email, "haskey member password!!").await;
    let member_id = member_login["user"]["id"].as_str().unwrap().to_string();

    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Key Sweep", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, member_email, "editor").await.status(), 200);

    let members = list_members(&stack, &owner_cookie, &store_id).await;
    assert_eq!(find_member(&members, member_email)["has_key"], false);

    // The owner wraps the share key to the member and uploads the envelope,
    // exactly as `ShareLink`'s key sweep does.
    let store_key = pimble_crypto::SymmetricKey::generate();
    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&store_key, key_id, &store_id, &owner_keys, &member_keys.public_keys());
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "envelopes": [ { "user_id": member_id, "key_id": key_id.to_string(), "envelope": envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);

    let members = list_members(&stack, &owner_cookie, &store_id).await;
    assert_eq!(find_member(&members, member_email)["has_key"], true);
    let _ = owner_email;
}

/// `GET /stores/{id}/keys` names the store's owners as signers, so a
/// recipient can accept an envelope signed by the sharer rather than only by
/// themselves.
#[tokio::test(flavor = "multi_thread")]
async fn store_keys_lists_the_owners_as_signers() {
    let stack = skip_without_rhypedb!();
    let owner_email = "signer-owner@example.com";
    let (_signup, owner_keys) = signup_with_material(&stack, owner_email, "signer owner password!!").await;
    let (owner_login, owner_cookie) = verify_then_login(&stack, owner_email, "signer owner password!!").await;
    let owner_id = owner_login["user"]["id"].as_str().unwrap().to_string();
    let member_email = "signer-member@example.com";
    let (_member_login, member_cookie) = signup_verify_login(&stack, member_email, "signer member password!!").await;

    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Signed Share", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, member_email, "reader").await.status(), 200);

    // The member, who has no envelope yet, still learns whose signature to
    // trust.
    let resp = stack.http.get(format!("{}/stores/{store_id}/keys", stack.base_url)).header("Cookie", &member_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["envelopes"].as_array().unwrap().len(), 0);
    let signers = body["signers"].as_array().unwrap();
    assert_eq!(signers.len(), 1, "only the owner signs: {signers:?}");
    assert_eq!(signers[0]["user_id"], owner_id);
    assert_eq!(signers[0]["email"], owner_email);
    assert_eq!(signers[0]["public_signing_key"], owner_keys.public_keys().signing);
}

/// A member may leave a share themselves (and loses their envelopes doing
/// it); a member who is not an owner may not remove anyone else.
#[tokio::test(flavor = "multi_thread")]
async fn a_member_removes_themself_but_not_each_other() {
    let stack = skip_without_rhypedb!();
    let owner_email = "leave-owner@example.com";
    let (_signup, owner_keys) = signup_with_material(&stack, owner_email, "leave owner password!!").await;
    let (_owner_login, owner_cookie) = verify_then_login(&stack, owner_email, "leave owner password!!").await;
    let leaver_email = "leaver@example.com";
    let (_leaver_signup, leaver_keys) = signup_with_material(&stack, leaver_email, "leaver password!!").await;
    let (leaver_login, leaver_cookie) = verify_then_login(&stack, leaver_email, "leaver password!!").await;
    let leaver_id = leaver_login["user"]["id"].as_str().unwrap().to_string();
    let (bystander_login, bystander_cookie) = signup_verify_login(&stack, "bystander@example.com", "bystander password!!").await;
    let bystander_id = bystander_login["user"]["id"].as_str().unwrap().to_string();

    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Leavable", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, leaver_email, "editor").await.status(), 200);
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, "bystander@example.com", "reader").await.status(), 200);

    // The leaver holds an envelope for the store.
    let store_key = pimble_crypto::SymmetricKey::generate();
    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&store_key, key_id, &store_id, &owner_keys, &leaver_keys.public_keys());
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "envelopes": [ { "user_id": leaver_id, "key_id": key_id.to_string(), "envelope": envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // A reader cannot remove another member.
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/members/{leaver_id}", stack.base_url))
        .header("Cookie", &bystander_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // ...but can remove themself.
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/members/{bystander_id}", stack.base_url))
        .header("Cookie", &bystander_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // And so can the editor, whose envelopes go with the grant.
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/members/{leaver_id}", stack.base_url))
        .header("Cookie", &leaver_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let hosted = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();
    let leaver = stack.app_state.db.find_user_by_email(leaver_email).await.unwrap().unwrap();
    assert!(stack.app_state.db.find_grant(leaver.rid, hosted.rid).await.unwrap().is_none());
    assert!(
        stack.app_state.db.key_grants_for_user_and_store(leaver.rid, hosted.rid).await.unwrap().is_empty(),
        "removing a member must delete their envelopes for the store"
    );
    let members = list_members(&stack, &owner_cookie, &store_id).await;
    assert_eq!(members.len(), 1, "only the owner is left: {members:?}");
}

/// An invitation can be withdrawn, and withdrawing one that is not there is
/// not an error.
#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_can_be_withdrawn() {
    let stack = skip_without_rhypedb!();
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "withdraw-owner@example.com", "withdraw owner password!!").await;
    let (_other_body, other_cookie) = signup_verify_login(&stack, "withdraw-other@example.com", "withdraw other password!!").await;
    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Withdrawable", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let invitee = "withdrawn@example.com";
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, invitee, "reader").await.status(), 200);
    assert_eq!(list_members(&stack, &owner_cookie, &store_id).await.len(), 2);

    // The address is url-encoded in the path, as a client must send it.
    let encoded: String = url::form_urlencoded::byte_serialize(invitee.as_bytes()).collect();

    // Only an owner may withdraw one.
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/invitations/{encoded}", stack.base_url))
        .header("Cookie", &other_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/invitations/{encoded}", stack.base_url))
        .header("Cookie", &owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(list_members(&stack, &owner_cookie, &store_id).await.len(), 1, "the invitation is gone");

    // Again: still 200, nothing to do.
    let resp = stack
        .http
        .delete(format!("{}/stores/{store_id}/invitations/{encoded}", stack.base_url))
        .header("Cookie", &owner_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

/// Owner is not a role one can be invited into: an address nobody has proven
/// belongs to a real account must not be able to delete the store.
#[tokio::test(flavor = "multi_thread")]
async fn inviting_an_owner_without_an_account_is_refused() {
    let stack = skip_without_rhypedb!();
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "no-owner-invite@example.com", "no owner invite password!!").await;
    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Owners Only", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let resp = put_member(&stack, &owner_cookie, &store_id, "stranger@example.com", "owner").await;
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"], "bad_request");

    // An editor invitation to the same address is fine.
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, "stranger@example.com", "editor").await.status(), 200);
}

/// `share: true` is only meaningful for an encrypted store.
#[tokio::test(flavor = "multi_thread")]
async fn a_share_must_be_a_vault_store() {
    let stack = skip_without_rhypedb!();
    let (_body, cookie) = signup_verify_login(&stack, "plain-share@example.com", "plain share password!!").await;

    let resp = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "Plain Share", "share": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"], "bad_request");

    // A plain store without `share` is still fine, and lists as share: false.
    let store = create_store_with_body(&stack, &cookie, json!({ "name": "Plain Notes" })).await;
    assert_eq!(store["share"], false);
    assert!(store["shared_by"].is_null());
}

/// A store's name goes out in sharing mails and into every member's app, so
/// it is bounded and kept to one line (the same rule `mail::one_line`
/// applies) rather than stored however it arrived.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_name_is_bounded_and_kept_to_one_line() {
    let stack = skip_without_rhypedb!();
    let (_body, cookie) = signup_verify_login(&stack, "naming@example.com", "naming password!!").await;

    let resp = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": "x".repeat(201) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "a name past the limit is refused");
    assert_eq!(resp.json::<Value>().await.unwrap()["error"], "bad_request");

    // Exactly at the limit is fine; control characters never reach the row.
    let store = create_store_with_body(&stack, &cookie, json!({ "name": "y".repeat(200) })).await;
    assert_eq!(store["name"], "y".repeat(200));
    let store = create_store_with_body(&stack, &cookie, json!({ "name": "  Recipes \r\n and  Notes " })).await;
    assert_eq!(store["name"], "Recipes and Notes");

    // Whitespace alone is still an empty name.
    let resp = stack
        .http
        .post(format!("{}/stores", stack.base_url))
        .header("Cookie", &cookie)
        .json(&json!({ "name": " \r\n\t " }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// `GET /stores` says which rows are shares and, for a row the caller does
/// not own, who shared it.
#[tokio::test(flavor = "multi_thread")]
async fn list_stores_reports_share_and_shared_by() {
    let stack = skip_without_rhypedb!();
    let owner_email = "listing-owner@example.com";
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, owner_email, "listing owner password!!").await;
    let (_member_body, member_cookie) = signup_verify_login(&stack, "listing-member@example.com", "listing member password!!").await;

    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Listed Share", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, "listing-member@example.com", "editor").await.status(), 200);

    let owner_row = list_stores(&stack, &owner_cookie).await.into_iter().find(|s| s["store_id"] == store_id.as_str()).unwrap();
    assert_eq!(owner_row["share"], true);
    assert!(owner_row["shared_by"].is_null(), "an owner's own store is shared by nobody: {owner_row}");

    let member_row = list_stores(&stack, &member_cookie).await.into_iter().find(|s| s["store_id"] == store_id.as_str()).unwrap();
    assert_eq!(member_row["share"], true);
    assert_eq!(member_row["role"], "editor");
    assert_eq!(member_row["shared_by"], owner_email);
}

/// One sharing mail per (store, address) per minute: a second `PUT members`
/// still upserts, and still answers, but sends nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_invitation_within_the_minute_sends_no_second_mail() {
    let stack = skip_without_rhypedb!();
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "quiet-owner@example.com", "quiet owner password!!").await;
    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Quiet Share", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();

    let invitee = "invited-twice@example.com";
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, invitee, "reader").await.status(), 200);
    assert_eq!(mail_count(&stack, invitee), 1);

    // Immediately again, with a different role: accepted, applied, silent.
    let resp = put_member(&stack, &owner_cookie, &store_id, invitee, "editor").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["role"], "editor");
    assert_eq!(mail_count(&stack, invitee), 1, "a repeat within the minute must send no second mail");

    let members = list_members(&stack, &owner_cookie, &store_id).await;
    assert_eq!(find_member(&members, invitee)["role"], "editor", "the repeat still changed the role");
    assert_eq!(members.len(), 2, "a repeat must not create a second invitation");
}

/// A store holds so many members plus invitations and no more. The cap is
/// fifty in production; this stack lowers it so the test is cheap.
#[tokio::test(flavor = "multi_thread")]
async fn store_member_cap_refuses_one_too_many() {
    let stack = match spawn_stack_with_members_cap(2).await {
        Some(s) => s,
        None => {
            eprintln!("SKIP: no rhypedb-server binary found");
            return;
        }
    };
    let (_owner_body, owner_cookie) = signup_verify_login(&stack, "capped-owner@example.com", "capped owner password!!").await;
    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Capped", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();

    // The owner is one of the two; one invitation fills the store.
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, "capped-one@example.com", "reader").await.status(), 200);

    let resp = put_member(&stack, &owner_cookie, &store_id, "capped-two@example.com", "reader").await;
    assert_eq!(resp.status(), 409);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"], "conflict");

    // Changing an existing invitation's role adds nobody, so it still works.
    assert_eq!(put_member(&stack, &owner_cookie, &store_id, "capped-one@example.com", "editor").await.status(), 200);
}

/// Deleting a store takes its invitations and every member's envelopes with
/// it, and answers 200 even when the hosted server refuses to delete the
/// vault directory (the row is deleted regardless; the store is unreachable
/// either way).
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_store_removes_invitations_and_key_grants() {
    let stack = skip_without_rhypedb!();
    let owner_email = "deleting-owner@example.com";
    let (_signup, owner_keys) = signup_with_material(&stack, owner_email, "deleting owner password!!").await;
    let (owner_login, owner_cookie) = verify_then_login(&stack, owner_email, "deleting owner password!!").await;
    let owner_id = owner_login["user"]["id"].as_str().unwrap().to_string();

    let store = create_store_with_body(&stack, &owner_cookie, json!({ "name": "Doomed", "kind": "vault", "share": true })).await;
    let store_id = store["store_id"].as_str().unwrap().to_string();
    let hosted = stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap();

    assert_eq!(put_member(&stack, &owner_cookie, &store_id, "doomed-invitee@example.com", "reader").await.status(), 200);
    let key_id = uuid::Uuid::new_v4();
    let envelope = wrap_store_key(&pimble_crypto::SymmetricKey::generate(), key_id, &store_id, &owner_keys, &owner_keys.public_keys());
    let resp = stack
        .http
        .put(format!("{}/stores/{store_id}/keys", stack.base_url))
        .header("Cookie", &owner_cookie)
        .json(&json!({ "envelopes": [ { "user_id": owner_id, "key_id": key_id.to_string(), "envelope": envelope } ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let owner = stack.app_state.db.find_user_by_email(owner_email).await.unwrap().unwrap();
    assert!(!stack.app_state.db.key_grants_for_user_and_store(owner.rid, hosted.rid).await.unwrap().is_empty());

    // 200 whether or not the hosted server's `deleteVaultStore` succeeds.
    let resp = stack.http.delete(format!("{}/stores/{store_id}", stack.base_url)).header("Cookie", &owner_cookie).send().await.unwrap();
    assert_eq!(resp.status(), 200, "a hosted-side failure must not fail the request: {:?}", resp.text().await);

    assert!(stack.app_state.db.find_hosted_store(&store_id).await.unwrap().unwrap().deleted);
    assert!(stack.app_state.db.invitations_for_store(hosted.rid).await.unwrap().is_empty(), "a deleted store keeps no invitations");
    assert!(stack.app_state.db.key_grants_for_user_and_store(owner.rid, hosted.rid).await.unwrap().is_empty(), "a deleted store keeps no envelopes");
    assert!(stack.app_state.db.grants_for_store(hosted.rid).await.unwrap().is_empty());
}

/// A `HostedStore` row written before Phase 2b has no `share` field at all
/// (RhypeDB never backfills one). Reading it must not fail — it is simply not
/// a share, which is what every store from before this phase really is.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_row_without_share_reads_as_not_shared() {
    let stack = skip_without_rhypedb!();
    let (_body, cookie) = signup_verify_login(&stack, "old-row@example.com", "old row password!!").await;
    let user = stack.app_state.db.find_user_by_email("old-row@example.com").await.unwrap().unwrap();

    let store_id = uuid::Uuid::new_v4().to_string();
    let hosted = stack
        .app_state
        .db
        .create_hosted_store_without_share_for_tests(&store_id, "From Before Sharing", "old.pimble", "vault")
        .await
        .unwrap();
    assert!(!hosted.share, "an absent `share` reads as false, not an error");
    stack.app_state.db.create_grant(user.rid, hosted.rid, &store_id, "owner").await.unwrap();

    let row = list_stores(&stack, &cookie).await.into_iter().find(|s| s["store_id"] == store_id.as_str()).expect("the old row should list");
    assert_eq!(row["share"], false);
    assert_eq!(row["kind"], "vault");
}
