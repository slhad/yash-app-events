use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::{atomic_write_before_rename, EventRecord, EventState, StateSnapshot};

const MAXIMUM_TEMPLATE_BYTES: usize = 64 * 1024;
const MAXIMUM_ARGUMENTS: usize = 64;
const MAXIMUM_ARGUMENT_BYTES: usize = 4 * 1024;
const MAXIMUM_COMMAND_TIMEOUT_MS: u64 = 30_000;

/// Portable, inert suggestion that cannot name an authorized local sink.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OutputRecipe {
    pub schema: u16,
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub trigger: OutputTrigger,
    pub format: OutputFormat,
    pub suggested_sink: OutputRecipeSink,
}

impl OutputRecipe {
    /// Validates the bounded inert recipe contract.
    ///
    /// # Errors
    ///
    /// Returns validation or JSON-size errors. Recipes never validate or authorize a
    /// filesystem destination or executable path because neither is part of this type.
    pub fn validate(&self) -> Result<(), OutputRouteError> {
        if self.schema != 1 {
            return Err(OutputRouteError::Invalid(
                "unsupported output recipe schema",
            ));
        }
        if self.name.trim().is_empty() || self.name.len() > 128 {
            return Err(OutputRouteError::Invalid(
                "recipe name must contain 1 through 128 bytes",
            ));
        }
        if self.description.len() > 4 * 1024 {
            return Err(OutputRouteError::Invalid(
                "recipe description must not exceed 4 KiB",
            ));
        }
        let placeholder_route = OutputRoute {
            id: self.id,
            name: self.name.clone(),
            enabled: false,
            trigger: self.trigger.clone(),
            format: self.format.clone(),
            sink: OutputSink::File {
                path: PathBuf::from("/recipe-validation-placeholder"),
                mode: FileMode::Append,
            },
            source_recipe: None,
        };
        placeholder_route.validate()?;
        match &self.suggested_sink {
            OutputRecipeSink::File {
                suggested_filename, ..
            } => {
                if suggested_filename.is_empty()
                    || suggested_filename.len() > 255
                    || suggested_filename.contains('/')
                    || suggested_filename.contains('\\')
                    || suggested_filename == "."
                    || suggested_filename == ".."
                {
                    return Err(OutputRouteError::Invalid(
                        "recipe suggested filename must be one safe path component",
                    ));
                }
            }
            OutputRecipeSink::Command {
                program_name,
                args,
                timeout_ms,
            } => {
                if program_name.trim().is_empty() || program_name.len() > 128 {
                    return Err(OutputRouteError::Invalid(
                        "recipe program name must contain 1 through 128 bytes",
                    ));
                }
                if args.len() > MAXIMUM_ARGUMENTS
                    || args
                        .iter()
                        .any(|argument| argument.len() > MAXIMUM_ARGUMENT_BYTES)
                {
                    return Err(OutputRouteError::Invalid("recipe arguments are too large"));
                }
                if *timeout_ms == 0 || *timeout_ms > MAXIMUM_COMMAND_TIMEOUT_MS {
                    return Err(OutputRouteError::Invalid(
                        "recipe command timeout must be within 1..=30000 ms",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Sink intent carried by a portable recipe without any local authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputRecipeSink {
    File {
        mode: FileMode,
        suggested_filename: String,
    },
    Command {
        program_name: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
}

/// One machine-local, profile-scoped output route.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OutputRoute {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub trigger: OutputTrigger,
    pub format: OutputFormat,
    pub sink: OutputSink,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_recipe: Option<OutputRecipeSource>,
}

/// Immutable provenance for a route instantiated from portable inert content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutputRecipeSource {
    pub profile_id: String,
    pub recipe_id: Uuid,
    pub sha256: String,
}

impl OutputRoute {
    /// Validates resource limits and the no-shell execution boundary.
    ///
    /// # Errors
    ///
    /// Returns an actionable validation or JSON-size error for unsafe routes.
    pub fn validate(&self) -> Result<(), OutputRouteError> {
        if self.name.trim().is_empty() || self.name.len() > 128 {
            return Err(OutputRouteError::Invalid(
                "route name must contain 1 through 128 bytes",
            ));
        }
        if let OutputTrigger::Event { events, states: _ } = &self.trigger {
            if events.len() > 256
                || events
                    .iter()
                    .any(|event| event.is_empty() || event.len() > 128)
            {
                return Err(OutputRouteError::Invalid("event filters are invalid"));
            }
        }
        match &self.format {
            OutputFormat::JsonTemplate { template }
                if serde_json::to_vec(template)?.len() > MAXIMUM_TEMPLATE_BYTES =>
            {
                return Err(OutputRouteError::Invalid(
                    "JSON template must not exceed 64 KiB",
                ));
            }
            OutputFormat::TextTemplate { template, .. }
                if template.len() > MAXIMUM_TEMPLATE_BYTES =>
            {
                return Err(OutputRouteError::Invalid(
                    "text template must not exceed 64 KiB",
                ));
            }
            _ => {}
        }
        match &self.sink {
            OutputSink::File { path, .. } => {
                if !path.is_absolute() {
                    return Err(OutputRouteError::Invalid(
                        "output file path must be absolute",
                    ));
                }
            }
            OutputSink::Command {
                program,
                args,
                timeout_ms,
            } => {
                if !program.is_absolute() {
                    return Err(OutputRouteError::Invalid(
                        "command program path must be absolute",
                    ));
                }
                if args.len() > MAXIMUM_ARGUMENTS
                    || args
                        .iter()
                        .any(|argument| argument.len() > MAXIMUM_ARGUMENT_BYTES)
                {
                    return Err(OutputRouteError::Invalid("command arguments are too large"));
                }
                if *timeout_ms == 0 || *timeout_ms > MAXIMUM_COMMAND_TIMEOUT_MS {
                    return Err(OutputRouteError::Invalid(
                        "command timeout must be within 1..=30000 ms",
                    ));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn accepts_event(&self, event: &EventRecord) -> bool {
        match &self.trigger {
            OutputTrigger::Event { events, states } => {
                (events.is_empty() || events.contains(&event.event))
                    && (states.is_empty() || states.contains(&event.state))
            }
            OutputTrigger::StateChange => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputTrigger {
    Event {
        #[serde(default)]
        events: Vec<String>,
        #[serde(default)]
        states: Vec<EventState>,
    },
    StateChange,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputFormat {
    Full,
    JsonTemplate {
        template: Value,
    },
    TextTemplate {
        template: String,
        #[serde(default = "default_true")]
        trailing_newline: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    Append,
    Replace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSink {
    File {
        path: PathBuf,
        mode: FileMode,
    },
    Command {
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
}

const fn default_timeout_ms() -> u64 {
    5_000
}

const fn default_true() -> bool {
    true
}

/// Data made available to a route JSON template.
#[derive(Clone, Debug, Serialize)]
pub struct RouteContext<'a> {
    pub kind: &'static str,
    pub event: Option<&'a EventRecord>,
    pub state: &'a StateSnapshot,
}

/// Successful delivery details returned to RPC tests and diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RouteReceipt {
    pub route_id: Uuid,
    pub bytes: usize,
    pub command_exit_code: Option<i32>,
}

/// Renders the complete contract, a recursive JSON template, or a raw text template.
///
/// # Errors
///
/// Returns route validation, serialization, or unknown-placeholder errors.
pub fn render_payload(
    route: &OutputRoute,
    context: &RouteContext<'_>,
) -> Result<Value, OutputRouteError> {
    route.validate()?;
    let source = serde_json::to_value(context)?;
    match &route.format {
        OutputFormat::Full => context.event.map_or_else(
            || serde_json::to_value(context.state).map_err(OutputRouteError::from),
            |event| serde_json::to_value(event).map_err(OutputRouteError::from),
        ),
        OutputFormat::JsonTemplate { template } => render_value(template, &source),
        OutputFormat::TextTemplate { template, .. } => render_text(template, &source),
    }
}

/// Delivers a rendered payload to one file or direct executable sink.
///
/// # Errors
///
/// Returns route validation, rendering, file I/O, process, exit, or timeout errors.
pub fn execute_route(
    route: &OutputRoute,
    context: &RouteContext<'_>,
) -> Result<RouteReceipt, OutputRouteError> {
    let payload = render_payload(route, context)?;
    let mut bytes = match &route.format {
        OutputFormat::TextTemplate { .. } => payload
            .as_str()
            .ok_or(OutputRouteError::Invalid(
                "text template did not render as text",
            ))?
            .as_bytes()
            .to_vec(),
        OutputFormat::Full | OutputFormat::JsonTemplate { .. } => serde_json::to_vec(&payload)?,
    };
    if !matches!(
        &route.format,
        OutputFormat::TextTemplate {
            trailing_newline: false,
            ..
        }
    ) {
        bytes.push(b'\n');
    }
    let command_exit_code = match &route.sink {
        OutputSink::File { path, mode } => {
            let parent = path
                .parent()
                .ok_or(OutputRouteError::Invalid("output file has no parent"))?;
            fs::create_dir_all(parent)?;
            match mode {
                FileMode::Append => {
                    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
                    file.write_all(&bytes)?;
                    file.flush()?;
                }
                FileMode::Replace => atomic_write_before_rename(path, &bytes, || Ok(()))?,
            }
            None
        }
        OutputSink::Command {
            program,
            args,
            timeout_ms,
        } => Some(run_command(program, args, &bytes, *timeout_ms)?),
    };
    Ok(RouteReceipt {
        route_id: route.id,
        bytes: bytes.len(),
        command_exit_code,
    })
}

fn render_value(template: &Value, source: &Value) -> Result<Value, OutputRouteError> {
    match template {
        Value::String(value) => render_string(value, source),
        Value::Array(values) => values
            .iter()
            .map(|value| render_value(value, source))
            .collect(),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), render_value(value, source)?)))
            .collect(),
        other => Ok(other.clone()),
    }
}

fn render_string(template: &str, source: &Value) -> Result<Value, OutputRouteError> {
    if let Some(path) = template
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
        .filter(|path| !path.contains("{{") && !path.contains("}}"))
    {
        return lookup(source, path).cloned();
    }
    let mut rendered = template.to_owned();
    while let Some(start) = rendered.find("{{") {
        let Some(relative_end) = rendered[start + 2..].find("}}") else {
            return Err(OutputRouteError::Template("unclosed placeholder".into()));
        };
        let end = start + 2 + relative_end;
        let path = &rendered[start + 2..end];
        let value = lookup(source, path)?;
        let replacement = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        rendered.replace_range(start..end + 2, &replacement);
    }
    Ok(Value::String(rendered))
}

fn render_text(template: &str, source: &Value) -> Result<Value, OutputRouteError> {
    let rendered = render_string(template, source)?;
    Ok(Value::String(
        rendered
            .as_str()
            .map_or_else(|| rendered.to_string(), str::to_owned),
    ))
}

fn lookup<'a>(source: &'a Value, path: &str) -> Result<&'a Value, OutputRouteError> {
    if path.is_empty() {
        return Ok(source);
    }
    path.split('.').try_fold(source, |value, segment| {
        value
            .get(segment)
            .ok_or_else(|| OutputRouteError::MissingPlaceholder(path.to_owned()))
    })
}

fn run_command(
    program: &PathBuf,
    args: &[String],
    input: &[u8],
    timeout_ms: u64,
) -> Result<i32, OutputRouteError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or(OutputRouteError::CommandStdin)?
        .write_all(input)?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some(status) = child.try_wait()? {
            return status
                .code()
                .filter(|code| *code == 0)
                .ok_or(OutputRouteError::CommandFailed(status.code()));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Err(OutputRouteError::CommandTimeout(timeout_ms));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug, Error)]
pub enum OutputRouteError {
    #[error("invalid output route: {0}")]
    Invalid(&'static str),
    #[error("output route template failed: {0}")]
    Template(String),
    #[error("output route template is waiting for placeholder {0}")]
    MissingPlaceholder(String),
    #[error("output route I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("output route JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("output command stdin was unavailable")]
    CommandStdin,
    #[error("output command exited unsuccessfully with code {0:?}")]
    CommandFailed(Option<i32>),
    #[error("output command exceeded its {0} ms timeout")]
    CommandTimeout(u64),
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::OUTPUT_SCHEMA_VERSION;

    fn state() -> StateSnapshot {
        StateSnapshot {
            schema: OUTPUT_SCHEMA_VERSION,
            daemon_instance: Uuid::nil(),
            sequence: 4,
            updated_at: "2026-07-17T10:00:00.000Z".into(),
            capture: json!({"active":true}),
            active_profile: Some("profile".into()),
            observations: json!({"stage":{"value":"2"}}),
            events: json!({"stage_changed":true}),
        }
    }

    fn event() -> EventRecord {
        EventRecord {
            schema: OUTPUT_SCHEMA_VERSION,
            daemon_instance: Uuid::nil(),
            sequence: 4,
            timestamp: EventRecord::timestamp_rfc3339(Utc::now()),
            profile_id: "profile".into(),
            game: "blazblue_entropy_effect".into(),
            event: "stage_changed".into(),
            state: EventState::Updated,
            value: json!(2),
            confidence: 0.99,
        }
    }

    #[test]
    fn typed_placeholders_preserve_json_values() {
        let route = OutputRoute {
            id: Uuid::nil(),
            name: "marker payload".into(),
            enabled: true,
            trigger: OutputTrigger::Event {
                events: vec!["stage_changed".into()],
                states: vec![EventState::Updated],
            },
            format: OutputFormat::JsonTemplate {
                template: json!({
                    "marker":"stage-{{event.value}}",
                    "stage":"{{event.value}}",
                    "game":"{{event.game}}"
                }),
            },
            sink: OutputSink::File {
                path: PathBuf::from("/tmp/unused"),
                mode: FileMode::Append,
            },
            source_recipe: None,
        };
        let event = event();
        assert!(route.accepts_event(&event));
        assert_eq!(
            render_payload(
                &route,
                &RouteContext {
                    kind: "event",
                    event: Some(&event),
                    state: &state(),
                }
            )
            .unwrap(),
            json!({"marker":"stage-2","stage":2,"game":"blazblue_entropy_effect"})
        );
    }

    #[test]
    fn append_and_replace_file_routes_write_valid_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("markers.jsonl");
        let mut route = OutputRoute {
            id: Uuid::new_v4(),
            name: "file".into(),
            enabled: true,
            trigger: OutputTrigger::StateChange,
            format: OutputFormat::Full,
            sink: OutputSink::File {
                path: path.clone(),
                mode: FileMode::Append,
            },
            source_recipe: None,
        };
        let snapshot = state();
        let context = RouteContext {
            kind: "state_change",
            event: None,
            state: &snapshot,
        };
        execute_route(&route, &context).unwrap();
        execute_route(&route, &context).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        route.sink = OutputSink::File {
            path: path.clone(),
            mode: FileMode::Replace,
        };
        execute_route(&route, &context).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 1);
    }

    #[test]
    fn text_template_replace_file_controls_its_trailing_newline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stage.txt");
        let mut route = OutputRoute {
            id: Uuid::new_v4(),
            name: "raw stage".into(),
            enabled: true,
            trigger: OutputTrigger::StateChange,
            format: OutputFormat::TextTemplate {
                template: "{{state.observations.stage.value}}".into(),
                trailing_newline: true,
            },
            sink: OutputSink::File {
                path: path.clone(),
                mode: FileMode::Replace,
            },
            source_recipe: None,
        };
        let snapshot = state();
        let receipt = execute_route(
            &route,
            &RouteContext {
                kind: "state_change",
                event: None,
                state: &snapshot,
            },
        )
        .unwrap();
        assert_eq!(receipt.bytes, 2);
        assert_eq!(fs::read_to_string(&path).unwrap(), "2\n");
        route.format = OutputFormat::TextTemplate {
            template: "{{state.observations.stage.value}}".into(),
            trailing_newline: false,
        };
        let receipt = execute_route(
            &route,
            &RouteContext {
                kind: "state_change",
                event: None,
                state: &snapshot,
            },
        )
        .unwrap();
        assert_eq!(receipt.bytes, 1);
        assert_eq!(fs::read(path).unwrap(), b"2");
    }

    #[test]
    fn command_receives_compact_json_on_stdin_without_a_shell() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stdin.json");
        let route = OutputRoute {
            id: Uuid::new_v4(),
            name: "command".into(),
            enabled: true,
            trigger: OutputTrigger::StateChange,
            format: OutputFormat::JsonTemplate {
                template: json!({"sequence":"{{state.sequence}}"}),
            },
            sink: OutputSink::Command {
                program: PathBuf::from("/usr/bin/tee"),
                args: vec![path.display().to_string()],
                timeout_ms: 1_000,
            },
            source_recipe: None,
        };
        let snapshot = state();
        let receipt = execute_route(
            &route,
            &RouteContext {
                kind: "state_change",
                event: None,
                state: &snapshot,
            },
        )
        .unwrap();
        assert_eq!(receipt.command_exit_code, Some(0));
        assert_eq!(fs::read_to_string(path).unwrap(), "{\"sequence\":4}\n");
    }

    #[test]
    fn portable_recipe_contains_intent_but_no_authorized_local_sink() {
        let recipe = OutputRecipe {
            schema: 1,
            id: Uuid::new_v4(),
            name: "Yash stage marker".into(),
            description: "Suggest a marker for each stage transition".into(),
            trigger: OutputTrigger::Event {
                events: vec!["stage_changed".into()],
                states: vec![EventState::Updated],
            },
            format: OutputFormat::JsonTemplate {
                template: json!({"stage":"{{event.value}}"}),
            },
            suggested_sink: OutputRecipeSink::Command {
                program_name: "yash".into(),
                args: vec!["ipc".into(), "command".into(), "marker".into()],
                timeout_ms: 5_000,
            },
        };
        recipe.validate().unwrap();
        let serialized = serde_json::to_value(&recipe).unwrap();
        assert_eq!(serialized["suggested_sink"]["program_name"], "yash");
        assert!(serialized["suggested_sink"].get("program").is_none());
        assert!(serialized.get("enabled").is_none());
    }
}
