//! Durable event JSONL and atomically replaced current-state outputs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

mod diagnostic;
mod routing;
pub use diagnostic::{
    export_diagnostic_bundle, plan_diagnostic_bundle, DiagnosticBundle, DiagnosticCrop,
    DiagnosticError, DiagnosticLimits, DiagnosticPlan,
};
pub use routing::{
    execute_route, render_payload, FileMode, OutputFormat, OutputRecipe, OutputRecipeSink,
    OutputRecipeSource, OutputRoute, OutputRouteError, OutputSink, OutputTrigger, RouteContext,
    RouteReceipt,
};

/// Current event and state output schema version.
pub const OUTPUT_SCHEMA_VERSION: u16 = 1;

/// Externally stable meaningful transition record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventRecord {
    pub schema: u16,
    pub daemon_instance: Uuid,
    pub sequence: u64,
    pub timestamp: String,
    pub profile_id: String,
    pub game: String,
    pub event: String,
    pub state: EventState,
    pub value: Value,
    pub confidence: f64,
}

impl EventRecord {
    #[must_use]
    pub fn timestamp_rfc3339(timestamp: DateTime<Utc>) -> String {
        timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventState {
    Entered,
    Updated,
    Left,
}

/// Atomically replaced latest daemon state.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StateSnapshot {
    pub schema: u16,
    pub daemon_instance: Uuid,
    pub sequence: u64,
    pub updated_at: String,
    pub capture: Value,
    pub active_profile: Option<String>,
    pub observations: Value,
    pub events: Value,
}

/// Transition-log durability configuration.
#[derive(Clone, Copy, Debug)]
pub struct OutputConfig {
    pub flush_every_transitions: u32,
    pub rotate_after_bytes: u64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            flush_every_transitions: 1,
            rotate_after_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Stateful append-only JSONL writer and atomic state publisher.
#[derive(Debug)]
pub struct OutputWriter {
    root: PathBuf,
    config: OutputConfig,
    event_writer: Option<BufWriter<File>>,
    transitions_since_flush: u32,
}

impl OutputWriter {
    /// Creates an output sink without performing I/O until its first write.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, config: OutputConfig) -> Self {
        Self {
            root: root.into(),
            config,
            event_writer: None,
            transitions_since_flush: 0,
        }
    }

    /// Appends exactly one compact JSON object and applies the configured flush/rotation policy.
    ///
    /// # Errors
    ///
    /// Returns serialization or I/O failures for the daemon to surface without stopping capture.
    pub fn append_event(&mut self, event: &EventRecord) -> Result<(), OutputError> {
        self.ensure_event_writer()?;
        let writer = self
            .event_writer
            .as_mut()
            .ok_or(OutputError::WriterUnavailable)?;
        serde_json::to_writer(&mut *writer, event)?;
        writer.write_all(b"\n")?;
        self.transitions_since_flush = self.transitions_since_flush.saturating_add(1);
        if self.config.flush_every_transitions > 0
            && self.transitions_since_flush >= self.config.flush_every_transitions
        {
            writer.flush()?;
            self.transitions_since_flush = 0;
        }
        Ok(())
    }

    /// Atomically publishes current state using same-directory flush, sync, and rename.
    ///
    /// # Errors
    ///
    /// Returns serialization or I/O failures while retaining the previous complete snapshot.
    pub fn write_state(&self, state: &StateSnapshot) -> Result<(), OutputError> {
        let bytes = serde_json::to_vec_pretty(state)?;
        atomic_write_before_rename(&self.root.join("state.json"), &bytes, || Ok(()))?;
        Ok(())
    }

    fn ensure_event_writer(&mut self) -> Result<(), OutputError> {
        if self.event_writer.is_some() {
            return Ok(());
        }
        fs::create_dir_all(&self.root)?;
        let path = self.root.join("events.jsonl");
        if self.config.rotate_after_bytes > 0
            && path
                .metadata()
                .is_ok_and(|metadata| metadata.len() >= self.config.rotate_after_bytes)
        {
            let rotated = self.root.join("events.jsonl.1");
            if rotated.exists() {
                fs::remove_file(&rotated)?;
            }
            fs::rename(&path, rotated)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.event_writer = Some(BufWriter::new(file));
        Ok(())
    }
}

fn atomic_write_before_rename(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".state.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        before_rename()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Durable output failure deliberately returned to orchestration rather than panicking.
#[derive(Debug, Error)]
pub enum OutputError {
    #[error("output I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("output JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event writer was not initialized")]
    WriterUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn event(sequence: u64) -> EventRecord {
        let timestamp = Utc.with_ymd_and_hms(2026, 7, 11, 16, 43, 27).unwrap();
        EventRecord {
            schema: 1,
            daemon_instance: Uuid::nil(),
            sequence,
            timestamp: EventRecord::timestamp_rfc3339(timestamp),
            profile_id: "profile".into(),
            game: "synthetic_game".into(),
            event: "critical_health".into(),
            state: EventState::Entered,
            value: serde_json::json!(0.17),
            confidence: 0.91,
        }
    }

    #[test]
    fn transition_is_one_compact_jsonl_record_with_golden_contract() {
        let directory = tempfile::tempdir().unwrap();
        let mut writer = OutputWriter::new(directory.path(), OutputConfig::default());
        writer.append_event(&event(106)).unwrap();
        let contents = fs::read_to_string(directory.path().join("events.jsonl")).unwrap();
        assert_eq!(
            contents,
            concat!(
                r#"{"schema":1,"daemon_instance":"00000000-0000-0000-0000-000000000000","sequence":106,"timestamp":"2026-07-11T16:43:27.000Z","profile_id":"profile","game":"synthetic_game","event":"critical_health","state":"entered","value":0.17,"confidence":0.91}"#,
                "\n"
            )
        );
    }

    #[test]
    fn updated_transition_has_a_stable_external_state() {
        let directory = tempfile::tempdir().unwrap();
        let mut record = event(107);
        record.state = EventState::Updated;
        let mut writer = OutputWriter::new(directory.path(), OutputConfig::default());
        writer.append_event(&record).unwrap();
        let contents = fs::read_to_string(directory.path().join("events.jsonl")).unwrap();
        assert!(contents.contains("\"state\":\"updated\""));
    }

    #[test]
    fn interrupted_state_write_retains_previous_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        atomic_write_before_rename(&path, b"old", || Ok(())).unwrap();
        assert!(
            atomic_write_before_rename(&path, b"new", || Err(io::Error::other("injected")))
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"old");
    }

    #[test]
    fn output_failure_is_returned_not_panicked() {
        let directory = tempfile::tempdir().unwrap();
        let root_as_file = directory.path().join("not-a-directory");
        fs::write(&root_as_file, b"file").unwrap();
        let mut writer = OutputWriter::new(root_as_file, OutputConfig::default());
        assert!(matches!(
            writer.append_event(&event(1)),
            Err(OutputError::Io(_))
        ));
    }
}
