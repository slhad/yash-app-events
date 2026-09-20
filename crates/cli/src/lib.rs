//! Scriptable CLI and timeout-aware protocol-v1 client.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use thiserror::Error;
use yash_app_events_engine::ReplayManifest;
use yash_app_events_profile::{
    export_profile, load_profile, load_profile_bundle, load_profile_for_capacity_analysis,
    CollectionPolicy, OutputRoute, Profile, StorageError,
};
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
    /// Analyze one PNG and return spatial JSON without embedding image bytes.
    Analyze {
        image: PathBuf,
        /// Analyze with one legacy monolithic profile.
        #[arg(
            long,
            conflicts_with = "profile_bundle",
            required_unless_present = "profile_bundle"
        )]
        profile_id: Option<String>,
        /// Analyze through a bounded router plus independently validated member profiles.
        #[arg(
            long = "profile-bundle",
            visible_alias = "bundle",
            conflicts_with = "profile_id",
            required_unless_present = "profile_id"
        )]
        profile_bundle: Option<PathBuf>,
        /// Optional soft transition context as one bounded JSON object.
        #[arg(long = "context-json", value_name = "JSON")]
        context_json: Option<String>,
    },
    /// Ask the daemon to stop cleanly.
    Shutdown,
    /// Manage portable profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Browse and install reviewed profiles from the public catalog.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
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
    /// Evaluate portable external detector regression packages.
    Suite {
        #[command(subcommand)]
        command: SuiteCommand,
    },
    /// Configure passive evidence capture and review collected batches.
    Collection {
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// Configure profile-scoped file and command output routes.
    Output {
        #[command(subcommand)]
        command: OutputCommand,
    },
    /// Review and export a privacy-bounded diagnostic bundle.
    Diagnostic {
        #[command(subcommand)]
        command: DiagnosticCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum OutputCommand {
    List {
        profile_id: String,
    },
    /// Create or replace a route from a JSON document.
    Set {
        profile_id: String,
        route: PathBuf,
    },
    Enable {
        profile_id: String,
        route_id: String,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        enabled: bool,
    },
    Remove {
        profile_id: String,
        route_id: String,
    },
    /// Deliver a sample payload through the configured sink.
    Test {
        profile_id: String,
        route_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SuiteCommand {
    /// Evaluate every case without installing or modifying the pinned profile.
    Evaluate {
        path: PathBuf,
        /// Include phase and slowest-case wall-clock timings in the result.
        #[arg(long)]
        timings: bool,
        /// Print bounded phase/case progress to stderr in human mode.
        #[arg(long)]
        progress: bool,
        /// Evaluate only this exact case ID; repeat for multiple cases.
        #[arg(long = "case")]
        case_ids: Vec<String>,
        /// Evaluate cases carrying this category; repeatable.
        #[arg(long)]
        category: Vec<String>,
        /// Evaluate cases using this package-relative fixture; repeatable.
        #[arg(long = "changed-fixture")]
        fixtures: Vec<PathBuf>,
        /// Evaluate cases explicitly expecting this scene; repeatable.
        #[arg(long)]
        scene: Vec<String>,
        /// Evaluate cases explicitly expecting this overlay; repeatable.
        #[arg(long)]
        overlay: Vec<String>,
        /// Evaluate cases asserting this element/target alias; repeatable.
        #[arg(long = "element")]
        elements: Vec<String>,
        /// Stop after the first failing selected case.
        #[arg(long)]
        fail_fast: bool,
        /// Return the operation ID immediately for later status/cancel commands.
        #[arg(long)]
        detach: bool,
        /// Bypass immutable inventory caches for a cold correctness run.
        #[arg(long)]
        no_cache: bool,
        /// Atomically write the complete JSON result and print only a receipt.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Inspect a running or retained suite operation.
    Status {
        operation_id: String,
        #[arg(long)]
        result: bool,
    },
    /// Cooperatively cancel a suite operation.
    Cancel { operation_id: String },
}

#[derive(Debug, Subcommand)]
pub enum CollectionCommand {
    PolicyGet {
        profile_id: String,
    },
    PolicySet {
        profile_id: String,
        dataset_root: PathBuf,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        enabled: bool,
        #[arg(long, default_value_t = 70)]
        interval_seconds: u64,
        #[arg(long, default_value_t = 10)]
        jitter_seconds: u64,
        #[arg(long, default_value_t = 0.015)]
        similarity_threshold: f32,
        #[arg(long, default_value_t = 1000)]
        maximum_pending_items: usize,
        #[arg(long, default_value_t = 2_147_483_648)]
        maximum_bytes: u64,
        #[arg(long = "novelty-target")]
        novelty_targets: Vec<String>,
    },
    Status {
        profile_id: String,
    },
    Items {
        profile_id: String,
        #[arg(long)]
        status: Option<String>,
    },
    Get {
        profile_id: String,
        id: String,
    },
    Review {
        profile_id: String,
        id: String,
        action: String,
        #[arg(long)]
        reason: Option<String>,
        /// JSON object mapping observation names to expected-observation contracts.
        #[arg(long)]
        expected: Option<PathBuf>,
    },
    AutoReview {
        profile_id: String,
        #[arg(long)]
        promote: bool,
    },
    Compare {
        first: PathBuf,
        second: PathBuf,
        #[arg(long, default_value_t = 0.015)]
        threshold: f32,
    },
}

#[derive(Debug, Subcommand)]
pub enum DiagnosticCommand {
    /// Show the exact redacted entries and total size before export.
    Plan {
        #[arg(long)]
        profile_id: Option<String>,
        #[arg(long = "element-id")]
        element_ids: Vec<String>,
    },
    /// Export only after reviewing a plan and confirming its exact total size.
    Export {
        path: PathBuf,
        #[arg(long)]
        profile_id: Option<String>,
        #[arg(long = "element-id")]
        element_ids: Vec<String>,
        #[arg(long)]
        expected_total_bytes: usize,
        #[arg(long)]
        privacy_reviewed: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    List,
    Get {
        profile_id: String,
    },
    /// List retained snapshots from oldest to current.
    Revisions {
        profile_id: String,
    },
    /// Read one retained snapshot.
    RevisionGet {
        profile_id: String,
        revision: u64,
    },
    /// Restore a snapshot by committing it as a new current revision.
    Rollback {
        profile_id: String,
        revision: u64,
        #[arg(long)]
        expected_revision: u64,
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
    /// Build a portable archive from a validated profile directory without a daemon.
    Pack {
        directory: PathBuf,
        path: PathBuf,
    },
    /// Report resource usage and read-only consolidation opportunities offline.
    AnalyzeCapacity {
        path: PathBuf,
    },
    /// Validate a read-only profile routing bundle without a daemon.
    Bundle {
        #[command(subcommand)]
        command: ProfileBundleCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileBundleCommand {
    Validate { path: PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    /// Show whether a validated catalog is cached.
    Status,
    /// Fetch and validate the newest catalog revision.
    Refresh,
    /// List compatible profiles from the last validated catalog.
    List,
    /// Download, verify, and safely import one reviewed package.
    Install {
        id: String,
        version: String,
        #[arg(long)]
        catalog_revision: u64,
        #[arg(long)]
        sha256: String,
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
    AutoGet,
    AutoSet {
        profile_id: String,
        process_match: String,
        #[arg(long, default_value_t = true)]
        enabled: bool,
    },
    Snapshot {
        path: PathBuf,
    },
}

const MAX_ANALYSIS_CONTEXT_JSON_BYTES: usize = 16 * 1024;

fn parse_analysis_context_json(raw: &str) -> Result<Value, CliError> {
    if raw.len() > MAX_ANALYSIS_CONTEXT_JSON_BYTES {
        return Err(CliError::Validation(
            "analysis context JSON exceeds 16 KiB".into(),
        ));
    }
    let value = serde_json::from_str::<Value>(raw)
        .map_err(|error| CliError::Validation(format!("invalid analysis context JSON: {error}")))?;
    if !value.is_object() {
        return Err(CliError::Validation(
            "analysis context JSON must be an object".into(),
        ));
    }
    Ok(value)
}

/// Executes one finite command and returns its protocol-shaped value.
///
/// # Errors
///
/// Returns connection, timeout, protocol, RPC, or offline-validation errors.
#[allow(clippy::too_many_lines)]
pub async fn execute(cli: &Cli) -> Result<Value, CliError> {
    if let Command::Profile { command } = &cli.command {
        match command {
            ProfileCommand::Validate { path } => {
                let profile =
                    load_profile(path).map_err(|error| CliError::Validation(error.to_string()))?;
                return Ok(
                    json!({"valid": true, "schema": profile.schema, "profile_id": profile.id, "revision": profile.revision}),
                );
            }
            ProfileCommand::Pack { directory, path } => {
                let manifest = export_profile(directory, path)
                    .map_err(|error| CliError::Validation(error.to_string()))?;
                return Ok(json!({
                    "packed": true,
                    "profile_id": manifest.profile_id,
                    "schema": manifest.profile_schema,
                    "files": manifest.files.len(),
                    "path": path,
                }));
            }
            ProfileCommand::AnalyzeCapacity { path } => {
                return analyze_capacity_path(path);
            }
            ProfileCommand::Bundle {
                command: ProfileBundleCommand::Validate { path },
            } => {
                let bundle = load_profile_bundle(path)
                    .map_err(|error| CliError::Validation(error.to_string()))?;
                return Ok(json!({
                    "valid": true,
                    "schema": bundle.schema,
                    "name": bundle.name,
                    "game": bundle.game,
                    "router_profile_id": bundle.router_profile_id,
                    "router_profile_revision": bundle.router_profile_revision,
                    "members": bundle.members.len(),
                }));
            }
            _ => {}
        }
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
    let timeout_ms = if matches!(
        cli.command,
        Command::Suite { .. }
            | Command::Catalog {
                command: CatalogCommand::Refresh | CatalogCommand::Install { .. }
            }
    ) {
        cli.timeout_ms.max(60_000)
    } else {
        cli.timeout_ms
    };
    let mut client = UnixRpcClient::connect(
        &socket,
        Duration::from_millis(timeout_ms),
        "yash-eventsctl",
        env!("CARGO_PKG_VERSION"),
    )
    .await?;
    let value =
        match &cli.command {
            Command::Version => client.call(method::VERSION, Value::Null).await?,
            Command::Status => client.call(method::STATUS, Value::Null).await?,
            Command::State => client.call(method::STATE_GET, Value::Null).await?,
            Command::Analyze {
                image,
                profile_id,
                profile_bundle,
                context_json,
            } => {
                let path = std::fs::canonicalize(image).map_err(|error| {
                    CliError::Replay(format!("cannot open analysis image: {error}"))
                })?;
                let context = context_json
                    .as_deref()
                    .map(parse_analysis_context_json)
                    .transpose()?;
                let params = if let Some(profile_id) = profile_id {
                    json!({
                        "profile_id": profile_id,
                        "path": path,
                        "timeout_ms": cli.timeout_ms,
                        "context": context,
                    })
                } else {
                    let bundle = profile_bundle.as_ref().ok_or_else(|| {
                        CliError::Validation(
                            "analyze requires --profile-id or --profile-bundle".into(),
                        )
                    })?;
                    let bundle = std::fs::canonicalize(bundle).map_err(|error| {
                        CliError::Replay(format!("cannot open analysis bundle: {error}"))
                    })?;
                    json!({
                        "bundle": bundle,
                        "path": path,
                        "timeout_ms": cli.timeout_ms,
                        "context": context,
                    })
                };
                client
                    .call(method::ANALYZE_IMAGE, params)
                    .await?
            }
            Command::Shutdown => client.call(method::SHUTDOWN, Value::Null).await?,
            Command::Profile { command } => match command {
                ProfileCommand::List => client.call(method::PROFILE_LIST, Value::Null).await?,
                ProfileCommand::Get { profile_id } => {
                    client
                        .call(method::PROFILE_GET, json!({"profile_id": profile_id}))
                        .await?
                }
                ProfileCommand::Revisions { profile_id } => {
                    client
                        .call(method::PROFILE_REVISIONS, json!({"profile_id":profile_id}))
                        .await?
                }
                ProfileCommand::RevisionGet {
                    profile_id,
                    revision,
                } => {
                    client
                        .call(
                            method::PROFILE_REVISION_GET,
                            json!({"profile_id":profile_id,"revision":revision}),
                        )
                        .await?
                }
                ProfileCommand::Rollback {
                    profile_id,
                    revision,
                    expected_revision,
                } => {
                    client
                        .call(
                            method::PROFILE_ROLLBACK,
                            json!({"profile_id":profile_id,"revision":revision,"expected_revision":expected_revision}),
                        )
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
                ProfileCommand::Validate { .. }
                | ProfileCommand::Pack { .. }
                | ProfileCommand::AnalyzeCapacity { .. }
                | ProfileCommand::Bundle { .. } => unreachable!(),
            },
            Command::Catalog { command } => match command {
                CatalogCommand::Status => {
                    client.call(method::CATALOG_STATUS, Value::Null).await?
                }
                CatalogCommand::Refresh => {
                    client.call(method::CATALOG_REFRESH, Value::Null).await?
                }
                CatalogCommand::List => client.call(method::CATALOG_LIST, Value::Null).await?,
                CatalogCommand::Install {
                    id,
                    version,
                    catalog_revision,
                    sha256,
                } => {
                    client
                        .call(
                            method::CATALOG_INSTALL,
                            json!({
                                "id": id,
                                "version": version,
                                "catalog_revision": catalog_revision,
                                "sha256": sha256,
                            }),
                        )
                        .await?
                }
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
            Command::Suite {
                command:
                    SuiteCommand::Evaluate {
                        path,
                        timings,
                        progress,
                        case_ids,
                        category,
                        fixtures,
                        scene,
                        overlay,
                        elements,
                        fail_fast,
                        detach,
                        no_cache,
                        output,
                    },
            } => {
                let path = std::fs::canonicalize(path)
                    .map_err(|error| CliError::Replay(format!("cannot open suite: {error}")))?;
                let operation_started = Instant::now();
                let started = client
                    .call(
                        method::SUITE_START,
                        json!({
                            "path":path,"timings":timings,"case_ids":case_ids,
                            "categories":category,"fixtures":fixtures,"scenes":scene,
                            "overlays":overlay,"elements":elements,"fail_fast":fail_fast,
                            "no_cache":no_cache,
                        }),
                    )
                    .await?;
                if *detach {
                    started
                } else {
                let operation_id = started["operation_id"].as_str().ok_or_else(|| {
                    CliError::Replay("suite start omitted operation_id".into())
                })?;
                let result = loop {
                    let status = client
                        .call(
                            method::SUITE_STATUS,
                            json!({"operation_id":operation_id,"result":true}),
                        )
                        .await?;
                    if *progress && !cli.json {
                        eprintln!(
                            "suite {}: {} {}/{} {}",
                            status["phase"].as_str().unwrap_or("unknown"),
                            status["completed"].as_u64().unwrap_or(0),
                            status["total"].as_u64().unwrap_or(0),
                            status["current_case"].as_str().unwrap_or(""),
                            status["elapsed_ms"].as_u64().unwrap_or(0),
                        );
                    }
                    match status["status"].as_str() {
                        Some("completed") => break status["result"].clone(),
                        Some("cancelled" | "failed") => {
                            return Err(CliError::Replay(status["error"].to_string()));
                        }
                        _ if operation_started.elapsed() >= Duration::from_millis(timeout_ms) => {
                            let _ = client
                                .call(
                                    method::SUITE_CANCEL,
                                    json!({"operation_id":operation_id}),
                                )
                                .await;
                            return Err(CliError::OperationTimeout(status.to_string()));
                        }
                        _ => tokio::time::sleep(Duration::from_millis(250)).await,
                    }
                };
                if let Some(output) = output {
                    let bytes = serde_json::to_vec_pretty(&result)
                        .map_err(|error| CliError::Replay(error.to_string()))?;
                    write_suite_result(output, &bytes)?;
                    json!({
                        "passed":result["passed"],
                        "summary":result["summary"],
                        "timings":result["timings"],
                        "output":output,
                    })
                } else {
                    result
                }
                }
            }
            Command::Suite {
                command:
                    SuiteCommand::Status {
                        operation_id,
                        result,
                    },
            } => {
                client
                    .call(
                        method::SUITE_STATUS,
                        json!({"operation_id":operation_id,"result":result}),
                    )
                    .await?
            }
            Command::Suite {
                command: SuiteCommand::Cancel { operation_id },
            } => {
                client
                    .call(
                        method::SUITE_CANCEL,
                        json!({"operation_id":operation_id}),
                    )
                    .await?
            }
            Command::Collection { command } => match command {
                CollectionCommand::PolicyGet { profile_id } => client
                    .call(method::COLLECTION_POLICY_GET, json!({"profile_id":profile_id}))
                    .await?,
                CollectionCommand::PolicySet {
                    profile_id,
                    dataset_root,
                    enabled,
                    interval_seconds,
                    jitter_seconds,
                    similarity_threshold,
                    maximum_pending_items,
                    maximum_bytes,
                    novelty_targets,
                } => {
                    let dataset_root = if dataset_root.is_absolute() {
                        dataset_root.clone()
                    } else {
                        std::env::current_dir()
                            .map_err(|error| CliError::Validation(error.to_string()))?
                            .join(dataset_root)
                    };
                    let policy = CollectionPolicy {
                        enabled: *enabled,
                        dataset_root,
                        interval_seconds: *interval_seconds,
                        jitter_seconds: *jitter_seconds,
                        similarity_threshold: *similarity_threshold,
                        maximum_pending_items: *maximum_pending_items,
                        maximum_bytes: *maximum_bytes,
                        novelty_targets: novelty_targets.clone(),
                    };
                    client
                        .call(
                            method::COLLECTION_POLICY_SET,
                            json!({"profile_id":profile_id,"policy":policy}),
                        )
                        .await?
                }
                CollectionCommand::Status { profile_id } => client
                    .call(method::COLLECTION_STATUS, json!({"profile_id":profile_id}))
                    .await?,
                CollectionCommand::Items { profile_id, status } => client
                    .call(
                        method::COLLECTION_ITEMS,
                        json!({"profile_id":profile_id,"status":status}),
                    )
                    .await?,
                CollectionCommand::Get { profile_id, id } => client
                    .call(
                        method::COLLECTION_ITEM_GET,
                        json!({"profile_id":profile_id,"id":id}),
                    )
                    .await?,
                CollectionCommand::Review {
                    profile_id,
                    id,
                    action,
                    reason,
                    expected,
                } => {
                    let expected_observations = expected
                        .as_ref()
                        .map(|path| {
                            std::fs::read(path)
                                .map_err(|error| CliError::Validation(error.to_string()))
                                .and_then(|bytes| {
                                    serde_json::from_slice::<Value>(&bytes)
                                        .map_err(|error| CliError::Validation(error.to_string()))
                                })
                        })
                        .transpose()?
                        .unwrap_or_else(|| json!({}));
                    client
                        .call(
                            method::COLLECTION_REVIEW,
                            json!({
                                "profile_id":profile_id,"id":id,"action":action,
                                "reason":reason,"expected_observations":expected_observations
                            }),
                        )
                        .await?
                }
                CollectionCommand::AutoReview {
                    profile_id,
                    promote,
                } => client
                    .call(
                        method::COLLECTION_AUTO_REVIEW,
                        json!({"profile_id":profile_id,"promote":promote}),
                    )
                    .await?,
                CollectionCommand::Compare {
                    first,
                    second,
                    threshold,
                } => {
                    let first = std::fs::canonicalize(first)
                        .map_err(|error| CliError::Validation(error.to_string()))?;
                    let second = std::fs::canonicalize(second)
                        .map_err(|error| CliError::Validation(error.to_string()))?;
                    client
                        .call(
                            method::COLLECTION_COMPARE,
                            json!({"first":first,"second":second,"threshold":threshold}),
                        )
                    .await?
                }
            },
            Command::Output { command } => match command {
                OutputCommand::List { profile_id } => client
                    .call(method::OUTPUT_LIST, json!({"profile_id":profile_id}))
                    .await?,
                OutputCommand::Set { profile_id, route } => {
                    let route = serde_json::from_slice::<OutputRoute>(&std::fs::read(route).map_err(
                        |error| CliError::Validation(format!("read output route: {error}")),
                    )?)
                    .map_err(|error| {
                        CliError::Validation(format!("parse output route JSON: {error}"))
                    })?;
                    client
                        .call(
                            method::OUTPUT_SET,
                            json!({"profile_id":profile_id,"route":route}),
                        )
                        .await?
                }
                OutputCommand::Enable {
                    profile_id,
                    route_id,
                    enabled,
                } => {
                    client
                        .call(
                            method::OUTPUT_ENABLE,
                            json!({"profile_id":profile_id,"route_id":route_id,"enabled":enabled}),
                        )
                        .await?
                }
                OutputCommand::Remove {
                    profile_id,
                    route_id,
                } => {
                    client
                        .call(
                            method::OUTPUT_REMOVE,
                            json!({"profile_id":profile_id,"route_id":route_id}),
                        )
                        .await?
                }
                OutputCommand::Test {
                    profile_id,
                    route_id,
                } => {
                    client
                        .call(
                            method::OUTPUT_TEST,
                            json!({"profile_id":profile_id,"route_id":route_id}),
                        )
                        .await?
                }
            },
            Command::Diagnostic { command } => match command {
                DiagnosticCommand::Plan {
                    profile_id,
                    element_ids,
                } => {
                    client
                        .call(
                            method::DIAGNOSTIC_PLAN,
                            json!({"profile_id":profile_id,"selected_element_ids":element_ids}),
                        )
                        .await?
                }
                DiagnosticCommand::Export {
                    path,
                    profile_id,
                    element_ids,
                    expected_total_bytes,
                    privacy_reviewed,
                } => client
                    .call(
                        method::DIAGNOSTIC_EXPORT,
                        json!({
                            "path":path,
                            "bundle":{"profile_id":profile_id,"selected_element_ids":element_ids},
                            "privacy_reviewed":privacy_reviewed,
                            "expected_total_uncompressed_bytes":expected_total_bytes,
                        }),
                    )
                    .await?,
            },
        };
    Ok(value)
}

fn write_suite_result(path: &std::path::Path, bytes: &[u8]) -> Result<(), CliError> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| CliError::Replay(format!("cannot create suite result: {error}")))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| CliError::Replay(format!("cannot write suite result: {error}")))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| CliError::Replay(format!("cannot install suite result: {error}")))
}

/// Runs the read-only capacity report while preserving normal profile admission.
///
/// `load_profile` intentionally rejects profiles with more than 512 elements. The
/// analyzer is also useful for deciding how to reduce such a profile, so this path
/// permits only that one known validation error and deserializes the same JSON once
/// more without calling `Profile::validate`. It never writes, activates, packs, or
/// otherwise makes the over-limit profile usable.
fn analyze_capacity_path(path: &std::path::Path) -> Result<Value, CliError> {
    let profile = match load_profile(path) {
        Ok(profile) => profile,
        Err(error) if is_element_capacity_only(&error) => load_profile_for_capacity_analysis(path)
            .map_err(|error| {
                CliError::Validation(format!("cannot inspect over-limit profile: {error}"))
            })?,
        Err(error) => return Err(CliError::Validation(error.to_string())),
    };

    let asset_root = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut report = profile.analyze_capacity_with_asset_root(asset_root);
    if report.elements.used > report.elements.maximum {
        let excess = report.elements.used - report.elements.maximum;
        report.suggestions.insert(
            0,
            format!(
                "analysis only: profile has {} detector element(s) over the {}-element admission limit; normal load, save, pack, and activation still reject it; remove, consolidate, or split {} element(s)",
                report.elements.used,
                report.elements.maximum,
                excess,
            ),
        );
    }
    serde_json::to_value(report).map_err(|error| CliError::Validation(error.to_string()))
}

fn is_element_capacity_only(error: &StorageError) -> bool {
    let StorageError::Validation(errors) = error else {
        return false;
    };
    errors.0.len() == 1
        && errors.0[0].path == "elements"
        && errors.0[0].message.contains("detector elements requested")
        && errors.0[0].message.contains("capacity limit")
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
        CaptureCommand::AutoGet => client.call(method::CAPTURE_AUTO_GET, Value::Null).await,
        CaptureCommand::AutoSet {
            profile_id,
            process_match,
            enabled,
        } => client
            .call(
                method::CAPTURE_AUTO_SET,
                json!({"profile_id":profile_id,"process_match":process_match,"enabled":enabled}),
            )
            .await,
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
        Command::Profile {
            command: ProfileCommand::Pack { .. },
        } => format!(
            "packed profile {} ({} files)",
            value["profile_id"].as_str().unwrap_or("?"),
            value["files"].as_u64().unwrap_or(0)
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
    #[error("suite operation timed out: {0}")]
    OperationTimeout(String),
}

impl CliError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Client(ClientError::Io(_) | ClientError::Disconnected) => 3,
            Self::Client(ClientError::Timeout) | Self::OperationTimeout(_) => 6,
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
    use yash_app_events_profile::{Detector, DetectorId, Element, ElementId, NormalizedRegion};

    #[test]
    fn analyze_command_requires_image_and_profile() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "--json",
            "analyze",
            "/tmp/frame.png",
            "--profile-id",
            "00000000-0000-0000-0000-000000000001",
        ])
        .unwrap();
        let Command::Analyze {
            image,
            profile_id,
            profile_bundle,
            ..
        } = cli.command
        else {
            panic!("analyze command was not parsed");
        };
        assert_eq!(image, PathBuf::from("/tmp/frame.png"));
        assert_eq!(
            profile_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(profile_bundle, None);
    }

    #[test]
    fn analyze_command_accepts_a_profile_bundle_alias() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "analyze",
            "/tmp/frame.png",
            "--bundle",
            "/tmp/queen.profile-bundle.json",
        ])
        .unwrap();
        let Command::Analyze {
            profile_id,
            profile_bundle,
            ..
        } = cli.command
        else {
            panic!("analyze command was not parsed");
        };
        assert_eq!(profile_id, None);
        assert_eq!(
            profile_bundle,
            Some(PathBuf::from("/tmp/queen.profile-bundle.json"))
        );
    }

    #[test]
    fn analyze_command_accepts_bounded_context_json() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "analyze",
            "/tmp/frame.png",
            "--profile-id",
            "00000000-0000-0000-0000-000000000001",
            "--context-json",
            r#"{"expected_scene_ids":[]}"#,
        ])
        .unwrap();
        let Command::Analyze { context_json, .. } = cli.command else {
            panic!("analyze command was not parsed");
        };
        assert_eq!(
            context_json.as_deref(),
            Some(r#"{"expected_scene_ids":[]}"#)
        );
        assert_eq!(
            parse_analysis_context_json(context_json.as_deref().unwrap()).unwrap(),
            json!({"expected_scene_ids": []})
        );
    }

    #[test]
    fn analysis_context_json_must_be_a_small_object() {
        assert!(parse_analysis_context_json("[]").is_err());
        assert!(parse_analysis_context_json("not-json").is_err());
        assert!(parse_analysis_context_json(&"x".repeat(16 * 1024 + 1)).is_err());
    }

    #[test]
    fn analyze_command_rejects_missing_or_conflicting_profile_selector() {
        assert!(Cli::try_parse_from(["yash-eventsctl", "analyze", "/tmp/frame.png"]).is_err());
        assert!(Cli::try_parse_from([
            "yash-eventsctl",
            "analyze",
            "/tmp/frame.png",
            "--profile-id",
            "00000000-0000-0000-0000-000000000001",
            "--profile-bundle",
            "/tmp/bundle.json",
        ])
        .is_err());
    }

    #[test]
    fn catalog_install_requires_reviewed_revision_and_hash() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "catalog",
            "install",
            "blazblue-entropy-effect.stage-tracker",
            "1.0.0",
            "--catalog-revision",
            "7",
            "--sha256",
            &"a".repeat(64),
        ])
        .unwrap();
        let Command::Catalog {
            command:
                CatalogCommand::Install {
                    catalog_revision,
                    sha256,
                    ..
                },
        } = cli.command
        else {
            panic!("catalog install command was not parsed");
        };
        assert_eq!(catalog_revision, 7);
        assert_eq!(sha256.len(), 64);
    }

    #[test]
    fn diagnostic_export_requires_explicit_review_inputs() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "diagnostic",
            "export",
            "/tmp/diagnostic.zip",
            "--expected-total-bytes",
            "123",
            "--privacy-reviewed",
        ])
        .unwrap();
        let Command::Diagnostic {
            command:
                DiagnosticCommand::Export {
                    expected_total_bytes,
                    privacy_reviewed,
                    ..
                },
        } = cli.command
        else {
            panic!("diagnostic command was not parsed");
        };
        assert_eq!(expected_total_bytes, 123);
        assert!(privacy_reviewed);
    }
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
            cache_root: directory.join("cache"),
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
            command: ProfileCommand::Revisions {
                profile_id: profile_id.clone(),
            },
        };
        let revisions = execute(&cli).await.unwrap();
        assert_eq!(revisions.as_array().unwrap().len(), 1);
        cli.command = Command::Profile {
            command: ProfileCommand::RevisionGet {
                profile_id: profile_id.clone(),
                revision: 0,
            },
        };
        assert_eq!(execute(&cli).await.unwrap()["revision"], 0);
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
        cli.command = Command::Collection {
            command: CollectionCommand::PolicySet {
                profile_id: profile_id.clone(),
                dataset_root: directory.path().join("dataset"),
                enabled: true,
                interval_seconds: 70,
                jitter_seconds: 10,
                similarity_threshold: 0.015,
                maximum_pending_items: 100,
                maximum_bytes: 10 * 1024 * 1024,
                novelty_targets: vec!["stage".into()],
            },
        };
        assert_eq!(execute(&cli).await.unwrap()["policy"]["enabled"], true);
        cli.command = Command::Collection {
            command: CollectionCommand::AutoReview {
                profile_id: profile_id.clone(),
                promote: true,
            },
        };
        assert_eq!(execute(&cli).await.unwrap()["processed"], 0);
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
        let mut cli = Cli {
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

        let bundle_path = directory.path().join("bundle.json");
        std::fs::write(
            &bundle_path,
            serde_json::to_vec(&json!({
                "schema": 1,
                "name": "Demo control",
                "game": "demo_game",
                "router_profile_id": profile.id,
                "router_profile_revision": profile.revision,
                "members": [{
                    "profile_id": profile.id,
                    "profile_revision": profile.revision,
                    "fallback": true
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        cli.command = Command::Profile {
            command: ProfileCommand::Bundle {
                command: ProfileBundleCommand::Validate { path: bundle_path },
            },
        };
        let result = execute(&cli).await.unwrap();
        assert_eq!(result["valid"], true);
        assert_eq!(result["members"], 1);
    }

    fn write_over_limit_profile(path: &Path, name: &str) -> Profile {
        let mut profile = Profile::new(name, "demo_game", 1920, 1080);
        for index in 0..513 {
            profile.elements.push(Element {
                id: ElementId::new(),
                name: format!("element-{index}"),
                enabled: true,
                color: "#fff".into(),
                region: NormalizedRegion {
                    x: 0.1,
                    y: 0.1,
                    width: 0.2,
                    height: 0.1,
                },
                detector: Detector::RegionChange {
                    id: DetectorId::new(),
                    threshold: 0.1,
                    preprocessing: Vec::new(),
                },
            });
        }
        std::fs::write(path, serde_json::to_vec_pretty(&profile).unwrap()).unwrap();
        profile
    }

    #[tokio::test]
    async fn analyze_capacity_inspects_over_limit_profile_without_admitting_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("over-limit.json");
        let profile = write_over_limit_profile(&path, "Overflow");
        let mut cli = Cli {
            json: true,
            socket: Some(directory.path().join("missing.sock")),
            timeout_ms: 1,
            command: Command::Profile {
                command: ProfileCommand::AnalyzeCapacity { path: path.clone() },
            },
        };

        let report = execute(&cli).await.unwrap();
        assert_eq!(report["elements"]["used"], 513);
        assert_eq!(report["elements"]["maximum"], 512);
        assert_eq!(report["elements"]["warning"], "limit_reached");
        assert_eq!(report["always_evaluated_anchor_count"], 0);
        assert_eq!(report["always_evaluated_anchor_cost"], 0);
        assert!(report["suggestions"][0]
            .as_str()
            .unwrap()
            .contains("analysis only"));

        cli.command = Command::Profile {
            command: ProfileCommand::Validate { path },
        };
        let validation = execute(&cli).await.unwrap_err();
        assert!(matches!(validation, CliError::Validation(_)));
        assert_eq!(profile.elements.len(), 513);
    }

    #[tokio::test]
    async fn analyze_capacity_resolves_relative_template_assets_from_profile_directory() {
        let directory = tempfile::tempdir().unwrap();
        let templates = directory.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        std::fs::write(templates.join("first.png"), b"same template bytes").unwrap();
        std::fs::write(templates.join("second.png"), b"same template bytes").unwrap();

        let mut profile = Profile::new("Templates", "demo_game", 1920, 1080);
        profile.elements = ["first", "second"]
            .into_iter()
            .map(|name| Element {
                id: ElementId::new(),
                name: name.into(),
                enabled: true,
                color: "#fff".into(),
                region: NormalizedRegion {
                    x: 0.1,
                    y: 0.1,
                    width: 0.2,
                    height: 0.1,
                },
                detector: Detector::Template {
                    id: DetectorId::new(),
                    templates: vec![format!("templates/{name}.png").into()],
                    masks: Vec::new(),
                    threshold: 0.9,
                    preprocessing: Vec::new(),
                },
            })
            .collect();
        let path = directory.path().join("profile.json");
        std::fs::write(path.clone(), serde_json::to_vec_pretty(&profile).unwrap()).unwrap();

        let cli = Cli {
            json: true,
            socket: Some(directory.path().join("missing.sock")),
            timeout_ms: 1,
            command: Command::Profile {
                command: ProfileCommand::AnalyzeCapacity { path },
            },
        };

        let report = execute(&cli).await.unwrap();
        assert_eq!(report["elements"]["used"], 2);
        assert_eq!(
            report["duplicate_detector_groups"],
            json!([["first", "second"]])
        );
    }

    #[tokio::test]
    async fn analyze_capacity_does_not_bypass_other_validation_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid-over-limit.json");
        write_over_limit_profile(&path, "");
        let cli = Cli {
            json: true,
            socket: Some(directory.path().join("missing.sock")),
            timeout_ms: 1,
            command: Command::Profile {
                command: ProfileCommand::AnalyzeCapacity { path },
            },
        };

        let error = execute(&cli).await.unwrap_err();
        assert!(matches!(
            error,
            CliError::Validation(message) if message.contains("2 error(s)")
        ));
    }

    #[tokio::test]
    async fn offline_pack_builds_a_valid_archive_without_a_socket() {
        let directory = tempfile::tempdir().unwrap();
        let profile_directory = directory.path().join("profile");
        std::fs::create_dir(&profile_directory).unwrap();
        let profile = Profile::new("Packable", "demo_game", 558, 992);
        yash_app_events_profile::save_profile(&profile_directory.join("profile.json"), &profile)
            .unwrap();
        let archive = directory.path().join("profile.hudprofile");
        let cli = Cli {
            json: true,
            socket: Some(directory.path().join("missing.sock")),
            timeout_ms: 1,
            command: Command::Profile {
                command: ProfileCommand::Pack {
                    directory: profile_directory,
                    path: archive.clone(),
                },
            },
        };
        let result = execute(&cli).await.unwrap();
        assert_eq!(result["packed"], true);
        assert_eq!(result["profile_id"], profile.id.to_string());
        assert!(archive.is_file());
    }

    #[test]
    fn exit_codes_are_stable_by_failure_category() {
        assert_eq!(CliError::Client(ClientError::Timeout).exit_code(), 6);
        assert_eq!(CliError::Client(ClientError::Disconnected).exit_code(), 3);
        assert_eq!(CliError::Validation("bad".into()).exit_code(), 5);
        assert_eq!(CliError::FollowRequiresStream.exit_code(), 4);
        assert_eq!(
            CliError::OperationTimeout("phase=cases".into()).exit_code(),
            6
        );
    }

    #[test]
    fn external_suite_command_accepts_any_package_path() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "suite",
            "evaluate",
            "/tmp/blazblue-entropy-effect",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Suite {
                command: SuiteCommand::Evaluate { .. }
            }
        ));
    }

    #[test]
    fn external_suite_timings_are_explicitly_opt_in() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "suite",
            "evaluate",
            "/tmp/game-suite",
            "--timings",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Suite {
                command: SuiteCommand::Evaluate { timings: true, .. }
            }
        ));
    }

    #[test]
    fn external_suite_focused_detached_controls_are_scriptable() {
        let cli = Cli::try_parse_from([
            "yash-eventsctl",
            "suite",
            "evaluate",
            "/tmp/game-suite",
            "--case",
            "coin-trial",
            "--category",
            "authoring",
            "--fail-fast",
            "--detach",
            "--no-cache",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Suite {
                command: SuiteCommand::Evaluate {
                    fail_fast: true,
                    detach: true,
                    no_cache: true,
                    ..
                }
            }
        ));
        assert!(Cli::try_parse_from([
            "yash-eventsctl",
            "suite",
            "status",
            "00000000-0000-4000-8000-000000000001",
            "--result",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "yash-eventsctl",
            "suite",
            "cancel",
            "00000000-0000-4000-8000-000000000001",
        ])
        .is_ok());
    }

    #[test]
    fn suite_result_output_replaces_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.json");
        std::fs::write(&path, b"old").unwrap();
        write_suite_result(&path, br#"{"passed":true}"#).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"passed":true}"#);
        assert!(!path
            .with_extension(format!("tmp-{}", std::process::id()))
            .exists());
    }

    #[test]
    fn collection_policy_and_batch_review_commands_are_scriptable() {
        let policy = Cli::try_parse_from([
            "yash-eventsctl",
            "collection",
            "policy-set",
            "00000000-0000-0000-0000-000000000001",
            "/tmp/game-package",
            "--interval-seconds",
            "70",
            "--novelty-target",
            "stage",
        ])
        .unwrap();
        assert!(matches!(
            policy.command,
            Command::Collection {
                command: CollectionCommand::PolicySet { .. }
            }
        ));
        let review = Cli::try_parse_from([
            "yash-eventsctl",
            "collection",
            "auto-review",
            "00000000-0000-0000-0000-000000000001",
            "--promote",
        ])
        .unwrap();
        assert!(matches!(
            review.command,
            Command::Collection {
                command: CollectionCommand::AutoReview { promote: true, .. }
            }
        ));
    }

    #[test]
    fn output_routes_have_scriptable_set_enable_and_test_commands() {
        let set = Cli::try_parse_from([
            "yash-eventsctl",
            "output",
            "set",
            "00000000-0000-0000-0000-000000000001",
            "stage-marker.json",
        ])
        .unwrap();
        assert!(matches!(
            set.command,
            Command::Output {
                command: OutputCommand::Set { .. }
            }
        ));
        let test = Cli::try_parse_from([
            "yash-eventsctl",
            "output",
            "test",
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ])
        .unwrap();
        assert!(matches!(
            test.command,
            Command::Output {
                command: OutputCommand::Test { .. }
            }
        ));
    }
}
