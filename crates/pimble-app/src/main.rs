//! Pimble Desktop Application
//!
//! Entry point for the Slint-based desktop application.

mod app;
mod backend;
mod cosmic_editor;
mod state;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    tracing::info!("Starting Pimble...");

    // Run the application
    if let Err(e) = app::run() {
        tracing::error!("Application error: {}", e);
        std::process::exit(1);
    }
}
