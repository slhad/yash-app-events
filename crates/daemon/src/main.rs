use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize structured logging: {error}"))?;
    info!(version = env!("CARGO_PKG_VERSION"), "daemon initialized");
    Ok(())
}
