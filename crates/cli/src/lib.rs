//! Scriptable CLI and timeout-aware protocol-v1 client.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use thiserror::Error;
use yash_app_events_engine::ReplayManifest;
use yash_app_events_profile::{load_profile, Profile};
use yash_app_events_protocol::{method, ClientError, UnixRpcClient};

/// `yash-eventsctl` command line.
#[derive(Debug, Parser)]
#[command(version, about = "Inspect and control yash-app-eventsd")]
pub struct Cli {
    /// Emit stable compact JSON.
    #[arg(long, global = true)]
    pub json: bool,
    /// Override the daemon control socket.
    #[arg(long, global = true, env = "YASH_APP_EVENTS_SOCKET")]
    pub socket: Option<PathBuf>,
    /// Per-connect and per-response timeout in milliseconds.
    #[arg(long, global = true, default_value_t = 5000)]
    pub timeout_ms: u64,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show daemon version and protocol compatibility.
    Version,
    /// Show capture, profile, output, and connection status.
    Status,
    /// Read the current state snapshot.
    State,
    /// Ask the daemon to stop cleanly.
    Shutdown,
    /// Manage portable profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Follow bounded event notifications until disconnected.
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Select, inspect, and stop the daemon-owned portal capture session.
    Capture {
        #[command(subcommand)]
        command: CaptureCommand,
    },
    /// Evaluate a versioned synthetic replay manifest through the daemon engine.
    Replay { manifest: PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    List,
    Get {
        profile_id: String,
    },
    Create {
        name: String,
        game: String,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
    },
    Duplicate {
        profile_id: String,
        name: String,
    },
    Activate {
        profile_id: String,
    },
    Trash {
        profile_id: String,
    },
    Restore {
        profile_id: String,
    },
    Import {
        path: PathBuf,
    },
    Export {
        profile_id: String,
        path: PathBuf,
    },
    /// Validate a profile directly without a running daemon.
    Validate {
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum EventsCommand {
    Follow,
}

#[derive(Debug, Subcommand)]
pub enum CaptureCommand {
    Select {
        #[arg(long, default_value = "window_or_monitor")]
        source: String,
        #[arg(long)]
        profile_id: Option<String>,
    },
    Start,
    Stop,
    Status,
    Snapshot {
        path: PathBuf,
    },
}

/// Executes one finite command and returns its protocol-shaped value.
///
/// # Errors
///
/// Returns connection, timeout, protocol, RPC, or offline-validation errors.
#[allow(clippy::too_many_lines)]
pub async fn execute(cli: &Cli) -> Result<Value, CliError> {
    if let Command::Profile {
        command: ProfileCommand::Validate { path },
    } = &cli.command
    {
        let profile =
            load_profile(path).map_err(|error| CliError::Validation(error.to_string()))?;
        return Ok(
            json!({"valid": true, "schema": profile.schema, "profile_id": profile.id, "revision": profile.revision}),
        );
    }
    if matches!(
        cli.command,
        Command::Events {
            command: EventsCommand::Follow
        }
    ) {
        return Err(CliError::FollowRequiresStream);
    }
    let replay_manifest = if let Command::Replay { manifest } = &cli.command {
        let bytes = std::fs::read(manifest)
            .map_err(|error| CliError::Replay(format!("cannot read manifest: {error}")))?;
        Some(
            serde_json::from_slice::<ReplayManifest>(&bytes)
                .map_err(|error| CliError::Replay(format!("invalid manifest: {error}")))?,
        )
    } else {
        None
    };
    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let mut client = UnixRpcClient::connect(
        &socket,
        Duration::from_millis(cli.timeout_ms),
        "yash-eventsctl",
        env!("CARGO_PKG_VERSION"),
    )
    .await?;
    let value = match &cli.command {
        Command::Version => client.call(method::VERSION, Value::Null).await?,
        Command::Status => client.call(method::STATUS, Value::Null).await?,
        Command::State => client.call(method::STATE_GET, Value::Null).await?,
        Command::Shutdown => client.call(method::SHUTDOWN, Value::Null).await?,
        Command::Profile { command } => match command {
            ProfileCommand::List => client.call(method::PROFILE_LIST, Value::Null).await?,
            ProfileCommand::Get { profile_id } => {
                client
                    .call(method::PROFILE_GET, json!({"profile_id": profile_id}))
                    .await?
            }
            ProfileCommand::Create {
                name,
                game,
                width,
                height,
            } => {
                let profile = Profile::new(name, game, *width, *height);
                client
                    .call(method::PROFILE_CREATE, json!({"profile": profile}))
                    .await?
            }
            ProfileCommand::Duplicate { profile_id, name } => {
                client
                    .call(
                        method::PROFILE_DUPLICATE,
                        json!({"profile_id":profile_id,"name":name}),
                    )
                    .await?
            }
            ProfileCommand::Activate { profile_id } => {
                client
                    .call(method::PROFILE_ACTIVATE, json!({"profile_id":profile_id}))
                    .await?
            }
            ProfileCommand::Trash { profile_id } => {
                client
                    .call(method::PROFILE_TRASH, json!({"profile_id":profile_id}))
                    .await?
            }
            ProfileCommand::Restore { profile_id } => {
                client
                    .call(method::PROFILE_RESTORE, json!({"profile_id":profile_id}))
                    .await?
            }
            ProfileCommand::Import { path } => {
                client
                    .call(method::PROFILE_IMPORT, json!({"path":path}))
                    .await?
            }
            ProfileCommand::Export { profile_id, path } => {
                client
                    .call(
                        method::PROFILE_EXPORT,
                        json!({"profile_id":profile_id,"path":path}),
                    )
                    .await?
            }
            ProfileCommand::Validate { .. } => unreachable!(),
        },
        Command::Capture { command } => execute_capture(&mut client, command).await?,
        Command::Events { .. } => unreachable!(),
        Command::Replay { .. } => {
            client
                .call(
                    method::REPLAY_EVALUATE,
                    serde_json::to_value(replay_manifest.as_ref().ok_or_else(|| {
                        CliError::Replay("internal manifest routing error".into())
                    })?)
                    .map_err(|error| CliError::Replay(error.to_string()))?,
                )
                .await?
        }
    };
    Ok(value)
}

async fn execute_capture(
    client: &mut UnixRpcClient,
    command: &CaptureCommand,
) -> Result<Value, CliError> {
    let result = match command {
        CaptureCommand::Select { source, profile_id } => {
            client
                .call(
                    method::CAPTURE_SELECT,
                    json!({"source":source,"profile_id":profile_id}),
                )
                .await
        }
        CaptureCommand::Start => client.call(method::CAPTURE_START, Value::Null).await,
        CaptureCommand::Stop => client.call(method::CAPTURE_STOP, Value::Null).await,
        CaptureCommand::Status => client.call(method::CAPTURE_STATUS, Value::Null).await,
        CaptureCommand::Snapshot { path } => {
            client
                .call(method::CAPTURE_SNAPSHOT, json!({"path":path}))
                .await
        }
    };
    result.map_err(Into::into)
}

/// Opens an event subscription after handshake.
///
/// # Errors
///
/// Returns connection, timeout, handshake, or subscription errors.
pub async fn event_stream(cli: &Cli) -> Result<UnixRpcClient, CliError> {
    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let mut client = UnixRpcClient::connect(
        &socket,
        Duration::from_millis(cli.timeout_ms),
        "yash-eventsctl",
        env!("CARGO_PKG_VERSION"),
    )
    .await?;
    client.call(method::EVENTS_SUBSCRIBE, Value::Null).await?;
    Ok(client)
}

/// Formats one result for human or machine consumption.
#[must_use]
pub fn format_result(command: &Command, value: &Value, json_output: bool) -> String {
    if json_output {
        return serde_json::to_string(value)
            .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"));
    }
    match command {
        Command::Version => format!(
            "yash-app-eventsd {} (protocol {})",
            value["version"].as_str().unwrap_or("unknown"),
            value["protocol"].as_u64().unwrap_or(0)
        ),
        Command::Status => format!(
            "capture: {}\nsource: {}\nclients: {}\nactive profile: {}",
            if value["capture_active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "stopped"
            },
            value["selected_source"].as_str().unwrap_or("none"),
            value["connected_clients"].as_u64().unwrap_or(0),
            value["active_profile"].as_str().unwrap_or("none")
        ),
        Command::Shutdown => "daemon is shutting down".into(),
        Command::Profile {
            command: ProfileCommand::List,
        } => value.as_array().map_or_else(
            || "no profiles".into(),
            |profiles| {
                profiles
                    .iter()
                    .map(|profile| {
                        format!(
                            "{}\t{}\trev {}",
                            profile["id"].as_str().unwrap_or("?"),
                            profile["name"].as_str().unwrap_or("?"),
                            profile["revision"].as_u64().unwrap_or(0)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        ),
        Command::Profile {
            command: ProfileCommand::Validate { .. },
        } => format!(
            "valid profile {} (schema {})",
            value["profile_id"].as_str().unwrap_or("?"),
            value["schema"].as_u64().unwrap_or(0)
        ),
        _ => serde_json::to_string_pretty(value)
            .unwrap_or_else(|error| format!("unable to format result: {error}")),
    }
}

#[must_use]
pub fn default_socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || {
            PathBuf::from("/run/user")
                .join(unsafe_current_uid())
                .join("yash-app-events/control.sock")
        },
        |directory| PathBuf::from(directory).join("yash-app-events/control.sock"),
    )
}

fn unsafe_current_uid() -> String {
    std::env::var("UID").unwrap_or_else(|_| "unknown".into())
}

/// Stable CLI failure categories used for process exit codes.
#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("profile validation failed: {0}")]
    Validation(String),
    #[error("event follow requires streaming execution")]
    FollowRequiresStream,
    #[error("replay evaluation failed: {0}")]
    Replay(String),
}

impl CliError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Client(ClientError::Io(_) | ClientError::Disconnected) => 3,
            Self::Client(ClientError::Timeout) => 6,
            Self::Validation(_) => 5,
            Self::Replay(_) => 7,
            Self::Client(ClientError::Json(_) | ClientError::Rpc(_))
            | Self::FollowRequiresStream => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use yash_app_eventsd::{run, ServerConfig};

    #[test]
    fn json_output_is_compact_and_stable() {
        let command = Command::Status;
        assert_eq!(
            format_result(
                &command,
                &json!({"capture_active":false,"connected_clients":2}),
                true
            ),
            r#"{"capture_active":false,"connected_clients":2}"#
        );
    }

    #[test]
    fn human_status_names_capture_visibility_fields() {
        let output = format_result(
            &Command::Status,
            &json!({"capture_active":false,"selected_source":null,"connected_clients":2,"active_profile":null}),
            false,
        );
        assert_eq!(
            output,
            "capture: stopped\nsource: none\nclients: 2\nactive profile: none"
        );
    }

    async fn daemon_cli(
        directory: &Path,
        command: Command,
    ) -> (
        Cli,
        tokio::task::JoinHandle<Result<(), yash_app_eventsd::ServerError>>,
    ) {
        let socket = directory.join("runtime/control.sock");
        let task = tokio::spawn(run(ServerConfig {
            socket_path: socket.clone(),
            data_root: directory.join("data"),
            config_root: directory.join("config"),
            state_root: directory.join("state"),
            maximum_connections: 8,
        }));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        (
            Cli {
                json: true,
                socket: Some(socket),
                timeout_ms: 1000,
                command,
            },
            task,
        )
    }

    #[tokio::test]
    async fn real_client_negotiates_and_manages_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let (mut cli, task) = daemon_cli(directory.path(), Command::Status).await;
        let status = execute(&cli).await.unwrap();
        assert_eq!(status["capture_active"], false);
        cli.command = Command::Capture {
            command: CaptureCommand::Status,
        };
        assert_eq!(execute(&cli).await.unwrap()["active"], false);
        cli.command = Command::Profile {
            command: ProfileCommand::Create {
                name: "Demo".into(),
                game: "demo_game".into(),
                width: 1280,
                height: 720,
            },
        };
        assert_eq!(execute(&cli).await.unwrap()["created"], true);
        cli.command = Command::Profile {
            command: ProfileCommand::List,
        };
        let profiles = execute(&cli).await.unwrap();
        assert_eq!(profiles.as_array().unwrap().len(), 1);
        assert_eq!(profiles[0]["name"], "Demo");
        let profile_id = profiles[0]["id"].as_str().unwrap().to_owned();
        cli.command = Command::Profile {
            command: ProfileCommand::Duplicate {
                profile_id: profile_id.clone(),
                name: "Copy".into(),
            },
        };
        let duplicate = execute(&cli).await.unwrap();
        assert_ne!(duplicate["id"], profile_id);
        cli.command = Command::Profile {
            command: ProfileCommand::Activate {
                profile_id: profile_id.clone(),
            },
        };
        assert_eq!(execute(&cli).await.unwrap()["active_profile"], profile_id);
        cli.command = Command::Profile {
            command: ProfileCommand::Trash {
                profile_id: profile_id.clone(),
            },
        };
        assert_eq!(execute(&cli).await.unwrap()["trashed"], true);
        cli.command = Command::Profile {
            command: ProfileCommand::Restore { profile_id },
        };
        assert_eq!(execute(&cli).await.unwrap()["restored"], true);
        cli.command = Command::Shutdown;
        assert_eq!(execute(&cli).await.unwrap()["shutting_down"], true);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn offline_validation_does_not_require_a_socket() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.json");
        let profile = Profile::new("Demo", "demo_game", 1920, 1080);
        yash_app_events_profile::save_profile(&path, &profile).unwrap();
        let cli = Cli {
            json: true,
            socket: Some(directory.path().join("missing.sock")),
            timeout_ms: 1,
            command: Command::Profile {
                command: ProfileCommand::Validate { path },
            },
        };
        let result = execute(&cli).await.unwrap();
        assert_eq!(result["valid"], true);
        assert_eq!(result["profile_id"], profile.id.to_string());
    }

    #[test]
    fn exit_codes_are_stable_by_failure_category() {
        assert_eq!(CliError::Client(ClientError::Timeout).exit_code(), 6);
        assert_eq!(CliError::Client(ClientError::Disconnected).exit_code(), 3);
        assert_eq!(CliError::Validation("bad".into()).exit_code(), 5);
        assert_eq!(CliError::FollowRequiresStream.exit_code(), 4);
    }
}
