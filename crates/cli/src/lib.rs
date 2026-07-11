//! Scriptable CLI and timeout-aware protocol-v1 client.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::time::timeout;
use yash_app_events_profile::{load_profile, Profile};
use yash_app_events_protocol::{method, Request, RequestId, Response, RpcError, PROTOCOL_VERSION};

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
    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let mut client = Client::connect(&socket, Duration::from_millis(cli.timeout_ms)).await?;
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
    };
    Ok(value)
}

async fn execute_capture(client: &mut Client, command: &CaptureCommand) -> Result<Value, CliError> {
    match command {
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
    }
}

/// Opens an event subscription after handshake.
///
/// # Errors
///
/// Returns connection, timeout, handshake, or subscription errors.
pub async fn event_stream(cli: &Cli) -> Result<Client, CliError> {
    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let mut client = Client::connect(&socket, Duration::from_millis(cli.timeout_ms)).await?;
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

/// One negotiated RPC client connection.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<ReadHalf<UnixStream>>,
    writer: WriteHalf<UnixStream>,
    timeout: Duration,
    next_id: i64,
}

impl Client {
    async fn connect(path: &Path, duration: Duration) -> Result<Self, CliError> {
        let stream = timeout(duration, UnixStream::connect(path))
            .await
            .map_err(|_| CliError::Timeout)??;
        let (reader, writer) = tokio::io::split(stream);
        let mut client = Self {
            reader: BufReader::new(reader),
            writer,
            timeout: duration,
            next_id: 1,
        };
        client.call(method::HANDSHAKE, json!({"protocol": PROTOCOL_VERSION, "client_name": "yash-eventsctl", "client_version": env!("CARGO_PKG_VERSION")})).await?;
        Ok(client)
    }

    async fn call(&mut self, method_name: &str, params: Value) -> Result<Value, CliError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request {
            jsonrpc: "2.0".into(),
            id: RequestId::Number(id),
            method: method_name.into(),
            params,
        };
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        timeout(self.timeout, self.writer.write_all(&bytes))
            .await
            .map_err(|_| CliError::Timeout)??;
        let mut line = String::new();
        timeout(self.timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| CliError::Timeout)??;
        if line.is_empty() {
            return Err(CliError::Disconnected);
        }
        let response: Response = serde_json::from_str(&line)?;
        response.result.ok_or_else(|| {
            CliError::Rpc(
                response
                    .error
                    .unwrap_or_else(|| RpcError::new(-32603, "response omitted result and error")),
            )
        })
    }

    /// Reads the next newline-framed subscription notification without a response timeout.
    ///
    /// # Errors
    ///
    /// Returns disconnect, I/O, or malformed JSON errors.
    pub async fn next_notification(&mut self) -> Result<Value, CliError> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        if line.is_empty() {
            return Err(CliError::Disconnected);
        }
        Ok(serde_json::from_str(&line)?)
    }
}

/// Stable CLI failure categories used for process exit codes.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("cannot connect to daemon: {0}")]
    Io(#[from] io::Error),
    #[error("daemon request timed out")]
    Timeout,
    #[error("daemon disconnected")]
    Disconnected,
    #[error("protocol JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon RPC error {}: {}", .0.code, .0.message)]
    Rpc(RpcError),
    #[error("profile validation failed: {0}")]
    Validation(String),
    #[error("event follow requires streaming execution")]
    FollowRequiresStream,
}

impl CliError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Io(_) | Self::Disconnected => 3,
            Self::Timeout => 6,
            Self::Validation(_) => 5,
            Self::Json(_) | Self::Rpc(_) | Self::FollowRequiresStream => 4,
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(CliError::Timeout.exit_code(), 6);
        assert_eq!(CliError::Disconnected.exit_code(), 3);
        assert_eq!(CliError::Validation("bad".into()).exit_code(), 5);
        assert_eq!(CliError::FollowRequiresStream.exit_code(), 4);
    }
}
