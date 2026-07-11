use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(Clone, Debug)]
pub struct DiagnosticBundle {
    pub logs: Vec<Value>,
    pub configuration: Value,
    pub metrics: Value,
    /// Crops are included only after the caller's explicit selection.
    pub selected_crops: Vec<DiagnosticCrop>,
}

#[derive(Clone, Debug)]
pub struct DiagnosticCrop {
    pub name: String,
    pub png: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct DiagnosticLimits {
    pub maximum_logs: usize,
    pub maximum_crops: usize,
    pub maximum_file_bytes: usize,
    pub maximum_total_bytes: usize,
}

impl Default for DiagnosticLimits {
    fn default() -> Self {
        Self {
            maximum_logs: 1_000,
            maximum_crops: 8,
            maximum_file_bytes: 4 * 1024 * 1024,
            maximum_total_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticPlan {
    pub entries: Vec<DiagnosticPlanEntry>,
    pub total_uncompressed_bytes: usize,
    pub privacy_warning: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticPlanEntry {
    pub path: String,
    pub bytes: usize,
    pub user_selected_image: bool,
}

/// Returns the exact redacted file list shown before an export is authorized.
///
/// # Errors
///
/// Rejects unsafe names, non-PNG crops, or configured resource-limit violations.
pub fn plan_diagnostic_bundle(
    bundle: &DiagnosticBundle,
    limits: DiagnosticLimits,
) -> Result<DiagnosticPlan, DiagnosticError> {
    let entries = prepared_entries(bundle, limits)?;
    Ok(DiagnosticPlan {
        total_uncompressed_bytes: entries.iter().map(|(_, bytes, _)| bytes.len()).sum(),
        entries: entries
            .into_iter()
            .map(|(path, bytes, user_selected_image)| DiagnosticPlanEntry {
                path,
                bytes: bytes.len(),
                user_selected_image,
            })
            .collect(),
        privacy_warning: "Review every entry. Selected crops may contain private on-screen information; full screenshots, portal tokens, capture bindings, and secrets are excluded by default.".into(),
    })
}

/// Atomically writes the previously reviewable, redacted diagnostic archive.
///
/// # Errors
///
/// Rejects invalid inputs and leaves an existing destination unchanged on failure.
pub fn export_diagnostic_bundle(
    destination: &Path,
    bundle: &DiagnosticBundle,
    limits: DiagnosticLimits,
) -> Result<DiagnosticPlan, DiagnosticError> {
    let plan = plan_diagnostic_bundle(bundle, limits)?;
    let entries = prepared_entries(bundle, limits)?;
    let parent = destination.parent().ok_or(DiagnosticError::UnsafePath)?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".diagnostic.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o600);
        for (path, bytes, _) in entries {
            writer.start_file(path, options)?;
            writer.write_all(&bytes)?;
        }
        let file = writer.finish()?;
        file.sync_all()?;
        fs::rename(&temporary, destination)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map(|()| plan)
}

type PreparedEntry = (String, Vec<u8>, bool);

fn prepared_entries(
    bundle: &DiagnosticBundle,
    limits: DiagnosticLimits,
) -> Result<Vec<PreparedEntry>, DiagnosticError> {
    if bundle.logs.len() > limits.maximum_logs || bundle.selected_crops.len() > limits.maximum_crops
    {
        return Err(DiagnosticError::LimitExceeded);
    }
    let redacted_logs: Vec<_> = bundle.logs.iter().cloned().map(redact).collect();
    let mut entries = vec![
        (
            "logs.json".into(),
            serde_json::to_vec_pretty(&redacted_logs)?,
            false,
        ),
        (
            "configuration.json".into(),
            serde_json::to_vec_pretty(&redact(bundle.configuration.clone()))?,
            false,
        ),
        (
            "metrics.json".into(),
            serde_json::to_vec_pretty(&redact(bundle.metrics.clone()))?,
            false,
        ),
    ];
    for crop in &bundle.selected_crops {
        if crop.name.is_empty()
            || crop.name.len() > 64
            || !crop
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !crop.png.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
        {
            return Err(DiagnosticError::InvalidCrop);
        }
        entries.push((format!("crops/{}.png", crop.name), crop.png.clone(), true));
    }
    let mut total = 0_usize;
    for (_, bytes, _) in &entries {
        if bytes.len() > limits.maximum_file_bytes {
            return Err(DiagnosticError::LimitExceeded);
        }
        total = total
            .checked_add(bytes.len())
            .ok_or(DiagnosticError::LimitExceeded)?;
    }
    if total > limits.maximum_total_bytes {
        return Err(DiagnosticError::LimitExceeded);
    }
    Ok(entries)
}

fn redact(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter(|(key, _)| !sensitive_key(key))
                .map(|(key, value)| (key, redact(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact).collect()),
        other => other,
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "authorization",
        "cookie",
        "capture_binding",
        "restore_token",
        "portal_session",
        "window_id",
    ]
    .iter()
    .any(|sensitive| key.contains(sensitive))
}

#[derive(Debug, Error)]
pub enum DiagnosticError {
    #[error("diagnostic bundle resource limit exceeded")]
    LimitExceeded,
    #[error("diagnostic crop name or PNG content is invalid")]
    InvalidCrop,
    #[error("diagnostic destination is unsafe")]
    UnsafePath,
    #[error("diagnostic bundle I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("diagnostic bundle JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("diagnostic ZIP failed: {0}")]
    Zip(#[from] zip::result::ZipError),
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use serde_json::json;
    use zip::ZipArchive;

    use super::*;

    fn bundle() -> DiagnosticBundle {
        DiagnosticBundle {
            logs: vec![json!({"message":"started","nested":{"authorization":"bearer"}})],
            configuration: json!({
                "analysis_fps":10,
                "restore_token":"private",
                "capture_bindings":{"profile":"machine-local"},
                "nested":{"api_secret":"private","safe":true}
            }),
            metrics: json!({"frames":12,"portal_session":"private"}),
            selected_crops: vec![DiagnosticCrop {
                name: "health".into(),
                png: vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3],
            }],
        }
    }

    #[test]
    fn plan_discloses_exact_files_and_privacy_warning() {
        let plan = plan_diagnostic_bundle(&bundle(), DiagnosticLimits::default()).unwrap();
        assert_eq!(plan.entries.len(), 4);
        assert_eq!(plan.entries[3].path, "crops/health.png");
        assert!(plan.entries[3].user_selected_image);
        assert!(plan.privacy_warning.contains("private on-screen"));
    }

    #[test]
    fn export_recursively_excludes_secrets_and_machine_bindings() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("diagnostic.zip");
        export_diagnostic_bundle(&destination, &bundle(), DiagnosticLimits::default()).unwrap();
        let mut archive = ZipArchive::new(fs::File::open(destination).unwrap()).unwrap();
        let mut configuration = String::new();
        archive
            .by_name("configuration.json")
            .unwrap()
            .read_to_string(&mut configuration)
            .unwrap();
        assert!(configuration.contains("analysis_fps"));
        assert!(configuration.contains("\"safe\": true"));
        assert!(!configuration.contains("private"));
        assert!(!configuration.contains("restore_token"));
        assert!(!configuration.contains("capture_bindings"));
    }

    #[test]
    fn invalid_images_and_size_limits_are_rejected_without_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("diagnostic.zip");
        fs::write(&destination, b"previous").unwrap();
        let mut invalid = bundle();
        invalid.selected_crops[0].png = vec![0; 20];
        assert!(matches!(
            export_diagnostic_bundle(&destination, &invalid, DiagnosticLimits::default()),
            Err(DiagnosticError::InvalidCrop)
        ));
        let limits = DiagnosticLimits {
            maximum_total_bytes: 1,
            ..DiagnosticLimits::default()
        };
        assert!(matches!(
            export_diagnostic_bundle(&destination, &bundle(), limits),
            Err(DiagnosticError::LimitExceeded)
        ));
        assert_eq!(fs::read(destination).unwrap(), b"previous");
    }
}
