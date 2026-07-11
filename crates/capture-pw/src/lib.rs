//! Linux XDG Desktop Portal and direct `PipeWire` capture backend.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode,
};
use futures_util::StreamExt as _;
use pipewire as pw;
use pw::{properties::properties, spa};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use yash_app_events_capture::{Frame, FrameLayout, LatestFrameSlot, PixelFormat};

/// Backend name reported through status and diagnostics.
pub const BACKEND_NAME: &str = "xdg-desktop-portal-pipewire";

/// User-selectable portal source categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceSelection {
    Monitor,
    Window,
    MonitorOrWindow,
}

/// Portal selection and restoration options.
#[derive(Clone, Debug)]
pub struct PortalOptions {
    pub sources: SourceSelection,
    pub restore_token: Option<String>,
    pub persist_restore: bool,
}

impl Default for PortalOptions {
    fn default() -> Self {
        Self {
            sources: SourceSelection::MonitorOrWindow,
            restore_token: None,
            persist_restore: true,
        }
    }
}

/// Selected source metadata returned before frames begin arriving.
#[derive(Clone, Debug)]
pub struct SelectedSource {
    pub pipewire_node_id: u32,
    pub label: String,
    pub restore_token: Option<String>,
}

/// Live metrics updated from the nonblocking `PipeWire` process callback.
#[derive(Clone, Debug, Default)]
pub struct CaptureMetrics {
    pub input_frames: u64,
    pub replaced_frames: u64,
    pub input_fps: f32,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pixel_format: Option<&'static str>,
    pub last_frame_age: Option<Duration>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
struct MetricState {
    metrics: CaptureMetrics,
    first_frame: Option<Instant>,
    last_frame: Option<Instant>,
}

/// An active portal session. Dropping it requests prompt resource release.
#[derive(Debug)]
pub struct PortalCapture {
    selected: SelectedSource,
    stop: mpsc::Sender<()>,
    task: Option<tokio::task::JoinHandle<Result<(), CaptureError>>>,
    metrics: Arc<Mutex<MetricState>>,
}

impl PortalCapture {
    /// Opens the source picker (or restores a source), then starts direct `PipeWire` capture.
    ///
    /// # Errors
    ///
    /// Returns actionable portal cancellation/denial/session/stream errors.
    pub async fn start(
        options: PortalOptions,
        slot: LatestFrameSlot,
    ) -> Result<Self, CaptureError> {
        let (stop_sender, stop_receiver) = mpsc::channel(1);
        let (ready_sender, ready_receiver) = oneshot::channel();
        let metrics = Arc::new(Mutex::new(MetricState::default()));
        let task_metrics = Arc::clone(&metrics);
        let mut task = tokio::spawn(portal_task(
            options,
            slot,
            task_metrics,
            stop_receiver,
            ready_sender,
        ));
        let selected = tokio::select! {
            ready = ready_receiver => ready.map_err(|_| CaptureError::SessionEnded)?,
            result = &mut task => match result.map_err(CaptureError::Join)? {
                Ok(()) => return Err(CaptureError::SessionEnded),
                Err(error) => return Err(error),
            },
        };
        Ok(Self {
            selected,
            stop: stop_sender,
            task: Some(task),
            metrics,
        })
    }

    #[must_use]
    pub fn selected_source(&self) -> &SelectedSource {
        &self.selected
    }

    #[must_use]
    pub fn metrics(&self) -> CaptureMetrics {
        let mut state = self
            .metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.metrics.last_frame_age = state.last_frame.map(|last| last.elapsed());
        state.metrics.clone()
    }

    /// Stops capture and waits for the portal and `PipeWire` resources to close.
    pub async fn stop(mut self) {
        let _ = self.stop.send(()).await;
        if let Some(task) = self.task.take() {
            if let Ok(Err(error)) = task.await {
                tracing::error!(%error, "portal capture ended while stopping");
            }
        }
    }
}

impl Drop for PortalCapture {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

async fn portal_task(
    options: PortalOptions,
    slot: LatestFrameSlot,
    metrics: Arc<Mutex<MetricState>>,
    mut stop: mpsc::Receiver<()>,
    ready: oneshot::Sender<SelectedSource>,
) -> Result<(), CaptureError> {
    let proxy = Screencast::new().await.map_err(classify_portal_error)?;
    let sources = match options.sources {
        SourceSelection::Monitor => SourceType::Monitor.into(),
        SourceSelection::Window => SourceType::Window.into(),
        SourceSelection::MonitorOrWindow => SourceType::Monitor | SourceType::Window,
    };
    let mut restore_token = options.restore_token.as_deref();
    let (session, response) = loop {
        let session = proxy
            .create_session()
            .await
            .map_err(classify_portal_error)?;
        let selected = proxy
            .select_sources(
                &session,
                CursorMode::Hidden,
                sources,
                false,
                restore_token,
                if options.persist_restore {
                    PersistMode::ExplicitlyRevoked
                } else {
                    PersistMode::DoNot
                },
            )
            .await
            .map_err(classify_portal_error);
        if let Err(error) = selected {
            if restore_token.take().is_some() && should_retry_without_restore(&error) {
                continue;
            }
            return Err(error);
        }
        let started = match proxy
            .start(&session, None)
            .await
            .map_err(classify_portal_error)
        {
            Ok(request) => request.response().map_err(classify_portal_error),
            Err(error) => Err(error),
        };
        match started {
            Ok(response) => break (session, response),
            Err(error)
                if restore_token.take().is_some() && should_retry_without_restore(&error) => {}
            Err(error) => return Err(error),
        }
    };
    let stream = response
        .streams()
        .first()
        .ok_or(CaptureError::NoSource)?
        .to_owned();
    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(classify_portal_error)?;
    let selected = SelectedSource {
        pipewire_node_id: stream.pipe_wire_node_id(),
        label: format!("portal-node-{}", stream.pipe_wire_node_id()),
        restore_token: response.restore_token().map(ToOwned::to_owned),
    };
    let (pw_stop, pw_receiver) = pw::channel::channel();
    let node_id = selected.pipewire_node_id;
    let thread = std::thread::Builder::new()
        .name("yash-pipewire-capture".into())
        .spawn(move || run_pipewire(node_id, fd, slot, metrics, pw_receiver))
        .map_err(CaptureError::Thread)?;
    if ready.send(selected).is_err() {
        let _ = pw_stop.send(());
        join_pipewire(thread).await?;
        return Ok(());
    }
    let mut closed = session
        .receive_closed()
        .await
        .map_err(classify_portal_error)?;
    tokio::select! {
        _ = stop.recv() => { let _ = session.close().await; }
        _ = closed.next() => {}
    }
    let _ = pw_stop.send(());
    join_pipewire(thread).await
}

async fn join_pipewire(thread: JoinHandle<Result<(), CaptureError>>) -> Result<(), CaptureError> {
    tokio::task::spawn_blocking(move || {
        thread
            .join()
            .map_err(|_| CaptureError::PipeWireThreadPanicked)?
    })
    .await
    .map_err(CaptureError::Join)?
}

#[derive(Debug)]
struct UserData {
    format: spa::param::video::VideoInfoRaw,
    slot: LatestFrameSlot,
    metrics: Arc<Mutex<MetricState>>,
    origin: Instant,
    sequence: u64,
}

fn run_pipewire(
    node_id: u32,
    fd: OwnedFd,
    slot: LatestFrameSlot,
    metrics: Arc<Mutex<MetricState>>,
    stop: pw::channel::Receiver<()>,
) -> Result<(), CaptureError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let stream = pw::stream::StreamRc::new(
        core,
        "yash-app-events",
        properties! { *pw::keys::MEDIA_TYPE => "Video", *pw::keys::MEDIA_CATEGORY => "Capture", *pw::keys::MEDIA_ROLE => "Screen" },
    )?;
    let data = UserData {
        format: spa::param::video::VideoInfoRaw::default(),
        slot,
        metrics,
        origin: Instant::now(),
        sequence: 0,
    };
    let loop_for_stop = mainloop.clone();
    let _stop_listener = stop.attach(mainloop.loop_(), move |()| loop_for_stop.quit());
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, user, _, new| {
            if let pw::stream::StreamState::Error(message) = new {
                set_metric_error(&user.metrics, format!("PipeWire stream error: {message}"));
            }
        })
        .param_changed(|_, user, id, param| parse_video_format(user, id, param))
        .process(process_buffer)
        .register()?;
    let values = video_format_parameters()?;
    let mut params = [spa::pod::Pod::from_bytes(&values).ok_or(CaptureError::FormatPod)?];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    mainloop.run();
    Ok(())
}

fn parse_video_format(user: &mut UserData, id: u32, param: Option<&spa::pod::Pod>) {
    let Some(param) = param else {
        return;
    };
    if id != spa::param::ParamType::Format.as_raw() {
        return;
    }
    if user.format.parse(param).is_err() {
        set_metric_error(
            &user.metrics,
            "failed to parse negotiated video format".into(),
        );
        return;
    }
    let format = user.format.format();
    let supported = matches!(
        format,
        spa::param::video::VideoFormat::RGB
            | spa::param::video::VideoFormat::RGBA
            | spa::param::video::VideoFormat::RGBx
    );
    if !supported {
        set_metric_error(
            &user.metrics,
            format!("unsupported negotiated pixel format: {format:?}"),
        );
    }
}

fn process_buffer(stream: &pw::stream::Stream, user: &mut UserData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let stride = usize::try_from(data.chunk().stride().unsigned_abs()).unwrap_or(0);
    let offset = usize::try_from(data.chunk().offset()).unwrap_or(0);
    let chunk_size = usize::try_from(data.chunk().size()).unwrap_or(0);
    let Some(bytes) = data.data() else {
        return;
    };
    let width = user.format.size().width;
    let height = user.format.size().height;
    let format = user.format.format();
    let (frame, label) = match frame_from_buffer(
        user.sequence,
        user.origin.elapsed(),
        width,
        height,
        format,
        stride,
        offset,
        chunk_size,
        bytes,
    ) {
        Ok(result) => result,
        Err(error) => {
            set_metric_error(&user.metrics, error);
            return;
        }
    };
    user.sequence = user.sequence.saturating_add(1);
    let replaced = user.slot.publish(frame);
    update_metrics(&user.metrics, width, height, label, replaced);
}

#[allow(clippy::too_many_arguments)]
fn frame_from_buffer(
    sequence: u64,
    timestamp: Duration,
    width: u32,
    height: u32,
    format: spa::param::video::VideoFormat,
    negotiated_stride: usize,
    offset: usize,
    chunk_size: usize,
    bytes: &[u8],
) -> Result<(Arc<Frame>, &'static str), String> {
    let (pixel_format, bytes_per_pixel, label, force_opaque_alpha) = match format {
        spa::param::video::VideoFormat::RGB => (PixelFormat::Rgb8, 3_usize, "rgb8", false),
        spa::param::video::VideoFormat::RGBA => (PixelFormat::Rgba8, 4_usize, "rgba8", false),
        spa::param::video::VideoFormat::RGBx => (PixelFormat::Rgba8, 4_usize, "rgba8", true),
        other => return Err(format!("unsupported negotiated pixel format: {other:?}")),
    };
    let packed = usize::try_from(width)
        .unwrap_or(0)
        .saturating_mul(bytes_per_pixel);
    let stride = negotiated_stride.max(packed);
    let required = stride.saturating_mul(usize::try_from(height).unwrap_or(0));
    let available = chunk_size.min(bytes.len().saturating_sub(offset));
    if required == 0 || available < required {
        return Err(format!(
            "short PipeWire frame: required {required} bytes, got {available}"
        ));
    }
    let mut frame_bytes = bytes[offset..offset + required].to_vec();
    if force_opaque_alpha {
        for row in 0..usize::try_from(height).unwrap_or(0) {
            let row_start = row.saturating_mul(stride);
            for pixel in
                frame_bytes[row_start..row_start.saturating_add(packed)].chunks_exact_mut(4)
            {
                pixel[3] = 255;
            }
        }
    }
    Frame::new(
        sequence,
        timestamp,
        FrameLayout {
            width,
            height,
            row_stride: stride,
            format: pixel_format,
        },
        Some("pipewire-node".to_owned()),
        Arc::from(frame_bytes),
    )
    .map(|frame| (Arc::new(frame), label))
    .map_err(|error| error.to_string())
}

fn video_format_parameters() -> Result<Vec<u8>, CaptureError> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::RGB,
            spa::param::video::VideoFormat::RGB,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::RGBx
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 16384,
                height: 16384
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 240, denom: 1 }
        ),
    );
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(object),
    )
    .map(|result| result.0.into_inner())
    .map_err(|error| CaptureError::PodSerialize(error.to_string()))
}

#[allow(clippy::cast_precision_loss)]
fn update_metrics(
    metrics: &Mutex<MetricState>,
    width: u32,
    height: u32,
    format: &'static str,
    replaced: bool,
) {
    let now = Instant::now();
    let mut state = metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let first = *state.first_frame.get_or_insert(now);
    state.last_frame = Some(now);
    state.metrics.input_frames = state.metrics.input_frames.saturating_add(1);
    state.metrics.replaced_frames = state
        .metrics
        .replaced_frames
        .saturating_add(u64::from(replaced));
    state.metrics.input_fps = state.metrics.input_frames as f32
        / now
            .saturating_duration_since(first)
            .as_secs_f32()
            .max(0.001);
    state.metrics.width = Some(width);
    state.metrics.height = Some(height);
    state.metrics.pixel_format = Some(format);
}

fn set_metric_error(metrics: &Mutex<MetricState>, error: String) {
    metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .metrics
        .error = Some(error);
}

fn classify_portal_error(error: ashpd::Error) -> CaptureError {
    let message = error.to_string();
    drop(error);
    classify_portal_message(message)
}

fn classify_portal_message(message: String) -> CaptureError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("cancel") {
        CaptureError::Cancelled(message)
    } else if lower.contains("denied") || lower.contains("permission") {
        CaptureError::Denied(message)
    } else {
        CaptureError::Portal(message)
    }
}

fn should_retry_without_restore(error: &CaptureError) -> bool {
    matches!(error, CaptureError::Portal(_))
}

/// Actionable capture lifecycle failure.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("screen selection was cancelled: {0}")]
    Cancelled(String),
    #[error("screen capture permission was denied: {0}")]
    Denied(String),
    #[error("desktop portal failed: {0}")]
    Portal(String),
    #[error("the portal returned no selected source")]
    NoSource,
    #[error("the portal capture session ended before becoming ready")]
    SessionEnded,
    #[error("PipeWire failed: {0}")]
    PipeWire(#[from] pw::Error),
    #[error("failed to spawn capture thread: {0}")]
    Thread(#[source] std::io::Error),
    #[error("PipeWire capture thread panicked")]
    PipeWireThreadPanicked,
    #[error("capture join task failed: {0}")]
    Join(#[source] tokio::task::JoinError),
    #[error("failed to construct PipeWire format pod")]
    FormatPod,
    #[error("failed to serialize PipeWire format pod: {0}")]
    PodSerialize(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_pod_requests_only_supported_packed_rgb_formats() {
        let bytes = video_format_parameters().unwrap();
        assert!(!bytes.is_empty());
        assert!(spa::pod::Pod::from_bytes(&bytes).is_some());
    }

    #[test]
    fn padded_chunk_is_copied_with_offset_and_stride() {
        let mut bytes = vec![99_u8; 4];
        bytes.extend(0_u8..24);
        bytes.extend([88_u8; 4]);
        let (frame, label) = frame_from_buffer(
            7,
            Duration::from_millis(10),
            2,
            2,
            spa::param::video::VideoFormat::RGBA,
            12,
            4,
            24,
            &bytes,
        )
        .unwrap();
        assert_eq!(label, "rgba8");
        assert_eq!(frame.sequence, 7);
        assert_eq!(frame.row_stride, 12);
        assert_eq!(frame.format, PixelFormat::Rgba8);
        assert_eq!(frame.data.len(), 24);
        assert_eq!(&frame.data[..3], &[0, 1, 2]);
    }

    #[test]
    fn unsupported_and_short_buffers_are_actionable_not_frames() {
        let unsupported = frame_from_buffer(
            0,
            Duration::ZERO,
            1,
            1,
            spa::param::video::VideoFormat::BGRx,
            4,
            0,
            4,
            &[0; 4],
        )
        .unwrap_err();
        assert!(unsupported.contains("unsupported negotiated pixel format"));
        let short = frame_from_buffer(
            0,
            Duration::ZERO,
            2,
            2,
            spa::param::video::VideoFormat::RGB,
            8,
            0,
            8,
            &[0; 8],
        )
        .unwrap_err();
        assert!(short.contains("short PipeWire frame"));
    }

    #[test]
    fn rgbx_padding_is_normalized_to_opaque_rgba() {
        let (frame, _) = frame_from_buffer(
            0,
            Duration::ZERO,
            2,
            1,
            spa::param::video::VideoFormat::RGBx,
            8,
            0,
            8,
            &[1, 2, 3, 0, 4, 5, 6, 17],
        )
        .unwrap();
        assert_eq!(&*frame.data, &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn metrics_track_replacement_rate_resolution_and_age() {
        let metrics = Mutex::new(MetricState::default());
        update_metrics(&metrics, 1920, 1080, "rgba8", false);
        update_metrics(&metrics, 1920, 1080, "rgba8", true);
        let state = metrics.lock().unwrap();
        assert_eq!(state.metrics.input_frames, 2);
        assert_eq!(state.metrics.replaced_frames, 1);
        assert_eq!(state.metrics.width, Some(1920));
        assert_eq!(state.metrics.pixel_format, Some("rgba8"));
        assert!(state.metrics.input_fps > 0.0);
    }

    #[test]
    fn portal_errors_distinguish_user_action_and_restore_fallback() {
        let cancelled = classify_portal_message("request cancelled by user".into());
        let denied = classify_portal_message("permission denied".into());
        let stale = classify_portal_message("restore token is no longer valid".into());
        assert!(matches!(cancelled, CaptureError::Cancelled(_)));
        assert!(matches!(denied, CaptureError::Denied(_)));
        assert!(should_retry_without_restore(&stale));
        assert!(!should_retry_without_restore(&cancelled));
    }
}
