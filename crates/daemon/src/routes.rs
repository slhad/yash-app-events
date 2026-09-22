use std::collections::HashMap;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;
use yash_app_events_output::{
    execute_rendered_route_validated, execute_route, render_payload, EventRecord, OutputRoute,
    OutputTrigger, RouteContext, StateSnapshot,
};
use yash_app_events_profile::{LocalConfig, ProfileId};

const ROUTE_QUEUE_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
struct Job {
    profile_id: ProfileId,
    event: Option<EventRecord>,
    state: Arc<StateSnapshot>,
}

/// Bounded background delivery of machine-local output routes.
#[derive(Debug)]
pub(crate) struct Router {
    sender: SyncSender<Job>,
    last_error: Arc<Mutex<Option<String>>>,
    route_cache: Arc<Mutex<HashMap<ProfileId, Arc<Vec<OutputRoute>>>>>,
}

impl Router {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn spawn(config: LocalConfig, notifications: broadcast::Sender<Value>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(ROUTE_QUEUE_CAPACITY);
        let last_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&last_error);
        let route_cache = Arc::new(Mutex::new(HashMap::new()));
        let worker_route_cache = Arc::clone(&route_cache);
        std::thread::Builder::new()
            .name("yash-output-routes".into())
            .spawn(move || {
                let mut previous_payloads: HashMap<(ProfileId, Uuid), Value> = HashMap::new();
                while let Ok(job) = receiver.recv() {
                    let routes = {
                        let mut route_cache = worker_route_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(routes) = route_cache.get(&job.profile_id).cloned() {
                            routes
                        } else {
                            match config.output_routes(job.profile_id) {
                                Ok(routes) => {
                                    let routes = Arc::new(routes);
                                    route_cache.insert(job.profile_id, Arc::clone(&routes));
                                    routes
                                }
                                Err(error) => {
                                    drop(route_cache);
                                    report_error(
                                        &worker_error,
                                        &notifications,
                                        None,
                                        &error.to_string(),
                                    );
                                    continue;
                                }
                            }
                        }
                    };
                    previous_payloads.retain(|(profile_id, route_id), _| {
                        *profile_id != job.profile_id
                            || routes.iter().any(|route| route.id == *route_id)
                    });
                    for route in routes.iter().filter(|route| route.enabled) {
                        let accepted = match &route.trigger {
                            OutputTrigger::Event { .. } => job
                                .event
                                .as_ref()
                                .is_some_and(|event| route.accepts_event(event)),
                            OutputTrigger::StateChange => job.event.is_none(),
                        };
                        if !accepted {
                            continue;
                        }
                        let route_event = match route.trigger {
                            OutputTrigger::Event { .. } => job.event.as_ref(),
                            OutputTrigger::StateChange => None,
                        };
                        let context = RouteContext {
                            kind: if route_event.is_some() {
                                "event"
                            } else {
                                "state_change"
                            },
                            event: route_event,
                            state: job.state.as_ref(),
                        };
                        let state_payload = if matches!(route.trigger, OutputTrigger::StateChange) {
                            match render_payload(route, &context) {
                                Ok(payload)
                                    if previous_payloads.get(&(job.profile_id, route.id))
                                        == Some(&payload) =>
                                {
                                    continue;
                                }
                                Ok(payload) => Some(payload),
                                Err(
                                    yash_app_events_output::OutputRouteError::MissingPlaceholder(_),
                                ) => continue,
                                Err(error) => {
                                    report_error(
                                        &worker_error,
                                        &notifications,
                                        Some(route.id),
                                        &error.to_string(),
                                    );
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        let delivery = match state_payload.as_ref() {
                            Some(payload) => execute_rendered_route_validated(route, payload),
                            None => execute_route(route, &context),
                        };
                        match delivery {
                            Ok(receipt) => {
                                if let Some(payload) = state_payload {
                                    previous_payloads.insert((job.profile_id, route.id), payload);
                                }
                                *worker_error
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                                tracing::debug!(
                                    profile_id = %job.profile_id,
                                    route_id = %route.id,
                                    bytes = receipt.bytes,
                                    command_exit_code = ?receipt.command_exit_code,
                                    "output route delivered"
                                );
                            }
                            Err(error) => report_error(
                                &worker_error,
                                &notifications,
                                Some(route.id),
                                &error.to_string(),
                            ),
                        }
                    }
                }
            })
            .expect("spawn output route worker");
        Self {
            sender,
            last_error,
            route_cache,
        }
    }

    pub(crate) fn publish(
        &self,
        profile_id: ProfileId,
        event: Option<EventRecord>,
        state: Arc<StateSnapshot>,
    ) -> Result<(), &'static str> {
        let should_queue = self
            .route_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&profile_id)
            .is_none_or(|routes| {
                routes.iter().any(|route| {
                    route.enabled
                        && matches!(
                            (&event, &route.trigger),
                            (Some(_), OutputTrigger::Event { .. })
                                | (None, OutputTrigger::StateChange)
                        )
                })
            });
        if !should_queue {
            return Ok(());
        }
        self.sender
            .try_send(Job {
                profile_id,
                event,
                state,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => "output route queue is full",
                TrySendError::Disconnected(_) => "output route worker stopped",
            })
    }

    pub(crate) fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn replace_routes(&self, profile_id: ProfileId, routes: Vec<OutputRoute>) {
        self.route_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(profile_id, Arc::new(routes));
    }

    pub(crate) fn invalidate_routes(&self, profile_id: ProfileId) {
        self.route_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&profile_id);
    }
}

fn report_error(
    last_error: &Mutex<Option<String>>,
    notifications: &broadcast::Sender<Value>,
    route_id: Option<Uuid>,
    error: &str,
) {
    tracing::error!(?route_id, %error, "output route delivery failed");
    *last_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_owned());
    let _ = notifications.send(json!({
        "type":"output_route_error",
        "route_id":route_id,
        "error":error
    }));
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;
    use yash_app_events_output::{
        FileMode, OutputFormat, OutputRoute, OutputSink, OutputTrigger, StateSnapshot,
    };

    use super::*;

    #[test]
    fn state_route_waits_quietly_for_a_derived_observation() {
        let directory = tempfile::tempdir().unwrap();
        let config = LocalConfig::new(directory.path().join("config"));
        let profile_id = ProfileId::new();
        let output_path = directory.path().join("stage.txt");
        config
            .set_output_route(
                profile_id,
                OutputRoute {
                    id: Uuid::new_v4(),
                    name: "stage".into(),
                    enabled: true,
                    trigger: OutputTrigger::StateChange,
                    format: OutputFormat::TextTemplate {
                        template: "{{state.observations.stage.value}}".into(),
                        trailing_newline: true,
                    },
                    sink: OutputSink::File {
                        path: output_path.clone(),
                        mode: FileMode::Replace,
                    },
                    source_recipe: None,
                },
            )
            .unwrap();
        let (notifications, _) = broadcast::channel(8);
        let router = Router::spawn(config, notifications);
        router
            .publish(
                profile_id,
                None,
                Arc::new(StateSnapshot {
                    schema: 1,
                    daemon_instance: Uuid::nil(),
                    sequence: 0,
                    updated_at: "2026-07-18T00:00:00Z".into(),
                    capture: json!({"active":false}),
                    active_profile: Some(profile_id.to_string()),
                    observations: json!({}),
                    events: json!({}),
                    context: Value::Null,
                }),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(router.last_error(), None);
        assert!(!output_path.exists());
    }
}
