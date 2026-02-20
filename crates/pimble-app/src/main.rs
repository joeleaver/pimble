//! Pimble Desktop Application
//!
//! Entry point for the Rinch-based desktop application.

mod app;
mod backend;
mod state;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("Starting Pimble with Rinch...");

    // Run the application
    app::run();
}
