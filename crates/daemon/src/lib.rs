//! State-owning daemon and bounded JSON-RPC Unix transport.

mod collection;
mod routes;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, TimeZone as _, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, oneshot, Notify};
use uuid::Uuid;
use yash_app_events_capture::LatestFrameSlot;
use yash_app_events_capture::{Frame, FrameLayout, PixelFormat, ReplaySource};
use yash_app_events_capture_pw::{PortalCapture, PortalOptions, SourceSelection};
use yash_app_events_catalog::{Catalog, CatalogError, CatalogService};
use yash_app_events_engine::collection::{
    CollectedFrame, CollectionItem, CollectionReason, ReviewRecord, ReviewStatus,
};
use yash_app_events_engine::suite::{
    ExpectedObservation, ExpectedValue, FramePlacement, RegressionCase, RegressionSuite, SuiteFrame,
};
use yash_app_events_engine::{
    evaluate_replay, normalized_to_pixels, AnalysisScheduler, CompositeRule, CompositeRuleConfig,
    FrameProcessor, NoopRule, NumericRule, NumericRuleConfig, Observation, ObservationRule,
    ObservationValue, ProcessedFrame, ReplayManifest, ReplayRegression, TemporalRule,
    TemporalRuleConfig, Transition, TransitionState, ValuePredicate,
};
use yash_app_events_output::{
    execute_route, export_diagnostic_bundle, plan_diagnostic_bundle, render_payload,
    DiagnosticBundle, DiagnosticCrop, DiagnosticLimits, EventRecord, EventState, OutputConfig,
    OutputFormat, OutputRecipeSource, OutputRoute, OutputSink, OutputTrigger, OutputWriter,
    RouteContext, StateSnapshot,
};
#[cfg(test)]
use yash_app_events_output::{FileMode, OutputRecipe, OutputRecipeSink};
use yash_app_events_profile::{
    export_profile, import_profile, load_profile, BarDirection, CollectionPolicy,
    Detector as ProfileDetector, DetectorId, ElementId, ImportLimits, LocalConfig,
    NormalizedRegion, Profile, ProfileId, ProfileStore, RuleId, RulePredicate, StoreError,
};
use yash_app_events_protocol::{
    error_code, method, nesting_within_limit, HandshakeParams, HandshakeResult, Notification,
    Request, RequestId, Response, RpcError, Status, MAXIMUM_MESSAGE_BYTES, MAXIMUM_NESTING_DEPTH,
    PROTOCOL_VERSION,
};
use yash_app_events_vision::{
    grayscale_crop, ClassifierConfig, ColorBarConfig, ColorBarDetector, Detection,
    Detector as VisionDetector, GrayImage, OcrConfig, OcrDetector, OnnxClassifierDetector,
    PreprocessPipeline, RegionChangeConfig, RegionChangeDetector, SevenSegmentConfig,
    SevenSegmentDetector, Template, TemplateConfig, TemplateDetector,
};

const SUBSCRIPTION_CAPACITY: usize = 64;

/// Configuration for one daemon instance.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub data_root: PathBuf,
    pub config_root: PathBuf,
    pub state_root: PathBuf,
    pub cache_root: PathBuf,
    pub maximum_connections: usize,
}

/// Running server state shared by short-lived connection tasks.
#[derive(Debug)]
struct State {
    instance: Uuid,
    profiles: ProfileStore,
    local_config: LocalConfig,
    catalog: CatalogService,
    connected: AtomicUsize,
    maximum_connections: usize,
    shutdown: Notify,
    notifications: broadcast::Sender<Value>,
    sequence: AtomicU64,
    output: Mutex<OutputWriter>,
    latest_snapshot: Mutex<Value>,
    output_error: Mutex<Option<String>>,
    latest_frame: LatestFrameSlot,
    capture: Mutex<Option<PortalCapture>>,
    analysis: Mutex<Option<LiveAnalysis>>,
    analysis_metrics: Mutex<AnalysisMetrics>,
    preview_clients: AtomicUsize,
    diagnostic_logs: Mutex<VecDeque<Value>>,
    collector: Mutex<collection::Runtime>,
    output_routes: routes::Router,
}

#[derive(Debug)]
struct LiveAnalysis {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Default)]
struct AnalysisMetrics {
    frames: u64,
    first: Option<Instant>,
    last: Option<Instant>,
    last_processing_latency: Option<Duration>,
    detector_errors: u64,
}

#[derive(Debug)]
struct PreviewLease {
    state: Arc<State>,
    active: bool,
    frozen: Option<Arc<Frame>>,
}

impl PreviewLease {
    fn new(state: Arc<State>) -> Self {
        Self {
            state,
            active: false,
            frozen: None,
        }
    }
    fn start(&mut self) {
        if !self.active {
            self.state.preview_clients.fetch_add(1, Ordering::Relaxed);
            self.active = true;
        }
    }
    fn stop(&mut self) {
        if self.active {
            self.state.preview_clients.fetch_sub(1, Ordering::Relaxed);
            self.active = false;
            self.frozen = None;
        }
    }
    fn freeze(&mut self) -> bool {
        self.frozen = self.state.latest_frame.latest();
        self.frozen.is_some()
    }
    fn unfreeze(&mut self) {
        self.frozen = None;
    }
    fn frozen(&self) -> Option<Arc<Frame>> {
        self.frozen.clone()
    }
}

impl Drop for PreviewLease {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs the daemon until a graceful-shutdown RPC or process cancellation.
///
/// # Errors
///
/// Returns socket setup or accept failures. Per-client protocol failures remain isolated.
#[allow(clippy::too_many_lines)]
pub async fn run(config: ServerConfig) -> Result<(), ServerError> {
    let listener = bind_socket(&config.socket_path).await?;
    let (notifications, _) = broadcast::channel(SUBSCRIPTION_CAPACITY);
    let instance = Uuid::new_v4();
    let initial_state = serde_json::to_value(StateSnapshot {
        schema: 1,
        daemon_instance: instance,
        sequence: 0,
        updated_at: EventRecord::timestamp_rfc3339(Utc::now()),
        capture: json!({"active":false,"source":null}),
        active_profile: None,
        observations: json!({}),
        events: json!({}),
    })?;
    let output_routes = routes::Router::spawn(
        LocalConfig::new(config.config_root.clone()),
        notifications.clone(),
    );
    let state = Arc::new(State {
        instance,
        profiles: ProfileStore::new(config.data_root, 20),
        local_config: LocalConfig::new(config.config_root),
        catalog: CatalogService::new(config.cache_root)?,
        connected: AtomicUsize::new(0),
        maximum_connections: config.maximum_connections,
        shutdown: Notify::new(),
        notifications,
        sequence: AtomicU64::new(0),
        output: Mutex::new(OutputWriter::new(
            config.state_root,
            OutputConfig::default(),
        )),
        latest_snapshot: Mutex::new(initial_state),
        output_error: Mutex::new(None),
        latest_frame: LatestFrameSlot::default(),
        capture: Mutex::new(None),
        analysis: Mutex::new(None),
        analysis_metrics: Mutex::new(AnalysisMetrics::default()),
        preview_clients: AtomicUsize::new(0),
        diagnostic_logs: Mutex::new(VecDeque::from([json!({
            "level":"info","message":"daemon started","daemon_instance":instance
        })])),
        collector: Mutex::new(collection::Runtime::default()),
        output_routes,
    });
    let monitor_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut previous = None;
        let mut stable_samples = 0_u8;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Ok(settings) = monitor_state.local_config.load_settings() else {
                continue;
            };
            let Some(profile_id) = settings.active_profile else {
                continue;
            };
            let Ok(Some(binding)) = monitor_state.local_config.capture_binding(profile_id) else {
                continue;
            };
            if !binding.auto_capture {
                previous = None;
                stable_samples = 0;
                continue;
            }
            let running = binding
                .process_match
                .as_deref()
                .is_some_and(process_running);
            if previous == Some(running) {
                stable_samples = stable_samples.saturating_add(1);
            } else {
                previous = Some(running);
                stable_samples = 1;
            }
            if stable_samples < 2 {
                continue;
            }
            let active = monitor_state
                .capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some();
            if running && !active {
                let _ = select_capture(
                    &monitor_state,
                    CaptureSelectParams {
                        source: Some("window_or_monitor".into()),
                        profile_id: Some(profile_id),
                    },
                )
                .await;
            } else if !running && active {
                let _ = stop_capture(&monitor_state).await;
            }
        }
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
    let mut preview = PreviewLease::new(Arc::clone(&state));
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
        if request.method == method::PREVIEW_START {
            preview.start();
        }
        if request.method == method::PREVIEW_STOP {
            preview.stop();
        }
        if request.method == method::PREVIEW_FREEZE {
            preview.freeze();
        }
        if request.method == method::PREVIEW_UNFREEZE {
            preview.unfreeze();
        }
        if request.method == method::STATUS_SUBSCRIBE || request.method == method::EVENTS_SUBSCRIBE
        {
            let receiver = state.notifications.subscribe();
            write_response(
                &mut writer,
                &Response::success(
                    request.id,
                    json!({"subscribed": true, "capacity": SUBSCRIPTION_CAPACITY}),
                ),
            )
            .await?;
            return serve_subscription(&mut writer, receiver).await;
        }
        let response = dispatch(request, &state, preview.frozen()).await;
        write_response(&mut writer, &response).await?;
    }
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    request: Request,
    state: &Arc<State>,
    frozen_frame: Option<Arc<Frame>>,
) -> Response {
    let id = request.id;
    let result: Result<Value, RpcError> = match request.method.as_str() {
        method::VERSION => {
            Ok(json!({"version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL_VERSION}))
        }
        method::CAPABILITIES => Ok(
            json!({"profiles":true,"profile_catalog":true,"profile_revisions":true,"subscriptions":true,"capture":true,"preview":true,"diagnostic_bundle":true,"output_routes":true,"output_recipes":true,"detectors":["color_bar","template","region_change","ocr","seven_segment","classifier"]}),
        ),
        method::STATUS => serde_json::to_value(status(state)).map_err(internal_error),
        method::SHUTDOWN => {
            let stopped = stop_capture(state).await;
            state.shutdown.notify_one();
            stopped.map(|_| json!({"shutting_down": true}))
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
        method::PROFILE_REVISIONS => parse::<ProfileIdParam>(request.params)
            .and_then(|params| {
                state
                    .profiles
                    .list_revisions(params.profile_id)
                    .map_err(store_error)
            })
            .and_then(|profiles| serde_json::to_value(profiles).map_err(internal_error)),
        method::PROFILE_REVISION_GET => parse::<RevisionParams>(request.params)
            .and_then(|params| {
                state
                    .profiles
                    .load_revision(params.profile_id, params.revision)
                    .map_err(store_error)
            })
            .and_then(|profile| serde_json::to_value(profile).map_err(internal_error)),
        method::PROFILE_ROLLBACK => parse::<RollbackParams>(request.params)
            .and_then(|params| {
                state
                    .profiles
                    .rollback(params.profile_id, params.revision, params.expected_revision)
                    .map_err(store_error)
            })
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
        method::OUTPUT_LIST => parse::<ProfileIdParam>(request.params)
            .and_then(|params| {
                state
                    .local_config
                    .output_routes(params.profile_id)
                    .map_err(internal_error)
            })
            .and_then(|routes| serde_json::to_value(routes).map_err(internal_error)),
        method::OUTPUT_SET => parse::<OutputSetParams>(request.params)
            .and_then(|mut params| {
                params.route.source_recipe = None;
                state
                    .local_config
                    .set_output_route(params.profile_id, params.route)
                    .map_err(internal_error)
            })
            .and_then(|routes| serde_json::to_value(routes).map_err(internal_error)),
        method::OUTPUT_ENABLE => parse::<OutputEnableParams>(request.params)
            .and_then(|params| {
                state
                    .local_config
                    .set_output_enabled(params.profile_id, params.route_id, params.enabled)
                    .map_err(internal_error)
            })
            .and_then(|routes| serde_json::to_value(routes).map_err(internal_error)),
        method::OUTPUT_REMOVE => parse::<OutputRouteParams>(request.params).and_then(|params| {
            state
                .local_config
                .remove_output_route(params.profile_id, params.route_id)
                .map(|removed| json!({"removed":removed}))
                .map_err(internal_error)
        }),
        method::OUTPUT_TEST => match parse::<OutputRouteParams>(request.params) {
            Ok(params) => test_output_route(state, params).await,
            Err(error) => Err(error),
        },
        method::OUTPUT_RECIPE_LIST => parse::<ProfileIdParam>(request.params)
            .and_then(|params| {
                state
                    .profiles
                    .output_recipes(params.profile_id)
                    .map_err(store_error)
            })
            .and_then(|recipes| serde_json::to_value(recipes).map_err(internal_error)),
        method::OUTPUT_RECIPE_PREVIEW => parse::<OutputRecipeDraftParams>(request.params)
            .and_then(|params| preview_output_recipe(state, &params)),
        method::OUTPUT_RECIPE_INSTALL => parse::<OutputRecipeInstallParams>(request.params)
            .and_then(|params| install_output_recipe(state, params)),
        method::PROFILE_DRAFT => parse::<ProfileParam>(request.params).and_then(|params| {
            state
                .profiles
                .save_draft(&params.profile)
                .map(|()| json!({"saved":true,"revision":params.profile.revision}))
                .map_err(store_error)
        }),
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
        method::CATALOG_STATUS => state
            .catalog
            .status()
            .and_then(|status| serde_json::to_value(status).map_err(CatalogError::from))
            .map_err(catalog_error),
        method::CATALOG_LIST => catalog_listing(state).map_err(catalog_error),
        method::CATALOG_REFRESH => match state.catalog.refresh().await {
            Ok(catalog) => catalog_listing_value(state, &catalog).map_err(catalog_error),
            Err(error) => Err(catalog_error(error)),
        },
        method::CATALOG_INSTALL => match parse::<CatalogInstallParams>(request.params) {
            Ok(params) => install_catalog_profile(state, params).await,
            Err(error) => Err(error),
        },
        method::STATE_GET => Ok(state
            .latest_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()),
        method::REPLAY_SYNTHETIC_HEALTH => parse::<SyntheticReplayParams>(request.params)
            .and_then(|params| run_synthetic_health(state, params)),
        method::DETECTOR_TEST => parse::<DetectorTestParams>(request.params)
            .and_then(|params| test_detector(state, &params, frozen_frame)),
        method::DETECTOR_CAPTURE_TEMPLATE => parse::<CaptureTemplateParams>(request.params)
            .and_then(|params| capture_template(state, &params)),
        method::REPLAY_PROFILE_DETECTOR => parse::<ProfileReplayParams>(request.params)
            .and_then(|params| run_profile_replay(state, &params)),
        method::REPLAY_EVALUATE => parse::<ReplayManifest>(request.params)
            .and_then(|manifest| evaluate_profile_replay(state, &manifest)),
        method::SUITE_EVALUATE => parse::<PathParam>(request.params)
            .and_then(|params| evaluate_regression_suite(state, &params.path)),
        method::COLLECTION_POLICY_GET => parse::<ProfileIdParam>(request.params)
            .and_then(|params| collection_policy_get(state, params.profile_id)),
        method::COLLECTION_POLICY_SET => parse::<CollectionPolicyParams>(request.params)
            .and_then(|params| collection_policy_set(state, &params)),
        method::COLLECTION_STATUS => parse::<ProfileIdParam>(request.params)
            .and_then(|params| collection_status(state, params.profile_id)),
        method::COLLECTION_ITEMS => parse::<CollectionItemsParams>(request.params)
            .and_then(|params| collection_items(state, &params)),
        method::COLLECTION_ITEM_GET => parse::<CollectionItemParams>(request.params)
            .and_then(|params| collection_item_get(state, &params)),
        method::COLLECTION_REVIEW => parse::<CollectionReviewParams>(request.params)
            .and_then(|params| collection_review(state, params)),
        method::COLLECTION_AUTO_REVIEW => parse::<CollectionAutoReviewParams>(request.params)
            .and_then(|params| collection_auto_review(state, &params)),
        method::COLLECTION_COMPARE => parse::<CollectionCompareParams>(request.params)
            .and_then(|params| collection_compare(&params)),
        method::CAPTURE_SELECT => match parse::<CaptureSelectParams>(request.params) {
            Ok(params) => select_capture(state, params).await,
            Err(error) => Err(error),
        },
        method::CAPTURE_START => Ok({
            let mut status = capture_status(state);
            status["started"] = json!(status["active"].as_bool().unwrap_or(false));
            status
        }),
        method::CAPTURE_STOP => stop_capture(state).await,
        method::CAPTURE_STATUS => Ok(capture_status(state)),
        method::CAPTURE_AUTO_GET => auto_capture_status(state),
        method::CAPTURE_AUTO_SET => parse::<AutoCaptureParams>(request.params)
            .and_then(|params| set_auto_capture(state, params)),
        method::CAPTURE_SNAPSHOT => parse::<PathParam>(request.params)
            .and_then(|params| snapshot_capture(state, &params.path)),
        method::PREVIEW_START => {
            Ok(json!({"enabled":true,"clients":state.preview_clients.load(Ordering::Relaxed)}))
        }
        method::PREVIEW_STOP => {
            Ok(json!({"enabled":false,"clients":state.preview_clients.load(Ordering::Relaxed)}))
        }
        method::PREVIEW_FRAME => parse::<PreviewFrameParams>(request.params)
            .and_then(|params| preview_frame(state, &params)),
        method::PREVIEW_FREEZE => Ok(
            json!({"frozen":frozen_frame.is_some(),"sequence":frozen_frame.as_ref().map(|frame|frame.sequence)}),
        ),
        method::PREVIEW_UNFREEZE => Ok(json!({"frozen":false})),
        method::DIAGNOSTIC_PLAN => parse::<DiagnosticParams>(request.params)
            .and_then(|params| build_diagnostic_bundle(state, frozen_frame.as_deref(), &params))
            .and_then(|bundle| {
                plan_diagnostic_bundle(&bundle, DiagnosticLimits::default()).map_err(internal_error)
            })
            .and_then(|plan| serde_json::to_value(plan).map_err(internal_error)),
        method::DIAGNOSTIC_EXPORT => {
            parse::<DiagnosticExportParams>(request.params).and_then(|params| {
                if !params.privacy_reviewed {
                    return Err(RpcError::new(
                        error_code::INVALID_PARAMS,
                        "privacy_reviewed must be true after reviewing diagnostic.plan",
                    ));
                }
                let bundle =
                    build_diagnostic_bundle(state, frozen_frame.as_deref(), &params.bundle)?;
                let plan = plan_diagnostic_bundle(&bundle, DiagnosticLimits::default())
                    .map_err(internal_error)?;
                if plan.total_uncompressed_bytes != params.expected_total_uncompressed_bytes {
                    return Err(RpcError::new(
                        error_code::INVALID_PARAMS,
                        "diagnostic contents changed after privacy review; request a new plan",
                    ));
                }
                export_diagnostic_bundle(&params.path, &bundle, DiagnosticLimits::default())
                    .map_err(internal_error)
                    .and_then(|plan| serde_json::to_value(plan).map_err(internal_error))
            })
        }
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

fn catalog_error(error: impl std::fmt::Display) -> RpcError {
    RpcError {
        code: error_code::INTERNAL_ERROR,
        message: "profile catalog operation failed".into(),
        data: Some(json!({"detail": error.to_string()})),
    }
}

fn catalog_listing(state: &State) -> Result<Value, CatalogError> {
    let catalog = state.catalog.load()?;
    catalog_listing_value(state, &catalog)
}

fn catalog_listing_value(state: &State, catalog: &Catalog) -> Result<Value, CatalogError> {
    let application = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| CatalogError::Invalid("application version is invalid"))?;
    let profiles = catalog
        .compatible_profiles(&application)
        .into_iter()
        .map(|entry| {
            let installed = state
                .profiles
                .profiles_root()
                .join(entry.profile_id.to_string())
                .is_dir();
            let mut value = serde_json::to_value(entry)?;
            value["installed"] = json!(installed);
            Ok(value)
        })
        .collect::<Result<Vec<_>, CatalogError>>()?;
    Ok(json!({
        "schema": catalog.schema,
        "revision": catalog.revision,
        "generated_at": catalog.generated_at,
        "profiles": profiles,
    }))
}

async fn install_catalog_profile(
    state: &State,
    params: CatalogInstallParams,
) -> Result<Value, RpcError> {
    let catalog = state.catalog.load().map_err(catalog_error)?;
    if catalog.revision != params.catalog_revision {
        return Err(RpcError {
            code: error_code::REVISION_CONFLICT,
            message: "profile catalog changed; review the current entry before installing".into(),
            data: Some(json!({
                "expected": params.catalog_revision,
                "current": catalog.revision,
            })),
        });
    }
    let entry = catalog
        .profiles
        .iter()
        .find(|entry| entry.id == params.id && entry.version.to_string() == params.version)
        .ok_or_else(|| {
            RpcError::new(error_code::INVALID_PARAMS, "catalog profile was not found")
        })?;
    if entry.sha256 != params.sha256 {
        return Err(RpcError::new(
            error_code::REVISION_CONFLICT,
            "catalog package changed; review it again before installing",
        ));
    }
    if state
        .profiles
        .profiles_root()
        .join(entry.profile_id.to_string())
        .exists()
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "catalog profile version is already installed",
        ));
    }
    let package = state
        .catalog
        .download_package(entry)
        .await
        .map_err(catalog_error)?;
    let profile = import_profile(
        &package,
        state.profiles.profiles_root(),
        ImportLimits::default(),
    )
    .map_err(internal_error)?;
    Ok(json!({
        "installed": true,
        "catalog_id": entry.id,
        "version": entry.version,
        "profile": profile,
        "active": false,
    }))
}

#[allow(clippy::too_many_lines)]
fn run_synthetic_health(state: &State, params: SyntheticReplayParams) -> Result<Value, RpcError> {
    if params.fills.is_empty()
        || params.fills.len() > 10_000
        || params.fills.iter().any(|&fill| fill > 10)
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "fills must contain 1..=10000 values within 0..=10",
        ));
    }
    let detector_id = DetectorId::new();
    let element_id = ElementId::new();
    let detector = ColorBarDetector::new(ColorBarConfig {
        direction: BarDirection::LeftToRight,
        minimum_rgb: [180, 0, 0],
        maximum_rgb: [255, 60, 60],
        line_match_fraction: 0.8,
        maximum_gap_fraction: 0.02,
        mask: None,
    })
    .map_err(internal_error)?;
    let rule = NumericRule::new(NumericRuleConfig {
        id: RuleId::new(),
        event: "critical_health".into(),
        enter_below: 0.2,
        leave_above: 0.3,
        minimum_confidence: 0.0,
        required_samples: 2,
        sample_window: 3,
        cooldown: Duration::ZERO,
        stable_for: Duration::ZERO,
        emit_initial: false,
        update_interval: None,
    })
    .map_err(internal_error)?;
    let mut processor = FrameProcessor::new(
        AnalysisScheduler::new(10).map_err(internal_error)?,
        detector,
        NormalizedRegion {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        detector_id,
        element_id,
        rule,
    );
    let frames = params
        .fills
        .into_iter()
        .enumerate()
        .map(|(index, fill)| synthetic_health_frame(u64::try_from(index).unwrap_or(u64::MAX), fill))
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal_error)?;
    publish_replay(
        state,
        &mut processor,
        frames,
        &params.profile_id,
        &params.game,
    )
}

fn publish_replay<D: VisionDetector>(
    state: &State,
    processor: &mut FrameProcessor<D>,
    frames: Vec<Arc<Frame>>,
    profile_id: &str,
    game: &str,
) -> Result<Value, RpcError> {
    let start = Utc
        .with_ymd_and_hms(2026, 7, 11, 16, 43, 27)
        .single()
        .ok_or_else(|| internal_error("invalid replay epoch"))?;
    let mut publication = PublicationState::default();
    let mut emitted = Vec::new();
    let mut processed_count = 0_usize;
    for frame in ReplaySource::new(frames) {
        let Some(processed) = processor.process(&frame) else {
            continue;
        };
        processed_count = processed_count.saturating_add(1);
        let timestamp = start
            + TimeDelta::milliseconds(
                i64::try_from(processed.observation.timestamp_ms).unwrap_or(i64::MAX),
            );
        if let Some(record) = publish_processed(
            state,
            processed,
            profile_id,
            game,
            timestamp,
            json!({"active":false,"source":"replay"}),
            &mut publication,
        )? {
            emitted.push(record);
        }
    }
    Ok(json!({"frames": processed_count, "events": emitted}))
}

#[derive(Debug, Default)]
struct PublicationState {
    observations: serde_json::Map<String, Value>,
    event_states: serde_json::Map<String, Value>,
}

fn publish_processed(
    state: &State,
    processed: ProcessedFrame,
    profile_id: &str,
    game: &str,
    timestamp: DateTime<Utc>,
    capture: Value,
    publication: &mut PublicationState,
) -> Result<Option<Value>, RpcError> {
    publication.observations.insert(
        processed.observation.element_id.to_string(),
        serde_json::to_value(&processed.observation).map_err(internal_error)?,
    );
    let mut emitted = None;
    let mut event_record = None;
    if let Some(transition) = processed.transition {
        let sequence = state
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let event_state = match transition.state {
            TransitionState::Entered => EventState::Entered,
            TransitionState::Updated => EventState::Updated,
            TransitionState::Left => EventState::Left,
        };
        if event_state != EventState::Updated {
            publication.event_states.insert(
                transition.event.clone(),
                json!(matches!(event_state, EventState::Entered)),
            );
        }
        let record = EventRecord {
            schema: 1,
            daemon_instance: state.instance,
            sequence,
            timestamp: EventRecord::timestamp_rfc3339(timestamp),
            profile_id: profile_id.to_owned(),
            game: game.to_owned(),
            event: transition.event,
            state: event_state,
            value: json!(transition.value),
            confidence: f64::from(transition.confidence),
        };
        let record_value = serde_json::to_value(&record).map_err(internal_error)?;
        if let Err(error) = state
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .append_event(&record)
        {
            set_output_error(state, &error.to_string());
        }
        let _ = state.notifications.send(record_value.clone());
        event_record = Some(record);
        emitted = Some(record_value);
    }
    let snapshot = StateSnapshot {
        schema: 1,
        daemon_instance: state.instance,
        sequence: state.sequence.load(Ordering::Relaxed),
        updated_at: EventRecord::timestamp_rfc3339(timestamp),
        capture,
        active_profile: Some(profile_id.to_owned()),
        observations: Value::Object(publication.observations.clone()),
        events: Value::Object(publication.event_states.clone()),
    };
    let snapshot_value = serde_json::to_value(&snapshot).map_err(internal_error)?;
    if let Err(error) = state
        .output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .write_state(&snapshot)
    {
        set_output_error(state, &error.to_string());
    }
    if let Ok(route_profile_id) = serde_json::from_value::<ProfileId>(json!(profile_id)) {
        if let Err(error) =
            state
                .output_routes
                .publish(route_profile_id, event_record, snapshot.clone())
        {
            set_output_error(state, error);
        }
    }
    *state
        .latest_snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot_value;
    Ok(emitted)
}

fn synthetic_health_frame(
    sequence: u64,
    fill: u8,
) -> Result<Arc<Frame>, yash_app_events_capture::FrameError> {
    let mut bytes = vec![0_u8; 10 * 2 * 4];
    for y in 0..2 {
        for x in 0..10 {
            let offset = (y * 10 + x) * 4;
            bytes[offset..offset + 4].copy_from_slice(if x < usize::from(fill) {
                &[220, 20, 20, 255]
            } else {
                &[10, 10, 10, 255]
            });
        }
    }
    Frame::new(
        sequence,
        Duration::from_millis(sequence.saturating_mul(100)),
        FrameLayout {
            width: 10,
            height: 2,
            row_stride: 40,
            format: PixelFormat::Rgba8,
        },
        Some("replay".into()),
        Arc::from(bytes),
    )
    .map(Arc::new)
}

fn synthetic_color_bar_frame(
    sequence: u64,
    fill: u8,
    direction: BarDirection,
    minimum_rgb: [u8; 3],
    maximum_rgb: [u8; 3],
) -> Result<Arc<Frame>, yash_app_events_capture::FrameError> {
    let (width, height) = match direction {
        BarDirection::LeftToRight | BarDirection::RightToLeft => (10_usize, 2_usize),
        BarDirection::TopToBottom | BarDirection::BottomToTop => (2_usize, 10_usize),
    };
    let mut background = minimum_rgb;
    if let Some(channel) =
        (0..3).find(|&channel| minimum_rgb[channel] > 0 || maximum_rgb[channel] < u8::MAX)
    {
        background[channel] = if minimum_rgb[channel] > 0 {
            minimum_rgb[channel] - 1
        } else {
            maximum_rgb[channel] + 1
        };
    }
    let fill = usize::from(fill.min(10));
    let mut bytes = vec![0_u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let position = match direction {
                BarDirection::LeftToRight => x,
                BarDirection::RightToLeft => width - 1 - x,
                BarDirection::TopToBottom => y,
                BarDirection::BottomToTop => height - 1 - y,
            };
            let color = if position < fill {
                minimum_rgb
            } else {
                background
            };
            let offset = (y * width + x) * 4;
            bytes[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
        }
    }
    Frame::new(
        sequence,
        Duration::from_millis(sequence.saturating_mul(100)),
        FrameLayout {
            width: u32::try_from(width).unwrap_or(10),
            height: u32::try_from(height).unwrap_or(2),
            row_stride: width * 4,
            format: PixelFormat::Rgba8,
        },
        Some("replay".into()),
        Arc::from(bytes),
    )
    .map(Arc::new)
}

fn output_recipe_entry(
    state: &State,
    profile_id: ProfileId,
    recipe_id: Uuid,
    sha256: &str,
) -> Result<yash_app_events_profile::OutputRecipeEntry, RpcError> {
    let entry = state
        .profiles
        .output_recipes(profile_id)
        .map_err(store_error)?
        .into_iter()
        .find(|entry| entry.recipe.id == recipe_id)
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "output recipe was not found"))?;
    if entry.sha256 != sha256 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "output recipe changed after review; reload it before installing",
        ));
    }
    Ok(entry)
}

fn preview_output_recipe(
    state: &State,
    params: &OutputRecipeDraftParams,
) -> Result<Value, RpcError> {
    let entry = output_recipe_entry(state, params.profile_id, params.recipe_id, &params.sha256)?;
    let route = OutputRoute {
        id: Uuid::new_v4(),
        name: params.name.clone(),
        enabled: false,
        trigger: params.trigger.clone(),
        format: params.format.clone(),
        sink: OutputSink::File {
            path: PathBuf::from("/output-recipe-preview"),
            mode: yash_app_events_output::FileMode::Append,
        },
        source_recipe: Some(OutputRecipeSource {
            profile_id: params.profile_id.to_string(),
            recipe_id: entry.recipe.id,
            sha256: entry.sha256,
        }),
    };
    let snapshot = serde_json::from_value::<StateSnapshot>(
        state
            .latest_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .map_err(internal_error)?;
    let profile = state
        .profiles
        .load(params.profile_id)
        .map_err(store_error)?;
    let event = sample_output_event(state, params.profile_id, &profile.game, &route.trigger);
    let payload = render_payload(
        &route,
        &RouteContext {
            kind: if event.is_some() {
                "event"
            } else {
                "state_change"
            },
            event: event.as_ref(),
            state: &snapshot,
        },
    )
    .map_err(internal_error)?;
    Ok(json!({"executed":false,"payload":payload,"recipe_sha256":params.sha256}))
}

fn install_output_recipe(
    state: &State,
    params: OutputRecipeInstallParams,
) -> Result<Value, RpcError> {
    let entry = output_recipe_entry(
        state,
        params.draft.profile_id,
        params.draft.recipe_id,
        &params.draft.sha256,
    )?;
    let route = OutputRoute {
        id: Uuid::new_v4(),
        name: params.draft.name,
        enabled: false,
        trigger: params.draft.trigger,
        format: params.draft.format,
        sink: params.sink,
        source_recipe: Some(OutputRecipeSource {
            profile_id: params.draft.profile_id.to_string(),
            recipe_id: entry.recipe.id,
            sha256: entry.sha256,
        }),
    };
    route.validate().map_err(internal_error)?;
    let routes = state
        .local_config
        .set_output_route(params.draft.profile_id, route.clone())
        .map_err(internal_error)?;
    Ok(json!({"installed":route,"routes":routes}))
}

fn sample_output_event(
    state: &State,
    profile_id: ProfileId,
    game: &str,
    trigger: &OutputTrigger,
) -> Option<EventRecord> {
    match trigger {
        OutputTrigger::Event { events, states } => Some(EventRecord {
            schema: 1,
            daemon_instance: state.instance,
            sequence: state.sequence.load(Ordering::Relaxed),
            timestamp: EventRecord::timestamp_rfc3339(Utc::now()),
            profile_id: profile_id.to_string(),
            game: game.to_owned(),
            event: events
                .first()
                .cloned()
                .unwrap_or_else(|| "output_test".into()),
            state: states.first().copied().unwrap_or(EventState::Updated),
            value: json!("test"),
            confidence: 1.0,
        }),
        OutputTrigger::StateChange => None,
    }
}

async fn test_output_route(state: &State, params: OutputRouteParams) -> Result<Value, RpcError> {
    let route = state
        .local_config
        .output_routes(params.profile_id)
        .map_err(internal_error)?
        .into_iter()
        .find(|route| route.id == params.route_id)
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "output route was not found"))?;
    let snapshot = serde_json::from_value::<StateSnapshot>(
        state
            .latest_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .map_err(internal_error)?;
    let profile = state
        .profiles
        .load(params.profile_id)
        .map_err(store_error)?;
    let event = sample_output_event(state, params.profile_id, &profile.game, &route.trigger);
    tokio::task::spawn_blocking(move || {
        let context = RouteContext {
            kind: if event.is_some() {
                "event"
            } else {
                "state_change"
            },
            event: event.as_ref(),
            state: &snapshot,
        };
        let payload = render_payload(&route, &context)?;
        let receipt = execute_route(&route, &context)?;
        Ok::<Value, yash_app_events_output::OutputRouteError>(
            json!({"delivered":true,"payload":payload,"receipt":receipt}),
        )
    })
    .await
    .map_err(internal_error)?
    .map_err(internal_error)
}

fn set_output_error(state: &State, error: &str) {
    tracing::error!(%error, "output write failed");
    *state
        .output_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_owned());
    let mut logs = state
        .diagnostic_logs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if logs.len() == DiagnosticLimits::default().maximum_logs {
        logs.pop_front();
    }
    logs.push_back(json!({"level":"error","message":"output failure","error":error}));
    let _ = state
        .notifications
        .send(json!({"type":"output_error","error":error}));
}

#[derive(Debug)]
enum ConfiguredDetector {
    Color(ColorBarDetector),
    Template(TemplateDetector),
    RegionChange(RegionChangeDetector),
    Ocr(OcrDetector),
    SevenSegment(SevenSegmentDetector),
    Classifier(OnnxClassifierDetector),
}

impl VisionDetector for ConfiguredDetector {
    fn detect(&mut self, frame: &Frame, region: NormalizedRegion) -> Detection {
        match self {
            Self::Color(detector) => detector.detect(frame, region),
            Self::Template(detector) => detector.detect(frame, region),
            Self::RegionChange(detector) => detector.detect(frame, region),
            Self::Ocr(detector) => detector.detect(frame, region),
            Self::SevenSegment(detector) => detector.detect(frame, region),
            Self::Classifier(detector) => detector.detect(frame, region),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn configured_detector(
    detector: &ProfileDetector,
    directory: &Path,
) -> Result<ConfiguredDetector, RpcError> {
    match detector {
        ProfileDetector::ColorBar {
            direction,
            minimum_rgb,
            maximum_rgb,
            mask,
            ..
        } => {
            let mask = mask
                .as_ref()
                .map(|path| read_bounded_json::<Vec<bool>>(&directory.join(path)))
                .transpose()?;
            ColorBarDetector::new(ColorBarConfig {
                direction: *direction,
                minimum_rgb: *minimum_rgb,
                maximum_rgb: *maximum_rgb,
                line_match_fraction: 0.8,
                maximum_gap_fraction: 0.02,
                mask,
            })
            .map(ConfiguredDetector::Color)
            .map_err(internal_error)
        }
        ProfileDetector::Template {
            templates,
            masks,
            threshold,
            preprocessing,
            ..
        } => {
            let templates = load_templates(directory, templates, masks)?;
            TemplateDetector::new(TemplateConfig {
                templates,
                threshold: *threshold,
                preprocessing: PreprocessPipeline {
                    operations: preprocessing.clone(),
                },
            })
            .map(ConfiguredDetector::Template)
            .map_err(internal_error)
        }
        ProfileDetector::RegionChange {
            threshold,
            preprocessing,
            ..
        } => RegionChangeDetector::new(RegionChangeConfig {
            change_threshold: *threshold,
            preprocessing: PreprocessPipeline {
                operations: preprocessing.clone(),
            },
        })
        .map(ConfiguredDetector::RegionChange)
        .map_err(internal_error),
        ProfileDetector::Ocr {
            language,
            page_segmentation_mode,
            character_whitelist,
            change_trigger_threshold,
            maximum_interval_ms,
            preprocessing,
            empty_value,
            zero_pad_to,
            ..
        } => OcrDetector::new(OcrConfig {
            language: language.clone(),
            data_path: None,
            page_segmentation_mode: *page_segmentation_mode,
            character_whitelist: character_whitelist.clone(),
            change_trigger_threshold: *change_trigger_threshold,
            maximum_interval_ms: *maximum_interval_ms,
            preprocessing: PreprocessPipeline {
                operations: preprocessing.clone(),
            },
            empty_value: empty_value.clone(),
            zero_pad_to: *zero_pad_to,
        })
        .map(ConfiguredDetector::Ocr)
        .map_err(internal_error),
        ProfileDetector::SevenSegment {
            digits,
            separator_after,
            threshold,
            preprocessing,
            ..
        } => SevenSegmentDetector::new(SevenSegmentConfig {
            digits: *digits,
            separator_after: *separator_after,
            threshold: *threshold,
            preprocessing: PreprocessPipeline {
                operations: preprocessing.clone(),
            },
        })
        .map(ConfiguredDetector::SevenSegment)
        .map_err(internal_error),
        ProfileDetector::Classifier {
            model,
            model_sha256,
            labels,
            input_width,
            input_height,
            preprocessing,
            change_trigger_threshold,
            maximum_interval_ms,
            ..
        } => OnnxClassifierDetector::new(ClassifierConfig {
            model_path: directory.join(model),
            model_sha256: model_sha256.clone(),
            labels: labels.clone(),
            input_width: *input_width,
            input_height: *input_height,
            preprocessing: PreprocessPipeline {
                operations: preprocessing.clone(),
            },
            change_trigger_threshold: *change_trigger_threshold,
            maximum_interval_ms: *maximum_interval_ms,
        })
        .map(ConfiguredDetector::Classifier)
        .map_err(internal_error),
    }
}

fn load_templates(
    directory: &Path,
    paths: &[PathBuf],
    masks: &[Option<PathBuf>],
) -> Result<Vec<Template>, RpcError> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let image = read_bounded_json::<GrayImage>(&directory.join(path))?;
            let mask = masks
                .get(index)
                .and_then(Option::as_ref)
                .map(|path| read_bounded_json::<Vec<bool>>(&directory.join(path)))
                .transpose()?;
            Ok(Template {
                name: path.to_string_lossy().into_owned(),
                image,
                mask,
            })
        })
        .collect()
}

fn run_profile_replay(state: &State, params: &ProfileReplayParams) -> Result<Value, RpcError> {
    if params.values.is_empty() || params.values.len() > 10_000 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "values must contain 1..=10000 samples",
        ));
    }
    let profile = state
        .profiles
        .load(params.profile_id)
        .map_err(store_error)?;
    let element = profile
        .elements
        .iter()
        .find(|element| element.id == params.element_id)
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "element not found"))?;
    let rule = profile
        .rules
        .iter()
        .find(|rule| rule.element_id == element.id)
        .ok_or_else(|| {
            RpcError::new(
                error_code::INVALID_PARAMS,
                "element has no numeric event rule",
            )
        })?;
    let directory = state.profiles.profile_directory(profile.id);
    let detector = configured_detector(&element.detector, &directory)?;
    let frames = replay_fixture_frames(&element.detector, &directory, &params.values)?;
    let numeric_rule = NumericRule::new(NumericRuleConfig {
        id: rule.id,
        event: rule.event.clone(),
        enter_below: rule.enter_below,
        leave_above: rule.leave_above,
        minimum_confidence: rule.minimum_confidence,
        required_samples: usize::from(rule.required_samples),
        sample_window: usize::from(rule.sample_window),
        cooldown: Duration::from_millis(rule.cooldown_ms),
        stable_for: Duration::from_millis(rule.stable_for_ms),
        emit_initial: rule.emit_initial,
        update_interval: rule.update_interval_ms.map(Duration::from_millis),
    })
    .map_err(internal_error)?;
    let mut processor = FrameProcessor::new(
        AnalysisScheduler::new(10).map_err(internal_error)?,
        detector,
        full_frame_region(),
        detector_id(&element.detector),
        element.id,
        numeric_rule,
    );
    publish_replay(
        state,
        &mut processor,
        frames,
        &profile.id.to_string(),
        &profile.game,
    )
}

fn evaluate_profile_replay(state: &State, manifest: &ReplayManifest) -> Result<Value, RpcError> {
    let sample_count = manifest.values.len() + manifest.image_frames.len();
    if manifest.schema != 1
        || sample_count == 0
        || sample_count > 10_000
        || (!manifest.values.is_empty() && !manifest.image_frames.is_empty())
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "replay schema must be 1 and exactly one of values or image_frames must contain 1..=10000 samples",
        ));
    }
    let profile = state
        .profiles
        .load(manifest.profile_id)
        .map_err(store_error)?;
    let element = profile
        .elements
        .iter()
        .find(|element| element.id == manifest.element_id)
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "element not found"))?;
    let directory = state.profiles.profile_directory(profile.id);
    let frames = if manifest.image_frames.is_empty() {
        replay_fixture_frames(&element.detector, &directory, &manifest.values)?
    } else {
        replay_png_frames(&directory, &manifest.image_frames)?
    };
    let mut pipeline = build_live_pipeline(state, profile.id)?;
    let start = Utc
        .with_ymd_and_hms(2026, 7, 11, 16, 43, 27)
        .single()
        .ok_or_else(|| internal_error("invalid replay epoch"))?;
    let capture = json!({"active":false,"source":"replay"});
    let replay_context = ReplayPublicationContext {
        profile_id: &pipeline.profile_id,
        game: &pipeline.game,
        capture: &capture,
    };
    let mut publication = PublicationState::default();
    publication.event_states.extend(
        pipeline
            .initial_events
            .iter()
            .map(|event| (event.clone(), json!(false))),
    );
    let mut observations = Vec::new();
    let mut transitions = Vec::new();
    for frame in ReplaySource::new(frames) {
        for processor in &mut pipeline.processors {
            let Some(processed) = processor.process(&frame) else {
                continue;
            };
            let observation_timestamp_ms = processed.observation.timestamp_ms;
            let timestamp = start
                + TimeDelta::milliseconds(
                    i64::try_from(observation_timestamp_ms).unwrap_or(i64::MAX),
                );
            transitions.extend(publish_replay_observation(
                state,
                &mut pipeline.rules,
                &replay_context,
                &processed.observation,
                timestamp,
                &mut publication,
            )?);
            observations.push(processed.observation);
            for derived in &pipeline.derived {
                let Some(observation) = compose_observation(
                    derived,
                    &publication.observations,
                    observation_timestamp_ms,
                ) else {
                    continue;
                };
                transitions.extend(publish_replay_observation(
                    state,
                    &mut pipeline.rules,
                    &replay_context,
                    &observation,
                    timestamp,
                    &mut publication,
                )?);
                observations.push(observation);
            }
        }
    }
    let metrics = evaluate_replay(
        &manifest.expected_events,
        &transitions,
        &manifest.regression,
    );
    Ok(json!({"metrics":metrics,"observations":observations,"events":transitions}))
}

struct ReplayPublicationContext<'a> {
    profile_id: &'a str,
    game: &'a str,
    capture: &'a Value,
}

fn publish_replay_observation(
    state: &State,
    rules: &mut [CoordinatedRule],
    context: &ReplayPublicationContext<'_>,
    observation: &Observation,
    timestamp: DateTime<Utc>,
    publication: &mut PublicationState,
) -> Result<Vec<Transition>, RpcError> {
    let transitions: Vec<_> = rules
        .iter_mut()
        .filter_map(|rule| rule.observe(observation))
        .collect();
    if transitions.is_empty() {
        publish_processed(
            state,
            ProcessedFrame {
                observation: observation.clone(),
                transition: None,
            },
            context.profile_id,
            context.game,
            timestamp,
            context.capture.clone(),
            publication,
        )?;
    } else {
        for transition in &transitions {
            publish_processed(
                state,
                ProcessedFrame {
                    observation: observation.clone(),
                    transition: Some(transition.clone()),
                },
                context.profile_id,
                context.game,
                timestamp,
                context.capture.clone(),
                publication,
            )?;
        }
    }
    Ok(transitions)
}

fn evaluate_regression_suite(state: &State, requested: &Path) -> Result<Value, RpcError> {
    let manifest_path = if requested.is_dir() {
        requested.join("suite.json")
    } else {
        requested.to_path_buf()
    };
    let manifest_path = fs::canonicalize(&manifest_path).map_err(internal_error)?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "suite has no package root"))?
        .to_path_buf();
    let suite: RegressionSuite = read_suite_document(&manifest_path)?;
    validate_suite_manifest(&suite)?;
    let inventory = verify_suite_inventory(&root, &suite)?;
    require_in_inventory(&inventory, &suite.profile)?;
    let profile_path = safe_suite_path(&root, &suite.profile)?;
    let profile = load_profile(&profile_path).map_err(internal_error)?;
    profile.validate().map_err(internal_error)?;
    if profile.game != suite.game {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "suite game does not match pinned profile",
        ));
    }
    let profile_directory = profile_path.parent().ok_or_else(|| {
        RpcError::new(error_code::INVALID_PARAMS, "profile has no asset directory")
    })?;
    let aliases = profile_aliases(&profile)?;
    let mut case_results = Vec::new();
    let mut passed_cases = 0_usize;
    let mut total_frames = 0_usize;
    let mut total_assertions = 0_usize;
    let mut passed_assertions = 0_usize;
    let mut categories: HashMap<String, (usize, usize)> = HashMap::new();
    for case_path in &suite.cases {
        require_in_inventory(&inventory, case_path)?;
        let case: RegressionCase = read_suite_document(&safe_suite_path(&root, case_path)?)?;
        validate_suite_case(&case, &inventory)?;
        let result =
            evaluate_suite_case(state, &root, &profile, profile_directory, &aliases, &case)?;
        let passed = result["passed"].as_bool().unwrap_or(false);
        passed_cases += usize::from(passed);
        let assertions = result["assertions"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let assertions_passed = result["assertions_passed"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        total_frames += case.frames.len();
        total_assertions += assertions;
        passed_assertions += assertions_passed;
        for category in &case.categories {
            let counts = categories.entry(category.clone()).or_default();
            counts.0 += 1;
            counts.1 += usize::from(passed);
        }
        case_results.push(result);
    }
    let category_results: serde_json::Map<String, Value> = categories
        .into_iter()
        .map(|(name, (total, passed))| {
            (
                name,
                json!({"cases":total,"passed":passed,"failed":total-passed}),
            )
        })
        .collect();
    let passed = passed_cases == suite.cases.len() && passed_assertions == total_assertions;
    Ok(json!({
        "schema":1,
        "suite":{"id":suite.id,"name":suite.name,"game":suite.game},
        "profile":{"id":profile.id,"name":profile.name,"revision":profile.revision,"path":suite.profile},
        "passed":passed,
        "summary":{
            "cases":suite.cases.len(),"cases_passed":passed_cases,"cases_failed":suite.cases.len()-passed_cases,
            "frames":total_frames,"assertions":total_assertions,"assertions_passed":passed_assertions,
            "assertions_failed":total_assertions-passed_assertions,"categories":category_results
        },
        "cases":case_results
    }))
}

fn validate_suite_manifest(suite: &RegressionSuite) -> Result<(), RpcError> {
    if suite.schema != 1
        || suite.id.trim().is_empty()
        || suite.name.trim().is_empty()
        || suite.cases.is_empty()
        || suite.cases.len() > 1_000
        || suite.files.is_empty()
        || suite.files.len() > 20_000
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "suite schema must be 1 with names, 1..=1000 cases, and a bounded file inventory",
        ));
    }
    Ok(())
}

fn validate_suite_case(
    case: &RegressionCase,
    inventory: &HashSet<PathBuf>,
) -> Result<(), RpcError> {
    if case.schema != 1
        || case.id.trim().is_empty()
        || case.purpose.trim().is_empty()
        || case.frames.is_empty()
        || case.frames.len() > 10_000
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "case schema must be 1 with an id, purpose, and 1..=10000 frames",
        ));
    }
    if let Some(source) = &case.source_media {
        require_in_inventory(inventory, source)?;
    }
    let mut previous = None;
    for frame in &case.frames {
        require_in_inventory(inventory, &frame.image)?;
        if previous.is_some_and(|timestamp| frame.timestamp_ms < timestamp) {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                "case frame timestamps must be monotonic",
            ));
        }
        previous = Some(frame.timestamp_ms);
    }
    Ok(())
}

fn verify_suite_inventory(
    root: &Path,
    suite: &RegressionSuite,
) -> Result<HashSet<PathBuf>, RpcError> {
    let mut inventory = HashSet::new();
    for entry in &suite.files {
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                "invalid suite SHA-256",
            ));
        }
        if !inventory.insert(entry.path.clone()) {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                "duplicate suite file",
            ));
        }
        let path = safe_suite_path(root, &entry.path)?;
        let bytes = fs::read(path).map_err(internal_error)?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != entry.sha256.to_ascii_lowercase() {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                format!("suite file hash mismatch: {}", entry.path.display()),
            ));
        }
    }
    Ok(inventory)
}

fn require_in_inventory(inventory: &HashSet<PathBuf>, path: &Path) -> Result<(), RpcError> {
    if inventory.contains(path) {
        Ok(())
    } else {
        Err(RpcError::new(
            error_code::INVALID_PARAMS,
            format!(
                "suite reference is not integrity-pinned: {}",
                path.display()
            ),
        ))
    }
}

fn safe_suite_path(root: &Path, relative: &Path) -> Result<PathBuf, RpcError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "suite paths must be package-relative without parent traversal",
        ));
    }
    let root = fs::canonicalize(root).map_err(internal_error)?;
    let path = fs::canonicalize(root.join(relative)).map_err(internal_error)?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "suite path escapes the package or is not a regular file",
        ));
    }
    Ok(path)
}

fn read_suite_document<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, RpcError> {
    let metadata = fs::metadata(path).map_err(internal_error)?;
    if metadata.len() > 4 * 1024 * 1024 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "suite JSON exceeds 4 MiB",
        ));
    }
    serde_json::from_slice(&fs::read(path).map_err(internal_error)?).map_err(internal_error)
}

fn profile_aliases(profile: &Profile) -> Result<HashMap<String, ElementId>, RpcError> {
    let mut aliases = HashMap::new();
    for (id, name) in profile
        .elements
        .iter()
        .map(|element| (element.id, element.name.as_str()))
        .chain(
            profile
                .derived_observations
                .iter()
                .map(|derived| (derived.id, derived.name.as_str())),
        )
    {
        aliases.insert(id.to_string(), id);
        let alias = suite_alias(name);
        if aliases.insert(alias.clone(), id).is_some() {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                format!("profile contains ambiguous suite alias: {alias}"),
            ));
        }
    }
    Ok(aliases)
}

fn suite_alias(name: &str) -> String {
    let mut alias = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !alias.is_empty() {
                alias.push('_');
            }
            alias.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    alias
}

fn evaluate_suite_case(
    state: &State,
    root: &Path,
    profile: &Profile,
    profile_directory: &Path,
    aliases: &HashMap<String, ElementId>,
    case: &RegressionCase,
) -> Result<Value, RpcError> {
    let analysis_fps = state
        .local_config
        .load_settings()
        .map_err(internal_error)?
        .analysis_fps;
    let mut pipeline = build_profile_pipeline(profile.clone(), profile_directory, analysis_fps)?;
    let mut latest: HashMap<ElementId, Observation> = HashMap::new();
    let mut transitions = Vec::new();
    let mut assertion_results = Vec::new();
    let mut frame_results = Vec::new();
    let mut passed_assertions = 0_usize;
    for (sequence, sample) in case.frames.iter().enumerate() {
        let frame = prepare_suite_frame(root, profile, aliases, sample, sequence)?;
        for processor in &mut pipeline.processors {
            let Some(processed) = processor.process(&frame) else {
                continue;
            };
            transitions.extend(
                pipeline
                    .rules
                    .iter_mut()
                    .filter_map(|rule| rule.observe(&processed.observation)),
            );
            latest.insert(processed.observation.element_id, processed.observation);
        }
        let serialized: serde_json::Map<String, Value> = latest
            .iter()
            .map(|(id, observation)| {
                (
                    id.to_string(),
                    serde_json::to_value(observation).unwrap_or(Value::Null),
                )
            })
            .collect();
        for derived in &pipeline.derived {
            if let Some(observation) =
                compose_observation(derived, &serialized, sample.timestamp_ms)
            {
                transitions.extend(
                    pipeline
                        .rules
                        .iter_mut()
                        .filter_map(|rule| rule.observe(&observation)),
                );
                latest.insert(observation.element_id, observation);
            }
        }
        for (target, expected) in &sample.expected_observations {
            let id = aliases.get(target).ok_or_else(|| {
                RpcError::new(
                    error_code::INVALID_PARAMS,
                    format!("unknown observation target: {target}"),
                )
            })?;
            let actual = latest.get(id);
            let (passed, reason) = observation_matches(expected, actual);
            passed_assertions += usize::from(passed);
            assertion_results.push(json!({
                "frame":sequence,"timestamp_ms":sample.timestamp_ms,"target":target,
                "passed":passed,"reason":reason,"expected":expected,"actual":actual
            }));
        }
        let observed: serde_json::Map<String, Value> = profile
            .elements
            .iter()
            .map(|element| (element.id, suite_alias(&element.name)))
            .chain(
                profile
                    .derived_observations
                    .iter()
                    .map(|derived| (derived.id, suite_alias(&derived.name))),
            )
            .filter_map(|(id, alias)| {
                latest
                    .get(&id)
                    .and_then(|observation| serde_json::to_value(observation).ok())
                    .map(|observation| (alias, observation))
            })
            .collect();
        frame_results.push(json!({
            "frame":sequence,"timestamp_ms":sample.timestamp_ms,"image":sample.image,
            "observations":observed
        }));
    }
    let event_metrics = suite_event_metrics(case, &transitions);
    let assertions = assertion_results.len();
    let events_passed = event_metrics.as_ref().is_none_or(|metrics| metrics.passed);
    let passed = passed_assertions == assertions && events_passed;
    Ok(json!({
        "id":case.id,"purpose":case.purpose,"categories":case.categories,"passed":passed,
        "frames":case.frames.len(),"assertions":assertions,"assertions_passed":passed_assertions,
        "assertions_failed":assertions-passed_assertions,"observation_results":assertion_results,
        "frame_results":frame_results,
        "events_checked":case.check_events,"event_metrics":event_metrics,"events":transitions
    }))
}

fn suite_event_metrics(
    case: &RegressionCase,
    transitions: &[Transition],
) -> Option<yash_app_events_engine::ReplayMetrics> {
    case.check_events.then(|| {
        let expected_names: HashSet<_> = case
            .expected_events
            .iter()
            .map(|event| event.event.as_str())
            .collect();
        let relevant: Vec<_> = transitions
            .iter()
            .filter(|transition| expected_names.contains(transition.event.as_str()))
            .cloned()
            .collect();
        evaluate_replay(
            &case.expected_events,
            &relevant,
            &ReplayRegression::default(),
        )
    })
}

fn observation_matches(
    expected: &ExpectedObservation,
    actual: Option<&Observation>,
) -> (bool, String) {
    let Some(actual) = actual else {
        return (false, "observation was not produced".into());
    };
    if actual.status != expected.status {
        return (
            false,
            format!("status {:?} != {:?}", actual.status, expected.status),
        );
    }
    if let Some(minimum) = expected.minimum_confidence {
        if actual
            .confidence
            .is_none_or(|confidence| confidence < minimum)
        {
            return (false, format!("confidence is below {minimum}"));
        }
    }
    let value_matches = match (&expected.value, &actual.value) {
        (None, _) => true,
        (Some(ExpectedValue::Text(expected)), ObservationValue::Text(actual)) => expected == actual,
        (Some(ExpectedValue::Boolean(expected)), ObservationValue::Boolean(actual)) => {
            expected == actual
        }
        (Some(ExpectedValue::Number(expected_value)), ObservationValue::Number(actual)) => {
            (expected_value - actual).abs() <= expected.numeric_tolerance.unwrap_or(0.0)
        }
        _ => false,
    };
    if value_matches {
        (true, "matched".into())
    } else {
        (false, "typed value did not match".into())
    }
}

fn prepare_suite_frame(
    root: &Path,
    profile: &Profile,
    aliases: &HashMap<String, ElementId>,
    sample: &SuiteFrame,
    sequence: usize,
) -> Result<Arc<Frame>, RpcError> {
    let image = decode_suite_png(&safe_suite_path(root, &sample.image)?)?;
    let width = profile.layout.reference_width;
    let height = profile.layout.reference_height;
    let mut canvas = vec![
        0_u8;
        usize::try_from(width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .saturating_mul(4)
    ];
    match &sample.placement {
        FramePlacement::FullFrame => {
            if image.width != width || image.height != height {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "full-frame fixture must match the profile reference resolution",
                ));
            }
            canvas = image.pixels;
        }
        FramePlacement::PartialFrame { source_region } => {
            if image.width != source_region.width || image.height != source_region.height {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "partial-frame image dimensions must match source_region",
                ));
            }
            blit_scaled(&image, &mut canvas, width, height, *source_region)?;
        }
        FramePlacement::ZoneCrop { target } => {
            let id = aliases.get(target).ok_or_else(|| {
                RpcError::new(error_code::INVALID_PARAMS, "unknown zone-crop target")
            })?;
            let element = profile
                .elements
                .iter()
                .find(|element| element.id == *id)
                .ok_or_else(|| {
                    RpcError::new(
                        error_code::INVALID_PARAMS,
                        "zone crop must target a detector element",
                    )
                })?;
            let region =
                normalized_to_pixels(element.region, width, height).map_err(internal_error)?;
            blit_scaled(
                &image,
                &mut canvas,
                width,
                height,
                yash_app_events_engine::suite::PixelRectangle {
                    x: region.x,
                    y: region.y,
                    width: region.width,
                    height: region.height,
                },
            )?;
        }
    }
    Frame::new(
        u64::try_from(sequence).unwrap_or(u64::MAX),
        Duration::from_millis(sample.timestamp_ms),
        FrameLayout {
            width,
            height,
            row_stride: usize::try_from(width)
                .unwrap_or(usize::MAX)
                .saturating_mul(4),
            format: PixelFormat::Rgba8,
        },
        Some("external-suite".into()),
        Arc::from(canvas),
    )
    .map(Arc::new)
    .map_err(internal_error)
}

#[derive(Debug)]
struct SuiteImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn decode_suite_png(path: &Path) -> Result<SuiteImage, RpcError> {
    if fs::metadata(path).map_err(internal_error)?.len() > 16 * 1024 * 1024 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "suite PNG exceeds 16 MiB",
        ));
    }
    let mut decoder = png::Decoder::new(std::io::BufReader::new(
        fs::File::open(path).map_err(internal_error)?,
    ));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(internal_error)?;
    let mut output = vec![
        0;
        reader.output_buffer_size().ok_or_else(|| {
            RpcError::new(
                error_code::INVALID_PARAMS,
                "suite PNG exceeds decoder limits",
            )
        })?
    ];
    let info = reader.next_frame(&mut output).map_err(internal_error)?;
    if info.width == 0
        || info.height == 0
        || info.width > 4_096
        || info.height > 4_096
        || info.bit_depth != png::BitDepth::Eight
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "unsupported suite PNG",
        ));
    }
    let data = &output[..info.buffer_size()];
    let pixels = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        png::ColorType::Grayscale => data
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Indexed => {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                "suite PNG must be grayscale, RGB, or RGBA",
            ))
        }
    };
    Ok(SuiteImage {
        width: info.width,
        height: info.height,
        pixels,
    })
}

fn blit_scaled(
    source: &SuiteImage,
    destination: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    region: yash_app_events_engine::suite::PixelRectangle,
) -> Result<(), RpcError> {
    if region.width == 0
        || region.height == 0
        || region.x.saturating_add(region.width) > canvas_width
        || region.y.saturating_add(region.height) > canvas_height
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "invalid fixture placement",
        ));
    }
    let canvas_width = usize::try_from(canvas_width).unwrap_or(usize::MAX);
    for y in 0..region.height {
        for x in 0..region.width {
            let source_x = x.saturating_mul(source.width) / region.width;
            let source_y = y.saturating_mul(source.height) / region.height;
            let source_offset = usize::try_from(source_y.saturating_mul(source.width) + source_x)
                .unwrap_or(usize::MAX)
                .saturating_mul(4);
            let destination_offset = usize::try_from(
                (region.y + y).saturating_mul(u32::try_from(canvas_width).unwrap_or(u32::MAX))
                    + region.x
                    + x,
            )
            .unwrap_or(usize::MAX)
            .saturating_mul(4);
            destination[destination_offset..destination_offset + 4]
                .copy_from_slice(&source.pixels[source_offset..source_offset + 4]);
        }
    }
    Ok(())
}

fn replay_png_frames(directory: &Path, paths: &[PathBuf]) -> Result<Vec<Arc<Frame>>, RpcError> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "replay image path must be profile-relative without parent traversal",
                ));
            }
            let path = directory.join(path);
            let metadata = fs::metadata(&path).map_err(internal_error)?;
            if metadata.len() > 16 * 1024 * 1024 {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "replay PNG exceeds 16 MiB",
                ));
            }
            let decoder = png::Decoder::new(std::io::BufReader::new(
                fs::File::open(path).map_err(internal_error)?,
            ));
            let mut reader = decoder.read_info().map_err(internal_error)?;
            let mut output = vec![
                0;
                reader.output_buffer_size().ok_or_else(|| RpcError::new(
                    error_code::INVALID_PARAMS,
                    "replay PNG exceeds decoder limits",
                ))?
            ];
            let info = reader.next_frame(&mut output).map_err(internal_error)?;
            if info.width == 0
                || info.height == 0
                || info.width > 4_096
                || info.height > 4_096
                || info.bit_depth != png::BitDepth::Eight
            {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "replay PNG must be 8-bit and no larger than 4096x4096",
                ));
            }
            let data = &output[..info.buffer_size()];
            let (format, pixels): (PixelFormat, Vec<u8>) = match info.color_type {
                png::ColorType::Rgb => (PixelFormat::Rgb8, data.to_vec()),
                png::ColorType::Rgba => (PixelFormat::Rgba8, data.to_vec()),
                png::ColorType::Grayscale => (
                    PixelFormat::Rgb8,
                    data.iter()
                        .flat_map(|value| [*value, *value, *value])
                        .collect(),
                ),
                _ => {
                    return Err(RpcError::new(
                        error_code::INVALID_PARAMS,
                        "replay PNG must use grayscale, RGB, or RGBA pixels",
                    ))
                }
            };
            let sequence = u64::try_from(index).unwrap_or(u64::MAX);
            Frame::new(
                sequence,
                Duration::from_millis(sequence.saturating_mul(100)),
                FrameLayout {
                    width: info.width,
                    height: info.height,
                    row_stride: usize::try_from(info.width)
                        .unwrap_or(usize::MAX)
                        .saturating_mul(format.bytes_per_pixel()),
                    format,
                },
                Some("image-replay".into()),
                Arc::from(pixels),
            )
            .map(Arc::new)
            .map_err(internal_error)
        })
        .collect()
}

fn replay_fixture_frames(
    detector: &ProfileDetector,
    directory: &Path,
    values: &[u8],
) -> Result<Vec<Arc<Frame>>, RpcError> {
    values
        .iter()
        .copied()
        .enumerate()
        .map(|(index, value)| {
            let sequence = u64::try_from(index).unwrap_or(u64::MAX);
            match detector {
                ProfileDetector::ColorBar {
                    direction,
                    minimum_rgb,
                    maximum_rgb,
                    ..
                } => synthetic_color_bar_frame(
                    sequence,
                    value,
                    *direction,
                    *minimum_rgb,
                    *maximum_rgb,
                )
                .map_err(internal_error),
                ProfileDetector::Template { templates, .. } => {
                    let template = templates.first().ok_or_else(|| {
                        RpcError::new(error_code::INVALID_PARAMS, "no template asset")
                    })?;
                    let mut image = read_bounded_json::<GrayImage>(&directory.join(template))?;
                    if value == 0 {
                        for pixel in &mut image.pixels {
                            *pixel = 255_u8.saturating_sub(*pixel);
                        }
                    }
                    gray_frame(sequence, &image).map_err(internal_error)
                }
                ProfileDetector::RegionChange { .. } => GrayImage::new(2, 2, vec![value; 4])
                    .map_err(internal_error)
                    .and_then(|image| gray_frame(sequence, &image).map_err(internal_error)),
                ProfileDetector::Ocr { .. } => Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "OCR replay requires an image replay fixture",
                )),
                ProfileDetector::SevenSegment { .. } => Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "seven-segment replay requires an image replay fixture",
                )),
                ProfileDetector::Classifier { .. } => Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "classifier replay requires an image replay fixture",
                )),
            }
        })
        .collect()
}

const fn full_frame_region() -> NormalizedRegion {
    NormalizedRegion {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    }
}

#[allow(clippy::too_many_lines)]
fn test_detector(
    state: &State,
    params: &DetectorTestParams,
    frozen_frame: Option<Arc<Frame>>,
) -> Result<Value, RpcError> {
    let profile = state
        .profiles
        .load(params.profile_id)
        .map_err(store_error)?;
    let stored_element = profile
        .elements
        .iter()
        .find(|element| element.id == params.element_id)
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "element not found"))?;
    let element = params
        .element
        .as_ref()
        .map_or(stored_element, |element| element);
    if element.id != params.element_id {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "draft element ID does not match element_id",
        ));
    }
    let directory = state.profiles.profile_directory(profile.id);
    let full_region = NormalizedRegion {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    if params.use_frozen {
        let frame = frozen_frame.ok_or_else(|| {
            RpcError::new(
                error_code::INVALID_REQUEST,
                "freeze a preview frame before testing it",
            )
        })?;
        let mut detector = configured_detector(&element.detector, &directory)?;
        let first = detector.detect(&frame, element.region);
        // Stateful detectors need a baseline and a sample. Testing the same frozen
        // frame twice makes that lifecycle visible without fabricating a change.
        let detection = detector.detect(&frame, element.region);
        let original = grayscale_crop(&frame, element.region).map_err(internal_error)?;
        let processed = match &element.detector {
            ProfileDetector::ColorBar { .. } => original.clone(),
            ProfileDetector::Template { preprocessing, .. }
            | ProfileDetector::RegionChange { preprocessing, .. }
            | ProfileDetector::Ocr { preprocessing, .. }
            | ProfileDetector::SevenSegment { preprocessing, .. }
            | ProfileDetector::Classifier { preprocessing, .. } => PreprocessPipeline {
                operations: preprocessing.clone(),
            }
            .apply(&original)
            .map_err(internal_error)?,
        };
        return detector_test_response(element, &[first, detection], &original, &processed);
    }
    let (detections, original, processed_preview) = match &element.detector {
        ProfileDetector::ColorBar {
            direction,
            minimum_rgb,
            maximum_rgb,
            mask,
            ..
        } => {
            let mask = mask
                .as_ref()
                .map(|path| read_bounded_json::<Vec<bool>>(&directory.join(path)))
                .transpose()?;
            let mut detector = ColorBarDetector::new(ColorBarConfig {
                direction: *direction,
                minimum_rgb: *minimum_rgb,
                maximum_rgb: *maximum_rgb,
                line_match_fraction: 0.8,
                maximum_gap_fraction: 0.02,
                mask,
            })
            .map_err(internal_error)?;
            let frame = synthetic_health_frame(0, params.fill.min(10)).map_err(internal_error)?;
            let fill = usize::from(params.fill.min(10));
            let preview = GrayImage::new(
                10,
                2,
                (0..20)
                    .map(|index| if index % 10 < fill { 76 } else { 10 })
                    .collect(),
            )
            .map_err(internal_error)?;
            (
                vec![detector.detect(&frame, full_region)],
                preview.clone(),
                preview,
            )
        }
        ProfileDetector::Template {
            templates,
            masks,
            threshold,
            preprocessing,
            ..
        } => {
            let loaded = templates
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    let image = read_bounded_json::<GrayImage>(&directory.join(path))?;
                    let mask = masks
                        .get(index)
                        .and_then(Option::as_ref)
                        .map(|path| read_bounded_json::<Vec<bool>>(&directory.join(path)))
                        .transpose()?;
                    Ok(Template {
                        name: path.to_string_lossy().into_owned(),
                        image,
                        mask,
                    })
                })
                .collect::<Result<Vec<_>, RpcError>>()?;
            let fixture = loaded
                .first()
                .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "no template assets"))?
                .image
                .clone();
            let mut detector = TemplateDetector::new(TemplateConfig {
                templates: loaded,
                threshold: *threshold,
                preprocessing: PreprocessPipeline {
                    operations: preprocessing.clone(),
                },
            })
            .map_err(internal_error)?;
            let frame = gray_frame(0, &fixture).map_err(internal_error)?;
            let preview = PreprocessPipeline {
                operations: preprocessing.clone(),
            }
            .apply(&fixture)
            .map_err(internal_error)?;
            (vec![detector.detect(&frame, full_region)], fixture, preview)
        }
        ProfileDetector::RegionChange {
            threshold,
            preprocessing,
            ..
        } => {
            let mut detector = RegionChangeDetector::new(RegionChangeConfig {
                change_threshold: *threshold,
                preprocessing: PreprocessPipeline {
                    operations: preprocessing.clone(),
                },
            })
            .map_err(internal_error)?;
            let first = GrayImage::new(2, 2, vec![0; 4]).map_err(internal_error)?;
            let second =
                GrayImage::new(2, 2, vec![params.change_value; 4]).map_err(internal_error)?;
            let first_frame = gray_frame(0, &first).map_err(internal_error)?;
            let second_frame = gray_frame(1, &second).map_err(internal_error)?;
            let processed = PreprocessPipeline {
                operations: preprocessing.clone(),
            }
            .apply(&second)
            .map_err(internal_error)?;
            (
                vec![
                    detector.detect(&first_frame, full_region),
                    detector.detect(&second_frame, full_region),
                ],
                second,
                processed,
            )
        }
        ProfileDetector::Ocr { .. } => {
            return Err(RpcError::new(
                error_code::INVALID_REQUEST,
                "OCR detector tests require a frozen preview or image replay fixture",
            ));
        }
        ProfileDetector::SevenSegment { .. } => {
            return Err(RpcError::new(
                error_code::INVALID_REQUEST,
                "seven-segment detector tests require a frozen preview or image replay fixture",
            ));
        }
        ProfileDetector::Classifier { .. } => {
            return Err(RpcError::new(
                error_code::INVALID_REQUEST,
                "classifier detector tests require a frozen preview or image replay fixture",
            ));
        }
    };
    detector_test_response(element, &detections, &original, &processed_preview)
}

fn detector_test_response(
    element: &yash_app_events_profile::Element,
    detections: &[Detection],
    original: &GrayImage,
    processed: &GrayImage,
) -> Result<Value, RpcError> {
    let original = bounded_gray_preview(original, 512)?;
    let processed = bounded_gray_preview(processed, 512)?;
    let original_png = encode_preview_png(&original)?;
    let processed_png = encode_preview_png(&processed)?;
    Ok(
        json!({"detector_id": detector_id(&element.detector), "element_id": element.id, "observations": detections, "diagnostic": {"original_preview":{"mime":"image/png","width":original.width,"height":original.height,"bytes":original_png},"processed_preview":{"mime":"image/png","width":processed.width,"height":processed.height,"bytes":processed_png},"persistent_capture":false}}),
    )
}

fn capture_template(state: &State, params: &CaptureTemplateParams) -> Result<Value, RpcError> {
    if params.name.is_empty()
        || params.name.len() > 64
        || !params
            .name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "template name must use lowercase letters, digits, and underscores",
        ));
    }
    let profile = state
        .profiles
        .load(params.profile_id)
        .map_err(store_error)?;
    if profile.revision != params.expected_revision {
        return Err(RpcError {
            code: error_code::REVISION_CONFLICT,
            message: "profile revision conflict".into(),
            data: Some(json!({"expected":params.expected_revision,"current":profile.revision})),
        });
    }
    let element = profile
        .elements
        .iter()
        .find(|element| element.id == params.element_id)
        .ok_or_else(|| RpcError::new(error_code::INVALID_PARAMS, "element not found"))?;
    let frame = state.latest_frame.latest().ok_or_else(|| {
        RpcError::new(
            error_code::INVALID_REQUEST,
            "no captured frame is available",
        )
    })?;
    let image = grayscale_crop(&frame, element.region).map_err(internal_error)?;
    if image.width * image.height > 16_777_216 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "template crop exceeds 16 megapixels",
        ));
    }
    let relative = PathBuf::from(format!("templates/{}.json", params.name));
    let destination = state.profiles.profile_directory(profile.id).join(&relative);
    yash_app_events_profile::atomic_write(
        &destination,
        &serde_json::to_vec_pretty(&image).map_err(internal_error)?,
    )
    .map_err(internal_error)?;
    Ok(
        json!({"captured":true,"path":relative,"width":image.width,"height":image.height,"explicit":true}),
    )
}

fn encode_preview_png(image: &GrayImage) -> Result<Vec<u8>, RpcError> {
    if image.width > 512 || image.height > 512 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "diagnostic preview exceeds 512x512",
        ));
    }
    let width = u32::try_from(image.width).map_err(internal_error)?;
    let height = u32::try_from(image.height).map_err(internal_error)?;
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(internal_error)?;
        writer
            .write_image_data(&image.pixels)
            .map_err(internal_error)?;
    }
    Ok(bytes)
}

fn build_diagnostic_bundle(
    state: &State,
    frozen_frame: Option<&Frame>,
    params: &DiagnosticParams,
) -> Result<DiagnosticBundle, RpcError> {
    if params.selected_element_ids.len() > DiagnosticLimits::default().maximum_crops {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "too many selected diagnostic crops",
        ));
    }
    let settings = state.local_config.load_settings().map_err(internal_error)?;
    let profile_id = params.profile_id.or(settings.active_profile);
    let profile = profile_id
        .map(|profile_id| state.profiles.load(profile_id).map_err(store_error))
        .transpose()?;
    let mut selected_crops = Vec::new();
    if !params.selected_element_ids.is_empty() {
        let frame = frozen_frame.ok_or_else(|| {
            RpcError::new(
                error_code::INVALID_REQUEST,
                "freeze a preview before selecting diagnostic crops",
            )
        })?;
        let profile = profile.as_ref().ok_or_else(|| {
            RpcError::new(
                error_code::INVALID_PARAMS,
                "selected diagnostic crops require a profile",
            )
        })?;
        for element_id in &params.selected_element_ids {
            let element = profile
                .elements
                .iter()
                .find(|element| element.id == *element_id)
                .ok_or_else(|| {
                    RpcError::new(
                        error_code::INVALID_PARAMS,
                        "selected diagnostic element was not found",
                    )
                })?;
            let crop = grayscale_crop(frame, element.region).map_err(internal_error)?;
            let crop = bounded_gray_preview(&crop, 512)?;
            selected_crops.push(DiagnosticCrop {
                name: format!("element_{}", element.id),
                png: encode_preview_png(&crop)?,
            });
        }
    }
    let logs = state
        .diagnostic_logs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .cloned()
        .collect();
    let configuration = json!({"settings":settings,"profile":profile});
    let (analysis_frames, last_processing_latency_ms, detector_errors) = {
        let analysis = state
            .analysis_metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            analysis.frames,
            analysis
                .last_processing_latency
                .map(|value| value.as_secs_f64() * 1000.0),
            analysis.detector_errors,
        )
    };
    let metrics = json!({
        "status":status(state),
        "capture":capture_status(state),
        "analysis":{
            "frames":analysis_frames,
            "last_processing_latency_ms":last_processing_latency_ms,
            "detector_errors":detector_errors,
        },
    });
    Ok(DiagnosticBundle {
        logs,
        configuration,
        metrics,
        selected_crops,
    })
}

fn bounded_gray_preview(image: &GrayImage, maximum: usize) -> Result<GrayImage, RpcError> {
    if image.width <= maximum && image.height <= maximum {
        return Ok(image.clone());
    }
    let scale = (image.width.div_ceil(maximum)).max(image.height.div_ceil(maximum));
    let width = image.width.div_ceil(scale);
    let height = image.height.div_ceil(scale);
    let pixels = (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let source_x = (x * scale).min(image.width - 1);
                let source_y = (y * scale).min(image.height - 1);
                image.pixels[source_y * image.width + source_x]
            })
        })
        .collect();
    GrayImage::new(width, height, pixels).map_err(internal_error)
}

fn detector_id(detector: &ProfileDetector) -> DetectorId {
    match detector {
        ProfileDetector::ColorBar { id, .. }
        | ProfileDetector::Template { id, .. }
        | ProfileDetector::RegionChange { id, .. }
        | ProfileDetector::Ocr { id, .. }
        | ProfileDetector::SevenSegment { id, .. }
        | ProfileDetector::Classifier { id, .. } => *id,
    }
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, RpcError> {
    let metadata = fs::metadata(path).map_err(internal_error)?;
    if metadata.len() > 32 * 1024 * 1024 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "detector asset exceeds 32 MiB",
        ));
    }
    serde_json::from_slice(&fs::read(path).map_err(internal_error)?).map_err(internal_error)
}

fn gray_frame(
    sequence: u64,
    image: &GrayImage,
) -> Result<Arc<Frame>, yash_app_events_capture::FrameError> {
    let mut pixels = Vec::with_capacity(image.pixels.len() * 4);
    for &value in &image.pixels {
        pixels.extend_from_slice(&[value, value, value, 255]);
    }
    let width = u32::try_from(image.width).unwrap_or(u32::MAX);
    let height = u32::try_from(image.height).unwrap_or(u32::MAX);
    Frame::new(
        sequence,
        Duration::from_millis(sequence.saturating_mul(100)),
        FrameLayout {
            width,
            height,
            row_stride: image.width.saturating_mul(4),
            format: PixelFormat::Rgba8,
        },
        Some("detector-test".into()),
        Arc::from(pixels),
    )
    .map(Arc::new)
}

async fn select_capture(
    state: &Arc<State>,
    params: CaptureSelectParams,
) -> Result<Value, RpcError> {
    if state
        .capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
    {
        return Err(RpcError::new(
            error_code::INVALID_REQUEST,
            "capture is already active",
        ));
    }
    let profile_id = params.profile_id.or_else(|| {
        state
            .local_config
            .load_settings()
            .ok()
            .and_then(|settings| settings.active_profile)
    });
    let live_pipeline = profile_id
        .map(|profile_id| build_live_pipeline(state, profile_id))
        .transpose()?;
    let restore_token = profile_id
        .map(|profile_id| state.local_config.capture_binding(profile_id))
        .transpose()
        .map_err(internal_error)?
        .flatten()
        .map(|binding| binding.restore_token);
    let sources = match params.source.as_deref().unwrap_or("window_or_monitor") {
        "monitor" => SourceSelection::Monitor,
        "window" => SourceSelection::Window,
        "window_or_monitor" => SourceSelection::MonitorOrWindow,
        _ => {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                "source must be monitor, window, or window_or_monitor",
            ))
        }
    };
    let capture = PortalCapture::start(
        PortalOptions {
            sources,
            restore_token,
            persist_restore: true,
        },
        state.latest_frame.clone(),
    )
    .await
    .map_err(internal_error)?;
    let selected = capture.selected_source().clone();
    if let (Some(profile_id), Some(token)) = (profile_id, selected.restore_token.clone()) {
        let previous = state
            .local_config
            .capture_binding(profile_id)
            .ok()
            .flatten();
        state
            .local_config
            .set_capture_binding(
                profile_id,
                yash_app_events_profile::CaptureBinding {
                    restore_token: token,
                    source_label: Some(selected.label.clone()),
                    auto_capture: previous
                        .as_ref()
                        .is_some_and(|binding| binding.auto_capture),
                    process_match: previous.and_then(|binding| binding.process_match),
                },
            )
            .map_err(internal_error)?;
    }
    *state
        .capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(capture);
    if let Some(pipeline) = live_pipeline {
        start_live_analysis(state, pipeline);
    }
    Ok(
        json!({"active":true,"source":selected.label,"pipewire_node_id":selected.pipewire_node_id,"restore_token_saved":profile_id.is_some() && selected.restore_token.is_some(),"analysis_profile":profile_id}),
    )
}

async fn stop_capture(state: &State) -> Result<Value, RpcError> {
    let analysis = state
        .analysis
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(analysis) = analysis {
        let _ = analysis.stop.send(());
        let _ = analysis.task.await;
    }
    let capture = state
        .capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let stopped = capture.is_some();
    if let Some(capture) = capture {
        capture.stop().await;
    }
    Ok(json!({"active":false,"stopped":stopped}))
}

fn auto_capture_status(state: &State) -> Result<Value, RpcError> {
    let profile_id = state
        .local_config
        .load_settings()
        .map_err(internal_error)?
        .active_profile;
    let binding = profile_id
        .map(|id| state.local_config.capture_binding(id))
        .transpose()
        .map_err(internal_error)?
        .flatten();
    Ok(json!({
        "profile_id": profile_id,
        "enabled": binding.as_ref().is_some_and(|binding| binding.auto_capture),
        "process_match": binding.as_ref().and_then(|binding| binding.process_match.as_deref()),
        "process_running": binding.as_ref().and_then(|binding| binding.process_match.as_deref()).is_some_and(process_running),
    }))
}

fn set_auto_capture(state: &State, params: AutoCaptureParams) -> Result<Value, RpcError> {
    let binding = state
        .local_config
        .set_auto_capture(params.profile_id, params.enabled, params.process_match)
        .map_err(internal_error)?;
    Ok(
        json!({"profile_id":params.profile_id,"enabled":binding.auto_capture,"process_match":binding.process_match}),
    )
}

fn collection_policy_get(state: &State, profile_id: ProfileId) -> Result<Value, RpcError> {
    state.profiles.load(profile_id).map_err(store_error)?;
    let policy = state
        .local_config
        .collection_policy(profile_id)
        .map_err(internal_error)?;
    serde_json::to_value(json!({"profile_id":profile_id,"policy":policy})).map_err(internal_error)
}

fn collection_policy_set(
    state: &State,
    params: &CollectionPolicyParams,
) -> Result<Value, RpcError> {
    state
        .profiles
        .load(params.profile_id)
        .map_err(store_error)?;
    state
        .local_config
        .set_collection_policy(params.profile_id, params.policy.clone())
        .map_err(internal_error)?;
    let mut runtime = state
        .collector
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runtime.profile_id.as_deref() == Some(params.profile_id.to_string().as_str()) {
        runtime.configure(params.profile_id.to_string(), params.policy.clone());
    }
    Ok(json!({"profile_id":params.profile_id,"policy":params.policy}))
}

fn collection_status(state: &State, profile_id: ProfileId) -> Result<Value, RpcError> {
    let policy = state
        .local_config
        .collection_policy(profile_id)
        .map_err(internal_error)?;
    let storage = collection::storage_stats(&policy.dataset_root);
    let runtime = state
        .collector
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(json!({
        "profile_id":profile_id,"policy":policy,"storage":storage,
        "runtime":{
            "saved":runtime.saved,"duplicates":runtime.duplicates,"errors":runtime.errors,
            "last_item":runtime.last_item,"last_error":runtime.last_error,
            "write_in_progress":runtime.write_in_progress
        }
    }))
}

fn collection_items(state: &State, params: &CollectionItemsParams) -> Result<Value, RpcError> {
    let policy = state
        .local_config
        .collection_policy(params.profile_id)
        .map_err(internal_error)?;
    let items = collection::list_items(&policy.dataset_root, params.status);
    let summaries: Vec<_> = items
        .into_iter()
        .map(|item| {
            json!({
                "id":item.id,"created_at":item.created_at,"status":item.review.status,
                "reason":item.reason,"observations":item.observations.len(),
                "transitions":item.transitions.len(),"promoted_case":item.review.promoted_case
            })
        })
        .collect();
    Ok(json!({"profile_id":params.profile_id,"items":summaries}))
}

fn collection_item_get(state: &State, params: &CollectionItemParams) -> Result<Value, RpcError> {
    let policy = state
        .local_config
        .collection_policy(params.profile_id)
        .map_err(internal_error)?;
    let (root, item) =
        collection::load_item(&policy.dataset_root, &params.id).map_err(internal_error)?;
    let suggested: BTreeMap<_, _> = item
        .observations
        .iter()
        .filter_map(|(name, observation)| {
            collection::expected_from_observation(observation)
                .map(|expected| (name.clone(), expected))
        })
        .collect();
    Ok(json!({
        "item":item,"image_path":root.join("frame.png"),
        "suggested_expected_observations":suggested
    }))
}

fn collection_review(state: &State, params: CollectionReviewParams) -> Result<Value, RpcError> {
    let policy = state
        .local_config
        .collection_policy(params.profile_id)
        .map_err(internal_error)?;
    let (current_root, mut item) =
        collection::load_item(&policy.dataset_root, &params.id).map_err(internal_error)?;
    if item.profile_id != params.profile_id.to_string() {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "collection item belongs to a different profile",
        ));
    }
    if !params.expected_observations.is_empty() {
        item.review.expected_observations = params.expected_observations;
    }
    item.review.reviewer = Some("rpc".into());
    item.review.reviewed_at = Some(EventRecord::timestamp_rfc3339(Utc::now()));
    item.review.reason = params.reason;
    let destination = match params.action.as_str() {
        "accept" => {
            item.review.status = ReviewStatus::Accepted;
            collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
        }
        "correct" => {
            if item.review.expected_observations.is_empty() {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    "correct requires expected_observations",
                ));
            }
            item.review.status = ReviewStatus::Accepted;
            collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
        }
        "reject" => {
            item.review.status = ReviewStatus::Rejected;
            collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
        }
        "promote" => collection::promote_item(&policy.dataset_root, &current_root, &mut item),
        _ => {
            return Err(RpcError::new(
                error_code::INVALID_PARAMS,
                "collection action must be accept, correct, reject, or promote",
            ))
        }
    }
    .map_err(internal_error)?;
    Ok(json!({"item":item,"path":destination}))
}

#[allow(clippy::too_many_lines)]
fn collection_auto_review(
    state: &State,
    params: &CollectionAutoReviewParams,
) -> Result<Value, RpcError> {
    let policy = state
        .local_config
        .collection_policy(params.profile_id)
        .map_err(internal_error)?;
    let mut representatives = collection::list_items(&policy.dataset_root, None)
        .into_iter()
        .filter(|item| {
            matches!(
                item.review.status,
                ReviewStatus::Accepted | ReviewStatus::Promoted
            )
        })
        .map(|item| {
            let signature =
                collection::evidence_signature(&item.observations, &policy.novelty_targets);
            (item.thumbnail, signature)
        })
        .collect::<Vec<_>>();
    let pending = collection::list_items(&policy.dataset_root, Some(ReviewStatus::Pending));
    let mut accepted = 0_usize;
    let mut rejected = 0_usize;
    let mut promoted = 0_usize;
    let mut needs_correction = 0_usize;
    let mut decisions = Vec::new();
    for mut item in pending {
        let (current_root, _) =
            collection::load_item(&policy.dataset_root, &item.id).map_err(internal_error)?;
        let image = fs::read(current_root.join("frame.png")).map_err(internal_error)?;
        let signature = collection::evidence_signature(&item.observations, &policy.novelty_targets);
        item.review.reviewer = Some("auto:conservative-v1".into());
        item.review.reviewed_at = Some(EventRecord::timestamp_rfc3339(Utc::now()));
        if collection::image_sha256(&image) != item.image_sha256 {
            item.review.status = ReviewStatus::Rejected;
            item.review.reason = Some("image hash mismatch".into());
            collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
                .map_err(internal_error)?;
            rejected += 1;
        } else if representatives.iter().any(|(thumbnail, previous)| {
            collection::similarity(thumbnail, &item.thumbnail)
                .is_some_and(|difference| difference <= policy.similarity_threshold)
                && previous == &signature
        }) {
            item.review.status = ReviewStatus::Rejected;
            item.review.reason = Some("perceptual duplicate with equivalent evidence".into());
            collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
                .map_err(internal_error)?;
            rejected += 1;
        } else {
            item.review.expected_observations = item
                .observations
                .iter()
                .filter_map(|(name, observation)| {
                    collection::expected_from_observation(observation)
                        .map(|expected| (name.clone(), expected))
                })
                .collect();
            let uncertain = item.observations.values().any(|observation| {
                observation.status == yash_app_events_engine::ObservationStatus::Error
                    || (observation.status == yash_app_events_engine::ObservationStatus::Valid
                        && observation.confidence.unwrap_or(0.0) < 0.5)
            });
            if item.review.expected_observations.is_empty() || uncertain {
                item.review.status = ReviewStatus::NeedsCorrection;
                item.review.reason = Some(
                    "uncertain evidence retained for visual correction; no ground truth fabricated"
                        .into(),
                );
                collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
                    .map_err(internal_error)?;
                needs_correction += 1;
            } else if params.promote {
                item.review.reason = Some("high-confidence novel evidence auto-promoted".into());
                collection::promote_item(&policy.dataset_root, &current_root, &mut item)
                    .map_err(internal_error)?;
                promoted += 1;
                representatives.push((item.thumbnail.clone(), signature));
            } else {
                item.review.status = ReviewStatus::Accepted;
                item.review.reason = Some("high-confidence novel evidence accepted".into());
                collection::save_reviewed_item(&policy.dataset_root, &current_root, &item)
                    .map_err(internal_error)?;
                accepted += 1;
                representatives.push((item.thumbnail.clone(), signature));
            }
        }
        decisions
            .push(json!({"id":item.id,"status":item.review.status,"reason":item.review.reason}));
    }
    Ok(json!({
        "profile_id":params.profile_id,"processed":decisions.len(),"accepted":accepted,
        "rejected":rejected,"promoted":promoted,"needs_correction":needs_correction,
        "decisions":decisions
    }))
}

fn collection_compare(params: &CollectionCompareParams) -> Result<Value, RpcError> {
    if !params.threshold.is_finite() || !(0.0..=0.25).contains(&params.threshold) {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "collection similarity threshold must be within 0..=0.25",
        ));
    }
    let first_image = decode_suite_png(&params.first)?;
    let second_image = decode_suite_png(&params.second)?;
    if first_image.width != second_image.width || first_image.height != second_image.height {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "collection comparison requires equal image dimensions",
        ));
    }
    let frame = |image: SuiteImage, sequence| {
        Frame::new(
            sequence,
            Duration::ZERO,
            FrameLayout {
                width: image.width,
                height: image.height,
                row_stride: usize::try_from(image.width)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(4),
                format: PixelFormat::Rgba8,
            },
            Some("collection-comparison".into()),
            Arc::from(image.pixels),
        )
        .map_err(internal_error)
    };
    let first = frame(first_image, 0)?;
    let second = frame(second_image, 1)?;
    let difference = collection::similarity(
        &collection::thumbnail(&first),
        &collection::thumbnail(&second),
    )
    .ok_or_else(|| internal_error("collection thumbnails are incompatible"))?;
    Ok(json!({
        "first":params.first,"second":params.second,"width":first.width,"height":first.height,
        "difference":difference,"threshold":params.threshold,
        "duplicate":difference <= params.threshold
    }))
}

fn process_running(needle: &str) -> bool {
    std::fs::read_dir("/proc")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                && std::fs::read(entry.path().join("cmdline"))
                    .ok()
                    .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
        })
}

#[derive(Debug)]
struct LivePipeline {
    processors: Vec<FrameProcessor<ConfiguredDetector, NoopRule>>,
    rules: Vec<CoordinatedRule>,
    derived: Vec<yash_app_events_profile::DerivedObservation>,
    initial_events: Vec<String>,
    profile_id: String,
    game: String,
    profile_revision: u64,
    profile_sha256: String,
    collection_aliases: HashMap<ElementId, String>,
    collection_policy: CollectionPolicy,
}

#[derive(Debug)]
struct CoordinatedRule {
    element_id: Option<ElementId>,
    runtime: Box<dyn ObservationRule + Send>,
}

#[derive(Debug)]
struct RapidIncreaseRule {
    id: RuleId,
    event: String,
    minimum_delta: f64,
    within_ms: u64,
    cooldown_ms: u64,
    minimum_confidence: f32,
    history: VecDeque<(u64, f64)>,
    last_emit: Option<u64>,
}

impl ObservationRule for RapidIncreaseRule {
    fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        if observation.status != yash_app_events_engine::ObservationStatus::Valid {
            return None;
        }
        let confidence = observation.confidence.unwrap_or(0.0);
        if confidence < self.minimum_confidence {
            return None;
        }
        let value = match &observation.value {
            yash_app_events_engine::ObservationValue::Number(value) => *value,
            yash_app_events_engine::ObservationValue::Text(value) => value.trim().parse().ok()?,
            _ => return None,
        };
        let cutoff = observation.timestamp_ms.saturating_sub(self.within_ms);
        while self
            .history
            .front()
            .is_some_and(|(timestamp, _)| *timestamp < cutoff)
        {
            self.history.pop_front();
        }
        let minimum = self
            .history
            .iter()
            .map(|(_, value)| *value)
            .reduce(f64::min)
            .unwrap_or(value);
        self.history.push_back((observation.timestamp_ms, value));
        if value - minimum < self.minimum_delta
            || self.last_emit.is_some_and(|last| {
                observation.timestamp_ms.saturating_sub(last) < self.cooldown_ms
            })
        {
            return None;
        }
        self.last_emit = Some(observation.timestamp_ms);
        Some(Transition {
            rule_id: self.id,
            event: self.event.clone(),
            timestamp_ms: observation.timestamp_ms,
            state: TransitionState::Entered,
            value,
            confidence,
        })
    }
}

impl CoordinatedRule {
    fn observe(&mut self, observation: &Observation) -> Option<Transition> {
        if self
            .element_id
            .is_some_and(|element_id| element_id != observation.element_id)
        {
            return None;
        }
        self.runtime.observe(observation)
    }
}

fn build_live_pipeline(state: &State, profile_id: ProfileId) -> Result<LivePipeline, RpcError> {
    let profile = state.profiles.load(profile_id).map_err(store_error)?;
    let settings = state.local_config.load_settings().map_err(internal_error)?;
    let directory = state.profiles.profile_directory(profile.id);
    let mut pipeline = build_profile_pipeline(profile, &directory, settings.analysis_fps)?;
    pipeline.collection_policy = state
        .local_config
        .collection_policy(profile_id)
        .map_err(internal_error)?;
    Ok(pipeline)
}

fn build_profile_pipeline(
    profile: Profile,
    directory: &Path,
    analysis_fps: u8,
) -> Result<LivePipeline, RpcError> {
    let profile_revision = profile.revision;
    let profile_sha256 = collection::profile_sha256(&profile);
    let collection_aliases = profile
        .elements
        .iter()
        .map(|element| (element.id, suite_alias(&element.name)))
        .chain(
            profile
                .derived_observations
                .iter()
                .map(|derived| (derived.id, suite_alias(&derived.name))),
        )
        .collect();
    let processors = profile
        .elements
        .iter()
        .filter(|element| element.enabled)
        .map(|element| {
            Ok(FrameProcessor::new(
                AnalysisScheduler::new(analysis_fps).map_err(internal_error)?,
                configured_detector(&element.detector, directory)?,
                element.region,
                detector_id(&element.detector),
                element.id,
                NoopRule,
            ))
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    if processors.is_empty() {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "active profile has no enabled element",
        ));
    }
    let rules = profile
        .rules
        .iter()
        .map(build_coordinated_rule)
        .collect::<Result<Vec<_>, RpcError>>()?;
    Ok(LivePipeline {
        processors,
        rules,
        derived: profile
            .derived_observations
            .into_iter()
            .filter(|derived| derived.enabled)
            .collect(),
        initial_events: profile
            .rules
            .iter()
            .map(|rule| rule.event.clone())
            .collect(),
        profile_id: profile.id.to_string(),
        game: profile.game,
        profile_revision,
        profile_sha256,
        collection_aliases,
        collection_policy: CollectionPolicy::default(),
    })
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn build_coordinated_rule(
    rule: &yash_app_events_profile::EventRule,
) -> Result<CoordinatedRule, RpcError> {
    let common = || TemporalRuleConfig {
        id: rule.id,
        event: rule.event.clone(),
        predicate: ValuePredicate::Boolean { expected: true },
        minimum_confidence: rule.minimum_confidence,
        required_samples: usize::from(rule.required_samples),
        sample_window: usize::from(rule.sample_window),
        stable_for: Duration::from_millis(rule.stable_for_ms),
        cooldown: Duration::from_millis(rule.cooldown_ms),
        emit_initial: rule.emit_initial,
        update_interval: rule.update_interval_ms.map(Duration::from_millis),
    };
    let (element_id, runtime): (_, Box<dyn ObservationRule + Send>) = match &rule.predicate {
        RulePredicate::RapidIncrease {
            minimum_delta,
            within_ms,
        } => (
            Some(rule.element_id),
            Box::new(RapidIncreaseRule {
                id: rule.id,
                event: rule.event.clone(),
                minimum_delta: *minimum_delta as f64,
                within_ms: *within_ms,
                cooldown_ms: rule.cooldown_ms,
                minimum_confidence: rule.minimum_confidence,
                history: VecDeque::new(),
                last_emit: None,
            }),
        ),
        RulePredicate::NumericBelow => (
            Some(rule.element_id),
            Box::new(
                NumericRule::new(NumericRuleConfig {
                    id: rule.id,
                    event: rule.event.clone(),
                    enter_below: rule.enter_below,
                    leave_above: rule.leave_above,
                    minimum_confidence: rule.minimum_confidence,
                    required_samples: usize::from(rule.required_samples),
                    sample_window: usize::from(rule.sample_window),
                    cooldown: Duration::from_millis(rule.cooldown_ms),
                    stable_for: Duration::from_millis(rule.stable_for_ms),
                    emit_initial: rule.emit_initial,
                    update_interval: rule.update_interval_ms.map(Duration::from_millis),
                })
                .map_err(internal_error)?,
            ),
        ),
        RulePredicate::Boolean { expected } => {
            let mut config = common();
            config.predicate = ValuePredicate::Boolean {
                expected: *expected,
            };
            (
                Some(rule.element_id),
                Box::new(TemporalRule::new(config).map_err(internal_error)?),
            )
        }
        RulePredicate::TextEquals { expected } => {
            let mut config = common();
            config.predicate = ValuePredicate::TextEquals {
                expected: expected.clone(),
            };
            (
                Some(rule.element_id),
                Box::new(TemporalRule::new(config).map_err(internal_error)?),
            )
        }
        RulePredicate::TextContains { needle } => {
            let mut config = common();
            config.predicate = ValuePredicate::TextContains {
                needle: needle.clone(),
            };
            (
                Some(rule.element_id),
                Box::new(TemporalRule::new(config).map_err(internal_error)?),
            )
        }
        predicate @ (RulePredicate::All { .. } | RulePredicate::Any { .. }) => (
            None,
            Box::new(
                CompositeRule::new(CompositeRuleConfig {
                    id: rule.id,
                    event: rule.event.clone(),
                    predicate: predicate.clone(),
                    minimum_confidence: rule.minimum_confidence,
                    required_samples: usize::from(rule.required_samples),
                    sample_window: usize::from(rule.sample_window),
                    stable_for: Duration::from_millis(rule.stable_for_ms),
                    cooldown: Duration::from_millis(rule.cooldown_ms),
                    emit_initial: rule.emit_initial,
                    update_interval: rule.update_interval_ms.map(Duration::from_millis),
                })
                .map_err(internal_error)?,
            ),
        ),
    };
    Ok(CoordinatedRule {
        element_id,
        runtime,
    })
}

fn start_live_analysis(state: &Arc<State>, mut pipeline: LivePipeline) {
    let (stop, mut stopped) = oneshot::channel();
    *state
        .analysis_metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = AnalysisMetrics::default();
    state
        .collector
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .configure(
            pipeline.profile_id.clone(),
            pipeline.collection_policy.clone(),
        );
    let task_state = Arc::clone(state);
    let task = tokio::spawn(async move {
        let mut publication = PublicationState::default();
        publication.event_states.extend(
            pipeline
                .initial_events
                .iter()
                .map(|event| (event.clone(), json!(false))),
        );
        let mut last_sequence = None;
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                () = tokio::time::sleep(Duration::from_millis(5)) => {
                    let Some(frame) = task_state.latest_frame.latest() else { continue; };
                    if last_sequence == Some(frame.sequence) { continue; }
                    last_sequence = Some(frame.sequence);
                    let started = Instant::now();
                    let capture = json!({"active":true,"source":frame.source_id,"resolution":[frame.width,frame.height],"pixel_format":format!("{:?}",frame.format).to_ascii_lowercase()});
                    let mut collection_observations = BTreeMap::new();
                    let mut collection_transitions = Vec::new();
                    for processor in &mut pipeline.processors {
                        let Some(processed) = processor.process(&frame) else { continue; };
                        if let Some(alias) = pipeline.collection_aliases.get(&processed.observation.element_id) {
                            collection_observations.insert(alias.clone(), processed.observation.clone());
                        }
                        let observation_timestamp_ms = processed.observation.timestamp_ms;
                        update_analysis_metrics(&task_state.analysis_metrics, started.elapsed(), processed.observation.status == yash_app_events_engine::ObservationStatus::Error);
                        let transitions: Vec<_> = pipeline.rules.iter_mut().filter_map(|rule| rule.observe(&processed.observation)).collect();
                        collection_transitions.extend(transitions.iter().cloned());
                        if transitions.is_empty() {
                            if let Err(error) = publish_processed(&task_state,processed,&pipeline.profile_id,&pipeline.game,Utc::now(),capture.clone(),&mut publication) { set_output_error(&task_state,&format!("{}: {}",error.code,error.message)); }
                        } else {
                            for transition in transitions {
                                let publication_item = ProcessedFrame { observation: processed.observation.clone(), transition: Some(transition) };
                                if let Err(error) = publish_processed(&task_state,publication_item,&pipeline.profile_id,&pipeline.game,Utc::now(),capture.clone(),&mut publication) { set_output_error(&task_state,&format!("{}: {}",error.code,error.message)); }
                            }
                        }
                        for derived in &pipeline.derived {
                            let Some(observation) = compose_observation(derived, &publication.observations, observation_timestamp_ms) else { continue; };
                            if let Some(alias) = pipeline.collection_aliases.get(&observation.element_id) {
                                collection_observations.insert(alias.clone(), observation.clone());
                            }
                            let transitions: Vec<_> = pipeline.rules.iter_mut().filter_map(|rule| rule.observe(&observation)).collect();
                            collection_transitions.extend(transitions.iter().cloned());
                            if transitions.is_empty() {
                                let item = ProcessedFrame { observation, transition: None };
                                if let Err(error) = publish_processed(&task_state,item,&pipeline.profile_id,&pipeline.game,Utc::now(),capture.clone(),&mut publication) { set_output_error(&task_state,&format!("{}: {}",error.code,error.message)); }
                            } else {
                                for transition in transitions {
                                    let item = ProcessedFrame { observation: observation.clone(), transition: Some(transition) };
                                    if let Err(error) = publish_processed(&task_state,item,&pipeline.profile_id,&pipeline.game,Utc::now(),capture.clone(),&mut publication) { set_output_error(&task_state,&format!("{}: {}",error.code,error.message)); }
                                }
                            }
                        }
                    }
                    schedule_collection(
                        &task_state,
                        &pipeline,
                        Arc::clone(&frame),
                        collection_observations,
                        collection_transitions,
                    );
                }
            }
        }
    });
    *state
        .analysis
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(LiveAnalysis { stop, task });
}

#[allow(clippy::too_many_lines)]
fn schedule_collection(
    state: &Arc<State>,
    pipeline: &LivePipeline,
    frame: Arc<Frame>,
    observations: BTreeMap<String, Observation>,
    transitions: Vec<Transition>,
) {
    let now = Instant::now();
    let thumbnail = collection::thumbnail(&frame);
    let evidence =
        collection::evidence_signature(&observations, &pipeline.collection_policy.novelty_targets);
    let (policy, scene_difference, scene_novel, evidence_novel) = {
        let mut runtime = state
            .collector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !runtime.due(now, frame.sequence) {
            return;
        }
        runtime.last_attempt = Some(now);
        let Some(policy) = runtime.policy.clone() else {
            return;
        };
        let scene_difference = runtime
            .last_thumbnail
            .as_deref()
            .and_then(|previous| collection::similarity(previous, &thumbnail));
        let scene_novel =
            scene_difference.is_none_or(|difference| difference > policy.similarity_threshold);
        let evidence_novel = runtime
            .last_evidence
            .as_ref()
            .is_none_or(|previous| previous != &evidence);
        if !scene_novel && !evidence_novel && transitions.is_empty() {
            runtime.duplicates = runtime.duplicates.saturating_add(1);
            return;
        }
        runtime.write_in_progress = true;
        (policy, scene_difference, scene_novel, evidence_novel)
    };
    let state = Arc::clone(state);
    let profile_id = pipeline.profile_id.clone();
    let profile_revision = pipeline.profile_revision;
    let profile_sha256 = pipeline.profile_sha256.clone();
    let game = pipeline.game.clone();
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<(String, PathBuf), RpcError> {
            let usage = collection::storage_stats(&policy.dataset_root);
            if usage.pending_items >= policy.maximum_pending_items
                || usage.bytes >= policy.maximum_bytes
            {
                return Err(RpcError::new(
                    error_code::INVALID_REQUEST,
                    "collection quota reached; review pending evidence or increase the limit",
                ));
            }
            let png = encode_frame_png(&frame)?;
            if usage
                .bytes
                .saturating_add(u64::try_from(png.len()).unwrap_or(u64::MAX))
                > policy.maximum_bytes
            {
                return Err(RpcError::new(
                    error_code::INVALID_REQUEST,
                    "collection byte quota would be exceeded",
                ));
            }
            let id = format!(
                "{}-{}",
                Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
                frame.sequence
            );
            let mut evidence_reasons = Vec::new();
            if scene_novel {
                evidence_reasons.push("scene_novel".into());
            }
            if evidence_novel {
                evidence_reasons.push("detector_evidence_changed".into());
            }
            if !transitions.is_empty() {
                evidence_reasons.push("event_transition".into());
            }
            if observations.values().any(|observation| {
                observation.status != yash_app_events_engine::ObservationStatus::Valid
                    || observation.confidence.unwrap_or(0.0) < 0.5
            }) {
                evidence_reasons.push("uncertain_observation".into());
            }
            let item = CollectionItem {
                schema: 1,
                id: id.clone(),
                created_at: EventRecord::timestamp_rfc3339(Utc::now()),
                game,
                profile_id,
                profile_revision,
                profile_sha256,
                frame: CollectedFrame {
                    sequence: frame.sequence,
                    timestamp_ms: u64::try_from(frame.timestamp.as_millis()).unwrap_or(u64::MAX),
                    width: frame.width,
                    height: frame.height,
                    pixel_format: format!("{:?}", frame.format).to_ascii_lowercase(),
                    source: frame.source_id.clone(),
                    image: PathBuf::from("frame.png"),
                },
                image_sha256: collection::image_sha256(&png),
                observations,
                transitions,
                reason: CollectionReason {
                    trigger: "interval".into(),
                    scene_difference,
                    scene_novel,
                    evidence_novel,
                    evidence_reasons,
                },
                review: ReviewRecord::default(),
                thumbnail: thumbnail.clone(),
            };
            let path = collection::persist_item(&policy.dataset_root, &item, &png)
                .map_err(internal_error)?;
            Ok((id, path))
        })();
        let mut runtime = state
            .collector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        runtime.write_in_progress = false;
        match result {
            Ok((id, _)) => {
                runtime.saved = runtime.saved.saturating_add(1);
                runtime.last_item = Some(id);
                runtime.last_thumbnail = Some(thumbnail);
                runtime.last_evidence = Some(evidence);
                runtime.last_error = None;
            }
            Err(error) => {
                runtime.errors = runtime.errors.saturating_add(1);
                runtime.last_error = Some(error.message);
            }
        }
    });
}

fn compose_observation(
    derived: &yash_app_events_profile::DerivedObservation,
    observations: &serde_json::Map<String, Value>,
    timestamp_ms: u64,
) -> Option<Observation> {
    let mut text = derived.format.clone();
    let mut confidence = 1.0_f32;
    for input in &derived.inputs {
        let observation: Observation =
            serde_json::from_value(observations.get(&input.element_id.to_string())?.clone())
                .ok()?;
        if observation.status != yash_app_events_engine::ObservationStatus::Valid {
            return Some(Observation {
                detector_id: derived.detector_id,
                element_id: derived.id,
                timestamp_ms,
                value: yash_app_events_engine::ObservationValue::None,
                confidence: None,
                status: yash_app_events_engine::ObservationStatus::Unknown,
                diagnostic: format!("input {} is not valid", input.name),
            });
        }
        let value = match observation.value {
            yash_app_events_engine::ObservationValue::Text(value) => value,
            yash_app_events_engine::ObservationValue::Number(value) => value.to_string(),
            yash_app_events_engine::ObservationValue::Boolean(value) => value.to_string(),
            yash_app_events_engine::ObservationValue::None => {
                return Some(Observation {
                    detector_id: derived.detector_id,
                    element_id: derived.id,
                    timestamp_ms,
                    value: yash_app_events_engine::ObservationValue::None,
                    confidence: None,
                    status: yash_app_events_engine::ObservationStatus::Unknown,
                    diagnostic: format!("input {} has no value", input.name),
                });
            }
        };
        text = text.replace(&format!("{{{}}}", input.name), &value);
        confidence = confidence.min(observation.confidence.unwrap_or(0.0));
    }
    Some(Observation {
        detector_id: derived.detector_id,
        element_id: derived.id,
        timestamp_ms,
        value: yash_app_events_engine::ObservationValue::Text(text),
        confidence: Some(confidence),
        status: yash_app_events_engine::ObservationStatus::Valid,
        diagnostic: format!("composed from {} inputs", derived.inputs.len()),
    })
}

fn update_analysis_metrics(metrics: &Mutex<AnalysisMetrics>, latency: Duration, errored: bool) {
    let now = Instant::now();
    let mut metrics = metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    metrics.first.get_or_insert(now);
    metrics.last = Some(now);
    metrics.frames = metrics.frames.saturating_add(1);
    metrics.last_processing_latency = Some(latency);
    metrics.detector_errors = metrics.detector_errors.saturating_add(u64::from(errored));
}

#[allow(clippy::cast_precision_loss)]
fn analysis_fps(state: &State) -> f32 {
    let metrics = state
        .analysis_metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    metrics
        .first
        .zip(metrics.last)
        .map_or(0.0, |(first, last)| {
            metrics.frames.saturating_sub(1) as f32
                / last
                    .saturating_duration_since(first)
                    .as_secs_f32()
                    .max(0.001)
        })
}

fn capture_status(state: &State) -> Value {
    let analysis = state
        .analysis_metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let latency = analysis
        .last_processing_latency
        .map(|value| value.as_secs_f64() * 1000.0);
    let detector_errors = analysis.detector_errors;
    drop(analysis);
    let capture = state
        .capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(capture) = capture.as_ref() else {
        return json!({"active":false,"source":null,"metrics":{"input_fps":0.0,"analysis_fps":analysis_fps(state),"replaced_frames":state.latest_frame.replacements(),"last_processing_latency_ms":latency,"detector_errors":detector_errors}});
    };
    let metrics = capture.metrics();
    json!({"active":true,"source":capture.selected_source().label,"pipewire_node_id":capture.selected_source().pipewire_node_id,"metrics":{"input_frames":metrics.input_frames,"input_fps":metrics.input_fps,"analysis_fps":analysis_fps(state),"replaced_frames":metrics.replaced_frames,"last_frame_age_ms":metrics.last_frame_age.and_then(|age|u64::try_from(age.as_millis()).ok()),"last_processing_latency_ms":latency,"detector_errors":detector_errors,"width":metrics.width,"height":metrics.height,"pixel_format":metrics.pixel_format,"error":metrics.error}})
}

fn snapshot_capture(state: &State, path: &Path) -> Result<Value, RpcError> {
    let frame = state.latest_frame.latest().ok_or_else(|| {
        RpcError::new(
            error_code::INVALID_REQUEST,
            "no captured frame is available",
        )
    })?;
    let bytes = encode_frame_png(&frame)?;
    yash_app_events_profile::atomic_write(path, &bytes).map_err(internal_error)?;
    Ok(json!({"saved":true,"path":path,"width":frame.width,"height":frame.height,"explicit":true}))
}

fn preview_frame(state: &State, params: &PreviewFrameParams) -> Result<Value, RpcError> {
    if state.preview_clients.load(Ordering::Relaxed) == 0 {
        return Err(RpcError::new(
            error_code::INVALID_REQUEST,
            "preview.start is required",
        ));
    }
    let frame = state.latest_frame.latest().ok_or_else(|| {
        RpcError::new(
            error_code::INVALID_REQUEST,
            "no captured frame is available",
        )
    })?;
    let maximum_width = params.maximum_width.clamp(320, 1600);
    let maximum_height = params.maximum_height.clamp(180, 900);
    let preview = downscale_frame(&frame, maximum_width, maximum_height)?;
    let bytes = encode_frame_png(&preview)?;
    Ok(
        json!({"sequence":frame.sequence,"timestamp_ms":u64::try_from(frame.timestamp.as_millis()).unwrap_or(u64::MAX),"width":preview.width,"height":preview.height,"mime":"image/png","bytes":bytes}),
    )
}

fn downscale_frame(
    frame: &Frame,
    maximum_width: u32,
    maximum_height: u32,
) -> Result<Frame, RpcError> {
    let (width, height) = if frame.width <= maximum_width && frame.height <= maximum_height {
        (frame.width, frame.height)
    } else if u64::from(frame.width) * u64::from(maximum_height)
        >= u64::from(frame.height) * u64::from(maximum_width)
    {
        (
            maximum_width,
            u32::try_from(
                (u64::from(frame.height) * u64::from(maximum_width) / u64::from(frame.width))
                    .max(1),
            )
            .map_err(internal_error)?,
        )
    } else {
        (
            u32::try_from(
                (u64::from(frame.width) * u64::from(maximum_height) / u64::from(frame.height))
                    .max(1),
            )
            .map_err(internal_error)?,
            maximum_height,
        )
    };
    let bytes_per_pixel = frame.format.bytes_per_pixel();
    let width_usize = usize::try_from(width).map_err(internal_error)?;
    let height_usize = usize::try_from(height).map_err(internal_error)?;
    let row_stride = width_usize
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| internal_error("preview size overflow"))?;
    let mut pixels = vec![
        0_u8;
        row_stride
            .checked_mul(height_usize)
            .ok_or_else(|| internal_error("preview size overflow"))?
    ];
    for y in 0..height_usize {
        for x in 0..width_usize {
            let source_x = x * usize::try_from(frame.width).map_err(internal_error)? / width_usize;
            let source_y =
                y * usize::try_from(frame.height).map_err(internal_error)? / height_usize;
            let source = source_y
                .checked_mul(frame.row_stride)
                .and_then(|offset| offset.checked_add(source_x * bytes_per_pixel))
                .ok_or_else(|| internal_error("preview source overflow"))?;
            let destination = y * row_stride + x * bytes_per_pixel;
            pixels[destination..destination + bytes_per_pixel].copy_from_slice(
                frame
                    .data
                    .get(source..source + bytes_per_pixel)
                    .ok_or_else(|| internal_error("preview source truncated"))?,
            );
        }
    }
    Frame::new(
        frame.sequence,
        frame.timestamp,
        FrameLayout {
            width,
            height,
            row_stride,
            format: frame.format,
        },
        frame.source_id.clone(),
        Arc::from(pixels),
    )
    .map_err(internal_error)
}

fn encode_frame_png(frame: &Frame) -> Result<Vec<u8>, RpcError> {
    let bytes_per_pixel = frame.format.bytes_per_pixel();
    let packed_stride = usize::try_from(frame.width)
        .map_err(internal_error)?
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| internal_error("snapshot size overflow"))?;
    let height = usize::try_from(frame.height).map_err(internal_error)?;
    let mut packed = Vec::with_capacity(
        packed_stride
            .checked_mul(height)
            .ok_or_else(|| internal_error("snapshot size overflow"))?,
    );
    for row in 0..height {
        let offset = row
            .checked_mul(frame.row_stride)
            .ok_or_else(|| internal_error("snapshot row overflow"))?;
        let end = offset
            .checked_add(packed_stride)
            .ok_or_else(|| internal_error("snapshot row overflow"))?;
        packed.extend_from_slice(
            frame
                .data
                .get(offset..end)
                .ok_or_else(|| internal_error("snapshot frame is truncated"))?,
        );
    }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, frame.width, frame.height);
        encoder.set_color(match frame.format {
            PixelFormat::Rgb8 => png::ColorType::Rgb,
            PixelFormat::Rgba8 => png::ColorType::Rgba,
        });
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(internal_error)?;
        writer.write_image_data(&packed).map_err(internal_error)?;
    }
    Ok(output)
}

fn status(state: &State) -> Status {
    let usage = process_usage();
    let capture = state
        .capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let capture_metrics = capture.as_ref().map(PortalCapture::metrics);
    let settings = state.local_config.load_settings().unwrap_or_default();
    let analysis = state
        .analysis_metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let last_processing_latency_ms = analysis
        .last_processing_latency
        .map(|latency| latency.as_secs_f64() * 1000.0);
    let detector_errors = analysis.detector_errors;
    drop(analysis);
    Status {
        daemon_instance: state.instance,
        capture_active: capture.is_some(),
        selected_source: capture
            .as_ref()
            .map(|capture| capture.selected_source().label.clone()),
        connected_clients: state.connected.load(Ordering::Relaxed),
        active_profile: settings.active_profile.map(|profile| profile.to_string()),
        input_fps: capture_metrics
            .as_ref()
            .map_or(0.0, |metrics| metrics.input_fps),
        analysis_fps: analysis_fps(state),
        replaced_frames: capture_metrics.as_ref().map_or_else(
            || state.latest_frame.replacements(),
            |metrics| metrics.replaced_frames,
        ),
        last_processing_latency_ms,
        detector_errors,
        daemon_cpu_percent: usage.cpu_percent,
        daemon_rss_bytes: usage.rss_bytes,
        output_error: state
            .output_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .or_else(|| state.output_routes.last_error()),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcessUsage {
    cpu_percent: f32,
    rss_bytes: u64,
}

#[derive(Debug, Default)]
struct ProcessSampler {
    previous_cpu_ns: Option<u64>,
    previous_at: Option<Instant>,
    last_usage: ProcessUsage,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn process_usage() -> ProcessUsage {
    static SAMPLER: OnceLock<Mutex<ProcessSampler>> = OnceLock::new();
    let cpu_ns = fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|value| {
            let fields = value
                .rsplit_once(") ")?
                .1
                .split_whitespace()
                .collect::<Vec<_>>();
            let user = fields.get(11)?.parse::<u64>().ok()?;
            let system = fields.get(12)?.parse::<u64>().ok()?;
            Some(user.saturating_add(system).saturating_mul(10_000_000))
        });
    let rss_bytes = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|value| {
            value.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        })
        .unwrap_or(0)
        .saturating_mul(1024);
    let now = Instant::now();
    let mut sampler = SAMPLER
        .get_or_init(|| Mutex::new(ProcessSampler::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if sampler
        .previous_at
        .is_some_and(|previous| now.duration_since(previous) < Duration::from_millis(500))
    {
        return sampler.last_usage;
    }
    let cpu_percent = cpu_ns
        .zip(sampler.previous_cpu_ns)
        .zip(sampler.previous_at)
        .map_or(0.0, |((current, previous), previous_at)| {
            let wall_ns = now.duration_since(previous_at).as_nanos().max(1) as f64;
            ((current.saturating_sub(previous) as f64 / wall_ns) * 100.0) as f32
        });
    sampler.previous_cpu_ns = cpu_ns;
    sampler.previous_at = Some(now);
    let usage = ProcessUsage {
        cpu_percent,
        rss_bytes,
    };
    sampler.last_usage = usage;
    usage
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
struct RevisionParams {
    profile_id: ProfileId,
    revision: u64,
}
#[derive(Deserialize)]
struct RollbackParams {
    profile_id: ProfileId,
    revision: u64,
    expected_revision: u64,
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
struct CatalogInstallParams {
    id: String,
    version: String,
    catalog_revision: u64,
    sha256: String,
}
#[derive(Deserialize)]
struct ExportParams {
    profile_id: ProfileId,
    path: PathBuf,
}
#[derive(Deserialize)]
struct SyntheticReplayParams {
    fills: Vec<u8>,
    #[serde(default = "default_profile_id")]
    profile_id: String,
    #[serde(default = "default_game")]
    game: String,
}
#[derive(Clone, Deserialize)]
struct DetectorTestParams {
    profile_id: ProfileId,
    element_id: ElementId,
    #[serde(default = "default_fill")]
    fill: u8,
    #[serde(default = "default_change_value")]
    change_value: u8,
    #[serde(default)]
    use_frozen: bool,
    #[serde(default)]
    element: Option<yash_app_events_profile::Element>,
}
#[derive(Deserialize)]
struct PreviewFrameParams {
    #[serde(default = "default_preview_width")]
    maximum_width: u32,
    #[serde(default = "default_preview_height")]
    maximum_height: u32,
}

const fn default_preview_width() -> u32 {
    1280
}

const fn default_preview_height() -> u32 {
    720
}
#[derive(Deserialize)]
struct CaptureTemplateParams {
    profile_id: ProfileId,
    element_id: ElementId,
    expected_revision: u64,
    name: String,
}
#[derive(Deserialize)]
struct ProfileReplayParams {
    profile_id: ProfileId,
    element_id: ElementId,
    values: Vec<u8>,
}
#[derive(Deserialize)]
struct CaptureSelectParams {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    profile_id: Option<ProfileId>,
}
#[derive(Deserialize)]
struct AutoCaptureParams {
    profile_id: ProfileId,
    enabled: bool,
    process_match: Option<String>,
}

#[derive(Deserialize)]
struct OutputSetParams {
    profile_id: ProfileId,
    route: OutputRoute,
}

#[derive(Deserialize)]
struct OutputRouteParams {
    profile_id: ProfileId,
    route_id: Uuid,
}

#[derive(Deserialize)]
struct OutputEnableParams {
    profile_id: ProfileId,
    route_id: Uuid,
    enabled: bool,
}

#[derive(Deserialize)]
struct OutputRecipeDraftParams {
    profile_id: ProfileId,
    recipe_id: Uuid,
    sha256: String,
    name: String,
    trigger: OutputTrigger,
    format: OutputFormat,
}

#[derive(Deserialize)]
struct OutputRecipeInstallParams {
    #[serde(flatten)]
    draft: OutputRecipeDraftParams,
    sink: OutputSink,
}

#[derive(Deserialize)]
struct CollectionPolicyParams {
    profile_id: ProfileId,
    policy: CollectionPolicy,
}

#[derive(Deserialize)]
struct CollectionItemsParams {
    profile_id: ProfileId,
    #[serde(default)]
    status: Option<ReviewStatus>,
}

#[derive(Deserialize)]
struct CollectionItemParams {
    profile_id: ProfileId,
    id: String,
}

#[derive(Deserialize)]
struct CollectionReviewParams {
    profile_id: ProfileId,
    id: String,
    action: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    expected_observations: BTreeMap<String, ExpectedObservation>,
}

#[derive(Deserialize)]
struct CollectionAutoReviewParams {
    profile_id: ProfileId,
    #[serde(default)]
    promote: bool,
}

#[derive(Deserialize)]
struct CollectionCompareParams {
    first: PathBuf,
    second: PathBuf,
    #[serde(default = "default_collection_similarity_threshold")]
    threshold: f32,
}

const fn default_collection_similarity_threshold() -> f32 {
    0.015
}

#[derive(Clone, Debug, Deserialize)]
struct DiagnosticParams {
    #[serde(default)]
    profile_id: Option<ProfileId>,
    #[serde(default)]
    selected_element_ids: Vec<ElementId>,
}

#[derive(Clone, Debug, Deserialize)]
struct DiagnosticExportParams {
    path: PathBuf,
    bundle: DiagnosticParams,
    privacy_reviewed: bool,
    expected_total_uncompressed_bytes: usize,
}
const fn default_fill() -> u8 {
    5
}
const fn default_change_value() -> u8 {
    255
}

fn default_profile_id() -> String {
    "synthetic-health".into()
}
fn default_game() -> String {
    "synthetic_game".into()
}

/// Daemon transport failure.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("daemon I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("daemon JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("profile catalog initialization failed: {0}")]
    Catalog(#[from] CatalogError),
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
            state_root: directory.join("state"),
            cache_root: directory.join("cache"),
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

    async fn notification(connection: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(1), connection.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn cached_catalog_is_listed_and_install_requires_reviewed_revision() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("cache")).unwrap();
        fs::write(
            directory.path().join("cache/catalog.json"),
            serde_json::to_vec(&json!({
                "schema": 1,
                "revision": 4,
                "generated_at": "2026-07-18T00:00:00Z",
                "profiles": [{
                    "id": "demo-game.stage-tracker",
                    "game": "demo_game",
                    "game_slug": "demo-game",
                    "profile_slug": "stage-tracker",
                    "profile_id": "00000000-0000-0000-0000-000000000001",
                    "name": "Stage tracker",
                    "description": "Media-free stage tracker",
                    "version": "1.0.0",
                    "package": "profile--demo-game--stage-tracker--v1.0.0.hudprofile",
                    "bytes": 42,
                    "sha256": "0".repeat(64),
                    "profile_schema": 1,
                    "minimum_app_version": "0.0.1",
                    "media_free": true,
                    "detectors": ["ocr"],
                    "tested_layouts": [],
                    "output_recipes": ["Current stage"],
                    "license": "MIT",
                    "verification": {
                        "status": "verified",
                        "date": "2026-07-18",
                        "evidence": "fixture"
                    },
                    "withdrawn": false
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut client).await;
        let status = call(&mut client, 2, method::CATALOG_STATUS, Value::Null).await;
        assert_eq!(status["result"]["revision"], 4);
        let list = call(&mut client, 3, method::CATALOG_LIST, Value::Null).await;
        assert_eq!(list["result"]["profiles"][0]["installed"], false);
        let install = call(
            &mut client,
            4,
            method::CATALOG_INSTALL,
            json!({
                "id": "demo-game.stage-tracker",
                "version": "1.0.0",
                "catalog_revision": 3,
                "sha256": "0".repeat(64),
            }),
        )
        .await;
        assert_eq!(install["error"]["code"], error_code::REVISION_CONFLICT);
        call(&mut client, 5, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
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
        let history = call(
            &mut client,
            5,
            method::PROFILE_REVISIONS,
            json!({"profile_id":profile.id}),
        )
        .await;
        assert_eq!(history["result"].as_array().unwrap().len(), 2);
        let rollback = call(
            &mut client,
            6,
            method::PROFILE_ROLLBACK,
            json!({"profile_id":profile.id,"revision":0,"expected_revision":1}),
        )
        .await;
        assert_eq!(rollback["result"]["revision"], 2);
        let old = call(
            &mut client,
            7,
            method::PROFILE_REVISION_GET,
            json!({"profile_id":profile.id,"revision":1}),
        )
        .await;
        assert_eq!(old["result"]["revision"], 1);
        call(&mut client, 8, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn synthetic_health_replay_reaches_files_state_and_live_subscription() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut subscriber = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        let mut control = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut subscriber).await;
        handshake(&mut control).await;
        let acknowledgement = call(&mut subscriber, 2, method::EVENTS_SUBSCRIBE, Value::Null).await;
        assert_eq!(acknowledgement["result"]["capacity"], 64);
        let replay = call(
            &mut control,
            2,
            method::REPLAY_SYNTHETIC_HEALTH,
            json!({"fills":[8,8,1,1,1,4,4],"profile_id":"synthetic","game":"synthetic_game"}),
        )
        .await;
        assert_eq!(replay["result"]["events"].as_array().unwrap().len(), 2);
        let entered = notification(&mut subscriber).await;
        let left = notification(&mut subscriber).await;
        assert_eq!(entered["method"], "event");
        assert_eq!(entered["params"]["state"], "entered");
        assert_eq!(entered["params"]["sequence"], 1);
        assert_eq!(left["params"]["state"], "left");
        assert_eq!(left["params"]["sequence"], 2);

        let state = call(&mut control, 3, method::STATE_GET, Value::Null).await;
        assert_eq!(state["result"]["sequence"], 2);
        assert_eq!(state["result"]["events"]["critical_health"], false);
        let durable_state: Value =
            serde_json::from_slice(&fs::read(directory.path().join("state/state.json")).unwrap())
                .unwrap();
        assert_eq!(durable_state, state["result"]);
        let lines: Vec<Value> = fs::read_to_string(directory.path().join("state/events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], entered["params"]);
        assert_eq!(lines[1], left["params"]);
        call(&mut control, 4, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn profile_output_route_can_be_enabled_tested_and_runs_on_matching_events() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut client).await;
        let profile = Profile::new("Output routes", "blazblue_entropy_effect", 10, 2);
        assert_eq!(
            call(
                &mut client,
                2,
                method::PROFILE_CREATE,
                json!({"profile":profile}),
            )
            .await["result"]["created"],
            true
        );
        let output_path = directory.path().join("stage-markers.jsonl");
        let route = OutputRoute {
            id: Uuid::new_v4(),
            name: "BlazBlue stage markers".into(),
            enabled: false,
            trigger: OutputTrigger::Event {
                events: vec!["critical_health".into()],
                states: vec![EventState::Entered, EventState::Left],
            },
            format: OutputFormat::JsonTemplate {
                template: json!({
                    "marker":"{{event.event}}-{{event.state}}",
                    "value":"{{event.value}}",
                    "sequence":"{{event.sequence}}"
                }),
            },
            sink: OutputSink::File {
                path: output_path.clone(),
                mode: FileMode::Append,
            },
            source_recipe: None,
        };
        let set = call(
            &mut client,
            3,
            method::OUTPUT_SET,
            json!({"profile_id":profile.id,"route":route}),
        )
        .await;
        assert_eq!(set["result"][0]["enabled"], false);
        let enabled = call(
            &mut client,
            4,
            method::OUTPUT_ENABLE,
            json!({"profile_id":profile.id,"route_id":route.id,"enabled":true}),
        )
        .await;
        assert_eq!(enabled["result"][0]["enabled"], true);
        let tested = call(
            &mut client,
            5,
            method::OUTPUT_TEST,
            json!({"profile_id":profile.id,"route_id":route.id}),
        )
        .await;
        assert_eq!(tested["result"]["delivered"], true, "{tested}");
        fs::remove_file(&output_path).unwrap();

        let replay = call(
            &mut client,
            6,
            method::REPLAY_SYNTHETIC_HEALTH,
            json!({
                "fills":[8,8,1,1,1,4,4],
                "profile_id":profile.id,
                "game":"blazblue_entropy_effect"
            }),
        )
        .await;
        assert_eq!(replay["result"]["events"].as_array().unwrap().len(), 2);
        for _ in 0..100 {
            if fs::read_to_string(&output_path).is_ok_and(|contents| contents.lines().count() == 2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let markers: Vec<Value> = fs::read_to_string(output_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0]["marker"], "critical_health-entered");
        assert_eq!(markers[1]["marker"], "critical_health-left");
        call(&mut client, 7, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn profile_replay_publishes_raw_text_state_changes_to_file_routes() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut client).await;
        let mut profile = Profile::new("Replay output", "blazblue_entropy_effect", 10, 2);
        let element_id = ElementId::new();
        profile.elements.push(yash_app_events_profile::Element {
            id: element_id,
            name: "Health".into(),
            enabled: true,
            color: "#f00".into(),
            region: full_frame_region(),
            detector: ProfileDetector::ColorBar {
                id: DetectorId::new(),
                direction: BarDirection::LeftToRight,
                minimum_rgb: [180, 0, 0],
                maximum_rgb: [255, 60, 60],
                mask: None,
            },
        });
        call(
            &mut client,
            2,
            method::PROFILE_CREATE,
            json!({"profile":profile}),
        )
        .await;
        let output_path = directory.path().join("replay-state.txt");
        let route = OutputRoute {
            id: Uuid::new_v4(),
            name: "Replay state".into(),
            enabled: true,
            trigger: OutputTrigger::StateChange,
            format: OutputFormat::TextTemplate {
                template: format!("{{{{state.observations.{element_id}.value.value}}}}"),
                trailing_newline: true,
            },
            sink: OutputSink::File {
                path: output_path.clone(),
                mode: FileMode::Append,
            },
            source_recipe: None,
        };
        call(
            &mut client,
            3,
            method::OUTPUT_SET,
            json!({"profile_id":profile.id,"route":route}),
        )
        .await;
        let replay = call(
            &mut client,
            4,
            method::REPLAY_EVALUATE,
            json!({
                "schema":1,
                "profile_id":profile.id,
                "element_id":element_id,
                "values":[8,1],
                "image_frames":[],
                "expected_events":[],
                "regression":{
                    "minimum_precision":1.0,
                    "minimum_recall":1.0,
                    "maximum_mean_latency_ms":null
                }
            }),
        )
        .await;
        assert_eq!(replay["result"]["metrics"]["passed"], true, "{replay}");
        for _ in 0..100 {
            if fs::read_to_string(&output_path).is_ok_and(|contents| contents.lines().count() == 2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let states: Vec<f64> = fs::read_to_string(output_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(states, vec![0.8, 0.1]);
        call(&mut client, 5, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn portable_recipe_requires_hash_review_and_installs_disabled_with_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut client).await;
        let profile = Profile::new("Recipe profile", "blazblue_entropy_effect", 10, 2);
        call(
            &mut client,
            2,
            method::PROFILE_CREATE,
            json!({"profile":profile}),
        )
        .await;
        let recipe = OutputRecipe {
            schema: 1,
            id: Uuid::new_v4(),
            name: "Yash stage marker".into(),
            description: "Create one marker for each detected stage".into(),
            trigger: OutputTrigger::Event {
                events: vec!["stage_changed".into()],
                states: vec![EventState::Updated],
            },
            format: OutputFormat::JsonTemplate {
                template: json!({"marker":"stage-{{event.value}}"}),
            },
            suggested_sink: OutputRecipeSink::Command {
                program_name: "yash".into(),
                args: vec!["ipc".into(), "command".into(), "marker".into()],
                timeout_ms: 5_000,
            },
        };
        let recipes = directory
            .path()
            .join("data/profiles")
            .join(profile.id.to_string())
            .join("output-recipes");
        fs::create_dir_all(&recipes).unwrap();
        fs::write(
            recipes.join("yash-stage-marker.json"),
            serde_json::to_vec_pretty(&recipe).unwrap(),
        )
        .unwrap();
        let listed = call(
            &mut client,
            3,
            method::OUTPUT_RECIPE_LIST,
            json!({"profile_id":profile.id}),
        )
        .await;
        let entry = &listed["result"][0];
        assert_eq!(entry["recipe"]["name"], "Yash stage marker");
        let sha256 = entry["sha256"].as_str().unwrap();
        let draft = json!({
            "profile_id":profile.id,
            "recipe_id":recipe.id,
            "sha256":sha256,
            "name":"Customized stage marker",
            "trigger":recipe.trigger,
            "format":recipe.format
        });
        let previewed = call(&mut client, 4, method::OUTPUT_RECIPE_PREVIEW, draft.clone()).await;
        assert_eq!(previewed["result"]["executed"], false);
        assert_eq!(previewed["result"]["payload"]["marker"], "stage-test");
        let destination = directory.path().join("must-not-exist-before-enable.jsonl");
        let mut install = draft;
        install["sink"] = json!({
            "kind":"file","path":destination,"mode":"append"
        });
        let installed = call(&mut client, 5, method::OUTPUT_RECIPE_INSTALL, install).await;
        assert_eq!(installed["result"]["installed"]["enabled"], false);
        assert_eq!(
            installed["result"]["installed"]["source_recipe"]["sha256"],
            sha256
        );
        assert!(!destination.exists());

        let stale = call(
            &mut client,
            6,
            method::OUTPUT_RECIPE_PREVIEW,
            json!({
                "profile_id":profile.id,"recipe_id":recipe.id,"sha256":"00",
                "name":"stale","trigger":recipe.trigger,"format":recipe.format
            }),
        )
        .await;
        assert_eq!(stale["error"]["code"], error_code::INVALID_PARAMS);
        call(&mut client, 7, method::SHUTDOWN, Value::Null).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn detector_test_rpc_executes_all_deterministic_detector_types() {
        let directory = tempfile::tempdir().unwrap();
        let (socket, task) = start(directory.path()).await;
        let mut client = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut client).await;

        let mut color_profile = Profile::new("Color", "synthetic_game", 10, 2);
        let color_element = ElementId::new();
        color_profile
            .elements
            .push(yash_app_events_profile::Element {
                id: color_element,
                name: "Health".into(),
                enabled: true,
                color: "#f00".into(),
                region: NormalizedRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                detector: ProfileDetector::ColorBar {
                    id: DetectorId::new(),
                    direction: BarDirection::LeftToRight,
                    minimum_rgb: [180, 0, 0],
                    maximum_rgb: [255, 60, 60],
                    mask: None,
                },
            });
        color_profile
            .rules
            .push(yash_app_events_profile::EventRule {
                id: RuleId::new(),
                element_id: color_element,
                event: "critical_health".into(),
                enter_below: 0.2,
                leave_above: 0.3,
                minimum_confidence: 0.0,
                required_samples: 2,
                sample_window: 3,
                cooldown_ms: 0,
                predicate: RulePredicate::default(),
                stable_for_ms: 0,
                emit_initial: false,
                update_interval_ms: None,
            });
        call(
            &mut client,
            2,
            method::PROFILE_CREATE,
            json!({"profile":color_profile}),
        )
        .await;
        let color = call(
            &mut client,
            3,
            method::DETECTOR_TEST,
            json!({"profile_id":color_profile.id,"element_id":color_element,"fill":5}),
        )
        .await;
        assert_eq!(color["result"]["observations"][0]["value"], 0.5);
        let evaluation = call(
            &mut client,
            30,
            method::REPLAY_EVALUATE,
            json!({
                "schema":1,
                "profile_id":color_profile.id,
                "element_id":color_element,
                "values":[8,8,1,1,1,4,4],
                "expected_events":[
                    {"event":"critical_health","state":"entered","timestamp_ms":300,"tolerance_ms":0},
                    {"event":"critical_health","state":"left","timestamp_ms":600,"tolerance_ms":0}
                ],
                "regression":{"minimum_precision":1.0,"minimum_recall":1.0,"maximum_mean_latency_ms":0.0}
            }),
        )
        .await;
        assert_eq!(evaluation["result"]["metrics"]["passed"], true);
        assert_eq!(evaluation["result"]["metrics"]["matched"], 2);
        assert_eq!(evaluation["result"]["events"].as_array().unwrap().len(), 2);

        let mut template_profile = Profile::new("Template", "synthetic_game", 3, 3);
        let template_element = ElementId::new();
        template_profile
            .elements
            .push(yash_app_events_profile::Element {
                id: template_element,
                name: "Icon".into(),
                enabled: true,
                color: "#0f0".into(),
                region: NormalizedRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                detector: ProfileDetector::Template {
                    id: DetectorId::new(),
                    templates: vec![PathBuf::from("templates/icon.json")],
                    masks: Vec::new(),
                    threshold: 0.99,
                    preprocessing: Vec::new(),
                },
            });
        template_profile
            .rules
            .push(yash_app_events_profile::EventRule {
                id: RuleId::new(),
                element_id: template_element,
                event: "icon_missing".into(),
                enter_below: 0.5,
                leave_above: 0.5,
                minimum_confidence: 0.0,
                required_samples: 2,
                sample_window: 2,
                cooldown_ms: 0,
                predicate: RulePredicate::Boolean { expected: false },
                stable_for_ms: 0,
                emit_initial: false,
                update_interval_ms: None,
            });
        call(
            &mut client,
            4,
            method::PROFILE_CREATE,
            json!({"profile":template_profile}),
        )
        .await;
        let template_directory = directory
            .path()
            .join("data/profiles")
            .join(template_profile.id.to_string())
            .join("templates");
        fs::create_dir_all(&template_directory).unwrap();
        fs::write(
            template_directory.join("icon.json"),
            serde_json::to_vec(
                &GrayImage::new(3, 3, vec![0, 255, 0, 255, 255, 255, 0, 255, 0]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let template = call(
            &mut client,
            5,
            method::DETECTOR_TEST,
            json!({"profile_id":template_profile.id,"element_id":template_element}),
        )
        .await;
        assert_eq!(template["result"]["observations"][0]["value"], 1.0);

        let mut change_profile = Profile::new("Change", "synthetic_game", 2, 2);
        let change_element = ElementId::new();
        change_profile
            .elements
            .push(yash_app_events_profile::Element {
                id: change_element,
                name: "Loading".into(),
                enabled: true,
                color: "#00f".into(),
                region: NormalizedRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                detector: ProfileDetector::RegionChange {
                    id: DetectorId::new(),
                    threshold: 0.1,
                    preprocessing: Vec::new(),
                },
            });
        change_profile
            .rules
            .push(yash_app_events_profile::EventRule {
                id: RuleId::new(),
                element_id: change_element,
                event: "region_stable".into(),
                enter_below: 0.2,
                leave_above: 0.3,
                minimum_confidence: 0.0,
                required_samples: 1,
                sample_window: 1,
                cooldown_ms: 0,
                predicate: yash_app_events_profile::RulePredicate::default(),
                stable_for_ms: 0,
                emit_initial: false,
                update_interval_ms: None,
            });
        call(
            &mut client,
            6,
            method::PROFILE_CREATE,
            json!({"profile":change_profile}),
        )
        .await;
        let change = call(
            &mut client,
            7,
            method::DETECTOR_TEST,
            json!({"profile_id":change_profile.id,"element_id":change_element,"change_value":255}),
        )
        .await;
        assert_eq!(change["result"]["observations"][0]["status"], "unknown");
        assert_eq!(change["result"]["observations"][1]["value"], 1.0);
        assert_eq!(change["result"]["diagnostic"]["persistent_capture"], false);
        assert_eq!(
            change["result"]["diagnostic"]["processed_preview"]["mime"],
            "image/png"
        );
        assert_eq!(
            change["result"]["diagnostic"]["processed_preview"]["bytes"][0],
            137
        );
        let mut subscriber = BufReader::new(UnixStream::connect(&socket).await.unwrap());
        handshake(&mut subscriber).await;
        call(&mut subscriber, 8, method::EVENTS_SUBSCRIBE, Value::Null).await;
        let template_replay = call(&mut client, 9, method::REPLAY_PROFILE_DETECTOR, json!({"profile_id":template_profile.id,"element_id":template_element,"values":[1,1,0,0,1,1]})).await;
        assert_eq!(
            template_replay["result"]["events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let region_replay = call(&mut client, 10, method::REPLAY_PROFILE_DETECTOR, json!({"profile_id":change_profile.id,"element_id":change_element,"values":[0,0,255,255]})).await;
        assert_eq!(
            region_replay["result"]["events"].as_array().unwrap().len(),
            2
        );
        let notifications = [
            notification(&mut subscriber).await,
            notification(&mut subscriber).await,
            notification(&mut subscriber).await,
            notification(&mut subscriber).await,
        ];
        assert_eq!(
            notifications
                .iter()
                .map(|value| value["params"]["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "icon_missing",
                "icon_missing",
                "region_stable",
                "region_stable"
            ]
        );
        assert_eq!(
            notifications
                .iter()
                .map(|value| value["params"]["state"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["entered", "left", "left", "entered"]
        );
        let durable: Vec<Value> = fs::read_to_string(directory.path().join("state/events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            &durable[durable.len() - notifications.len()..],
            notifications
                .iter()
                .map(|value| value["params"].clone())
                .collect::<Vec<_>>()
        );
        let snapshot = call(&mut client, 11, method::STATE_GET, Value::Null).await;
        assert_eq!(snapshot["result"]["events"]["region_stable"], true);
        assert_eq!(snapshot["result"]["sequence"], 6);
        call(&mut client, 12, method::SHUTDOWN, Value::Null).await;
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
            state_root: directory.path().join("state"),
            cache_root: directory.path().join("cache"),
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

    #[test]
    fn explicit_snapshot_encoder_removes_row_padding_and_produces_png() {
        let frame = Frame::new(
            0,
            Duration::ZERO,
            FrameLayout {
                width: 2,
                height: 1,
                row_stride: 8,
                format: PixelFormat::Rgb8,
            },
            Some("test".into()),
            Arc::from([255, 0, 0, 0, 255, 0, 99, 99]),
        )
        .unwrap();
        let encoded = encode_frame_png(&frame).unwrap();
        assert_eq!(&encoded[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn diagnostic_preview_downscales_without_changing_aspect_ratio() {
        let image = GrayImage::new(1_440, 45, vec![127; 1_440 * 45]).unwrap();
        let preview = bounded_gray_preview(&image, 512).unwrap();
        assert_eq!((preview.width, preview.height), (480, 15));
        assert_eq!(preview.pixels.len(), 480 * 15);
    }

    #[test]
    fn configured_color_replay_fixture_preserves_fill_value() {
        let frame = synthetic_color_bar_frame(
            0,
            8,
            BarDirection::LeftToRight,
            [175, 175, 175],
            [255, 255, 255],
        )
        .unwrap();
        let mut detector = ColorBarDetector::new(ColorBarConfig {
            direction: BarDirection::LeftToRight,
            minimum_rgb: [175, 175, 175],
            maximum_rgb: [255, 255, 255],
            line_match_fraction: 0.8,
            maximum_gap_fraction: 0.02,
            mask: None,
        })
        .unwrap();
        let detection = detector.detect(&frame, full_frame_region());
        assert_eq!(
            detection.value,
            Some(yash_app_events_vision::DetectionValue::Number(0.8))
        );
        assert_eq!(
            detection.status,
            yash_app_events_vision::DetectionStatus::Valid
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn ocr_image_replay_reaches_text_rule_through_common_coordinator() {
        let directory = tempfile::tempdir().unwrap();
        let (notifications, _) = broadcast::channel(8);
        let local_config = LocalConfig::new(directory.path().join("config"));
        let output_routes = routes::Router::spawn(local_config.clone(), notifications.clone());
        let state = State {
            instance: Uuid::new_v4(),
            profiles: ProfileStore::new(directory.path().join("data"), 20),
            local_config,
            catalog: CatalogService::new(directory.path().join("cache")).unwrap(),
            connected: AtomicUsize::new(0),
            maximum_connections: 8,
            shutdown: Notify::new(),
            notifications,
            sequence: AtomicU64::new(0),
            output: Mutex::new(OutputWriter::new(
                directory.path().join("state"),
                OutputConfig::default(),
            )),
            latest_snapshot: Mutex::new(json!({})),
            output_error: Mutex::new(None),
            latest_frame: LatestFrameSlot::default(),
            capture: Mutex::new(None),
            analysis: Mutex::new(None),
            analysis_metrics: Mutex::new(AnalysisMetrics::default()),
            preview_clients: AtomicUsize::new(0),
            diagnostic_logs: Mutex::new(VecDeque::new()),
            collector: Mutex::new(collection::Runtime::default()),
            output_routes,
        };
        state
            .local_config
            .save_settings(&yash_app_events_profile::Settings::default())
            .unwrap();
        let mut profile = Profile::new("OCR", "synthetic_game", 640, 120);
        let element_id = ElementId::new();
        profile.elements.push(yash_app_events_profile::Element {
            id: element_id,
            name: "Result text".into(),
            enabled: true,
            color: "#fff".into(),
            region: full_frame_region(),
            detector: ProfileDetector::Ocr {
                id: DetectorId::new(),
                language: "eng".into(),
                page_segmentation_mode: 7,
                character_whitelist: Some("ABCDEFGHIJKLMNOPQRSTUVWXYZ ".into()),
                change_trigger_threshold: 0.01,
                maximum_interval_ms: 1_000,
                preprocessing: Vec::new(),
                empty_value: None,
                zero_pad_to: None,
            },
        });
        profile.rules.push(yash_app_events_profile::EventRule {
            id: RuleId::new(),
            element_id,
            event: "victory".into(),
            enter_below: 0.0,
            leave_above: 0.0,
            minimum_confidence: 0.5,
            required_samples: 1,
            sample_window: 1,
            cooldown_ms: 0,
            predicate: RulePredicate::TextEquals {
                expected: "VICTORY".into(),
            },
            stable_for_ms: 0,
            emit_initial: false,
            update_interval_ms: None,
        });
        state.profiles.create(&profile).unwrap();
        let profile_directory = state.profiles.profile_directory(profile.id);
        fs::copy(
            "../vision/tests/fixtures/ocr/localized.png",
            profile_directory.join("localized.png"),
        )
        .unwrap();
        fs::copy(
            "../vision/tests/fixtures/ocr/victory.png",
            profile_directory.join("victory.png"),
        )
        .unwrap();
        let diagnostic_frame =
            replay_png_frames(&profile_directory, &[PathBuf::from("localized.png")])
                .unwrap()
                .remove(0);
        let diagnostic = build_diagnostic_bundle(
            &state,
            Some(&diagnostic_frame),
            &DiagnosticParams {
                profile_id: Some(profile.id),
                selected_element_ids: vec![element_id],
            },
        )
        .unwrap();
        let diagnostic_plan =
            plan_diagnostic_bundle(&diagnostic, DiagnosticLimits::default()).unwrap();
        assert_eq!(diagnostic_plan.entries.len(), 4);
        assert!(diagnostic_plan.entries[3].user_selected_image);
        let result = evaluate_profile_replay(
            &state,
            &ReplayManifest {
                schema: 1,
                profile_id: profile.id,
                element_id,
                values: Vec::new(),
                image_frames: vec!["localized.png".into(), "victory.png".into()],
                expected_events: vec![yash_app_events_engine::ExpectedEvent {
                    event: "victory".into(),
                    state: TransitionState::Entered,
                    timestamp_ms: 100,
                    tolerance_ms: 0,
                }],
                regression: yash_app_events_engine::ReplayRegression::default(),
            },
        )
        .unwrap();
        assert_eq!(result["metrics"]["passed"], true);
        assert_eq!(result["events"][0]["event"], "victory");
        assert_eq!(result["observations"][1]["value"]["type"], "text");
        assert_eq!(result["observations"][1]["value"]["value"], "VICTORY");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn classifier_image_replay_validates_model_and_emits_label_event() {
        let directory = tempfile::tempdir().unwrap();
        let (notifications, _) = broadcast::channel(8);
        let local_config = LocalConfig::new(directory.path().join("config"));
        let output_routes = routes::Router::spawn(local_config.clone(), notifications.clone());
        let state = State {
            instance: Uuid::new_v4(),
            profiles: ProfileStore::new(directory.path().join("data"), 20),
            local_config,
            catalog: CatalogService::new(directory.path().join("cache")).unwrap(),
            connected: AtomicUsize::new(0),
            maximum_connections: 8,
            shutdown: Notify::new(),
            notifications,
            sequence: AtomicU64::new(0),
            output: Mutex::new(OutputWriter::new(
                directory.path().join("state"),
                OutputConfig::default(),
            )),
            latest_snapshot: Mutex::new(json!({})),
            output_error: Mutex::new(None),
            latest_frame: LatestFrameSlot::default(),
            capture: Mutex::new(None),
            analysis: Mutex::new(None),
            analysis_metrics: Mutex::new(AnalysisMetrics::default()),
            preview_clients: AtomicUsize::new(0),
            diagnostic_logs: Mutex::new(VecDeque::new()),
            collector: Mutex::new(collection::Runtime::default()),
            output_routes,
        };
        state
            .local_config
            .save_settings(&yash_app_events_profile::Settings::default())
            .unwrap();
        let mut profile = Profile::new("Classifier", "synthetic_game", 8, 8);
        let element_id = ElementId::new();
        profile.elements.push(yash_app_events_profile::Element {
            id: element_id,
            name: "HUD icon class".into(),
            enabled: true,
            color: "#fff".into(),
            region: full_frame_region(),
            detector: ProfileDetector::Classifier {
                id: DetectorId::new(),
                model: "hud_icon.onnx".into(),
                model_sha256: "12ac2f734bbe111526ef82db676086b75a30635a28c2ab8a032a1b1f10759fc6"
                    .into(),
                labels: vec!["orb".into(), "cross".into()],
                input_width: 8,
                input_height: 8,
                preprocessing: vec![yash_app_events_profile::PreprocessOperation::Resize {
                    width: 8,
                    height: 8,
                }],
                change_trigger_threshold: 0.01,
                maximum_interval_ms: 1_000,
            },
        });
        profile.rules.push(yash_app_events_profile::EventRule {
            id: RuleId::new(),
            element_id,
            event: "cross_icon".into(),
            enter_below: 0.0,
            leave_above: 0.0,
            minimum_confidence: 0.5,
            required_samples: 1,
            sample_window: 1,
            cooldown_ms: 0,
            predicate: RulePredicate::TextEquals {
                expected: "cross".into(),
            },
            stable_for_ms: 0,
            emit_initial: false,
            update_interval_ms: None,
        });
        state.profiles.create(&profile).unwrap();
        let profile_directory = state.profiles.profile_directory(profile.id);
        for name in ["hud_icon.onnx", "orb_0.png", "cross_0.png"] {
            fs::copy(
                Path::new("../vision/tests/fixtures/classifier").join(name),
                profile_directory.join(name),
            )
            .unwrap();
        }
        let result = evaluate_profile_replay(
            &state,
            &ReplayManifest {
                schema: 1,
                profile_id: profile.id,
                element_id,
                values: Vec::new(),
                image_frames: vec!["orb_0.png".into(), "cross_0.png".into()],
                expected_events: vec![yash_app_events_engine::ExpectedEvent {
                    event: "cross_icon".into(),
                    state: TransitionState::Entered,
                    timestamp_ms: 100,
                    tolerance_ms: 0,
                }],
                regression: yash_app_events_engine::ReplayRegression::default(),
            },
        )
        .unwrap();
        assert_eq!(result["metrics"]["passed"], true);
        assert_eq!(result["events"][0]["event"], "cross_icon");
        assert_eq!(result["observations"][1]["value"]["value"], "cross");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn live_latest_frame_worker_throttles_sixty_fps_and_publishes_transition() {
        let directory = tempfile::tempdir().unwrap();
        let (notifications, _) = broadcast::channel(64);
        let local_config = LocalConfig::new(directory.path().join("config"));
        let output_routes = routes::Router::spawn(local_config.clone(), notifications.clone());
        let state = Arc::new(State {
            instance: Uuid::new_v4(),
            profiles: ProfileStore::new(directory.path().join("data"), 20),
            local_config,
            catalog: CatalogService::new(directory.path().join("cache")).unwrap(),
            connected: AtomicUsize::new(0),
            maximum_connections: 8,
            shutdown: Notify::new(),
            notifications,
            sequence: AtomicU64::new(0),
            output: Mutex::new(OutputWriter::new(
                directory.path().join("state"),
                OutputConfig::default(),
            )),
            latest_snapshot: Mutex::new(json!({})),
            output_error: Mutex::new(None),
            latest_frame: LatestFrameSlot::default(),
            capture: Mutex::new(None),
            analysis: Mutex::new(None),
            analysis_metrics: Mutex::new(AnalysisMetrics::default()),
            preview_clients: AtomicUsize::new(0),
            diagnostic_logs: Mutex::new(VecDeque::new()),
            collector: Mutex::new(collection::Runtime::default()),
            output_routes,
        });
        state
            .local_config
            .save_settings(&yash_app_events_profile::Settings::default())
            .unwrap();
        let mut profile = Profile::new("Live", "synthetic_game", 10, 2);
        let element_id = ElementId::new();
        profile.elements.push(yash_app_events_profile::Element {
            id: element_id,
            name: "Health".into(),
            enabled: true,
            color: "#f00".into(),
            region: NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            detector: ProfileDetector::ColorBar {
                id: DetectorId::new(),
                direction: BarDirection::LeftToRight,
                minimum_rgb: [180, 0, 0],
                maximum_rgb: [255, 60, 60],
                mask: None,
            },
        });
        let second_element_id = ElementId::new();
        profile.elements.push(yash_app_events_profile::Element {
            id: second_element_id,
            name: "Shield".into(),
            enabled: true,
            color: "#00f".into(),
            region: NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            detector: ProfileDetector::ColorBar {
                id: DetectorId::new(),
                direction: BarDirection::LeftToRight,
                minimum_rgb: [180, 0, 0],
                maximum_rgb: [255, 60, 60],
                mask: None,
            },
        });
        profile.rules.push(yash_app_events_profile::EventRule {
            id: RuleId::new(),
            element_id,
            event: "critical_health".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.0,
            required_samples: 1,
            sample_window: 1,
            cooldown_ms: 0,
            predicate: yash_app_events_profile::RulePredicate::default(),
            stable_for_ms: 0,
            emit_initial: false,
            update_interval_ms: None,
        });
        profile.rules.push(yash_app_events_profile::EventRule {
            id: RuleId::new(),
            element_id,
            event: "both_critical".into(),
            enter_below: 0.2,
            leave_above: 0.3,
            minimum_confidence: 0.0,
            required_samples: 1,
            sample_window: 1,
            cooldown_ms: 0,
            predicate: RulePredicate::All {
                conditions: vec![
                    yash_app_events_profile::ObservationCondition {
                        element_id,
                        predicate: yash_app_events_profile::AtomicRulePredicate::NumericBelow {
                            threshold_micros: 200_000,
                        },
                    },
                    yash_app_events_profile::ObservationCondition {
                        element_id: second_element_id,
                        predicate: yash_app_events_profile::AtomicRulePredicate::NumericBelow {
                            threshold_micros: 200_000,
                        },
                    },
                ],
            },
            stable_for_ms: 0,
            emit_initial: false,
            update_interval_ms: None,
        });
        state.profiles.create(&profile).unwrap();
        let replay = evaluate_profile_replay(
            &state,
            &ReplayManifest {
                schema: 1,
                profile_id: profile.id,
                element_id,
                values: vec![8, 1],
                image_frames: Vec::new(),
                expected_events: vec![
                    yash_app_events_engine::ExpectedEvent {
                        event: "critical_health".into(),
                        state: TransitionState::Entered,
                        timestamp_ms: 100,
                        tolerance_ms: 0,
                    },
                    yash_app_events_engine::ExpectedEvent {
                        event: "both_critical".into(),
                        state: TransitionState::Entered,
                        timestamp_ms: 100,
                        tolerance_ms: 0,
                    },
                ],
                regression: yash_app_events_engine::ReplayRegression::default(),
            },
        )
        .unwrap();
        assert_eq!(replay["events"].as_array().unwrap().len(), 2);
        assert_eq!(replay["metrics"]["passed"], true);
        let pipeline = build_live_pipeline(&state, profile.id).unwrap();
        let mut receiver = state.notifications.subscribe();
        start_live_analysis(&state, pipeline);
        for sequence in 0..60_u64 {
            let source =
                synthetic_health_frame(sequence, if sequence < 30 { 8 } else { 1 }).unwrap();
            let frame = Frame::new(
                sequence,
                Duration::from_nanos(sequence * 1_000_000_000 / 60),
                FrameLayout {
                    width: source.width,
                    height: source.height,
                    row_stride: source.row_stride,
                    format: source.format,
                },
                source.source_id.clone(),
                Arc::clone(&source.data),
            )
            .unwrap();
            state.latest_frame.publish(Arc::new(frame));
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let mut events = Vec::new();
        for _ in 0..2 {
            events.push(
                tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(events[0]["event"], "critical_health");
        assert_eq!(events[1]["event"], "both_critical");
        assert!(events.iter().all(|event| event["state"] == "entered"));
        let analysis = state.analysis.lock().unwrap().take().unwrap();
        let _ = analysis.stop.send(());
        analysis.task.await.unwrap();
        let analyzed = state.analysis_metrics.lock().unwrap().frames;
        assert!((4..=20).contains(&analyzed), "analyzed {analyzed} frames");
        assert_eq!(state.latest_frame.replacements(), 59);
        assert_eq!(
            fs::read_to_string(directory.path().join("state/events.jsonl"))
                .unwrap()
                .lines()
                .count(),
            4
        );
        assert_eq!(state.latest_snapshot.lock().unwrap()["sequence"], 4);
        {
            let mut lease = PreviewLease::new(Arc::clone(&state));
            lease.start();
            let preview = preview_frame(
                &state,
                &PreviewFrameParams {
                    maximum_width: 320,
                    maximum_height: 180,
                },
            )
            .unwrap();
            assert!(preview["width"].as_u64().unwrap() <= 320);
            assert_eq!(preview["bytes"][0], 137);
            assert_eq!(state.preview_clients.load(Ordering::Relaxed), 1);
        }
        assert_eq!(state.preview_clients.load(Ordering::Relaxed), 0);
        let captured = capture_template(
            &state,
            &CaptureTemplateParams {
                profile_id: profile.id,
                element_id,
                expected_revision: 0,
                name: "health_crop".into(),
            },
        )
        .unwrap();
        assert_eq!(captured["explicit"], true);
        let asset = directory
            .path()
            .join("data/profiles")
            .join(profile.id.to_string())
            .join("templates/health_crop.json");
        let image: GrayImage = serde_json::from_slice(&fs::read(asset).unwrap()).unwrap();
        assert_eq!((image.width, image.height), (10, 2));
    }

    #[test]
    fn external_suite_assertions_are_typed_and_tolerant_only_when_requested() {
        let observation = Observation {
            detector_id: DetectorId::new(),
            element_id: ElementId::new(),
            timestamp_ms: 0,
            value: ObservationValue::Number(0.7317),
            confidence: Some(0.8),
            status: yash_app_events_engine::ObservationStatus::Valid,
            diagnostic: "fixture".into(),
        };
        let expected = ExpectedObservation {
            status: yash_app_events_engine::ObservationStatus::Valid,
            value: Some(ExpectedValue::Number(0.732)),
            numeric_tolerance: Some(0.001),
            minimum_confidence: Some(0.7),
        };
        assert!(observation_matches(&expected, Some(&observation)).0);
        let exact = ExpectedObservation {
            numeric_tolerance: None,
            ..expected
        };
        assert!(!observation_matches(&exact, Some(&observation)).0);
    }

    #[test]
    fn external_suite_paths_cannot_escape_the_package() {
        let package = tempfile::tempdir().unwrap();
        fs::write(package.path().join("inside.png"), b"fixture").unwrap();
        assert!(safe_suite_path(package.path(), Path::new("inside.png")).is_ok());
        assert!(safe_suite_path(package.path(), Path::new("../outside.png")).is_err());
        assert!(safe_suite_path(package.path(), Path::new("/tmp/outside.png")).is_err());
    }
}
