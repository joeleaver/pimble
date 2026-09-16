use pimble_cloud::config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
