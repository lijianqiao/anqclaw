mod config;
mod llm;
mod memory;
mod types;

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::AppConfig::load("config.toml")?;

    // Initialize tracing with JSON format based on config log level
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.app.log_level))?;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json().with_writer(std::io::stderr))
        .init();

    tracing::info!("anq-agent started");

    Ok(())
}