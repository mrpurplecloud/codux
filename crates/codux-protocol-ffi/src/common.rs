use codux_remote_transport::{
    RemoteControllerTransportConfig, RemoteTransport, RemoteTransportCandidate,
};
use codux_terminal_core::{RemoteRuntimeModel, RemoteSequenceGuard};
use std::any::Any;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::runtime::Runtime;

pub type FfiRemoteRuntimeModel = RemoteRuntimeModel;
pub(crate) const CONTROLLER_TRANSPORT_EVENT_LIMIT: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransportEventKind {
    LifecycleState,
    State,
    ControlMessage,
    TerminalOutput,
    Log,
    Other,
}

pub(crate) struct TransportEvent {
    pub(crate) json: String,
    kind: TransportEventKind,
}

pub struct FfiControllerTransport {
    pub(crate) transport: Arc<Mutex<Option<Arc<dyn RemoteTransport>>>>,
    pub(crate) events: Arc<Mutex<VecDeque<TransportEvent>>>,
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) connect_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
// Iroh controller endpoints are process-shared, so their Tokio executor must
// have the same lifetime instead of being dropped with each Flutter FFI handle.
static CONTROLLER_TRANSPORT_RUNTIME: OnceLock<Result<Arc<Runtime>, String>> = OnceLock::new();

pub(crate) fn controller_transport_runtime() -> Result<Arc<Runtime>, String> {
    CONTROLLER_TRANSPORT_RUNTIME
        .get_or_init(|| {
            Runtime::new()
                .map(Arc::new)
                .map_err(|error| format!("failed to create controller runtime: {error}"))
        })
        .clone()
}

#[unsafe(no_mangle)]
pub extern "C" fn codux_protocol_last_error() -> *mut c_char {
    let error = LAST_ERROR
        .lock()
        .ok()
        .and_then(|error| error.clone())
        .unwrap_or_default();
    string_to_c(error)
}

#[unsafe(no_mangle)]
pub extern "C" fn codux_protocol_string_free(value: *mut c_char) {
    if value.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(value));
    }
}

pub(crate) fn c_to_string(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(value).to_str().ok().map(str::to_string) }
}

pub(crate) fn string_to_c(value: impl Into<String>) -> *mut c_char {
    CString::new(value.into())
        .map(CString::into_raw)
        .unwrap_or(ptr::null_mut())
}

pub(crate) fn json_string_to_c(value: &impl serde::Serialize) -> *mut c_char {
    string_to_c(serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()))
}

pub(crate) fn clean_ffi_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(crate) fn optional_sequence(value: i64) -> Option<i64> {
    if value < 0 { None } else { Some(value) }
}

pub(crate) fn remote_sequence_guard_mut<'a>(
    guard: *mut RemoteSequenceGuard,
) -> Option<&'a mut RemoteSequenceGuard> {
    if guard.is_null() {
        return None;
    }
    unsafe { guard.as_mut() }
}

pub(crate) fn remote_runtime_model_ref<'a>(
    model: *const FfiRemoteRuntimeModel,
) -> Option<&'a FfiRemoteRuntimeModel> {
    if model.is_null() {
        return None;
    }
    unsafe { model.as_ref() }
}

pub(crate) fn remote_runtime_model_mut<'a>(
    model: *mut FfiRemoteRuntimeModel,
) -> Option<&'a mut FfiRemoteRuntimeModel> {
    if model.is_null() {
        return None;
    }
    unsafe { model.as_mut() }
}

pub(crate) fn controller_transport_ref<'a>(
    transport: *mut FfiControllerTransport,
) -> Option<&'a FfiControllerTransport> {
    if transport.is_null() {
        return None;
    }
    unsafe { transport.as_ref() }
}

pub(crate) fn controller_transport_config_from_json(
    config_json: &str,
) -> Result<RemoteControllerTransportConfig, String> {
    let value = serde_json::from_str::<serde_json::Value>(config_json)
        .map_err(|error| error.to_string())?;
    let direct_addrs = json_string_array_field(&value, "directAddrs");
    let transports = value
        .get("transports")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| RemoteTransportCandidate {
                    kind: item
                        .get("kind")
                        .or_else(|| item.get("transport"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    url: item
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    node_id: item
                        .get("nodeId")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    relay_url: item
                        .get("relayUrl")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    ticket: item
                        .get("ticket")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    relay_authentication: item
                        .get("relayAuthentication")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let relay_url = json_string_field(&value, "relayUrl");
    // The host publishes the relay it is *connected to* and omits the key when
    // it has none, so a host whose relay was unreachable at startup reports a
    // candidate with no relay at all. `relayUrl` here is the relay agreed at
    // pairing, which the phone stores separately and the host never overwrites
    // — the last relay route it has. A reported relay still wins: only one of
    // them can be the relay the host is actually homed on, so this is a
    // fallback, not a second route.
    let transports = transports
        .into_iter()
        .map(|mut candidate| {
            if candidate.relay_url.trim().is_empty() {
                candidate.relay_url = relay_url.clone();
            }
            candidate
        })
        .collect();
    Ok(RemoteControllerTransportConfig {
        relay_url,
        host_id: json_string_field(&value, "hostId"),
        device_id: json_string_field(&value, "deviceId"),
        device_token: json_string_field(&value, "deviceToken"),
        transports,
        direct_addrs,
    })
}

/// Read a JSON array of strings, dropping blanks. Absent or malformed reads as
/// empty: this carries best-effort hints, never anything a connection needs.
fn json_string_array_field(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn json_string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn set_last_error(message: impl Into<String>) {
    if let Ok(mut error) = LAST_ERROR.lock() {
        *error = Some(message.into());
    }
}

pub(crate) fn clear_last_error() {
    if let Ok(mut error) = LAST_ERROR.lock() {
        *error = None;
    }
}

pub(crate) fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "unknown panic payload".to_string()
}

pub(crate) fn push_transport_event(
    events: &Arc<Mutex<VecDeque<TransportEvent>>>,
    event: serde_json::Value,
) {
    let kind = transport_event_kind(&event);
    let json = event.to_string();
    if let Ok(mut events) = events.lock() {
        if matches!(
            kind,
            TransportEventKind::LifecycleState | TransportEventKind::State
        ) && let Some(index) = events.iter().position(|event| event.kind == kind)
        {
            events.remove(index);
        }
        while events.len() >= CONTROLLER_TRANSPORT_EVENT_LIMIT {
            let incoming_priority = transport_event_priority(kind);
            let removable = events
                .iter()
                .enumerate()
                .filter(|(_, event)| transport_event_priority(event.kind) <= incoming_priority)
                .min_by_key(|(_, event)| transport_event_priority(event.kind))
                .map(|(index, _)| index);
            let Some(index) = removable else {
                return;
            };
            events.remove(index);
        }
        events.push_back(TransportEvent { json, kind });
    }
}

fn transport_event_kind(event: &serde_json::Value) -> TransportEventKind {
    match event.get("kind").and_then(serde_json::Value::as_str) {
        Some("state") => match event
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
        {
            "connecting" | "connected" | "closed" | "failed" | "disconnected" => {
                TransportEventKind::LifecycleState
            }
            _ => TransportEventKind::State,
        },
        Some("message") => {
            let terminal_output = event
                .get("data")
                .and_then(serde_json::Value::as_str)
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
                .and_then(|envelope| {
                    envelope
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .map(|kind| kind == codux_protocol::REMOTE_TERMINAL_OUTPUT)
                })
                .unwrap_or(false);
            if terminal_output {
                TransportEventKind::TerminalOutput
            } else {
                TransportEventKind::ControlMessage
            }
        }
        Some("log") => TransportEventKind::Log,
        _ => TransportEventKind::Other,
    }
}

fn transport_event_priority(kind: TransportEventKind) -> u8 {
    match kind {
        TransportEventKind::Log => 0,
        TransportEventKind::TerminalOutput => 1,
        TransportEventKind::Other => 2,
        TransportEventKind::State => 3,
        TransportEventKind::ControlMessage => 4,
        TransportEventKind::LifecycleState => 5,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_relay_less_report_falls_back_to_the_relay_from_pairing() {
        // The product promise is that a phone reaches its host on any network:
        // direct when it can, relay when it cannot. A host whose relay is
        // unreachable at startup publishes no relay, so without this the dial
        // would have only direct routes -- exactly the case a relay covers.
        let config = controller_transport_config_from_json(
            r#"{
                "relayUrl": "https://paired-relay.example",
                "transports": [{ "kind": "iroh", "nodeId": "node-1", "ticket": "t" }]
            }"#,
        )
        .expect("config");
        assert_eq!(
            config.transports[0].relay_url, "https://paired-relay.example",
            "a report with no relay must fall back to the one agreed at pairing"
        );
    }

    #[test]
    fn a_reported_relay_wins_over_the_one_from_pairing() {
        // Only one relay can be the one the host is actually homed on, so the
        // fresh report replaces the stored guess rather than joining it.
        let config = controller_transport_config_from_json(
            r#"{
                "relayUrl": "https://paired-relay.example",
                "transports": [{
                    "kind": "iroh",
                    "nodeId": "node-1",
                    "relayUrl": "https://live-relay.example"
                }]
            }"#,
        )
        .expect("config");
        assert_eq!(config.transports[0].relay_url, "https://live-relay.example");
    }

    use serde_json::json;

    #[test]
    fn controller_transports_share_one_runtime() {
        let first = controller_transport_runtime().expect("first runtime");
        let second = controller_transport_runtime().expect("second runtime");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn transport_event_queue_is_bounded_and_preserves_lifecycle_state() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        push_transport_event(&events, json!({ "kind": "state", "state": "connected" }));
        for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT {
            push_transport_event(
                &events,
                json!({ "kind": "message", "data": index.to_string() }),
            );
        }

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::ControlMessage)
                .count(),
            CONTROLLER_TRANSPORT_EVENT_LIMIT - 1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::LifecycleState)
                .count(),
            1
        );
    }

    #[test]
    fn transport_event_queue_drops_logs_before_messages() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        push_transport_event(&events, json!({ "kind": "log", "message": "old" }));
        for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT - 1 {
            push_transport_event(
                &events,
                json!({ "kind": "message", "data": index.to_string() }),
            );
        }
        push_transport_event(&events, json!({ "kind": "state", "state": "closed" }));

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert!(
            !events
                .iter()
                .any(|event| event.kind == TransportEventKind::Log)
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == TransportEventKind::LifecycleState)
        );
    }

    #[test]
    fn transport_event_queue_drops_new_log_before_messages() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT {
            push_transport_event(
                &events,
                json!({ "kind": "message", "data": index.to_string() }),
            );
        }

        push_transport_event(&events, json!({ "kind": "log", "message": "new" }));

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert!(
            events
                .iter()
                .all(|event| event.kind == TransportEventKind::ControlMessage)
        );
    }

    #[test]
    fn transport_event_queue_preserves_control_reply_over_old_state() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        {
            let mut queue = events.lock().unwrap();
            for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT {
                queue.push_back(TransportEvent {
                    json: index.to_string(),
                    kind: TransportEventKind::State,
                });
            }
        }

        push_transport_event(&events, json!({ "kind": "message", "data": "new" }));

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::ControlMessage)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::State)
                .count(),
            CONTROLLER_TRANSPORT_EVENT_LIMIT - 1
        );
    }

    #[test]
    fn transport_event_queue_preserves_disconnect_over_control_replies() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT {
            push_transport_event(
                &events,
                json!({ "kind": "message", "data": index.to_string() }),
            );
        }

        push_transport_event(&events, json!({ "kind": "state", "state": "closed" }));

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::ControlMessage)
                .count(),
            CONTROLLER_TRANSPORT_EVENT_LIMIT - 1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::LifecycleState)
                .count(),
            1
        );
    }

    #[test]
    fn transport_event_queue_drops_new_path_state_before_control_replies() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT {
            push_transport_event(
                &events,
                json!({ "kind": "message", "data": index.to_string() }),
            );
        }

        push_transport_event(
            &events,
            json!({ "kind": "state", "state": "path:path=direct" }),
        );

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert!(
            events
                .iter()
                .all(|event| event.kind == TransportEventKind::ControlMessage)
        );
    }

    #[test]
    fn transport_event_queue_preserves_control_reply_over_terminal_output() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..CONTROLLER_TRANSPORT_EVENT_LIMIT {
            push_transport_event(
                &events,
                json!({
                    "kind": "message",
                    "data": json!({
                        "type": codux_protocol::REMOTE_TERMINAL_OUTPUT,
                        "payload": { "data": index.to_string() }
                    }).to_string()
                }),
            );
        }

        push_transport_event(
            &events,
            json!({
                "kind": "message",
                "data": json!({ "type": codux_protocol::REMOTE_PROJECT_LIST }).to_string()
            }),
        );

        let events = events.lock().unwrap();
        assert_eq!(events.len(), CONTROLLER_TRANSPORT_EVENT_LIMIT);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::ControlMessage)
                .count(),
            1
        );
    }

    #[test]
    fn transport_event_queue_coalesces_repeated_state_categories() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        push_transport_event(&events, json!({ "kind": "state", "state": "connecting" }));
        push_transport_event(
            &events,
            json!({ "kind": "state", "state": "connected:path=relay" }),
        );
        push_transport_event(
            &events,
            json!({ "kind": "state", "state": "latency:rtt=20;path=relay" }),
        );
        push_transport_event(
            &events,
            json!({ "kind": "state", "state": "latency:rtt=10;path=direct" }),
        );

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::LifecycleState)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == TransportEventKind::State)
                .count(),
            1
        );
        assert!(events.iter().any(|event| event.json.contains("rtt=10")));
    }
}
