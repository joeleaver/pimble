use pimble_cloud::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    let addr = format!("0.0.0.0:{}", config.port);

    let router = pimble_cloud::build_router(config).await?;

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("pimble-cloud listening on {}", addr);
    axum::serve(listener, router).await?;
    Ok(())
}
