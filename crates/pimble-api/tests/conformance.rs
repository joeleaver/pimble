//! The conformance suite (`pimble_api::conformance`) against the RPC backend
//! over a real in-process server, and against a router in front of it,
//! which must answer exactly as the backend it routes to. The browser's
//! vault runs the same suite in `web/`.

use pimble_api::conformance::{self, Fixture};
use pimble_api::rpc::RpcBackend;
use pimble_api::{Router, StoreCapabilities};
use pimble_client::PimbleClient;
use pimble_server::{PimbleServer, ServerConfig};

struct Stack {
    _server: PimbleServer,
    dir: tempfile::TempDir,
    url: String,
}

async fn stack() -> Stack {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        credentials_path: Some(dir.path().join("credentials.json")),
        keystore_path: Some(dir.path().join("keys.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    Stack { _server: server, dir, url }
}

async fn fixture(stack: &Stack, name: &str) -> Fixture {
    let service = PimbleClient::connect(&stack.url).await.expect("service connects");
    let (store, root) = service.create_store(stack.dir.path().join(format!("{name}.pimble")), name).await.expect("store");
    Fixture { store, root }
}

#[tokio::test]
async fn the_rpc_backend_conforms() {
    let stack = stack().await;
    let fx = fixture(&stack, "Conformance").await;
    let backend = RpcBackend::over(PimbleClient::connect(&stack.url).await.expect("connects"));
    let passed = conformance::run(&backend, fx, StoreCapabilities::SERVER).await;
    assert_eq!(passed.len(), 12, "every check ran: {passed:?}");
}

/// A router adds nothing of its own to a store it has no word about: the
/// same answers, the same errors.
#[tokio::test]
async fn a_router_in_front_answers_as_its_backend() {
    let stack = stack().await;
    let fx = fixture(&stack, "Routed").await;
    let router = Router::new(RpcBackend::over(PimbleClient::connect(&stack.url).await.expect("connects")));
    let passed = conformance::run(&router, fx, StoreCapabilities::SERVER).await;
    assert_eq!(passed.len(), 12, "every check ran: {passed:?}");
}
