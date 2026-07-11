use tracing::info;
use tracing_subscriber::EnvFilter;
use yash_app_eventsd::{run, ServerConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize structured logging: {error}"))?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is required"))?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map_or_else(
            || {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".local/share"))
            },
            |value| Some(value.into()),
        )
        .ok_or_else(|| anyhow::anyhow!("HOME or XDG_DATA_HOME is required"))?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(
            || {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".config"))
            },
            |value| Some(value.into()),
        )
        .ok_or_else(|| anyhow::anyhow!("HOME or XDG_CONFIG_HOME is required"))?;
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map_or_else(
            || {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".local/state"))
            },
            |value| Some(value.into()),
        )
        .ok_or_else(|| anyhow::anyhow!("HOME or XDG_STATE_HOME is required"))?;
    let config = ServerConfig {
        socket_path: std::path::PathBuf::from(runtime).join("yash-app-events/control.sock"),
        data_root: data.join("yash-app-events"),
        config_root: config_home.join("yash-app-events"),
        state_root: state_home.join("yash-app-events"),
        maximum_connections: 64,
    };
    info!(version = env!("CARGO_PKG_VERSION"), socket = %config.socket_path.display(), "daemon starting");
    run(config).await.map_err(Into::into)
}
