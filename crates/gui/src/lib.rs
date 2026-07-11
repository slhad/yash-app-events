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
    BarDirection, Detector, DetectorId, Element, ElementId, EventRule, NormalizedRegion,
    PreprocessOperation, Profile, ProfileStore, RuleId,
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
    PreviewStart,
    PreviewStop,
    PreviewFrame,
    DetectorTest,
    TemplateCapture,
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
                    while let Some(request) = request_receiver.recv().await {
                        if client.is_none() {
                            match UnixRpcClient::connect(
                                &socket,
                                Duration::from_secs(3),
                                "yash-app-events-gui",
                                env!("CARGO_PKG_VERSION"),
                            )
                            .await
                            {
                                Ok(connected) => client = Some(connected),
                                Err(error) => {
                                    let _ = response_sender.send(Response {
                                        kind: request.kind,
                                        payload: Payload::Error(error.to_string()),
                                    });
                                    continue;
                                }
                            }
                        }
                        let result = client
                            .as_mut()
                            .expect("client was initialized")
                            .call(request.method, request.params)
                            .await;
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
    preview_enabled: bool,
    frozen: bool,
    preview_pending: bool,
    preview_texture: Option<egui::TextureHandle>,
    last_preview: Instant,
    last_refresh: Instant,
    zoom: f32,
    pan: egui::Vec2,
    selected_region: Option<usize>,
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
            preview_enabled: false,
            frozen: false,
            preview_pending: false,
            preview_texture: None,
            last_preview: Instant::now(),
            last_refresh: Instant::now(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            selected_region: None,
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
        }
    }

    fn drain(&mut self, context: &egui::Context) {
        while let Ok(response) = self.worker.responses.try_recv() {
            match response.payload {
                Payload::Error(error) => {
                    self.error = Some(error);
                    self.preview_pending = false;
                }
                Payload::Preview(image) => {
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
                            "test · {} · value={} confidence={}",
                            observation["status"].as_str().unwrap_or("?"),
                            observation["value"],
                            observation["confidence"]
                        ));
                    }
                }
                Payload::Json(value) => match response.kind {
                    RequestKind::Profiles => self.apply_profiles(value),
                    RequestKind::Status => self.status = value,
                    RequestKind::State => {
                        let sequence = value["sequence"].as_u64().unwrap_or(0);
                        if sequence > self.last_state_sequence {
                            self.push_timeline(format!(
                                "transition seq {sequence} · {}",
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
                        self.worker
                            .send(RequestKind::Profiles, method::PROFILE_LIST, Value::Null);
                    }
                    RequestKind::Draft => self.draft_saved = true,
                    RequestKind::Mutation | RequestKind::Create => {
                        self.worker
                            .send(RequestKind::Profiles, method::PROFILE_LIST, Value::Null);
                    }
                    RequestKind::Capture => self.capture_status = value,
                    RequestKind::PreviewStart => {
                        self.preview_enabled = value["enabled"].as_bool().unwrap_or(false);
                    }
                    RequestKind::PreviewStop => {
                        self.preview_enabled = false;
                        self.preview_pending = false;
                    }
                    RequestKind::PreviewFrame => {}
                    RequestKind::DetectorTest => {
                        self.detector_result = value;
                        self.test_pending = false;
                    }
                    RequestKind::TemplateCapture => {
                        if let Some(path) = value["path"].as_str() {
                            self.add_template_path(path.into());
                        }
                    }
                },
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
                }
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

    fn schedule(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.worker
                .send(RequestKind::Status, method::STATUS, Value::Null);
            self.worker
                .send(RequestKind::State, method::STATE_GET, Value::Null);
            self.worker
                .send(RequestKind::Capture, method::CAPTURE_STATUS, Value::Null);
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
                Value::Null,
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
    }

    fn request_detector_test(&mut self) {
        if let (Some(profile), Some(index)) = (&self.draft, self.selected_region) {
            if let Some(element) = profile.elements.get(index) {
                self.worker.send(
                    RequestKind::DetectorTest,
                    method::DETECTOR_TEST,
                    json!({"profile_id":profile.id,"element_id":element.id,"use_frozen":self.frozen}),
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
            if let Some(index) = select_index { self.selected=Some(index); self.draft=Some(self.profiles[index].clone()); self.dirty_since=None; self.selected_region=None; }
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
            }
        });
    }

    fn topbar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("capture").show(context, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Select source").clicked() {
                    let profile_id = self.draft.as_ref().map(|profile| profile.id);
                    self.worker.send(
                        RequestKind::Capture,
                        method::CAPTURE_SELECT,
                        json!({"source":"window_or_monitor","profile_id":profile_id}),
                    );
                }
                if ui.button("Stop").clicked() {
                    self.worker
                        .send(RequestKind::Capture, method::CAPTURE_STOP, Value::Null);
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

    #[allow(clippy::too_many_lines)]
    fn detector_editor(&mut self, ui: &mut egui::Ui, profile: &mut Profile, index: usize) {
        ui.separator();
        ui.heading("Detector and event");
        let current = match &profile.elements[index].detector {
            Detector::ColorBar { .. } => "Color bar",
            Detector::Template { .. } => "Template",
            Detector::RegionChange { .. } => "Region change",
        };
        let mut chosen = current;
        egui::ComboBox::from_label("Detector type")
            .selected_text(chosen)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, "Color bar", "Color bar");
                ui.selectable_value(&mut chosen, "Template", "Template");
                ui.selectable_value(&mut chosen, "Region change", "Region change");
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
            });
            self.mark_dirty();
        }
        if let Some(rule_index) = rule_index {
            let rule = &mut profile.rules[rule_index];
            let mut rule_changed = false;
            ui.horizontal(|ui| {
                ui.label("Event name");
                rule_changed |= ui.text_edit_singleline(&mut rule.event).changed();
            });
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
            });
            rule.sample_window = rule.sample_window.max(rule.required_samples);
            ui.label(format!(
                "state={} · hysteresis {:.3} · evidence {} of {} · cooldown {} ms",
                self.state["events"][&rule.event],
                rule.leave_above - rule.enter_below,
                rule.required_samples,
                rule.sample_window,
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
        if response.drag_started() {
            if let Some(position) = pointer_position {
                self.selected_region = profile
                    .elements
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, element)| {
                        region_rect(canvas, self.zoom, self.pan, element.region).contains(position)
                    })
                    .map(|(index, _)| index);
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
        self.sidebar(context);
        self.editor(context);
        context.request_repaint_after(Duration::from_millis(50));
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
}
