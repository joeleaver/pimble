use pimble_cloud::config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `pimble-cloud --generate-vapid-key`: a new VAPID key pair for web push
    // (docs/NOTIFICATIONS_CONTRACT.md, wave 2), printed for the deployment's
    // secrets; nothing is started.
    if std::env::args().any(|a| a == "--generate-vapid-key") {
        let key = pimble_cloud::webpush::VapidKey::generate();
        println!("PIMBLE_VAPID_PRIVATE_KEY={}", key.private());
        println!("# public key (what browsers subscribe with): {}", key.public());
        return Ok(());
    }
    // `RUST_LOG` still overrides this; started without it, the service
    // otherwise logs nothing at all — not even the LogMailer warning or the
    // verification link, since `fmt::init()`'s own default filter admits no
    // directives when the env var is unset.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::from_env();
    let addr = format!("0.0.0.0:{}", config.port);

    let router = pimble_cloud::build_router(config).await?;

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("pimble-cloud listening on {}", addr);
    axum::serve(listener, router).await?;
    Ok(())
}
