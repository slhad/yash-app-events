//! Responsive egui protocol client and visual normalized-region editor.

use std::collections::VecDeque;
use std::fmt;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;
use yash_app_events_profile::{
    AtomicRulePredicate, BarDirection, DerivedInput, DerivedObservation, Detector, DetectorId,
    Element, ElementId, EventRule, NormalizedRegion, ObservationCondition, PreprocessOperation,
    Profile, ProfileId, ProfileStore, RuleId, RulePredicate,
};
use yash_app_events_protocol::{method, ClientError, UnixRpcClient};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestKind {
    Profiles,
    Status,
    State,
    Create,
    Commit,
    Draft,
    Mutation,
    Capture,
    CaptureSelect,
    PreviewStart,
    PreviewStop,
    PreviewFrame,
    DetectorTest,
    TemplateCapture,
    Replay,
    DiagnosticPlan,
    DiagnosticExport,
    RevisionHistory,
    Rollback,
    AutoCapture,
    CollectionPolicy,
    CollectionStatus,
    CollectionItems,
    CollectionItem,
    CollectionMutation,
    OutputRoutes,
    OutputMutation,
    OutputTest,
    OutputRecipeList,
    OutputRecipePreview,
    OutputRecipeInstall,
}

#[derive(Debug)]
struct Request {
    kind: RequestKind,
    method: &'static str,
    params: Value,
}

#[derive(Debug)]
enum Payload {
    Json(Value),
    Preview(PreviewImage),
    DetectorInspection {
        value: Value,
        original: PreviewImage,
        processed: PreviewImage,
    },
    CollectionInspection {
        value: Value,
        image: PreviewImage,
    },
    Error(String),
}

#[derive(Debug)]
struct Response {
    kind: RequestKind,
    payload: Payload,
}

#[derive(Clone, Debug)]
struct PreviewImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[derive(Debug)]
struct Worker {
    requests: tokio_mpsc::UnboundedSender<Request>,
    responses: mpsc::Receiver<Response>,
}

impl Worker {
    #[allow(clippy::too_many_lines)]
    fn spawn(socket: PathBuf) -> Self {
        let (requests, mut request_receiver) = tokio_mpsc::unbounded_channel::<Request>();
        let (response_sender, responses) = mpsc::channel();
        std::thread::Builder::new()
            .name("yash-gui-rpc".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = response_sender.send(Response {
                        kind: RequestKind::Status,
                        payload: Payload::Error("failed to create RPC runtime".into()),
                    });
                    return;
                };
                runtime.block_on(async move {
                    let mut client = None;
                    let mut preview_requested = false;
                    let mut freeze_requested = false;
                    while let Some(request) = request_receiver.recv().await {
                        match request.method {
                            method::PREVIEW_START => preview_requested = true,
                            method::PREVIEW_STOP => {
                                preview_requested = false;
                                freeze_requested = false;
                            }
                            method::PREVIEW_FREEZE => freeze_requested = true,
                            method::PREVIEW_UNFREEZE => freeze_requested = false,
                            _ => {}
                        }
                        let mut reconnected = false;
                        if client.is_none() {
                            match UnixRpcClient::connect(
                                &socket,
                                Duration::from_secs(3),
                                "yash-app-events-gui",
                                env!("CARGO_PKG_VERSION"),
                            )
                            .await
                            {
                                Ok(connected) => {
                                    client = Some(connected);
                                    reconnected = true;
                                }
                                Err(error) => {
                                    let _ = response_sender.send(Response {
                                        kind: request.kind,
                                        payload: Payload::Error(error.to_string()),
                                    });
                                    continue;
                                }
                            }
                        }
                        if reconnected
                            && preview_requested
                            && request.method != method::PREVIEW_START
                        {
                            let restore = async {
                                let client = client.as_mut().expect("client was initialized");
                                client.call(method::PREVIEW_START, Value::Null).await?;
                                if freeze_requested {
                                    client.call(method::PREVIEW_FREEZE, Value::Null).await?;
                                }
                                Ok::<(), ClientError>(())
                            }
                            .await;
                            if let Err(error) = restore {
                                client = None;
                                let _ = response_sender.send(Response {
                                    kind: request.kind,
                                    payload: Payload::Error(format!(
                                        "restore preview session: {error}"
                                    )),
                                });
                                continue;
                            }
                        }
                        if reconnected && request.method != method::PROFILE_LIST {
                            let refresh = client
                                .as_mut()
                                .expect("client was initialized")
                                .call(method::PROFILE_LIST, Value::Null)
                                .await;
                            match refresh {
                                Ok(value) => {
                                    let _ = response_sender.send(Response {
                                        kind: RequestKind::Profiles,
                                        payload: Payload::Json(value),
                                    });
                                }
                                Err(error) => {
                                    client = None;
                                    let _ = response_sender.send(Response {
                                        kind: RequestKind::Profiles,
                                        payload: Payload::Error(format!(
                                            "reload profiles after reconnect: {error}"
                                        )),
                                    });
                                    continue;
                                }
                            }
                        }
                        let params = if request.kind == RequestKind::Replay {
                            request.params["path"].as_str().map_or_else(
                                || Err("replay path is required".into()),
                                |path| {
                                    std::fs::read(path)
                                        .map_err(|error| format!("read replay: {error}"))
                                        .and_then(|bytes| {
                                            serde_json::from_slice(&bytes)
                                                .map_err(|error| format!("parse replay: {error}"))
                                        })
                                },
                            )
                        } else {
                            Ok(request.params)
                        };
                        let result = match params {
                            Ok(params) => {
                                let client = client.as_mut().expect("client was initialized");
                                if request.method == method::CAPTURE_SELECT
                                    || request.kind == RequestKind::Replay
                                {
                                    client
                                        .call_with_timeout(
                                            request.method,
                                            params,
                                            Duration::from_secs(300),
                                        )
                                        .await
                                } else {
                                    client.call(request.method, params).await
                                }
                            }
                            Err(error) => {
                                Err(ClientError::Rpc(yash_app_events_protocol::RpcError::new(
                                    yash_app_events_protocol::error_code::INVALID_PARAMS,
                                    error,
                                )))
                            }
                        };
                        let payload = match result {
                            Ok(value) if request.kind == RequestKind::PreviewFrame => {
                                match decode_preview(&value) {
                                    Ok(image) => Payload::Preview(image),
                                    Err(error) => Payload::Error(error),
                                }
                            }
                            Ok(value) if request.kind == RequestKind::DetectorTest => {
                                match (
                                    decode_preview(&value["diagnostic"]["original_preview"]),
                                    decode_preview(&value["diagnostic"]["processed_preview"]),
                                ) {
                                    (Ok(original), Ok(processed)) => Payload::DetectorInspection {
                                        value,
                                        original,
                                        processed,
                                    },
                                    (Err(error), _) | (_, Err(error)) => Payload::Error(error),
                                }
                            }
                            Ok(value) if request.kind == RequestKind::CollectionItem => {
                                match decode_collection_image(&value) {
                                    Ok(image) => Payload::CollectionInspection { value, image },
                                    Err(error) => Payload::Error(error),
                                }
                            }
                            Ok(value) => Payload::Json(value),
                            Err(error) => {
                                if !matches!(error, ClientError::Rpc(_)) {
                                    client = None;
                                }
                                Payload::Error(error.to_string())
                            }
                        };
                        let _ = response_sender.send(Response {
                            kind: request.kind,
                            payload,
                        });
                    }
                });
            })
            .expect("spawn GUI RPC worker");
        Self {
            requests,
            responses,
        }
    }

    fn send(&self, kind: RequestKind, method: &'static str, params: Value) {
        let _ = self.requests.send(Request {
            kind,
            method,
            params,
        });
    }
}

/// GUI state. All capture, decoding, profile I/O, and RPC work remains off the render thread.
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    worker: Worker,
    profiles: Vec<Profile>,
    selected: Option<usize>,
    draft: Option<Profile>,
    dirty_since: Option<Instant>,
    draft_saved: bool,
    status: Value,
    state: Value,
    error: Option<String>,
    new_name: String,
    new_game: String,
    import_path: String,
    export_path: String,
    restore_id: String,
    capture_status: Value,
    capture_select_pending: bool,
    preview_enabled: bool,
    frozen: bool,
    preview_pending: bool,
    preview_texture: Option<egui::TextureHandle>,
    last_preview: Instant,
    last_refresh: Instant,
    zoom: f32,
    pan: egui::Vec2,
    selected_region: Option<usize>,
    selected_derived: Option<usize>,
    drawing: bool,
    draw_start: Option<egui::Pos2>,
    drag_origin: Option<NormalizedRegion>,
    resizing: bool,
    continuous_test: bool,
    test_pending: bool,
    last_test: Instant,
    detector_result: Value,
    original_texture: Option<egui::TextureHandle>,
    processed_texture: Option<egui::TextureHandle>,
    timeline: VecDeque<String>,
    last_state_sequence: u64,
    replay_path: String,
    replay_result: Value,
    replay_frame: usize,
    replay_playing: bool,
    replay_last_step: Instant,
    diagnostic_path: String,
    diagnostic_plan: Value,
    diagnostic_privacy_reviewed: bool,
    revisions: Vec<Profile>,
    selected_revision: Option<usize>,
    rollback_confirm: bool,
    process_sampler: ProcessSampler,
    gui_usage: ProcessUsage,
    auto_capture_enabled: bool,
    auto_process_match: String,
    auto_process_running: bool,
    collection_enabled: bool,
    collection_dataset_root: String,
    collection_interval_seconds: u64,
    collection_jitter_seconds: u64,
    collection_similarity_threshold: f32,
    collection_maximum_pending: usize,
    collection_maximum_bytes: u64,
    collection_novelty_targets: String,
    collection_status: Value,
    collection_items: Vec<Value>,
    collection_selected: Value,
    collection_expected_json: String,
    collection_texture: Option<egui::TextureHandle>,
    output_routes: Vec<Value>,
    output_test: Value,
    output_recipes: Vec<Value>,
    selected_output_recipe: Option<usize>,
    output_recipe_editor: String,
    output_recipe_destination: String,
    output_recipe_arguments: String,
    output_recipe_timeout_ms: u64,
    output_recipe_command: bool,
    output_recipe_replace: bool,
    output_recipe_preview: Value,
}

impl fmt::Debug for App {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("App")
            .field("profiles", &self.profiles.len())
            .field("selected", &self.selected)
            .field("preview_enabled", &self.preview_enabled)
            .field("frozen", &self.frozen)
            .finish_non_exhaustive()
    }
}

impl App {
    #[must_use]
    pub fn new(_creation: &eframe::CreationContext<'_>) -> Self {
        let socket =
            std::env::var_os("YASH_APP_EVENTS_SOCKET").map_or_else(default_socket, PathBuf::from);
        let worker = Worker::spawn(socket);
        worker.send(RequestKind::Profiles, method::PROFILE_LIST, Value::Null);
        worker.send(RequestKind::Status, method::STATUS, Value::Null);
        Self {
            worker,
            profiles: Vec::new(),
            selected: None,
            draft: None,
            dirty_since: None,
            draft_saved: false,
            status: json!({}),
            state: json!({}),
            error: None,
            new_name: "New profile".into(),
            new_game: "game_id".into(),
            import_path: String::new(),
            export_path: String::new(),
            restore_id: String::new(),
            capture_status: json!({}),
            capture_select_pending: false,
            preview_enabled: false,
            frozen: false,
            preview_pending: false,
            preview_texture: None,
            last_preview: Instant::now(),
            last_refresh: Instant::now(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            selected_region: None,
            selected_derived: None,
            drawing: false,
            draw_start: None,
            drag_origin: None,
            resizing: false,
            continuous_test: false,
            test_pending: false,
            last_test: Instant::now(),
            detector_result: json!({}),
            original_texture: None,
            processed_texture: None,
            timeline: VecDeque::with_capacity(100),
            last_state_sequence: 0,
            replay_path: String::new(),
            replay_result: json!({}),
            replay_frame: 0,
            replay_playing: false,
            replay_last_step: Instant::now(),
            diagnostic_path: String::new(),
            diagnostic_plan: json!({}),
            diagnostic_privacy_reviewed: false,
            revisions: Vec::new(),
            selected_revision: None,
            rollback_confirm: false,
            process_sampler: ProcessSampler::default(),
            gui_usage: ProcessUsage::default(),
            auto_capture_enabled: false,
            auto_process_match: String::new(),
            auto_process_running: false,
            collection_enabled: false,
            collection_dataset_root: String::new(),
            collection_interval_seconds: 70,
            collection_jitter_seconds: 10,
            collection_similarity_threshold: 0.015,
            collection_maximum_pending: 1_000,
            collection_maximum_bytes: 2_147_483_648,
            collection_novelty_targets: String::new(),
            collection_status: json!({}),
            collection_items: Vec::new(),
            collection_selected: json!({}),
            collection_expected_json: "{}".into(),
            collection_texture: None,
            output_routes: Vec::new(),
            output_test: json!({}),
            output_recipes: Vec::new(),
            selected_output_recipe: None,
            output_recipe_editor: String::new(),
            output_recipe_destination: String::new(),
            output_recipe_arguments: "[]".into(),
            output_recipe_timeout_ms: 5_000,
            output_recipe_command: false,
            output_recipe_replace: false,
            output_recipe_preview: json!({}),
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
    fn drain(&mut self, context: &egui::Context) {
        while let Ok(response) = self.worker.responses.try_recv() {
            match response.payload {
                Payload::Error(error) => {
                    self.error = Some(error);
                    self.preview_pending = false;
                    if response.kind == RequestKind::CaptureSelect {
                        self.capture_select_pending = false;
                    }
                }
                Payload::Preview(image) => {
                    self.error = None;
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width, image.height],
                        &image.rgba,
                    );
                    self.preview_texture = Some(context.load_texture(
                        "capture-preview",
                        color,
                        egui::TextureOptions::LINEAR,
                    ));
                    self.preview_pending = false;
                }
                Payload::DetectorInspection {
                    value,
                    original,
                    processed,
                } => {
                    self.error = None;
                    self.detector_result = value;
                    self.original_texture = Some(load_preview_texture(
                        context,
                        "detector-original",
                        &original,
                    ));
                    self.processed_texture = Some(load_preview_texture(
                        context,
                        "detector-processed",
                        &processed,
                    ));
                    self.test_pending = false;
                    if let Some(observation) = self.detector_result["observations"]
                        .as_array()
                        .and_then(|items| items.last())
                    {
                        self.push_timeline(format!(
                            "test @{} ms · {} · value={} confidence={} · {}",
                            observation["timestamp_ms"].as_u64().unwrap_or(0),
                            observation["status"].as_str().unwrap_or("?"),
                            observation["value"],
                            observation["confidence"],
                            observation["diagnostic"].as_str().unwrap_or("")
                        ));
                    }
                }
                Payload::CollectionInspection { value, image } => {
                    self.error = None;
                    self.collection_selected = value;
                    self.collection_texture =
                        Some(load_preview_texture(context, "collection-review", &image));
                    let reviewed =
                        &self.collection_selected["item"]["review"]["expected_observations"];
                    let expected = if reviewed.as_object().is_some_and(|map| !map.is_empty()) {
                        reviewed.clone()
                    } else {
                        self.collection_selected["suggested_expected_observations"].clone()
                    };
                    self.collection_expected_json =
                        serde_json::to_string_pretty(&expected).unwrap_or_else(|_| "{}".into());
                }
                Payload::Json(value) => {
                    self.error = None;
                    match response.kind {
                        RequestKind::Profiles => self.apply_profiles(value),
                        RequestKind::RevisionHistory => {
                            match serde_json::from_value::<Vec<Profile>>(value) {
                                Ok(revisions) => {
                                    self.revisions = revisions;
                                    self.selected_revision = self.revisions.len().checked_sub(2);
                                }
                                Err(error) => {
                                    self.error = Some(format!("invalid revision history: {error}"));
                                }
                            }
                        }
                        RequestKind::Status => self.status = value,
                        RequestKind::State => {
                            let sequence = value["sequence"].as_u64().unwrap_or(0);
                            if sequence > self.last_state_sequence {
                                self.push_timeline(format!(
                                    "transition {} · seq {sequence} · {}",
                                    value["updated_at"].as_str().unwrap_or("unknown time"),
                                    value["events"]
                                ));
                                self.last_state_sequence = sequence;
                            }
                            self.state = value;
                        }
                        RequestKind::Commit => {
                            self.error = None;
                            self.dirty_since = None;
                            self.draft_saved = false;
                            self.worker.send(
                                RequestKind::Profiles,
                                method::PROFILE_LIST,
                                Value::Null,
                            );
                        }
                        RequestKind::Draft => self.draft_saved = true,
                        RequestKind::Mutation | RequestKind::Create => {
                            self.worker.send(
                                RequestKind::Profiles,
                                method::PROFILE_LIST,
                                Value::Null,
                            );
                        }
                        RequestKind::Rollback => {
                            self.rollback_confirm = false;
                            self.dirty_since = None;
                            self.worker.send(
                                RequestKind::Profiles,
                                method::PROFILE_LIST,
                                Value::Null,
                            );
                            if let Some(profile) = &self.draft {
                                self.worker.send(
                                    RequestKind::RevisionHistory,
                                    method::PROFILE_REVISIONS,
                                    json!({"profile_id":profile.id}),
                                );
                            }
                        }
                        RequestKind::AutoCapture => {
                            self.auto_capture_enabled = value["enabled"].as_bool().unwrap_or(false);
                            self.auto_process_match =
                                value["process_match"].as_str().unwrap_or("").to_owned();
                            self.auto_process_running =
                                value["process_running"].as_bool().unwrap_or(false);
                        }
                        RequestKind::CollectionPolicy => {
                            let policy = &value["policy"];
                            self.collection_enabled = policy["enabled"].as_bool().unwrap_or(false);
                            self.collection_dataset_root =
                                policy["dataset_root"].as_str().unwrap_or("").to_owned();
                            self.collection_interval_seconds =
                                policy["interval_seconds"].as_u64().unwrap_or(70);
                            self.collection_jitter_seconds =
                                policy["jitter_seconds"].as_u64().unwrap_or(10);
                            self.collection_similarity_threshold =
                                policy["similarity_threshold"].as_f64().unwrap_or(0.015) as f32;
                            self.collection_maximum_pending = policy["maximum_pending_items"]
                                .as_u64()
                                .and_then(|value| usize::try_from(value).ok())
                                .unwrap_or(1_000);
                            self.collection_maximum_bytes =
                                policy["maximum_bytes"].as_u64().unwrap_or(2_147_483_648);
                            self.collection_novelty_targets = policy["novelty_targets"]
                                .as_array()
                                .map(|items| {
                                    items
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                                .unwrap_or_default();
                        }
                        RequestKind::CollectionStatus => self.collection_status = value,
                        RequestKind::CollectionItems => {
                            self.collection_items =
                                value["items"].as_array().cloned().unwrap_or_default();
                        }
                        RequestKind::CollectionMutation => {
                            self.refresh_collection();
                        }
                        RequestKind::OutputRoutes | RequestKind::OutputMutation => {
                            self.output_routes = value.as_array().cloned().unwrap_or_default();
                        }
                        RequestKind::OutputTest => self.output_test = value,
                        RequestKind::OutputRecipeList => {
                            self.output_recipes = value.as_array().cloned().unwrap_or_default();
                            if self
                                .selected_output_recipe
                                .is_none_or(|index| index >= self.output_recipes.len())
                            {
                                self.selected_output_recipe = None;
                                if !self.output_recipes.is_empty() {
                                    self.select_output_recipe(0);
                                }
                            }
                        }
                        RequestKind::OutputRecipePreview => {
                            self.output_recipe_preview = value;
                        }
                        RequestKind::OutputRecipeInstall => {
                            self.output_routes =
                                value["routes"].as_array().cloned().unwrap_or_default();
                            self.output_recipe_preview = json!({
                                "installed":value["installed"],
                                "note":"Installed disabled; test and enable it above after review."
                            });
                        }
                        RequestKind::Capture | RequestKind::CaptureSelect => {
                            self.capture_status = value;
                            if response.kind == RequestKind::CaptureSelect {
                                self.capture_select_pending = false;
                            }
                        }
                        RequestKind::PreviewStart => {
                            self.preview_enabled = value["enabled"].as_bool().unwrap_or(false);
                        }
                        RequestKind::PreviewStop => {
                            self.preview_enabled = false;
                            self.preview_pending = false;
                        }
                        RequestKind::PreviewFrame | RequestKind::CollectionItem => {}
                        RequestKind::DetectorTest => {
                            self.detector_result = value;
                            self.test_pending = false;
                        }
                        RequestKind::TemplateCapture => {
                            if let Some(path) = value["path"].as_str() {
                                self.add_template_path(path.into());
                            }
                        }
                        RequestKind::Replay => {
                            self.replay_result = value;
                            self.replay_frame = 0;
                        }
                        RequestKind::DiagnosticPlan | RequestKind::DiagnosticExport => {
                            self.diagnostic_plan = value;
                            self.diagnostic_privacy_reviewed = false;
                        }
                    }
                }
            }
        }
    }

    fn apply_profiles(&mut self, value: Value) {
        match serde_json::from_value::<Vec<Profile>>(value) {
            Ok(profiles) => {
                let selected_id = self.draft.as_ref().map(|profile| profile.id);
                self.profiles = profiles;
                self.selected = selected_id
                    .and_then(|id| self.profiles.iter().position(|profile| profile.id == id))
                    .or_else(|| (!self.profiles.is_empty()).then_some(0));
                if self.dirty_since.is_none() {
                    self.draft = self.selected.map(|index| self.profiles[index].clone());
                    if self.draft.as_mut().is_some_and(ensure_stage_derived) {
                        self.mark_dirty();
                    }
                }
                if let Some(profile) = &self.draft {
                    self.worker.send(
                        RequestKind::RevisionHistory,
                        method::PROFILE_REVISIONS,
                        json!({"profile_id":profile.id}),
                    );
                }
                self.refresh_collection();
                self.refresh_outputs();
            }
            Err(error) => self.error = Some(format!("invalid profile response: {error}")),
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty_since = Some(Instant::now());
        self.draft_saved = false;
    }

    fn push_timeline(&mut self, entry: String) {
        if self.timeline.len() == 100 {
            self.timeline.pop_front();
        }
        self.timeline.push_back(entry);
    }

    fn refresh_collection(&self) {
        let Some(profile) = &self.draft else {
            return;
        };
        for (kind, method_name) in [
            (RequestKind::CollectionPolicy, method::COLLECTION_POLICY_GET),
            (RequestKind::CollectionStatus, method::COLLECTION_STATUS),
            (RequestKind::CollectionItems, method::COLLECTION_ITEMS),
        ] {
            self.worker
                .send(kind, method_name, json!({"profile_id":profile.id}));
        }
    }

    fn refresh_outputs(&self) {
        let Some(profile) = &self.draft else {
            return;
        };
        self.worker.send(
            RequestKind::OutputRoutes,
            method::OUTPUT_LIST,
            json!({"profile_id":profile.id}),
        );
        self.worker.send(
            RequestKind::OutputRecipeList,
            method::OUTPUT_RECIPE_LIST,
            json!({"profile_id":profile.id}),
        );
    }

    fn schedule(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.gui_usage = sample_process_usage(&mut self.process_sampler);
            self.worker
                .send(RequestKind::Status, method::STATUS, Value::Null);
            self.worker
                .send(RequestKind::State, method::STATE_GET, Value::Null);
            self.worker
                .send(RequestKind::Capture, method::CAPTURE_STATUS, Value::Null);
            self.worker.send(
                RequestKind::AutoCapture,
                method::CAPTURE_AUTO_GET,
                Value::Null,
            );
            if let Some(profile) = &self.draft {
                self.worker.send(
                    RequestKind::CollectionStatus,
                    method::COLLECTION_STATUS,
                    json!({"profile_id":profile.id}),
                );
                self.worker.send(
                    RequestKind::CollectionItems,
                    method::COLLECTION_ITEMS,
                    json!({"profile_id":profile.id}),
                );
            }
            self.last_refresh = Instant::now();
        }
        if self.preview_enabled
            && !self.frozen
            && !self.preview_pending
            && self.last_preview.elapsed() >= Duration::from_millis(100)
        {
            self.worker.send(
                RequestKind::PreviewFrame,
                method::PREVIEW_FRAME,
                json!({"maximum_width":1600,"maximum_height":900}),
            );
            self.preview_pending = true;
            self.last_preview = Instant::now();
        }
        if !self.draft_saved
            && self
                .dirty_since
                .is_some_and(|changed| changed.elapsed() >= Duration::from_secs(1))
        {
            if let Some(profile) = &self.draft {
                self.worker.send(
                    RequestKind::Draft,
                    method::PROFILE_DRAFT,
                    json!({"profile":profile}),
                );
            }
            self.draft_saved = true;
        }
        if self.continuous_test
            && !self.test_pending
            && self.last_test.elapsed() >= Duration::from_millis(500)
        {
            self.request_detector_test();
        }
        if self.replay_playing && self.replay_last_step.elapsed() >= Duration::from_millis(100) {
            let maximum = self.replay_result["events"].as_array().map_or(0, Vec::len);
            self.replay_frame = self.replay_frame.saturating_add(1).min(maximum);
            self.replay_playing = self.replay_frame < maximum;
            self.replay_last_step = Instant::now();
        }
    }

    fn request_detector_test(&mut self) {
        if let (Some(profile), Some(index)) = (&self.draft, self.selected_region) {
            if let Some(element) = profile.elements.get(index) {
                self.worker.send(
                    RequestKind::DetectorTest,
                    method::DETECTOR_TEST,
                    json!({"profile_id":profile.id,"element_id":element.id,"use_frozen":self.frozen,"element":element}),
                );
                self.test_pending = true;
                self.last_test = Instant::now();
            }
        }
    }

    fn add_template_path(&mut self, path: PathBuf) {
        if let (Some(profile), Some(index)) = (&mut self.draft, self.selected_region) {
            if let Some(Element {
                detector: Detector::Template { templates, .. },
                ..
            }) = profile.elements.get_mut(index)
            {
                templates.push(path);
                self.mark_dirty();
            }
        }
    }

    fn sidebar(&mut self, context: &egui::Context) {
        egui::SidePanel::left("profiles").resizable(true).default_width(300.0).show(context,|ui| {
            ui.heading("Profiles");
            ui.label(if self.status["capture_active"].as_bool().unwrap_or(false) { "Capture active" } else { "Capture stopped" });
            if let Some(error) = &self.error { ui.colored_label(egui::Color32::RED,error); }
            ui.separator();
            ui.horizontal(|ui| { ui.text_edit_singleline(&mut self.new_name); });
            ui.horizontal(|ui| { ui.text_edit_singleline(&mut self.new_game); if ui.button("Create").clicked() { let profile = Profile::new(&self.new_name,&self.new_game,1920,1080); self.worker.send(RequestKind::Create,method::PROFILE_CREATE,json!({"profile":profile})); } });
            ui.separator();
            let mut select_index = None;
            for (index,profile) in self.profiles.iter().enumerate() { if ui.selectable_label(self.selected == Some(index),format!("{} · rev {}",profile.name,profile.revision)).clicked() { select_index=Some(index); } }
            if let Some(index) = select_index {
                self.selected=Some(index);
                self.draft=Some(self.profiles[index].clone());
                self.dirty_since=None;
                self.selected_region=None;
                self.revisions.clear();
                self.selected_revision=None;
                self.rollback_confirm=false;
                self.output_routes.clear();
                self.output_recipes.clear();
                self.selected_output_recipe=None;
                self.worker.send(RequestKind::RevisionHistory,method::PROFILE_REVISIONS,json!({"profile_id":self.profiles[index].id}));
                self.worker.send(RequestKind::OutputRoutes,method::OUTPUT_LIST,json!({"profile_id":self.profiles[index].id}));
                self.worker.send(RequestKind::OutputRecipeList,method::OUTPUT_RECIPE_LIST,json!({"profile_id":self.profiles[index].id}));
            }
            ui.separator();
            if let Some(profile) = self.draft.clone() {
                ui.horizontal(|ui| {
                    if ui.button("Duplicate").clicked() { self.worker.send(RequestKind::Mutation,method::PROFILE_DUPLICATE,json!({"profile_id":profile.id,"name":format!("{} copy",profile.name)})); }
                    if ui.button("Activate").clicked() { self.worker.send(RequestKind::Mutation,method::PROFILE_ACTIVATE,json!({"profile_id":profile.id})); }
                    if ui.button("Trash").clicked() { self.worker.send(RequestKind::Mutation,method::PROFILE_TRASH,json!({"profile_id":profile.id})); }
                });
                ui.horizontal(|ui| { ui.label("Import"); ui.text_edit_singleline(&mut self.import_path); if ui.button("Go").clicked() { self.worker.send(RequestKind::Mutation,method::PROFILE_IMPORT,json!({"path":self.import_path})); } });
                ui.horizontal(|ui| { ui.label("Export"); ui.text_edit_singleline(&mut self.export_path); if ui.button("Go").clicked() { self.worker.send(RequestKind::Mutation,method::PROFILE_EXPORT,json!({"profile_id":profile.id,"path":self.export_path})); } });
                ui.horizontal(|ui| { ui.label("Restore ID"); ui.text_edit_singleline(&mut self.restore_id); if ui.button("Go").clicked() { self.worker.send(RequestKind::Mutation,method::PROFILE_RESTORE,json!({"profile_id":self.restore_id})); } });
                ui.collapsing("Diagnostic bundle", |ui| {
                    let selected_element_ids: Vec<_> = self
                        .selected_region
                        .and_then(|index| profile.elements.get(index))
                        .map(|element| vec![element.id])
                        .unwrap_or_default();
                    ui.label("Freeze the preview and select a zone to include its crop. Full screenshots, tokens, and machine capture bindings are excluded.");
                    if ui.button("Review exact contents").clicked() {
                        self.worker.send(
                            RequestKind::DiagnosticPlan,
                            method::DIAGNOSTIC_PLAN,
                            json!({"profile_id":profile.id,"selected_element_ids":selected_element_ids}),
                        );
                    }
                    if let Some(entries) = self.diagnostic_plan["entries"].as_array() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            self.diagnostic_plan["privacy_warning"].as_str().unwrap_or("Review all diagnostic entries before export."),
                        );
                        for entry in entries {
                            ui.label(format!("{} · {} bytes{}", entry["path"].as_str().unwrap_or("?"), entry["bytes"].as_u64().unwrap_or(0), if entry["user_selected_image"].as_bool().unwrap_or(false) { " · selected image" } else { "" }));
                        }
                        ui.label(format!("Total uncompressed: {} bytes", self.diagnostic_plan["total_uncompressed_bytes"].as_u64().unwrap_or(0)));
                        ui.checkbox(&mut self.diagnostic_privacy_reviewed, "I reviewed every listed entry and selected crop");
                        ui.text_edit_singleline(&mut self.diagnostic_path);
                        if ui.add_enabled(self.diagnostic_privacy_reviewed && !self.diagnostic_path.is_empty(), egui::Button::new("Export reviewed bundle")).clicked() {
                            self.worker.send(
                                RequestKind::DiagnosticExport,
                                method::DIAGNOSTIC_EXPORT,
                                json!({
                                    "path":self.diagnostic_path,
                                    "bundle":{"profile_id":profile.id,"selected_element_ids":selected_element_ids},
                                    "privacy_reviewed":true,
                                    "expected_total_uncompressed_bytes":self.diagnostic_plan["total_uncompressed_bytes"],
                                }),
                            );
                        }
                    }
                });
                self.revision_history_ui(ui, &profile);
            }
        });
    }

    fn revision_history_ui(&mut self, ui: &mut egui::Ui, profile: &Profile) {
        ui.collapsing("Revision history", |ui| {
            if self.revisions.is_empty() {
                ui.label("No retained revisions loaded.");
                if ui.button("Reload history").clicked() {
                    self.worker.send(RequestKind::RevisionHistory,method::PROFILE_REVISIONS,json!({"profile_id":profile.id}));
                }
                return;
            }
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for (index, revision) in self.revisions.iter().enumerate().rev() {
                    let current = revision.revision == profile.revision;
                    if ui.selectable_label(
                        self.selected_revision == Some(index),
                        format!("rev {}{}", revision.revision, if current { " · current" } else { "" }),
                    ).clicked() {
                        self.selected_revision = Some(index);
                        self.rollback_confirm = false;
                    }
                }
            });
            let Some(revision) = self.selected_revision.and_then(|index| self.revisions.get(index)) else { return; };
            ui.separator();
            ui.strong(format!("Compare rev {} → rev {}", revision.revision, profile.revision));
            ui.label(profile_comparison(revision, profile));
            let can_rollback = revision.revision != profile.revision && self.dirty_since.is_none();
            if self.rollback_confirm {
                ui.colored_label(egui::Color32::YELLOW, format!("This will create revision {} from revision {}. Current revision {} remains in history.", profile.revision.saturating_add(1), revision.revision, profile.revision));
                ui.horizontal(|ui| {
                    if ui.button("Confirm rollback").clicked() {
                        self.worker.send(RequestKind::Rollback,method::PROFILE_ROLLBACK,json!({"profile_id":profile.id,"revision":revision.revision,"expected_revision":profile.revision}));
                    }
                    if ui.button("Cancel").clicked() { self.rollback_confirm=false; }
                });
            } else {
                if ui.add_enabled(can_rollback, egui::Button::new(format!("Rollback to rev {}…", revision.revision))).clicked() {
                    self.rollback_confirm = true;
                }
                if self.dirty_since.is_some() {
                    ui.colored_label(egui::Color32::YELLOW, "Save or revert the current draft before rollback.");
                }
            }
        });
    }

    fn topbar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("capture").show(context, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.capture_select_pending,
                        egui::Button::new("Select source"),
                    )
                    .clicked()
                {
                    let profile_id = self.draft.as_ref().map(|profile| profile.id);
                    self.capture_select_pending = true;
                    self.worker.send(
                        RequestKind::CaptureSelect,
                        method::CAPTURE_SELECT,
                        json!({"source":"window_or_monitor","profile_id":profile_id}),
                    );
                }
                if self.capture_select_pending {
                    ui.spinner();
                    ui.label("Waiting for portal selection…");
                }
                if ui.button("Stop").clicked() {
                    self.worker
                        .send(RequestKind::Capture, method::CAPTURE_STOP, Value::Null);
                }
                ui.separator();
                ui.label("Game process");
                let process_changed = ui.text_edit_singleline(&mut self.auto_process_match).lost_focus();
                let enabled_changed = ui.checkbox(&mut self.auto_capture_enabled, "Auto capture").changed();
                ui.label(if self.auto_process_running { "running" } else { "stopped" });
                if (process_changed || enabled_changed) && self.draft.is_some() {
                    let profile_id = self.draft.as_ref().map(|profile| profile.id);
                    self.worker.send(RequestKind::AutoCapture, method::CAPTURE_AUTO_SET, json!({"profile_id":profile_id,"enabled":self.auto_capture_enabled,"process_match":self.auto_process_match}));
                }
                if ui.checkbox(&mut self.preview_enabled, "Preview").changed() {
                    self.worker.send(
                        if self.preview_enabled {
                            RequestKind::PreviewStart
                        } else {
                            RequestKind::PreviewStop
                        },
                        if self.preview_enabled {
                            method::PREVIEW_START
                        } else {
                            method::PREVIEW_STOP
                        },
                        Value::Null,
                    );
                }
                if ui.checkbox(&mut self.frozen, "Freeze").changed() {
                    self.worker.send(
                        RequestKind::Capture,
                        if self.frozen {
                            method::PREVIEW_FREEZE
                        } else {
                            method::PREVIEW_UNFREEZE
                        },
                        Value::Null,
                    );
                }
                ui.label(format!(
                    "input {:.1} FPS · analysis {:.1} FPS · replaced {}",
                    self.capture_status["metrics"]["input_fps"]
                        .as_f64()
                        .unwrap_or(0.0),
                    self.capture_status["metrics"]["analysis_fps"]
                        .as_f64()
                        .unwrap_or(0.0),
                    self.capture_status["metrics"]["replaced_frames"]
                        .as_u64()
                        .unwrap_or(0)
                ));
            });
        });
    }

    fn replay_panel(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::bottom("replay")
            .resizable(true)
            .default_height(95.0)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Replay manifest");
                    ui.text_edit_singleline(&mut self.replay_path);
                    if ui.button("Evaluate").clicked() {
                        self.worker.send(
                            RequestKind::Replay,
                            method::REPLAY_EVALUATE,
                            json!({"path":self.replay_path}),
                        );
                    }
                    if ui.button(if self.replay_playing { "Pause" } else { "Play" }).clicked() {
                        self.replay_playing = !self.replay_playing;
                        self.replay_last_step = Instant::now();
                    }
                });
                let maximum = self.replay_result["events"]
                    .as_array()
                    .map_or(0, Vec::len);
                ui.add(egui::Slider::new(&mut self.replay_frame, 0..=maximum).text("event cursor"));
                ui.label(format!(
                    "precision {:.3} · recall {:.3} · duplicates {} · misses {} · latency {} ms · {}",
                    self.replay_result["metrics"]["precision"].as_f64().unwrap_or(0.0),
                    self.replay_result["metrics"]["recall"].as_f64().unwrap_or(0.0),
                    self.replay_result["metrics"]["duplicates"].as_u64().unwrap_or(0),
                    self.replay_result["metrics"]["misses"].as_u64().unwrap_or(0),
                    self.replay_result["metrics"]["mean_latency_ms"],
                    if self.replay_result["metrics"]["passed"].as_bool().unwrap_or(false) { "PASS" } else { "NOT PASSED" }
                ));
            });
    }

    #[allow(clippy::cast_precision_loss)]
    fn runtime_panel(&mut self, context: &egui::Context) {
        egui::SidePanel::right("runtime_evidence")
            .resizable(true)
            .default_width(360.0)
            .min_width(280.0)
            .max_width(560.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Live evidence");
                    let active = self.capture_status["active"].as_bool().unwrap_or(false);
                    ui.colored_label(
                        if active { egui::Color32::LIGHT_GREEN } else { egui::Color32::YELLOW },
                        if active { "CAPTURING" } else { "CAPTURE STOPPED" },
                    );
                    ui.label(format!(
                        "{}×{} {} · input {:.1} FPS · analysis {:.1} FPS · latency {} ms · errors {}",
                        self.capture_status["metrics"]["width"].as_u64().unwrap_or(0),
                        self.capture_status["metrics"]["height"].as_u64().unwrap_or(0),
                        self.capture_status["metrics"]["pixel_format"].as_str().unwrap_or("—"),
                        self.capture_status["metrics"]["input_fps"].as_f64().unwrap_or(0.0),
                        self.capture_status["metrics"]["analysis_fps"].as_f64().unwrap_or(0.0),
                        display_json(&self.capture_status["metrics"]["last_processing_latency_ms"]),
                        self.capture_status["metrics"]["detector_errors"].as_u64().unwrap_or(0),
                    ));
                    ui.label(format!(
                        "Daemon {:.1}% CPU · {:.1} MiB RSS\nGUI {:.1}% CPU · {:.1} MiB RSS",
                        self.status["daemon_cpu_percent"].as_f64().unwrap_or(0.0),
                        self.status["daemon_rss_bytes"].as_f64().unwrap_or(0.0) / 1_048_576.0,
                        self.gui_usage.cpu_percent,
                        self.gui_usage.rss_bytes as f64 / 1_048_576.0,
                    ));
                    ui.separator();
                    ui.strong("Observations");
                    observations_ui(
                        ui,
                        &self.state["observations"],
                        self.draft.as_ref(),
                    );
                    ui.separator();
                    ui.strong("Event states");
                    ui.label(display_json(&self.state["events"]));
                    ui.separator();
                    ui.strong("Latest detector test");
                    if let Some(observation) = self.detector_result["observations"].as_array().and_then(|items| items.last()) {
                        ui.label(format!(
                            "status: {}\nvalue: {}\nconfidence: {}\ndiagnostic: {}",
                            observation["status"].as_str().unwrap_or("unknown"),
                            display_json(&observation["value"]),
                            display_json(&observation["confidence"]),
                            display_json(&observation["diagnostic"]),
                        ));
                    } else {
                        ui.label("No test yet — select a zone and click Test detector");
                    }
                    ui.separator();
                    self.output_routes_ui(ui);
                    ui.separator();
                    self.collection_ui(ui);
                });
            });
    }

    fn output_routes_ui(&mut self, ui: &mut egui::Ui) {
        let total_routes = self.output_routes.len();
        let enabled_routes = self
            .output_routes
            .iter()
            .filter(|route| route["enabled"].as_bool().unwrap_or(false))
            .count();
        let summary = if self.draft.is_some() {
            format!("Output routes — {enabled_routes}/{total_routes} enabled")
        } else {
            "Output routes".into()
        };
        egui::CollapsingHeader::new(summary)
            .id_salt("output-routes")
            .default_open(false)
            .show(ui, |ui| {
                let Some(profile_id) = self.draft.as_ref().map(|profile| profile.id) else {
                    ui.label("Select a profile first");
                    return;
                };
                ui.label("Machine-local routes run in the daemon. Command routes invoke a direct executable without a shell.");
                if self.output_routes.is_empty() {
                    ui.label("No routes configured. Add one with `yash-eventsctl output set`. ");
                }
                for route in self.output_routes.clone() {
                    let Some(route_id) = route["id"].as_str() else {
                        continue;
                    };
                    let summary = output_route_summary(&route);
                    egui::CollapsingHeader::new(summary)
                        .id_salt(("output-route", route_id))
                        .default_open(false)
                        .show(ui, |ui| {
                        let mut enabled = route["enabled"].as_bool().unwrap_or(false);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut enabled, "Enabled").changed() {
                                self.worker.send(
                                    RequestKind::OutputMutation,
                                    method::OUTPUT_ENABLE,
                                    json!({"profile_id":profile_id,"route_id":route_id,"enabled":enabled}),
                                );
                            }
                            if ui.button("Test output").clicked() {
                                self.worker.send(
                                    RequestKind::OutputTest,
                                    method::OUTPUT_TEST,
                                    json!({"profile_id":profile_id,"route_id":route_id}),
                                );
                            }
                        });
                        ui.strong("Trigger");
                        ui.label(
                            egui::RichText::new(display_json(&route["trigger"])).monospace(),
                        );
                        ui.strong("Sink");
                        ui.label(egui::RichText::new(display_json(&route["sink"])).monospace());
                        });
                }
                if !self.output_test.is_null()
                    && self.output_test.as_object().is_some_and(|value| !value.is_empty())
                {
                    ui.strong("Latest verification");
                    ui.label(
                        egui::RichText::new(display_json(&self.output_test)).monospace(),
                    );
                }
                ui.separator();
                egui::CollapsingHeader::new("Packaged route examples")
                    .default_open(false)
                    .show(ui, |ui| self.output_recipe_ui(ui, profile_id));
            });
    }

    fn select_output_recipe(&mut self, index: usize) {
        let Some(entry) = self.output_recipes.get(index).cloned() else {
            return;
        };
        self.selected_output_recipe = Some(index);
        self.output_recipe_editor = serde_json::to_string_pretty(&json!({
            "name":entry["recipe"]["name"],
            "trigger":entry["recipe"]["trigger"],
            "format":entry["recipe"]["format"]
        }))
        .unwrap_or_else(|_| "{}".into());
        let sink = &entry["recipe"]["suggested_sink"];
        self.output_recipe_command = sink["kind"] == "command";
        self.output_recipe_destination.clear();
        let arguments = sink.get("args").cloned().unwrap_or_else(|| json!([]));
        self.output_recipe_arguments =
            serde_json::to_string_pretty(&arguments).unwrap_or_else(|_| "[]".into());
        self.output_recipe_timeout_ms = sink["timeout_ms"].as_u64().unwrap_or(5_000);
        self.output_recipe_replace = sink["mode"] == "replace";
        self.output_recipe_preview = json!({});
    }

    #[allow(clippy::too_many_lines)]
    fn output_recipe_ui(&mut self, ui: &mut egui::Ui, profile_id: ProfileId) {
        ui.label("Recipes are portable but inert. Preview and edit them, choose a local destination, then install a disabled route.");
        if self.output_recipes.is_empty() {
            ui.label("This profile does not package any output recipes.");
            return;
        }
        for (index, entry) in self.output_recipes.clone().iter().enumerate() {
            let recipe = &entry["recipe"];
            let installed = self.output_routes.iter().any(|route| {
                route["source_recipe"]["recipe_id"] == recipe["id"]
                    && route["source_recipe"]["sha256"] == entry["sha256"]
            });
            let label = format!(
                "{}{}",
                recipe["name"].as_str().unwrap_or("Unnamed recipe"),
                if installed { " · installed" } else { "" }
            );
            if ui
                .selectable_label(self.selected_output_recipe == Some(index), label)
                .clicked()
            {
                self.select_output_recipe(index);
            }
        }
        let Some(index) = self.selected_output_recipe else {
            return;
        };
        let Some(entry) = self.output_recipes.get(index).cloned() else {
            return;
        };
        let recipe = &entry["recipe"];
        ui.group(|ui| {
            ui.label(recipe["description"].as_str().unwrap_or("No description"));
            ui.small(format!(
                "source {} · sha256 {}",
                entry["path"].as_str().unwrap_or("unknown"),
                entry["sha256"].as_str().unwrap_or("unknown")
            ));
            ui.label(format!(
                "Suggested sink: {}",
                display_json(&recipe["suggested_sink"])
            ));
            ui.label("Review/edit trigger and disclosed output template");
            ui.add(
                egui::TextEdit::multiline(&mut self.output_recipe_editor)
                    .desired_rows(10)
                    .code_editor(),
            );
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.output_recipe_command, false, "File sink");
                ui.selectable_value(&mut self.output_recipe_command, true, "Command sink");
            });
            ui.horizontal(|ui| {
                ui.label(if self.output_recipe_command {
                    "Absolute executable"
                } else {
                    "Absolute output file"
                });
                ui.text_edit_singleline(&mut self.output_recipe_destination);
            });
            if self.output_recipe_command {
                ui.label("Command arguments (JSON string array; no shell)");
                ui.add(
                    egui::TextEdit::multiline(&mut self.output_recipe_arguments)
                        .desired_rows(3)
                        .code_editor(),
                );
                ui.add(
                    egui::DragValue::new(&mut self.output_recipe_timeout_ms)
                        .range(1..=30_000)
                        .suffix(" ms timeout"),
                );
            } else {
                ui.checkbox(
                    &mut self.output_recipe_replace,
                    "Atomically replace instead of append",
                );
            }
            ui.horizontal(|ui| {
                if ui.button("Preview output (no side effect)").clicked() {
                    if let Some(params) = self.output_recipe_draft(profile_id, &entry) {
                        self.worker.send(
                            RequestKind::OutputRecipePreview,
                            method::OUTPUT_RECIPE_PREVIEW,
                            params,
                        );
                    }
                }
                if ui.button("Install disabled").clicked() {
                    self.install_output_recipe(profile_id, &entry);
                }
            });
            if self
                .output_recipe_preview
                .as_object()
                .is_some_and(|value| !value.is_empty())
            {
                ui.label(format!(
                    "Recipe result: {}",
                    display_json(&self.output_recipe_preview)
                ));
            }
        });
    }

    fn output_recipe_draft(&mut self, profile_id: ProfileId, entry: &Value) -> Option<Value> {
        let edited = match serde_json::from_str::<Value>(&self.output_recipe_editor) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                self.error = Some("output recipe edit must be a JSON object".into());
                return None;
            }
            Err(error) => {
                self.error = Some(format!("invalid output recipe JSON: {error}"));
                return None;
            }
        };
        Some(json!({
            "profile_id":profile_id,
            "recipe_id":entry["recipe"]["id"],
            "sha256":entry["sha256"],
            "name":edited["name"],
            "trigger":edited["trigger"],
            "format":edited["format"]
        }))
    }

    fn install_output_recipe(&mut self, profile_id: ProfileId, entry: &Value) {
        let Some(mut params) = self.output_recipe_draft(profile_id, entry) else {
            return;
        };
        let destination = PathBuf::from(self.output_recipe_destination.trim());
        if !destination.is_absolute() {
            self.error = Some("choose an absolute local executable or output path".into());
            return;
        }
        let sink = if self.output_recipe_command {
            let args = match serde_json::from_str::<Vec<String>>(&self.output_recipe_arguments) {
                Ok(args) => args,
                Err(error) => {
                    self.error = Some(format!(
                        "command arguments must be a JSON string array: {error}"
                    ));
                    return;
                }
            };
            json!({
                "kind":"command","program":destination,"args":args,
                "timeout_ms":self.output_recipe_timeout_ms
            })
        } else {
            json!({
                "kind":"file","path":destination,
                "mode":if self.output_recipe_replace { "replace" } else { "append" }
            })
        };
        params["sink"] = sink;
        self.worker.send(
            RequestKind::OutputRecipeInstall,
            method::OUTPUT_RECIPE_INSTALL,
            params,
        );
    }

    #[allow(clippy::too_many_lines)]
    fn collection_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Passive evidence collector")
            .default_open(false)
            .show(ui, |ui| {
                let Some(profile_id) = self.draft.as_ref().map(|profile| profile.id) else {
                    ui.label("Select a profile first");
                    return;
                };
                ui.checkbox(&mut self.collection_enabled, "Enabled while game capture is active");
                ui.horizontal(|ui| {
                    ui.label("Dataset package");
                    ui.text_edit_singleline(&mut self.collection_dataset_root);
                });
                ui.horizontal(|ui| {
                    ui.label("Interval");
                    ui.add(
                        egui::DragValue::new(&mut self.collection_interval_seconds)
                            .range(10..=86_400)
                            .suffix(" s"),
                    );
                    ui.label("jitter");
                    ui.add(
                        egui::DragValue::new(&mut self.collection_jitter_seconds)
                            .range(0..=3_600)
                            .suffix(" s"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Duplicate threshold");
                    ui.add(
                        egui::DragValue::new(&mut self.collection_similarity_threshold)
                            .range(0.0..=0.25)
                            .speed(0.001),
                    );
                    ui.label("pending limit");
                    ui.add(
                        egui::DragValue::new(&mut self.collection_maximum_pending)
                            .range(1..=100_000),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Byte limit");
                    ui.add(
                        egui::DragValue::new(&mut self.collection_maximum_bytes)
                            .range(1_048_576..=1_099_511_627_776_u64),
                    );
                });
                ui.label("Value-novelty targets (comma separated; leave empty to compare status/confidence only)");
                ui.text_edit_singleline(&mut self.collection_novelty_targets);
                if ui.button("Apply collector policy").clicked() {
                    let novelty_targets = self
                        .collection_novelty_targets
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>();
                    self.worker.send(
                        RequestKind::CollectionPolicy,
                        method::COLLECTION_POLICY_SET,
                        json!({
                            "profile_id":profile_id,
                            "policy":{
                                "enabled":self.collection_enabled,
                                "dataset_root":self.collection_dataset_root,
                                "interval_seconds":self.collection_interval_seconds,
                                "jitter_seconds":self.collection_jitter_seconds,
                                "similarity_threshold":self.collection_similarity_threshold,
                                "maximum_pending_items":self.collection_maximum_pending,
                                "maximum_bytes":self.collection_maximum_bytes,
                                "novelty_targets":novelty_targets,
                            }
                        }),
                    );
                }
                let storage = &self.collection_status["storage"];
                let runtime = &self.collection_status["runtime"];
                ui.label(format!(
                    "pending {} · needs correction {} · accepted {} · rejected {} · promoted {} · {:.1} MiB\nsaved {} · duplicate skips {} · errors {}{}",
                    storage["pending_items"].as_u64().unwrap_or(0),
                    storage["needs_correction_items"].as_u64().unwrap_or(0),
                    storage["accepted_items"].as_u64().unwrap_or(0),
                    storage["rejected_items"].as_u64().unwrap_or(0),
                    storage["promoted_items"].as_u64().unwrap_or(0),
                    storage["bytes"].as_f64().unwrap_or(0.0) / 1_048_576.0,
                    runtime["saved"].as_u64().unwrap_or(0),
                    runtime["duplicates"].as_u64().unwrap_or(0),
                    runtime["errors"].as_u64().unwrap_or(0),
                    runtime["last_error"]
                        .as_str()
                        .map_or(String::new(), |error| format!(" · {error}")),
                ));
                ui.horizontal(|ui| {
                    if ui.button("Refresh inbox").clicked() {
                        self.refresh_collection();
                    }
                    if ui.button("Auto-review safely").clicked() {
                        self.worker.send(
                            RequestKind::CollectionMutation,
                            method::COLLECTION_AUTO_REVIEW,
                            json!({"profile_id":profile_id,"promote":false}),
                        );
                    }
                    if ui.button("Auto-review + promote trusted").clicked() {
                        self.worker.send(
                            RequestKind::CollectionMutation,
                            method::COLLECTION_AUTO_REVIEW,
                            json!({"profile_id":profile_id,"promote":true}),
                        );
                    }
                });
                ui.separator();
                ui.strong(format!("Review batch ({})", self.collection_items.len()));
                for item in self.collection_items.clone() {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} · {} · {} obs",
                            item["created_at"].as_str().unwrap_or("?"),
                            item["status"].as_str().unwrap_or("?"),
                            item["observations"].as_u64().unwrap_or(0)
                        ));
                        if ui.button("Inspect").clicked() {
                            self.worker.send(
                                RequestKind::CollectionItem,
                                method::COLLECTION_ITEM_GET,
                                json!({"profile_id":profile_id,"id":item["id"]}),
                            );
                        }
                    });
                }
                let selected_id = self.collection_selected["item"]["id"]
                    .as_str()
                    .map(str::to_owned);
                if let Some(selected_id) = selected_id {
                    ui.separator();
                    ui.strong(format!("Selected {selected_id}"));
                    if let Some(texture) = &self.collection_texture {
                        let available = ui.available_width();
                        let size = texture.size_vec2();
                        let scale = (available / size.x).min(1.0);
                        ui.image((texture.id(), size * scale));
                    }
                    ui.label(display_json(
                        &self.collection_selected["item"]["observations"],
                    ));
                    ui.label("Corrected expected observations JSON");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.collection_expected_json)
                            .desired_rows(8)
                            .code_editor(),
                    );
                    ui.horizontal(|ui| {
                        for (label, action) in [
                            ("Accept", "accept"),
                            ("Correct", "correct"),
                            ("Reject", "reject"),
                            ("Promote", "promote"),
                        ] {
                            if ui.button(label).clicked() {
                                self.send_collection_review(profile_id, &selected_id, action);
                            }
                        }
                    });
                }
            });
    }

    fn send_collection_review(&mut self, profile_id: ProfileId, item_id: &str, action: &str) {
        let expected = match serde_json::from_str::<Value>(&self.collection_expected_json) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                self.error = Some("expected observations must be a JSON object".into());
                return;
            }
            Err(error) => {
                self.error = Some(format!("invalid expected-observation JSON: {error}"));
                return;
            }
        };
        self.worker.send(
            RequestKind::CollectionMutation,
            method::COLLECTION_REVIEW,
            json!({
                "profile_id":profile_id,"id":item_id,"action":action,
                "reason":"GUI review","expected_observations":expected
            }),
        );
    }

    #[allow(clippy::too_many_lines)]
    fn editor(&mut self, context: &egui::Context) {
        egui::CentralPanel::default().show(context, |ui| {
            let Some(mut profile) = self.draft.take() else {
                ui.centered_and_justified(|ui| ui.label("Create or select a profile"));
                return;
            };
            ui.horizontal(|ui| {
                ui.label("Name");
                if ui.text_edit_singleline(&mut profile.name).changed() {
                    self.mark_dirty();
                }
                ui.label("Game ID");
                if ui.text_edit_singleline(&mut profile.game).changed() {
                    self.mark_dirty();
                }
                if ui.button("Save").clicked() {
                    self.worker.send(
                        RequestKind::Commit,
                        method::PROFILE_COMMIT,
                        json!({"profile":profile,"expected_revision":profile.revision}),
                    );
                }
                if ui.button("Revert").clicked() {
                    if let Some(index) = self.selected {
                        profile = self.profiles[index].clone();
                        self.dirty_since = None;
                    }
                }
                ui.label(if self.dirty_since.is_some() {
                    if self.draft_saved {
                        "draft saved"
                    } else {
                        "unsaved"
                    }
                } else {
                    "committed"
                });
            });
            self.layout_compatibility_ui(ui, &profile);
            ui.strong("Detection hierarchy");
            for (derived_index, derived) in profile.derived_observations.iter().enumerate() {
                ui.group(|ui| {
                    if ui
                        .selectable_label(
                            self.selected_derived == Some(derived_index),
                            format!("{} · structured text · derived observation", derived.name),
                        )
                        .clicked()
                    {
                        self.selected_derived = Some(derived_index);
                        self.selected_region = None;
                    }
                    ui.indent(("derived-inputs", derived_index), |ui| {
                        ui.label("Composed from:");
                        for input in &derived.inputs {
                            if let Some((index, element)) = profile
                                .elements
                                .iter()
                                .enumerate()
                                .find(|(_, element)| element.id == input.element_id)
                            {
                                ui.horizontal(|ui| {
                                    ui.monospace(format!("{} →", input.name));
                                    if zone_selector(ui, self.selected_region, index, element) {
                                        self.selected_region = Some(index);
                                        self.selected_derived = None;
                                    }
                                });
                            }
                        }
                    });
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.strong("Independent zones");
                for (index, element) in
                    profile.elements.iter().enumerate().filter(|(_, element)| {
                        !matches!(element.name.as_str(), "Stage group" | "Stage counter")
                    })
                {
                    if zone_selector(ui, self.selected_region, index, element) {
                        self.selected_region = Some(index);
                        self.selected_derived = None;
                    }
                }
            });
            if let Some(index) = self.selected_derived {
                self.derived_editor(ui, &mut profile, index);
            }
            ui.horizontal(|ui| {
                ui.toggle_value(&mut self.drawing, "Draw region");
                if ui.button("Duplicate region").clicked() {
                    if let Some(index) = self.selected_region {
                        let id = profile.elements[index].id;
                        if let Ok(new_id) = ProfileStore::duplicate_element(&mut profile, id, false)
                        {
                            self.selected_region = profile
                                .elements
                                .iter()
                                .position(|element| element.id == new_id);
                            self.mark_dirty();
                        }
                    }
                }
                ui.add(egui::Slider::new(&mut self.zoom, 0.5..=4.0).text("Zoom"));
                if ui.button("Reset view").clicked() {
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                }
            });
            if let Some(index) = self.selected_region {
                if let Some(element) = profile.elements.get_mut(index) {
                    ui.horizontal(|ui| {
                        if ui.text_edit_singleline(&mut element.name).changed() {
                            self.mark_dirty();
                        }
                        if ui.checkbox(&mut element.enabled, "Enabled").changed() {
                            self.mark_dirty();
                        }
                        ui.label(format!(
                            "x {:.4} y {:.4} w {:.4} h {:.4}",
                            element.region.x,
                            element.region.y,
                            element.region.width,
                            element.region.height
                        ));
                        ui.label(format!(
                            "{:.0}×{:.0} px @ {}×{}",
                            f64::from(element.region.width)
                                * f64::from(profile.layout.reference_width),
                            f64::from(element.region.height)
                                * f64::from(profile.layout.reference_height),
                            profile.layout.reference_width,
                            profile.layout.reference_height
                        ));
                    });
                }
                self.detector_editor(ui, &mut profile, index);
            }
            let size = ui.available_size().max(egui::vec2(200.0, 150.0));
            let (response, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
            painter.rect_filled(response.rect, 0.0, egui::Color32::from_gray(24));
            let base_canvas = if let Some(texture) = &self.preview_texture {
                fit_rect(response.rect, texture.size_vec2())
            } else {
                response.rect
            };
            if let Some(texture) = &self.preview_texture {
                painter.image(
                    texture.id(),
                    egui::Rect::from_min_size(
                        base_canvas.min + self.pan,
                        base_canvas.size() * self.zoom,
                    ),
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            self.region_interaction(&response, &painter, base_canvas, &mut profile);
            self.draft = Some(profile);
        });
    }

    fn layout_compatibility_ui(&self, ui: &mut egui::Ui, profile: &Profile) {
        ui.collapsing("Layout compatibility", |ui| {
            let reference = (
                profile.layout.reference_width,
                profile.layout.reference_height,
            );
            ui.label(format!(
                "Profile reference: {}×{} · {}",
                reference.0,
                reference.1,
                aspect_ratio_label(reference.0, reference.1)
            ));
            ui.label(format!(
                "Zones: {} normalized rectangles · automatically scale with capture resolution",
                profile.elements.len()
            ));
            ui.label(format!(
                "UI scale: {} · language: {}",
                profile
                    .layout
                    .ui_scale
                    .map_or_else(|| "not specified".into(), |value| format!("{value:.2}")),
                profile.layout.language.as_deref().unwrap_or("not specified")
            ));
            let width = self.capture_status["metrics"]["width"].as_u64();
            let height = self.capture_status["metrics"]["height"].as_u64();
            let (Some(width), Some(height)) = (width, height) else {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Current capture unavailable — start capture to check compatibility.",
                );
                return;
            };
            let (width, height) = (
                u32::try_from(width).unwrap_or(u32::MAX),
                u32::try_from(height).unwrap_or(u32::MAX),
            );
            ui.label(format!(
                "Current capture: {width}×{height} · {} · scale {:.3}× / {:.3}×",
                aspect_ratio_label(width, height),
                f64::from(width) / f64::from(reference.0.max(1)),
                f64::from(height) / f64::from(reference.1.max(1)),
            ));
            let reference_aspect = f64::from(reference.0) / f64::from(reference.1.max(1));
            let capture_aspect = f64::from(width) / f64::from(height.max(1));
            let mismatch = ((capture_aspect / reference_aspect) - 1.0).abs();
            if mismatch > 0.01 {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("Aspect mismatch: {:.1}%. Zones may target the wrong pixels when the game is letterboxed, cropped, or uses a different HUD layout.", mismatch * 100.0),
                );
            } else if (width, height) == reference {
                ui.colored_label(egui::Color32::GREEN, "Exact reference resolution and aspect ratio.");
            } else {
                ui.colored_label(egui::Color32::GREEN, "Compatible aspect ratio; normalized zones scale automatically. Verify game UI scale if recognition differs.");
            }
            ui.small("Portal source/restore data is machine-local and is not exported with the portable profile.");
        });
    }

    #[allow(clippy::too_many_lines)]
    fn derived_editor(&mut self, ui: &mut egui::Ui, profile: &mut Profile, index: usize) {
        ui.separator();
        ui.heading("Derived observation properties");
        let Some(derived) = profile.derived_observations.get_mut(index) else {
            return;
        };
        ui.checkbox(&mut derived.enabled, "Enabled");
        ui.horizontal(|ui| {
            ui.label("Name");
            if ui.text_edit_singleline(&mut derived.name).changed() {
                self.mark_dirty();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Output format");
            if ui.text_edit_singleline(&mut derived.format).changed() {
                self.mark_dirty();
            }
        });
        ui.label("Use named placeholders such as {group} and {counter}.");
        ui.strong("Inputs");
        for input in &mut derived.inputs {
            ui.horizontal(|ui| {
                if ui.text_edit_singleline(&mut input.name).changed() {
                    self.mark_dirty();
                }
                let current = profile
                    .elements
                    .iter()
                    .find(|element| element.id == input.element_id)
                    .map_or("Missing", |element| element.name.as_str());
                egui::ComboBox::from_id_salt((derived.id.to_string(), input.name.clone()))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for element in &profile.elements {
                            if ui
                                .selectable_value(&mut input.element_id, element.id, &element.name)
                                .changed()
                            {
                                self.mark_dirty();
                            }
                        }
                    });
            });
        }
        if ui.button("Add input").clicked() {
            if let Some(element) = profile.elements.first() {
                derived.inputs.push(DerivedInput {
                    name: format!("input{}", derived.inputs.len() + 1),
                    element_id: element.id,
                });
                self.mark_dirty();
            }
        }
        let observation = self.state["observations"].get(derived.id.to_string());
        ui.strong("Live composed value");
        ui.label(observation.map_or_else(|| "None yet".into(), display_json));
        let rule_index = profile
            .rules
            .iter()
            .position(|rule| rule.element_id == derived.id);
        if rule_index.is_none() && ui.button("Add text event rule").clicked() {
            profile.rules.push(EventRule {
                id: RuleId::new(),
                element_id: derived.id,
                event: "stage_changed".into(),
                enter_below: 0.2,
                leave_above: 0.3,
                minimum_confidence: 0.8,
                required_samples: 2,
                sample_window: 3,
                cooldown_ms: 500,
                predicate: RulePredicate::TextEquals {
                    expected: derived.format.clone(),
                },
                stable_for_ms: 0,
                emit_initial: false,
                update_interval_ms: None,
            });
            self.mark_dirty();
        }
        if let Some(rule) = rule_index.and_then(|index| profile.rules.get_mut(index)) {
            ui.strong("Event rule");
            ui.horizontal(|ui| {
                ui.label("Event name");
                if ui.text_edit_singleline(&mut rule.event).changed() {
                    self.mark_dirty();
                }
            });
            match &mut rule.predicate {
                RulePredicate::TextEquals { expected } => {
                    ui.horizontal(|ui| {
                        ui.label("Text equals");
                        if ui.text_edit_singleline(expected).changed() {
                            self.mark_dirty();
                        }
                    });
                }
                RulePredicate::TextContains { needle } => {
                    ui.horizontal(|ui| {
                        ui.label("Text contains");
                        if ui.text_edit_singleline(needle).changed() {
                            self.mark_dirty();
                        }
                    });
                }
                _ => {
                    ui.label("This value currently uses a non-text predicate.");
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn detector_editor(&mut self, ui: &mut egui::Ui, profile: &mut Profile, index: usize) {
        ui.separator();
        ui.heading("Detector and event");
        let current = match &profile.elements[index].detector {
            Detector::ColorBar { .. } => "Color bar",
            Detector::Template { .. } => "Template",
            Detector::RegionChange { .. } => "Region change",
            Detector::Ocr { .. } => "OCR",
            Detector::SevenSegment { .. } => "Seven segment",
            Detector::Classifier { .. } => "Classifier",
        };
        let mut chosen = current;
        egui::ComboBox::from_label("Detector type")
            .selected_text(chosen)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, "Color bar", "Color bar");
                ui.selectable_value(&mut chosen, "Template", "Template");
                ui.selectable_value(&mut chosen, "Region change", "Region change");
                ui.selectable_value(&mut chosen, "OCR", "OCR");
                ui.selectable_value(&mut chosen, "Seven segment", "Seven segment");
                ui.selectable_value(&mut chosen, "Classifier", "Classifier");
            });
        if chosen != current {
            profile.elements[index].detector = match chosen {
                "Template" => Detector::Template {
                    id: DetectorId::new(),
                    templates: Vec::new(),
                    masks: Vec::new(),
                    threshold: 0.85,
                    preprocessing: Vec::new(),
                },
                "Region change" => Detector::RegionChange {
                    id: DetectorId::new(),
                    threshold: 0.1,
                    preprocessing: Vec::new(),
                },
                "OCR" => Detector::Ocr {
                    id: DetectorId::new(),
                    language: "eng".into(),
                    page_segmentation_mode: 7,
                    character_whitelist: None,
                    change_trigger_threshold: 0.02,
                    maximum_interval_ms: 1_000,
                    preprocessing: Vec::new(),
                    empty_value: None,
                    zero_pad_to: None,
                },
                "Seven segment" => Detector::SevenSegment {
                    id: DetectorId::new(),
                    digits: 4,
                    separator_after: Some(2),
                    threshold: 128,
                    preprocessing: Vec::new(),
                },
                "Classifier" => Detector::Classifier {
                    id: DetectorId::new(),
                    model: PathBuf::from("models/classifier.onnx"),
                    model_sha256: "0".repeat(64),
                    labels: vec!["absent".into(), "present".into()],
                    input_width: 8,
                    input_height: 8,
                    preprocessing: vec![PreprocessOperation::Resize {
                        width: 8,
                        height: 8,
                    }],
                    change_trigger_threshold: 0.02,
                    maximum_interval_ms: 1_000,
                },
                _ => Detector::ColorBar {
                    id: DetectorId::new(),
                    direction: BarDirection::LeftToRight,
                    minimum_rgb: [128, 0, 0],
                    maximum_rgb: [255, 128, 128],
                    mask: None,
                },
            };
            self.mark_dirty();
        }
        let selected_element_id = profile.elements[index].id;
        let mut changed = false;
        match &mut profile.elements[index].detector {
            Detector::ColorBar {
                direction,
                minimum_rgb,
                maximum_rgb,
                ..
            } => {
                egui::ComboBox::from_label("Fill direction")
                    .selected_text(format!("{direction:?}"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(direction, BarDirection::LeftToRight, "Left to right");
                        ui.selectable_value(direction, BarDirection::RightToLeft, "Right to left");
                        ui.selectable_value(direction, BarDirection::TopToBottom, "Top to bottom");
                        ui.selectable_value(direction, BarDirection::BottomToTop, "Bottom to top");
                    });
                ui.horizontal(|ui| {
                    ui.label("RGB minimum");
                    for value in minimum_rgb {
                        changed |= ui.add(egui::DragValue::new(value)).changed();
                    }
                    ui.label("maximum");
                    for value in maximum_rgb {
                        changed |= ui.add(egui::DragValue::new(value)).changed();
                    }
                });
            }
            Detector::Template {
                templates,
                threshold,
                preprocessing,
                ..
            } => {
                changed |= ui
                    .add(egui::Slider::new(threshold, 0.0..=1.0).text("Match threshold"))
                    .changed();
                let mut remove = None;
                for (template_index, path) in templates.iter_mut().enumerate() {
                    let mut text = path.to_string_lossy().into_owned();
                    ui.horizontal(|ui| {
                        if ui.text_edit_singleline(&mut text).changed() {
                            *path = PathBuf::from(&text);
                            changed = true;
                        }
                        if ui.small_button("Remove").clicked() {
                            remove = Some(template_index);
                        }
                    });
                }
                if let Some(remove) = remove {
                    templates.remove(remove);
                    changed = true;
                }
                if ui.button("Add template path").clicked() {
                    templates.push(PathBuf::from("templates/template.json"));
                    changed = true;
                }
                if ui.button("Capture template from latest frame").clicked() {
                    let name = format!("template_{}", templates.len() + 1);
                    self.worker.send(RequestKind::TemplateCapture,method::DETECTOR_CAPTURE_TEMPLATE,json!({"profile_id":profile.id,"element_id":selected_element_id,"expected_revision":profile.revision,"name":name}));
                }
                preprocessing_editor(ui, preprocessing, &mut changed);
            }
            Detector::RegionChange {
                threshold,
                preprocessing,
                ..
            } => {
                changed |= ui
                    .add(egui::Slider::new(threshold, 0.0..=1.0).text("Change threshold"))
                    .changed();
                preprocessing_editor(ui, preprocessing, &mut changed);
            }
            Detector::Ocr {
                language,
                page_segmentation_mode,
                character_whitelist,
                change_trigger_threshold,
                maximum_interval_ms,
                zero_pad_to,
                preprocessing,
                ..
            } => {
                ui.horizontal(|ui| {
                    ui.label("Tesseract language");
                    changed |= ui.text_edit_singleline(language).changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(page_segmentation_mode)
                                .range(0..=13)
                                .prefix("page mode "),
                        )
                        .changed();
                });
                let mut whitelist_enabled = character_whitelist.is_some();
                if ui
                    .checkbox(&mut whitelist_enabled, "Restrict recognized characters")
                    .changed()
                {
                    *character_whitelist = whitelist_enabled.then(String::new);
                    changed = true;
                }
                if let Some(whitelist) = character_whitelist {
                    ui.horizontal(|ui| {
                        ui.label("Character whitelist");
                        changed |= ui.text_edit_singleline(whitelist).changed();
                    });
                }
                let mut zero_pad_enabled = zero_pad_to.is_some();
                if ui
                    .checkbox(
                        &mut zero_pad_enabled,
                        "Preserve numeric width with leading zeroes",
                    )
                    .changed()
                {
                    *zero_pad_to = zero_pad_enabled.then_some(2);
                    changed = true;
                }
                if let Some(width) = zero_pad_to {
                    changed |= ui
                        .add(egui::DragValue::new(width).range(1..=16).prefix("width "))
                        .changed();
                }
                changed |= ui
                    .add(
                        egui::Slider::new(change_trigger_threshold, 0.0..=1.0)
                            .text("Run when crop change exceeds"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(maximum_interval_ms)
                            .range(100..=60_000)
                            .prefix("refresh at least every ms "),
                    )
                    .changed();
                preprocessing_editor(ui, preprocessing, &mut changed);
            }
            Detector::SevenSegment {
                digits,
                separator_after,
                threshold,
                preprocessing,
                ..
            } => {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::DragValue::new(digits).range(1..=8).prefix("digits "))
                        .changed();
                    let mut position = separator_after.unwrap_or(0);
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut position)
                                .range(0..=digits.saturating_sub(1))
                                .prefix("separator after "),
                        )
                        .changed();
                    *separator_after = (position != 0).then_some(position);
                    changed |= ui
                        .add(egui::DragValue::new(threshold).prefix("brightness threshold "))
                        .changed();
                });
                preprocessing_editor(ui, preprocessing, &mut changed);
            }
            Detector::Classifier {
                model,
                model_sha256,
                labels,
                input_width,
                input_height,
                preprocessing,
                change_trigger_threshold,
                maximum_interval_ms,
                ..
            } => {
                let mut model_text = model.to_string_lossy().into_owned();
                ui.horizontal(|ui| {
                    ui.label("Profile-relative ONNX model");
                    if ui.text_edit_singleline(&mut model_text).changed() {
                        *model = PathBuf::from(&model_text);
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Model SHA-256");
                    changed |= ui.text_edit_singleline(model_sha256).changed();
                });
                let mut labels_text = labels.join(",");
                ui.horizontal(|ui| {
                    ui.label("Labels (output order)");
                    if ui.text_edit_singleline(&mut labels_text).changed() {
                        *labels = labels_text
                            .split(',')
                            .map(str::trim)
                            .map(str::to_owned)
                            .collect();
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(input_width)
                                .range(1..=4_096)
                                .prefix("width "),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(input_height)
                                .range(1..=4_096)
                                .prefix("height "),
                        )
                        .changed();
                });
                changed |= ui
                    .add(
                        egui::Slider::new(change_trigger_threshold, 0.0..=1.0)
                            .text("Run when crop change exceeds"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(maximum_interval_ms)
                            .range(100..=60_000)
                            .prefix("refresh at least every ms "),
                    )
                    .changed();
                preprocessing_editor(ui, preprocessing, &mut changed);
            }
        }
        if changed {
            self.mark_dirty();
        }
        ui.horizontal(|ui| {
            if ui.button("Test detector").clicked() {
                self.request_detector_test();
            }
            ui.checkbox(&mut self.continuous_test, "Continuous test");
            if self.test_pending {
                ui.spinner();
            }
        });
        if let Some(observation) = self.detector_result["observations"]
            .as_array()
            .and_then(|items| items.last())
        {
            ui.label(format!(
                "{} · value={} · confidence={} · {}",
                observation["status"].as_str().unwrap_or("?"),
                observation["value"],
                observation["confidence"],
                observation["diagnostic"].as_str().unwrap_or("")
            ));
        }
        ui.horizontal(|ui| {
            if let Some(texture) = &self.original_texture {
                ui.vertical(|ui| {
                    ui.label("Original crop");
                    ui.image((
                        texture.id(),
                        fit_size(texture.size_vec2(), egui::vec2(180.0, 100.0)),
                    ));
                });
            }
            if let Some(texture) = &self.processed_texture {
                ui.vertical(|ui| {
                    ui.label("Processed crop");
                    ui.image((
                        texture.id(),
                        fit_size(texture.size_vec2(), egui::vec2(180.0, 100.0)),
                    ));
                });
            }
        });
        let element_id = selected_element_id;
        let rule_index = profile
            .rules
            .iter()
            .position(|rule| rule.element_id == element_id);
        if rule_index.is_none() && ui.button("Add numeric event rule").clicked() {
            profile.rules.push(EventRule {
                id: RuleId::new(),
                element_id,
                event: "region_event".into(),
                enter_below: 0.2,
                leave_above: 0.3,
                minimum_confidence: 0.8,
                required_samples: 2,
                sample_window: 3,
                cooldown_ms: 500,
                predicate: yash_app_events_profile::RulePredicate::default(),
                stable_for_ms: 0,
                emit_initial: false,
                update_interval_ms: None,
            });
            self.mark_dirty();
        }
        if let Some(rule_index) = rule_index {
            let element_options: Vec<_> = profile
                .elements
                .iter()
                .map(|element| (element.id, element.name.clone()))
                .collect();
            let rule = &mut profile.rules[rule_index];
            let mut rule_changed = false;
            ui.horizontal(|ui| {
                ui.label("Event name");
                rule_changed |= ui.text_edit_singleline(&mut rule.event).changed();
            });
            egui::ComboBox::from_label("Observation predicate")
                .selected_text(rule_predicate_name(&rule.predicate))
                .show_ui(ui, |ui| {
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::Numeric,
                        "Numeric below",
                        element_id,
                    );
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::Boolean,
                        "Boolean appearance",
                        element_id,
                    );
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::TextEquals,
                        "Text equals",
                        element_id,
                    );
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::TextContains,
                        "Text contains",
                        element_id,
                    );
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::RapidIncrease,
                        "Rapid numeric increase",
                        element_id,
                    );
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::All,
                        "All observations",
                        element_id,
                    );
                    rule_changed |= predicate_choice(
                        ui,
                        &mut rule.predicate,
                        RulePredicateChoice::Any,
                        "Any observation",
                        element_id,
                    );
                });
            match &mut rule.predicate {
                RulePredicate::NumericBelow => {
                    ui.horizontal(|ui| {
                        rule_changed |= ui
                            .add(
                                egui::DragValue::new(&mut rule.enter_below)
                                    .speed(0.01)
                                    .prefix("enter < "),
                            )
                            .changed();
                        rule_changed |= ui
                            .add(
                                egui::DragValue::new(&mut rule.leave_above)
                                    .speed(0.01)
                                    .prefix("leave > "),
                            )
                            .changed();
                    });
                }
                RulePredicate::Boolean { expected } => {
                    rule_changed |= ui.checkbox(expected, "Expected value").changed();
                }
                RulePredicate::TextEquals { expected } => {
                    ui.horizontal(|ui| {
                        ui.label("Expected text");
                        rule_changed |= ui.text_edit_singleline(expected).changed();
                    });
                }
                RulePredicate::TextContains { needle } => {
                    ui.horizontal(|ui| {
                        ui.label("Required substring");
                        rule_changed |= ui.text_edit_singleline(needle).changed();
                    });
                }
                RulePredicate::RapidIncrease {
                    minimum_delta,
                    within_ms,
                } => {
                    ui.horizontal(|ui| {
                        rule_changed |= ui
                            .add(egui::DragValue::new(minimum_delta).prefix("increase ≥ "))
                            .changed();
                        rule_changed |= ui
                            .add(egui::DragValue::new(within_ms).suffix(" ms"))
                            .changed();
                    });
                }
                RulePredicate::All { conditions } | RulePredicate::Any { conditions } => {
                    rule_changed |= composition_editor(ui, conditions, &element_options);
                }
            }
            ui.horizontal(|ui| {
                rule_changed |= ui
                    .add(
                        egui::Slider::new(&mut rule.minimum_confidence, 0.0..=1.0)
                            .text("confidence"),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                rule_changed |= ui
                    .add(
                        egui::DragValue::new(&mut rule.required_samples)
                            .range(1..=100)
                            .prefix("N "),
                    )
                    .changed();
                rule_changed |= ui
                    .add(
                        egui::DragValue::new(&mut rule.sample_window)
                            .range(1..=100)
                            .prefix("of M "),
                    )
                    .changed();
                rule_changed |= ui
                    .add(egui::DragValue::new(&mut rule.cooldown_ms).prefix("cooldown ms "))
                    .changed();
                rule_changed |= ui
                    .add(egui::DragValue::new(&mut rule.stable_for_ms).prefix("stable ms "))
                    .changed();
            });
            ui.horizontal(|ui| {
                rule_changed |= ui
                    .checkbox(&mut rule.emit_initial, "Emit initial state")
                    .changed();
                let mut updates_enabled = rule.update_interval_ms.is_some();
                if ui
                    .checkbox(&mut updates_enabled, "Emit active updates")
                    .changed()
                {
                    rule.update_interval_ms = updates_enabled.then_some(1_000);
                    rule_changed = true;
                }
                if let Some(interval) = &mut rule.update_interval_ms {
                    rule_changed |= ui
                        .add(
                            egui::DragValue::new(interval)
                                .range(1..=u64::MAX)
                                .prefix("every ms "),
                        )
                        .changed();
                }
            });
            rule.sample_window = rule.sample_window.max(rule.required_samples);
            ui.label(format!(
                "state={} · predicate={} · evidence {} of {} · stable {} ms · cooldown {} ms",
                self.state["events"][&rule.event],
                rule_predicate_name(&rule.predicate),
                rule.required_samples,
                rule.sample_window,
                rule.stable_for_ms,
                rule.cooldown_ms
            ));
            if rule_changed {
                self.mark_dirty();
            }
        }
        ui.collapsing("Recent observations and transitions", |ui| {
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .show(ui, |ui| {
                    for entry in self.timeline.iter().rev() {
                        ui.label(entry);
                    }
                });
        });
    }

    fn region_interaction(
        &mut self,
        response: &egui::Response,
        painter: &egui::Painter,
        canvas: egui::Rect,
        profile: &mut Profile,
    ) {
        for (index, element) in profile.elements.iter().enumerate() {
            let rect = region_rect(canvas, self.zoom, self.pan, element.region);
            painter.rect_stroke(
                rect,
                0.0,
                egui::Stroke::new(
                    if self.selected_region == Some(index) {
                        3.0
                    } else {
                        1.5
                    },
                    if element.enabled {
                        egui::Color32::LIGHT_GREEN
                    } else {
                        egui::Color32::GRAY
                    },
                ),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.left_top() + egui::vec2(3.0, 3.0),
                egui::Align2::LEFT_TOP,
                &element.name,
                egui::FontId::proportional(13.0),
                egui::Color32::WHITE,
            );
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan += response.drag_delta();
            return;
        }
        let pointer_position = response.interact_pointer_pos();
        if self.drawing {
            if response.drag_started() {
                self.draw_start = pointer_position;
            }
            if let (Some(start), Some(current)) = (self.draw_start, pointer_position) {
                painter.rect_stroke(
                    egui::Rect::from_two_pos(start, current),
                    0.0,
                    egui::Stroke::new(2.0, egui::Color32::YELLOW),
                    egui::StrokeKind::Inside,
                );
            }
            if response.drag_stopped() {
                if let (Some(start), Some(end)) = (self.draw_start.take(), pointer_position) {
                    if let Some(region) =
                        region_from_points(canvas, self.zoom, self.pan, start, end)
                    {
                        profile.elements.push(default_element(region));
                        self.selected_region = Some(profile.elements.len() - 1);
                        self.mark_dirty();
                    }
                }
                self.drawing = false;
            }
            return;
        }
        if response.clicked() {
            self.selected_region = pointer_position.and_then(|position| {
                region_at_position(&profile.elements, canvas, self.zoom, self.pan, position)
            });
        }
        if response.drag_started() {
            if let Some(position) = pointer_position {
                self.selected_region =
                    region_at_position(&profile.elements, canvas, self.zoom, self.pan, position);
                if let Some(index) = self.selected_region {
                    let rect =
                        region_rect(canvas, self.zoom, self.pan, profile.elements[index].region);
                    self.resizing = position.distance(rect.right_bottom()) < 16.0;
                    self.drag_origin = Some(profile.elements[index].region);
                }
            }
        }
        if response.dragged() {
            if let (Some(index), Some(origin)) = (self.selected_region, self.drag_origin) {
                let delta = response.drag_delta();
                let normalized = egui::vec2(
                    delta.x / (canvas.width() * self.zoom),
                    delta.y / (canvas.height() * self.zoom),
                );
                profile.elements[index].region = if self.resizing {
                    resize_region(origin, normalized)
                } else {
                    move_region(origin, normalized)
                };
                self.mark_dirty();
            }
        }
        if response.drag_stopped() {
            self.drag_origin = None;
            self.resizing = false;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain(context);
        self.schedule();
        self.topbar(context);
        self.runtime_panel(context);
        self.replay_panel(context);
        self.sidebar(context);
        self.editor(context);
        context.request_repaint_after(Duration::from_millis(50));
    }
}

fn display_json(value: &Value) -> String {
    match value {
        Value::Null => "—".into(),
        Value::String(value) => value.clone(),
        Value::Object(map) if map.is_empty() => "None yet".into(),
        Value::Array(items) if items.is_empty() => "None yet".into(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| "unavailable".into()),
    }
}

fn output_route_summary(route: &Value) -> String {
    let name = route["name"].as_str().unwrap_or("Unnamed route");
    let status = if route["enabled"].as_bool().unwrap_or(false) {
        "enabled"
    } else {
        "disabled"
    };
    let trigger = route["trigger"]["kind"]
        .as_str()
        .unwrap_or("unknown")
        .replace('_', " ");
    let sink_kind = route["sink"]["kind"].as_str().unwrap_or("unknown");
    let sink = route["sink"]["mode"].as_str().map_or_else(
        || sink_kind.to_owned(),
        |mode| format!("{sink_kind} ({mode})"),
    );
    format!("{name} · {status} · {trigger} → {sink}")
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
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn sample_process_usage(sampler: &mut ProcessSampler) -> ProcessUsage {
    let cpu_ns = std::fs::read_to_string("/proc/self/stat")
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
    let rss_bytes = std::fs::read_to_string("/proc/self/status")
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
    let cpu_percent = cpu_ns
        .zip(sampler.previous_cpu_ns)
        .zip(sampler.previous_at)
        .map_or(0.0, |((current, previous), previous_at)| {
            let wall_ns = now.duration_since(previous_at).as_nanos().max(1) as f64;
            ((current.saturating_sub(previous) as f64 / wall_ns) * 100.0) as f32
        });
    sampler.previous_cpu_ns = cpu_ns;
    sampler.previous_at = Some(now);
    ProcessUsage {
        cpu_percent,
        rss_bytes,
    }
}

fn observations_ui(ui: &mut egui::Ui, observations: &Value, profile: Option<&Profile>) {
    let Some(observations) = observations.as_object() else {
        ui.label(display_json(observations));
        return;
    };
    if observations.is_empty() {
        ui.label("None yet");
        return;
    }
    let hidden = hidden_observation_components(observations, profile);
    let mut items: Vec<_> = observations
        .iter()
        .filter(|(id, _)| !hidden.contains(*id))
        .map(|(id, observation)| {
            let (name, kind) = observation_identity(id, profile);
            (name, kind, id, observation)
        })
        .collect();
    items.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, kind, element_id, observation) in items {
        let status = observation["status"].as_str().unwrap_or("unknown");
        let summary = format!(
            "{name} · {kind} — {} [{status}]",
            observation_value_summary(&observation["value"])
        );
        egui::CollapsingHeader::new(summary)
            .id_salt(("live-observation", element_id))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(format!("Value: {}", display_json(&observation["value"])));
                ui.label(format!("Status: {status}"));
                ui.label(format!(
                    "Confidence: {}",
                    display_json(&observation["confidence"])
                ));
                ui.label(format!(
                    "Timestamp: {} ms",
                    observation["timestamp_ms"].as_u64().unwrap_or(0)
                ));
                ui.monospace(format!("Element ID: {element_id}"));
                ui.monospace(format!(
                    "Detector ID: {}",
                    observation["detector_id"].as_str().unwrap_or("—")
                ));
                ui.label(format!(
                    "Diagnostic: {}",
                    observation["diagnostic"].as_str().unwrap_or("—")
                ));
                if let Some(profile) = profile {
                    if let Some(element) = profile
                        .elements
                        .iter()
                        .find(|element| element.id.to_string() == *element_id)
                    {
                        ui.label(format!(
                            "Region: x {:.5} · y {:.5} · w {:.5} · h {:.5}",
                            element.region.x,
                            element.region.y,
                            element.region.width,
                            element.region.height
                        ));
                    }
                    if let Some(derived) = profile
                        .derived_observations
                        .iter()
                        .find(|derived| derived.id.to_string() == *element_id)
                    {
                        ui.label(format!("Format: {}", derived.format));
                        ui.label("Inputs:");
                        for input in &derived.inputs {
                            let source = profile
                                .elements
                                .iter()
                                .find(|element| element.id == input.element_id)
                                .map_or("missing", |element| element.name.as_str());
                            let value =
                                observations.get(&input.element_id.to_string()).map_or_else(
                                    || "—".into(),
                                    |item| observation_value_summary(&item["value"]),
                                );
                            ui.label(format!("  {} → {} — {}", input.name, source, value));
                        }
                    }
                }
            });
    }
}

fn hidden_observation_components(
    observations: &serde_json::Map<String, Value>,
    profile: Option<&Profile>,
) -> Vec<String> {
    let mut hidden = Vec::new();
    if let Some(profile) = profile {
        for derived in &profile.derived_observations {
            if observations.contains_key(&derived.id.to_string()) {
                hidden.extend(
                    derived
                        .inputs
                        .iter()
                        .map(|input| input.element_id.to_string()),
                );
            }
        }
    }
    hidden
}

fn observation_identity(element_id: &str, profile: Option<&Profile>) -> (String, String) {
    if let Some(element) = profile.and_then(|profile| {
        profile
            .elements
            .iter()
            .find(|element| element.id.to_string() == element_id)
    }) {
        return (
            element.name.clone(),
            detector_label(&element.detector).into(),
        );
    }
    if let Some(derived) = profile.and_then(|profile| {
        profile
            .derived_observations
            .iter()
            .find(|derived| derived.id.to_string() == element_id)
    }) {
        return (derived.name.clone(), "structured text".into());
    }
    (element_id.into(), "unknown detector".into())
}

fn observation_value_summary(value: &Value) -> String {
    let Some(value) = value.get("value") else {
        return display_json(value);
    };
    if let Some(number) = value.as_f64() {
        return format!("{number:.3}");
    }
    if let Some(text) = value.as_str() {
        return text.into();
    }
    display_json(value)
}

#[cfg(test)]
fn display_observations(observations: &Value, profile: Option<&Profile>) -> String {
    let Some(observations) = observations.as_object() else {
        return display_json(observations);
    };
    if observations.is_empty() {
        return "None yet".into();
    }
    let hidden_component_ids = hidden_observation_components(observations, profile);
    observations
        .iter()
        .filter_map(|(element_id, observation)| {
            if hidden_component_ids.contains(element_id) {
                return None;
            }
            let element = profile.and_then(|profile| {
                profile
                    .elements
                    .iter()
                    .find(|element| element.id.to_string() == *element_id)
            });
            let derived = profile.and_then(|profile| {
                profile
                    .derived_observations
                    .iter()
                    .find(|derived| derived.id.to_string() == *element_id)
            });
            let heading = element.map_or_else(
                || {
                    derived.map_or_else(
                        || element_id.clone(),
                        |derived| format!("{} · structured text", derived.name),
                    )
                },
                |element| format!("{} · {}", element.name, detector_label(&element.detector)),
            );
            Some(format!(
                "{heading}\nstatus: {}\nvalue: {}\nconfidence: {}\n{}",
                observation["status"].as_str().unwrap_or("unknown"),
                display_json(&observation["value"]),
                display_json(&observation["confidence"]),
                observation["diagnostic"].as_str().unwrap_or(""),
            ))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn detector_label(detector: &Detector) -> &'static str {
    match detector {
        Detector::ColorBar { .. } => "color bar",
        Detector::Template { .. } => "template",
        Detector::RegionChange { .. } => "region change",
        Detector::Ocr { .. } => "OCR",
        Detector::SevenSegment { .. } => "seven segment",
        Detector::Classifier { .. } => "classifier",
    }
}

fn aspect_ratio_label(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return "unknown aspect".into();
    }
    let divisor = greatest_common_divisor(width, height);
    format!("{}:{}", width / divisor, height / divisor)
}

const fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    if left == 0 {
        1
    } else {
        left
    }
}

fn ensure_stage_derived(profile: &mut Profile) -> bool {
    if !profile.derived_observations.is_empty() {
        return false;
    }
    let group = profile
        .elements
        .iter()
        .find(|element| element.name == "Stage group")
        .map(|element| element.id);
    let counter = profile
        .elements
        .iter()
        .find(|element| element.name == "Stage counter")
        .map(|element| element.id);
    let (Some(group), Some(counter)) = (group, counter) else {
        return false;
    };
    profile.derived_observations.push(DerivedObservation {
        id: ElementId::new(),
        detector_id: DetectorId::new(),
        name: "Stage".into(),
        enabled: true,
        format: "STAGE-{group} : {counter}".into(),
        inputs: vec![
            DerivedInput {
                name: "group".into(),
                element_id: group,
            },
            DerivedInput {
                name: "counter".into(),
                element_id: counter,
            },
        ],
    });
    true
}

fn zone_selector(
    ui: &mut egui::Ui,
    selected_region: Option<usize>,
    index: usize,
    element: &Element,
) -> bool {
    let label = format!(
        "{} · {}{}",
        element.name,
        detector_label(&element.detector),
        if element.enabled { "" } else { " · disabled" }
    );
    ui.selectable_label(selected_region == Some(index), label)
        .clicked()
}

fn profile_comparison(old: &Profile, current: &Profile) -> String {
    let mut changes = Vec::new();
    if old.name != current.name {
        changes.push(format!("Name: {} → {}", old.name, current.name));
    }
    if old.game != current.game {
        changes.push(format!("Game ID: {} → {}", old.game, current.game));
    }
    if old.layout != current.layout {
        changes.push("Reference layout changed".into());
    }
    for element in &old.elements {
        match current
            .elements
            .iter()
            .find(|candidate| candidate.id == element.id)
        {
            None => changes.push(format!("Zone removed: {}", element.name)),
            Some(candidate) if candidate != element => {
                changes.push(format!("Zone changed: {}", element.name));
            }
            Some(_) => {}
        }
    }
    for element in &current.elements {
        if !old
            .elements
            .iter()
            .any(|candidate| candidate.id == element.id)
        {
            changes.push(format!("Zone added: {}", element.name));
        }
    }
    let changed_rules = old
        .rules
        .iter()
        .filter(|rule| {
            current
                .rules
                .iter()
                .find(|candidate| candidate.id == rule.id)
                .is_none_or(|candidate| candidate != *rule)
        })
        .count()
        + current
            .rules
            .iter()
            .filter(|rule| !old.rules.iter().any(|candidate| candidate.id == rule.id))
            .count();
    if changed_rules > 0 {
        changes.push(format!("Event rules changed: {changed_rules}"));
    }
    if changes.is_empty() {
        "No content changes.".into()
    } else {
        changes.join("\n")
    }
}

fn default_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || PathBuf::from("/run/user/unknown/yash-app-events/control.sock"),
        |root| PathBuf::from(root).join("yash-app-events/control.sock"),
    )
}

fn decode_preview(value: &Value) -> Result<PreviewImage, String> {
    let bytes = value["bytes"]
        .as_array()
        .ok_or("preview omitted bytes")?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or("invalid preview byte")
        })
        .collect::<Result<Vec<_>, _>>()?;
    decode_png_bytes(bytes)
}

fn decode_collection_image(value: &Value) -> Result<PreviewImage, String> {
    let path = value["image_path"]
        .as_str()
        .ok_or("collection item omitted image path")?;
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 32 * 1024 * 1024 {
        return Err("collection review image exceeds 32 MiB".into());
    }
    let image = decode_png_bytes(std::fs::read(path).map_err(|error| error.to_string())?)?;
    Ok(downscale_preview(image, 960, 540))
}

fn decode_png_bytes(bytes: Vec<u8>) -> Result<PreviewImage, String> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let mut output = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("preview exceeds decoder limits")?
    ];
    let info = reader
        .next_frame(&mut output)
        .map_err(|error| error.to_string())?;
    let data = &output[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        png::ColorType::Grayscale => data
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        _ => return Err("unsupported preview PNG color type".into()),
    };
    Ok(PreviewImage {
        width: usize::try_from(info.width).map_err(|error| error.to_string())?,
        height: usize::try_from(info.height).map_err(|error| error.to_string())?,
        rgba,
    })
}

fn downscale_preview(
    image: PreviewImage,
    maximum_width: usize,
    maximum_height: usize,
) -> PreviewImage {
    if image.width <= maximum_width && image.height <= maximum_height {
        return image;
    }
    let image_width = image.width as u128;
    let image_height = image.height as u128;
    let maximum_width = maximum_width as u128;
    let maximum_height = maximum_height as u128;
    let (width, height) = if maximum_width * image_height <= maximum_height * image_width {
        (
            maximum_width,
            (image_height * maximum_width / image_width).max(1),
        )
    } else {
        (
            (image_width * maximum_height / image_height).max(1),
            maximum_height,
        )
    };
    let width = usize::try_from(width).unwrap_or(usize::MAX);
    let height = usize::try_from(height).unwrap_or(usize::MAX);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let source_y = y * image.height / height;
        for x in 0..width {
            let source_x = x * image.width / width;
            let offset = (source_y * image.width + source_x) * 4;
            rgba.extend_from_slice(&image.rgba[offset..offset + 4]);
        }
    }
    PreviewImage {
        width,
        height,
        rgba,
    }
}

fn region_rect(
    canvas: egui::Rect,
    zoom: f32,
    pan: egui::Vec2,
    region: NormalizedRegion,
) -> egui::Rect {
    let size = canvas.size() * zoom;
    egui::Rect::from_min_size(
        canvas.min + pan + egui::vec2(region.x * size.x, region.y * size.y),
        egui::vec2(region.width * size.x, region.height * size.y),
    )
}

fn region_at_position(
    elements: &[Element],
    canvas: egui::Rect,
    zoom: f32,
    pan: egui::Vec2,
    position: egui::Pos2,
) -> Option<usize> {
    elements
        .iter()
        .enumerate()
        .rev()
        .find(|(_, element)| region_rect(canvas, zoom, pan, element.region).contains(position))
        .map(|(index, _)| index)
}

fn fit_rect(outer: egui::Rect, size: egui::Vec2) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return outer;
    }
    let scale = (outer.width() / size.x).min(outer.height() / size.y);
    let fitted = size * scale;
    egui::Rect::from_center_size(outer.center(), fitted)
}

fn fit_size(size: egui::Vec2, maximum: egui::Vec2) -> egui::Vec2 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return maximum;
    }
    size * ((maximum.x / size.x).min(maximum.y / size.y).min(1.0))
}

fn load_preview_texture(
    context: &egui::Context,
    name: &str,
    image: &PreviewImage,
) -> egui::TextureHandle {
    let color = egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba);
    context.load_texture(name, color, egui::TextureOptions::LINEAR)
}

fn preprocessing_editor(
    ui: &mut egui::Ui,
    operations: &mut Vec<PreprocessOperation>,
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(format!("Preprocessing: {} operation(s)", operations.len()));
        if ui.button("+ threshold").clicked() {
            operations.push(PreprocessOperation::Threshold {
                minimum: 64,
                maximum: 255,
            });
            *changed = true;
        }
        if ui.button("+ resize").clicked() {
            operations.push(PreprocessOperation::Resize {
                width: 128,
                height: 128,
            });
            *changed = true;
        }
        if ui.button("+ invert").clicked() {
            operations.push(PreprocessOperation::Invert);
            *changed = true;
        }
        if ui.button("Clear").clicked() {
            operations.clear();
            *changed = true;
        }
    });
    for operation in operations.iter() {
        ui.small(format!("{operation:?}"));
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RulePredicateChoice {
    Numeric,
    Boolean,
    TextEquals,
    TextContains,
    RapidIncrease,
    All,
    Any,
}

fn rule_predicate_choice(predicate: &RulePredicate) -> RulePredicateChoice {
    match predicate {
        RulePredicate::NumericBelow => RulePredicateChoice::Numeric,
        RulePredicate::Boolean { .. } => RulePredicateChoice::Boolean,
        RulePredicate::TextEquals { .. } => RulePredicateChoice::TextEquals,
        RulePredicate::TextContains { .. } => RulePredicateChoice::TextContains,
        RulePredicate::RapidIncrease { .. } => RulePredicateChoice::RapidIncrease,
        RulePredicate::All { .. } => RulePredicateChoice::All,
        RulePredicate::Any { .. } => RulePredicateChoice::Any,
    }
}

fn rule_predicate_name(predicate: &RulePredicate) -> &'static str {
    match rule_predicate_choice(predicate) {
        RulePredicateChoice::Numeric => "numeric below",
        RulePredicateChoice::Boolean => "boolean appearance",
        RulePredicateChoice::TextEquals => "text equals",
        RulePredicateChoice::TextContains => "text contains",
        RulePredicateChoice::RapidIncrease => "rapid numeric increase",
        RulePredicateChoice::All => "all observations",
        RulePredicateChoice::Any => "any observation",
    }
}

fn predicate_choice(
    ui: &mut egui::Ui,
    predicate: &mut RulePredicate,
    choice: RulePredicateChoice,
    label: &str,
    element_id: ElementId,
) -> bool {
    if !ui
        .selectable_label(rule_predicate_choice(predicate) == choice, label)
        .clicked()
    {
        return false;
    }
    if rule_predicate_choice(predicate) != choice {
        *predicate = match choice {
            RulePredicateChoice::Numeric => RulePredicate::NumericBelow,
            RulePredicateChoice::Boolean => RulePredicate::Boolean { expected: true },
            RulePredicateChoice::TextEquals => RulePredicate::TextEquals {
                expected: "text".into(),
            },
            RulePredicateChoice::TextContains => RulePredicate::TextContains {
                needle: "text".into(),
            },
            RulePredicateChoice::RapidIncrease => RulePredicate::RapidIncrease {
                minimum_delta: 3,
                within_ms: 5_000,
            },
            RulePredicateChoice::All => RulePredicate::All {
                conditions: vec![ObservationCondition {
                    element_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                }],
            },
            RulePredicateChoice::Any => RulePredicate::Any {
                conditions: vec![ObservationCondition {
                    element_id,
                    predicate: AtomicRulePredicate::Boolean { expected: true },
                }],
            },
        };
        return true;
    }
    false
}

fn composition_editor(
    ui: &mut egui::Ui,
    conditions: &mut Vec<ObservationCondition>,
    elements: &[(ElementId, String)],
) -> bool {
    let mut changed = false;
    let mut remove = None;
    for (index, condition) in conditions.iter_mut().enumerate() {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("condition_element", index))
                    .selected_text(
                        elements
                            .iter()
                            .find(|(id, _)| *id == condition.element_id)
                            .map_or("missing element", |(_, name)| name),
                    )
                    .show_ui(ui, |ui| {
                        for (id, name) in elements {
                            changed |= ui
                                .selectable_value(&mut condition.element_id, *id, name)
                                .changed();
                        }
                    });
                let mut kind = atomic_predicate_kind(&condition.predicate);
                egui::ComboBox::from_id_salt(("condition_kind", index))
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for candidate in AtomicPredicateKind::ALL {
                            changed |= ui
                                .selectable_value(&mut kind, candidate, candidate.label())
                                .changed();
                        }
                    });
                if kind != atomic_predicate_kind(&condition.predicate) {
                    condition.predicate = kind.default_predicate();
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
            match &mut condition.predicate {
                AtomicRulePredicate::Boolean { expected } => {
                    changed |= ui.checkbox(expected, "Expected value").changed();
                }
                AtomicRulePredicate::TextEquals { expected } => {
                    changed |= ui.text_edit_singleline(expected).changed();
                }
                AtomicRulePredicate::TextContains { needle } => {
                    changed |= ui.text_edit_singleline(needle).changed();
                }
                AtomicRulePredicate::NumericBelow { threshold_micros } => {
                    changed |= ui
                        .add(egui::DragValue::new(threshold_micros).prefix("threshold µ "))
                        .changed();
                }
            }
        });
    }
    if let Some(index) = remove {
        conditions.remove(index);
        changed = true;
    }
    if conditions.len() < 16 && ui.button("Add observation condition").clicked() {
        conditions.push(ObservationCondition {
            element_id: elements.first().map_or_else(ElementId::new, |(id, _)| *id),
            predicate: AtomicRulePredicate::Boolean { expected: true },
        });
        changed = true;
    }
    changed
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AtomicPredicateKind {
    Boolean,
    TextEquals,
    TextContains,
    NumericBelow,
}

impl AtomicPredicateKind {
    const ALL: [Self; 4] = [
        Self::Boolean,
        Self::TextEquals,
        Self::TextContains,
        Self::NumericBelow,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::TextEquals => "text equals",
            Self::TextContains => "text contains",
            Self::NumericBelow => "numeric below",
        }
    }

    fn default_predicate(self) -> AtomicRulePredicate {
        match self {
            Self::Boolean => AtomicRulePredicate::Boolean { expected: true },
            Self::TextEquals => AtomicRulePredicate::TextEquals {
                expected: "text".into(),
            },
            Self::TextContains => AtomicRulePredicate::TextContains {
                needle: "text".into(),
            },
            Self::NumericBelow => AtomicRulePredicate::NumericBelow {
                threshold_micros: 200_000,
            },
        }
    }
}

fn atomic_predicate_kind(predicate: &AtomicRulePredicate) -> AtomicPredicateKind {
    match predicate {
        AtomicRulePredicate::Boolean { .. } => AtomicPredicateKind::Boolean,
        AtomicRulePredicate::TextEquals { .. } => AtomicPredicateKind::TextEquals,
        AtomicRulePredicate::TextContains { .. } => AtomicPredicateKind::TextContains,
        AtomicRulePredicate::NumericBelow { .. } => AtomicPredicateKind::NumericBelow,
    }
}

fn normalized_point(
    canvas: egui::Rect,
    zoom: f32,
    pan: egui::Vec2,
    point: egui::Pos2,
) -> egui::Pos2 {
    let local = (point - canvas.min - pan) / (canvas.size() * zoom);
    egui::pos2(local.x.clamp(0.0, 1.0), local.y.clamp(0.0, 1.0))
}
fn region_from_points(
    canvas: egui::Rect,
    zoom: f32,
    pan: egui::Vec2,
    a: egui::Pos2,
    b: egui::Pos2,
) -> Option<NormalizedRegion> {
    let a = normalized_point(canvas, zoom, pan, a);
    let b = normalized_point(canvas, zoom, pan, b);
    let region = NormalizedRegion {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (a.x - b.x).abs(),
        height: (a.y - b.y).abs(),
    };
    (region.width >= 0.002 && region.height >= 0.002).then_some(region)
}
fn move_region(origin: NormalizedRegion, delta: egui::Vec2) -> NormalizedRegion {
    NormalizedRegion {
        x: (origin.x + delta.x).clamp(0.0, 1.0 - origin.width),
        y: (origin.y + delta.y).clamp(0.0, 1.0 - origin.height),
        ..origin
    }
}
fn resize_region(origin: NormalizedRegion, delta: egui::Vec2) -> NormalizedRegion {
    NormalizedRegion {
        width: (origin.width + delta.x).clamp(0.002, 1.0 - origin.x),
        height: (origin.height + delta.y).clamp(0.002, 1.0 - origin.y),
        ..origin
    }
}
fn default_element(region: NormalizedRegion) -> Element {
    Element {
        id: ElementId::new(),
        name: "Region".into(),
        enabled: true,
        color: "#22c55e".into(),
        region,
        detector: Detector::ColorBar {
            id: DetectorId::new(),
            direction: BarDirection::LeftToRight,
            minimum_rgb: [128, 0, 0],
            maximum_rgb: [255, 128, 128],
            mask: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_aspect_ratio_is_human_readable() {
        assert_eq!(aspect_ratio_label(3840, 2160), "16:9");
        assert_eq!(aspect_ratio_label(3440, 1440), "43:18");
        assert_eq!(aspect_ratio_label(0, 0), "unknown aspect");
    }

    #[test]
    fn output_route_summary_is_compact_and_actionable() {
        assert_eq!(
            output_route_summary(&json!({
                "name":"Current stage",
                "enabled":true,
                "trigger":{"kind":"state_change"},
                "sink":{"kind":"file","mode":"replace"}
            })),
            "Current stage · enabled · state change → file (replace)"
        );
    }
    #[test]
    fn region_move_and_resize_remain_normalized() {
        let origin = NormalizedRegion {
            x: 0.8,
            y: 0.8,
            width: 0.2,
            height: 0.2,
        };
        assert!((move_region(origin, egui::vec2(0.5, 0.5)).x - 0.8).abs() < f32::EPSILON);
        let resized = resize_region(origin, egui::vec2(0.5, 0.5));
        assert!((resized.x + resized.width - 1.0).abs() < f32::EPSILON);
    }
    #[test]
    fn pointer_drawing_respects_zoom_and_pan() {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let region = region_from_points(
            canvas,
            2.0,
            egui::vec2(10.0, 0.0),
            egui::pos2(10.0, 0.0),
            egui::pos2(210.0, 100.0),
        )
        .unwrap();
        assert_eq!(
            region,
            NormalizedRegion {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5
            }
        );
    }

    #[test]
    fn ordinary_click_hit_testing_selects_topmost_region() {
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
        let elements = vec![
            default_element(NormalizedRegion {
                x: 0.1,
                y: 0.1,
                width: 0.5,
                height: 0.5,
            }),
            default_element(NormalizedRegion {
                x: 0.2,
                y: 0.2,
                width: 0.5,
                height: 0.5,
            }),
        ];
        assert_eq!(
            region_at_position(
                &elements,
                canvas,
                1.0,
                egui::Vec2::ZERO,
                egui::pos2(30.0, 30.0)
            ),
            Some(1)
        );
        assert_eq!(
            region_at_position(
                &elements,
                canvas,
                1.0,
                egui::Vec2::ZERO,
                egui::pos2(90.0, 90.0)
            ),
            None
        );
    }

    #[test]
    fn live_observations_use_profile_zone_names() {
        let mut profile = Profile::new("Game", "game", 1920, 1080);
        let mut element = default_element(NormalizedRegion {
            x: 0.0,
            y: 0.0,
            width: 0.1,
            height: 0.1,
        });
        element.name = "Stage".into();
        let element_id = element.id;
        profile.elements.push(element);
        let observations = json!({
            (element_id.to_string()): {
                "status": "valid",
                "value": {"type":"string","value":"STAGE-1"},
                "confidence": 0.9,
                "diagnostic": "recognized"
            }
        });

        let rendered = display_observations(&observations, Some(&profile));
        assert!(
            rendered.starts_with("Stage · color bar\nstatus: valid"),
            "{rendered}"
        );
        assert!(rendered.contains("STAGE-1"));
        assert!(!rendered.contains(&element_id.to_string()));
    }

    #[test]
    fn live_observations_compose_structured_stage_name() {
        let mut profile = Profile::new("Game", "game", 1920, 1080);
        let mut group = default_element(NormalizedRegion {
            x: 0.0,
            y: 0.0,
            width: 0.1,
            height: 0.1,
        });
        group.name = "Stage group".into();
        let group_id = group.id;
        let mut counter = group.clone();
        counter.id = ElementId::new();
        counter.name = "Stage counter".into();
        let counter_id = counter.id;
        profile.elements.extend([group, counter]);
        assert!(ensure_stage_derived(&mut profile));
        let derived_id = profile.derived_observations[0].id;
        let observations = json!({
            (group_id.to_string()): {"status":"valid","value":{"type":"text","value":"2"}},
            (counter_id.to_string()): {"status":"valid","value":{"type":"text","value":"10"}},
            (derived_id.to_string()): {"status":"valid","value":{"type":"text","value":"STAGE-2 : 10"},"confidence":0.9,"diagnostic":"composed from 2 inputs"}
        });

        let rendered = display_observations(&observations, Some(&profile));
        assert!(rendered.contains("Stage · structured text\nstatus: valid"));
        assert!(rendered.contains("STAGE-2 : 10"));
        assert!(!rendered.contains("Stage group ·"), "{rendered}");
        assert!(!rendered.contains("Stage counter ·"), "{rendered}");
    }

    #[test]
    fn revision_comparison_uses_stable_zone_identity() {
        let mut old = Profile::new("Game", "game", 1920, 1080);
        let mut element = default_element(NormalizedRegion {
            x: 0.0,
            y: 0.0,
            width: 0.1,
            height: 0.1,
        });
        element.name = "Stage".into();
        old.elements.push(element);
        let mut current = old.clone();
        current.elements[0].name = "Stage counter".into();
        current.elements.push(default_element(NormalizedRegion {
            x: 0.2,
            y: 0.0,
            width: 0.1,
            height: 0.1,
        }));

        let comparison = profile_comparison(&old, &current);
        assert!(comparison.contains("Zone changed: Stage"));
        assert!(comparison.contains("Zone added: Region"));
    }
}
