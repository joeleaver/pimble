//! The browser's diagnostics go through this service
//! (docs/DIAGNOSTICS_CONTRACT.md section 5): passed on with the send key it
//! holds and the signed-in account named, and refused when the page names
//! another account than its cookie's.

use super::*;

/// What the stand-in diagnostics server was sent: the path's last part, the
/// `Authorization` and `X-Pimble-Account` headers, and the body.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String, String)>>>;

/// A stand-in diagnostics server that records what it is sent, and an
/// accounts service with a send key passing on to it, sharing `stack`'s
/// database. Answers that service's `/api/v1` base.
async fn with_diagnostics_server(stack: &Stack) -> (String, Seen) {
    let seen: Seen = Default::default();
    let app = {
        let seen = seen.clone();
        axum::Router::new().route(
            "/v1/:what",
            axum::routing::post(move |axum::extract::Path(what): axum::extract::Path<String>, headers: axum::http::HeaderMap, body: String| {
                let seen = seen.clone();
                async move {
                    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                    seen.lock().unwrap().push((what.clone(), h("authorization"), h("x-pimble-account"), body));
                    if what == "reports" { r#"{"report":"ABCD2345"}"#.to_string() } else { r#"{"accepted":1,"rejected":0}"#.to_string() }
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut config = stack.app_state.config.clone();
    config.diagnostics_url = format!("http://{addr}");
    config.diagnostics_key = Some("send-key".to_string());
    let state = pimble_cloud::build_state(config).await.unwrap();
    let router = pimble_cloud::router_from_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cloud = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{cloud}/api/v1"), seen)
}

/// Passed on with the send key and the signed-in account named; nothing
/// without a session; 503 while no key is set.
#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_from_a_page_go_on_with_the_key_and_the_account() {
    let stack = skip_without_rhypedb!();
    let (_, cookie) = signup_verify_login(&stack, "diag@example.com", "correct horse battery").await;

    // No key: refused, and the page keeps what it queued.
    let resp = stack.http.post(format!("{}/diagnostics/records", stack.base_url)).header("Cookie", &cookie).body("{}\n").send().await.unwrap();
    assert_eq!(resp.status(), 503);

    let (base, seen) = with_diagnostics_server(&stack).await;
    let resp = stack.http.post(format!("{base}/diagnostics/records")).body("{\"kind\":\"log\"}\n").send().await.unwrap();
    assert_eq!(resp.status(), 401, "no session, nothing goes");
    let resp = stack.http.post(format!("{base}/diagnostics/records")).header("Cookie", &cookie).body("{\"kind\":\"log\"}\n").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let resp = stack.http.post(format!("{base}/diagnostics/reports")).header("Cookie", &cookie).header("content-type", "application/json").body("{\"note\":\"x\"}").send().await.unwrap();
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["report"], "ABCD2345");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    let user = stack.app_state.db.find_user_by_email("diag@example.com").await.unwrap().unwrap();
    for (what, auth, account, _) in &seen {
        assert_eq!(auth, "Bearer send-key", "{what} carries the service's key");
        assert_eq!(account, &user.user_uuid, "{what} names the account");
    }
    assert_eq!(seen[0].3, "{\"kind\":\"log\"}\n");
}

/// Tab A queued records as alice; in tab B alice signed out and bob in, so
/// the cookie tab A sends with is bob's. Naming alice, the send is refused
/// with `account_changed` and nothing reaches the diagnostics server; named
/// as bob, it goes.
#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_named_for_another_account_than_the_cookies_are_refused() {
    let stack = skip_without_rhypedb!();
    signup_verify_login(&stack, "alice.diag@example.com", "alice diagnostics pw").await;
    let (_, bob_cookie) = signup_verify_login(&stack, "bob.diag@example.com", "bob diagnostics pw").await;
    let alice = stack.app_state.db.find_user_by_email("alice.diag@example.com").await.unwrap().unwrap();
    let bob = stack.app_state.db.find_user_by_email("bob.diag@example.com").await.unwrap().unwrap();
    let (base, seen) = with_diagnostics_server(&stack).await;

    for what in ["records", "reports"] {
        let resp = stack
            .http
            .post(format!("{base}/diagnostics/{what}"))
            .header("Cookie", &bob_cookie)
            .header("X-Pimble-Account", &alice.user_uuid)
            .body("{\"kind\":\"log\",\"account\":\"alice\"}\n")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409, "{what}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "account_changed", "{what}");
        let message = body["message"].as_str().unwrap();
        assert!(message.starts_with(char::is_uppercase) && message.ends_with('.'), "a whole sentence: {message:?}");
    }
    assert!(seen.lock().unwrap().is_empty(), "nothing of alice's went on as bob");

    let resp = stack.http.post(format!("{base}/diagnostics/records")).header("Cookie", &bob_cookie).header("X-Pimble-Account", &bob.user_uuid).body("{\"kind\":\"log\"}\n").send().await.unwrap();
    assert_eq!(resp.status(), 200, "named as the cookie's account, it goes");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].2, bob.user_uuid);
}
