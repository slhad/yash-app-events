//! State-owning daemon and bounded JSON-RPC Unix transport.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Notify};
use uuid::Uuid;
use yash_app_events_profile::{
    export_profile, import_profile, ImportLimits, LocalConfig, Profile, ProfileId, ProfileStore,
    StoreError,
};
use yash_app_events_protocol::{
    error_code, method, nesting_within_limit, HandshakeParams, HandshakeResult, Notification,
    Request, RequestId, Response, RpcError, Status, MAXIMUM_MESSAGE_BYTES, MAXIMUM_NESTING_DEPTH,
    PROTOCOL_VERSION,
};

const SUBSCRIPTION_CAPACITY: usize = 64;

/// Configuration for one daemon instance.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub data_root: PathBuf,
    pub config_root: PathBuf,
    pub maximum_connections: usize,
}

/// Running server state shared by short-lived connection tasks.
#[derive(Debug)]
struct State {
    instance: Uuid,
    profiles: ProfileStore,
    local_config: LocalConfig,
    connected: AtomicUsize,
    maximum_connections: usize,
    shutdown: Notify,
    notifications: broadcast::Sender<Value>,
}

/// Runs the daemon until a graceful-shutdown RPC or process cancellation.
///
/// # Errors
///
/// Returns socket setup or accept failures. Per-client protocol failures remain isolated.
pub async fn run(config: ServerConfig) -> Result<(), ServerError> {
    let listener = bind_socket(&config.socket_path).await?;
    let (notifications, _) = broadcast::channel(SUBSCRIPTION_CAPACITY);
    let state = Arc::new(State {
        instance: Uuid::new_v4(),
        profiles: ProfileStore::new(config.data_root, 20),
        local_config: LocalConfig::new(config.config_root),
        connected: AtomicUsize::new(0),
        maximum_connections: config.maximum_connections,
        shutdown: Notify::new(),
        notifications,
    });
    loop {
        tokio::select! {
            () = state.shutdown.notified() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if state.connected.load(Ordering::Relaxed) >= state.maximum_connections {
                    drop(stream);
                    continue;
                }
                state.connected.fetch_add(1, Ordering::Relaxed);
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(error) = serve_connection(stream, Arc::clone(&state)).await {
                        tracing::warn!(%error, "client disconnected with protocol error");
                    }
                    state.connected.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }
    drop(listener);
    let _ = fs::remove_file(&config.socket_path);
    Ok(())
}

async fn bind_socket(path: &Path) -> Result<UnixListener, ServerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(ServerError::AlreadyRunning);
        }
        if !fs::symlink_metadata(path)?.file_type().is_socket() {
            return Err(ServerError::UnsafeStalePath);
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[allow(clippy::too_many_lines)]
async fn serve_connection(stream: UnixStream, state: Arc<State>) -> Result<(), ServerError> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    let mut negotiated = false;
    loop {
        buffer.clear();
        let bytes = reader.read_until(b'\n', &mut buffer).await?;
        if bytes == 0 {
            return Ok(());
        }
        if buffer.len() > MAXIMUM_MESSAGE_BYTES {
            return Err(ServerError::MessageTooLarge);
        }
        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        let Ok(value) = serde_json::from_slice::<Value>(&buffer) else {
            write_response(
                &mut writer,
                &Response::failure(
                    RequestId::Number(0),
                    RpcError::new(error_code::PARSE_ERROR, "parse error"),
                ),
            )
            .await?;
            continue;
        };
        if !nesting_within_limit(&value, MAXIMUM_NESTING_DEPTH) {
            return Err(ServerError::NestingTooDeep);
        }
        let request: Request = match serde_json::from_value::<Request>(value) {
            Ok(request) if request.jsonrpc == "2.0" => request,
            _ => {
                write_response(
                    &mut writer,
                    &Response::failure(
                        RequestId::Number(0),
                        RpcError::new(error_code::INVALID_REQUEST, "invalid request"),
                    ),
                )
                .await?;
                continue;
            }
        };
        if !negotiated && request.method != method::HANDSHAKE {
            write_response(
                &mut writer,
                &Response::failure(
                    request.id,
                    RpcError::new(
                        error_code::HANDSHAKE_REQUIRED,
                        "system.handshake must be the first call",
                    ),
                ),
            )
            .await?;
            continue;
        }
        if request.method == method::HANDSHAKE {
            let Ok(params) = serde_json::from_value::<HandshakeParams>(request.params) else {
                write_response(
                    &mut writer,
                    &Response::failure(
                        request.id,
                        RpcError::new(error_code::INVALID_PARAMS, "invalid handshake parameters"),
                    ),
                )
                .await?;
                continue;
            };
            if params.protocol != PROTOCOL_VERSION {
                write_response(
                    &mut writer,
                    &Response::failure(
                        request.id,
                        RpcError::new(
                            error_code::INCOMPATIBLE_VERSION,
                            "incompatible protocol version",
                        ),
                    ),
                )
                .await?;
                continue;
            }
            negotiated = true;
            let result = HandshakeResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                daemon_instance: state.instance,
            };
            write_response(
                &mut writer,
                &Response::success(request.id, serde_json::to_value(result)?),
            )
            .await?;
            continue;
        }
        if request.method == method::STATUS_SUBSCRIBE || request.method == method::EVENTS_SUBSCRIBE
        {
            write_response(
                &mut writer,
                &Response::success(
                    request.id,
                    json!({"subscribed": true, "capacity": SUBSCRIPTION_CAPACITY}),
                ),
            )
            .await?;
            return serve_subscription(&mut writer, state.notifications.subscribe()).await;
        }
        let response = dispatch(request, &state);
        write_response(&mut writer, &response).await?;
    }
}

#[allow(clippy::too_many_lines)]
fn dispatch(request: Request, state: &State) -> Response {
    let id = request.id;
    let result: Result<Value, RpcError> = match request.method.as_str() {
        method::VERSION => {
            Ok(json!({"version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL_VERSION}))
        }
        method::CAPABILITIES => {
            Ok(json!({"profiles": true, "subscriptions": true, "capture": false, "preview": false}))
        }
        method::STATUS => serde_json::to_value(status(state)).map_err(internal_error),
        method::SHUTDOWN => {
            state.shutdown.notify_one();
            Ok(json!({"shutting_down": true}))
        }
        method::PROFILE_LIST => state
            .profiles
            .list()
            .and_then(|profiles| {
                serde_json::to_value(profiles)
                    .map_err(yash_app_events_profile::StorageError::from)
                    .map_err(StoreError::from)
            })
            .map_err(store_error),
        method::PROFILE_GET => parse::<ProfileIdParam>(request.params)
            .and_then(|params| state.profiles.load(params.profile_id).map_err(store_error))
            .and_then(|profile| serde_json::to_value(profile).map_err(internal_error)),
        method::PROFILE_CREATE => parse::<ProfileParam>(request.params).and_then(|params| {
            state
                .profiles
                .create(&params.profile)
                .map(|()| json!({"created": true}))
                .map_err(store_error)
        }),
        method::PROFILE_COMMIT => parse::<CommitParams>(request.params)
            .and_then(|params| {
                state
                    .profiles
                    .commit(params.profile, params.expected_revision)
                    .map_err(store_error)
            })
            .and_then(|profile| serde_json::to_value(profile).map_err(internal_error)),
        method::PROFILE_DUPLICATE => parse::<DuplicateParams>(request.params)
            .and_then(|params| {
                state
                    .profiles
                    .duplicate_profile(params.profile_id, params.name)
                    .map_err(store_error)
            })
            .and_then(|profile| serde_json::to_value(profile).map_err(internal_error)),
        method::PROFILE_VALIDATE => parse::<ProfileParam>(request.params).and_then(|params| {
            params
                .profile
                .validate()
                .map(|()| json!({"valid": true}))
                .map_err(internal_error)
        }),
        method::PROFILE_IMPORT => parse::<PathParam>(request.params).and_then(|params| {
            import_profile(
                &params.path,
                state.profiles.profiles_root(),
                ImportLimits::default(),
            )
            .and_then(|profile| serde_json::to_value(profile).map_err(Into::into))
            .map_err(internal_error)
        }),
        method::PROFILE_EXPORT => parse::<ExportParams>(request.params).and_then(|params| {
            export_profile(
                &state.profiles.profile_directory(params.profile_id),
                &params.path,
            )
            .and_then(|manifest| serde_json::to_value(manifest).map_err(Into::into))
            .map_err(internal_error)
        }),
        method::PROFILE_TRASH => parse::<ProfileIdParam>(request.params).and_then(|params| {
            state
                .profiles
                .trash(params.profile_id)
                .map(|()| json!({"trashed": true}))
                .map_err(store_error)
        }),
        method::PROFILE_RESTORE => parse::<ProfileIdParam>(request.params).and_then(|params| {
            state
                .profiles
                .restore(params.profile_id)
                .map(|()| json!({"restored": true}))
                .map_err(store_error)
        }),
        method::PROFILE_ACTIVATE => parse::<ProfileIdParam>(request.params).and_then(|params| {
            state
                .profiles
                .load(params.profile_id)
                .map_err(store_error)?;
            let mut settings = state.local_config.load_settings().map_err(internal_error)?;
            settings.active_profile = Some(params.profile_id);
            state
                .local_config
                .save_settings(&settings)
                .map_err(internal_error)?;
            Ok(json!({"active_profile": params.profile_id}))
        }),
        method::STATE_GET => Ok(
            json!({"schema": 1, "daemon_instance": state.instance, "sequence": 0, "observations": {}, "events": {}}),
        ),
        _ => Err(RpcError::new(
            error_code::METHOD_NOT_FOUND,
            "method not found",
        )),
    };
    match result {
        Ok(value) => Response::success(id, value),
        Err(error) => Response::failure(id, error),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|error| RpcError {
        code: error_code::INVALID_PARAMS,
        message: "invalid parameters".into(),
        data: Some(json!({"detail": error.to_string()})),
    })
}

fn store_error(error: StoreError) -> RpcError {
    if let StoreError::RevisionConflict { expected, current } = error {
        RpcError {
            code: error_code::REVISION_CONFLICT,
            message: "profile revision conflict".into(),
            data: Some(json!({"expected": expected, "current": current})),
        }
    } else {
        internal_error(error)
    }
}

fn internal_error(error: impl std::fmt::Display) -> RpcError {
    RpcError {
        code: error_code::INTERNAL_ERROR,
        message: "internal error".into(),
        data: Some(json!({"detail": error.to_string()})),
    }
}

fn status(state: &State) -> Status {
    Status {
        daemon_instance: state.instance,
        capture_active: false,
        selected_source: None,
        connected_clients: state.connected.load(Ordering::Relaxed),
        active_profile: None,
        input_fps: 0.0,
        analysis_fps: 0.0,
        replaced_frames: 0,
        output_error: None,
    }
}

async fn serve_subscription(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    mut receiver: broadcast::Receiver<Value>,
) -> Result<(), ServerError> {
    loop {
        let notification = match receiver.recv().await {
            Ok(value) => Notification {
                jsonrpc: "2.0".into(),
                method: "event".into(),
                params: value,
            },
            Err(broadcast::error::RecvError::Lagged(skipped)) => Notification {
                jsonrpc: "2.0".into(),
                method: "subscription.lagged".into(),
                params: json!({"code": error_code::SUBSCRIPTION_LAGGED, "skipped": skipped}),
            },
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        };
        writer
            .write_all(&serde_json::to_vec(&notification)?)
            .await?;
        writer.write_all(b"\n").await?;
    }
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &Response,
) -> Result<(), ServerError> {
    writer.write_all(&serde_json::to_vec(response)?).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

#[derive(Deserialize)]
struct ProfileIdParam {
    profile_id: ProfileId,
}
#[derive(Deserialize)]
struct ProfileParam {
    profile: Profile,
}
#[derive(Deserialize)]
struct CommitParams {
    profile: Profile,
    expected_revision: u64,
}
#[derive(Deserialize)]
struct DuplicateParams {
    profile_id: ProfileId,
    name: String,
}
#[derive(Deserialize)]
struct PathParam {
    path: PathBuf,
}
#[derive(Deserialize)]
struct ExportParams {
    profile_id: ProfileId,
    path: PathBuf,
}

/// Daemon transport failure.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("daemon I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("daemon JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("another daemon already owns the control socket")]
    AlreadyRunning,
    #[error("refusing to remove a stale path that is not a Unix socket")]
    UnsafeStalePath,
    #[error("request exceeds the message-size limit")]
    MessageTooLarge,
    #[error("request exceeds the JSON nesting limit")]
    NestingTooDeep,
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use yash_app_events_profile::Profile;

    use super::*;

    async fn start(
        directory: &Path,
    ) -> (PathBuf, tokio::task::JoinHandle<Result<(), ServerError>>) {
        let socket = directory.join("runtime/control.sock");
        let config = ServerConfig {
            socket_path: socket.clone(),
            data_root: directory.join("data"),
            config_root: directory.join("config"),
            maximum_connections: 8,
        };
        let task = tokio::spawn(run(config));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        (socket, task)
    }

    async fn call(
        connection: &mut BufReader<UnixStream>,
        id: i64,
        method_name: &str,
        params: Value,
    ) -> Value {
        let request = json!({"jsonrpc":"2.0", "id":id, "method":method_name, "params":params});
        connection
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        connection.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn handshake(connection: &mut BufReader<UnixStream>) {
        let response = call(
            connection,
            1,
            method::HANDSHAKE,
            json!({"protocol":1,"client_name":"test","client_version":"0"}),
        )
        .await;
        assert_eq!(response["result"]["protocol"], 1);
    }

    #[tokio::test]
    async fn socket_is_private_and_two_clients_can_inspect_status() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut first = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        let mut second = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut first).await;
        handshake(&mut second).await;
        let first_status = call(&mut first, 2, method::STATUS, Value::Null).await;
        let second_status = call(&mut second, 2, method::STATUS, Value::Null).await;
        assert!(
            first_status["result"]["connected_clients"]
                .as_u64()
                .unwrap()
                >= 2
        );
        assert_eq!(
            first_status["result"]["daemon_instance"],
            second_status["result"]["daemon_instance"]
        );
        call(&mut first, 3, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn handshake_is_required_and_stale_revision_is_structured() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        let rejected = call(&mut client, 1, method::STATUS, Value::Null).await;
        assert_eq!(rejected["error"]["code"], error_code::HANDSHAKE_REQUIRED);
        handshake(&mut client).await;
        let profile = Profile::new("Demo", "demo_game", 1920, 1080);
        let created = call(
            &mut client,
            2,
            method::PROFILE_CREATE,
            json!({"profile":profile}),
        )
        .await;
        assert_eq!(created["result"]["created"], true);
        let committed = call(
            &mut client,
            3,
            method::PROFILE_COMMIT,
            json!({"profile":profile,"expected_revision":0}),
        )
        .await;
        assert_eq!(committed["result"]["revision"], 1);
        let conflict = call(
            &mut client,
            4,
            method::PROFILE_COMMIT,
            json!({"profile":profile,"expected_revision":0}),
        )
        .await;
        assert_eq!(conflict["error"]["code"], error_code::REVISION_CONFLICT);
        assert_eq!(conflict["error"]["data"]["current"], 1);
        call(&mut client, 5, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stale_socket_is_recovered_but_regular_file_is_never_removed() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(stale);
        let config = ServerConfig {
            socket_path: socket.clone(),
            data_root: directory.path().join("data"),
            config_root: directory.path().join("config"),
            maximum_connections: 8,
        };
        let task = tokio::spawn(run(config.clone()));
        for _ in 0..100 {
            if UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut client).await;
        call(&mut client, 2, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
        fs::write(&socket, b"do not delete").unwrap();
        assert!(matches!(
            run(config).await,
            Err(ServerError::UnsafeStalePath)
        ));
        assert_eq!(fs::read(socket).unwrap(), b"do not delete");
    }

    #[tokio::test]
    async fn bounded_subscription_reports_lag() {
        let (sender, mut receiver) = broadcast::channel(2);
        sender.send(json!(1)).unwrap();
        sender.send(json!(2)).unwrap();
        sender.send(json!(3)).unwrap();
        assert!(matches!(
            receiver.recv().await,
            Err(broadcast::error::RecvError::Lagged(1))
        ));
    }
}
