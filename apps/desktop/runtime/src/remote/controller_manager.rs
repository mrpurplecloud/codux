//! Pools live `RemoteController` connections to paired hosts, keyed by the
//! device id we minted during pairing. Lazily connects from the saved-host
//! store on first use, and bridges the async controller API into the
//! synchronous `RuntimeService` domain methods via `async_runtime::block_on`.
//!
//! Each connection is wired to the controller transport's link-state callback,
//! so a dropped iroh link is detected, the dead controller is evicted, and a
//! backoff reconnect loop is spawned. The desktop polls [`link_states`] to drive
//! the per-project connection badge and to re-attach terminals on recovery.

use super::controller::{
    RemoteController, new_device_id, parse_pairing_ticket, saved_transport_from_json,
};
use super::controller_store::{RemoteControllerStore, SavedRemoteHost, SavedRemoteTransport};
use codux_protocol::REMOTE_TRANSPORT_IROH;
use codux_remote_transport::RemoteTransportStateHandler;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);

/// Liveness of the client→host iroh link for a paired device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControllerLinkState {
    /// A connect/reconnect attempt is in flight.
    Connecting,
    /// The link is up and usable.
    Connected,
    /// The link dropped; a reconnect loop is retrying in the background.
    Disconnected,
}

impl ControllerLinkState {
    /// Map a transport `on_state` string to a link state. The controller
    /// transport emits `"connecting"`, `"connected"` (and `"connected:path=…"`),
    /// and `"closed"`; anything that isn't a connected/connecting marker is
    /// treated as a drop.
    fn from_transport_state(state: &str) -> Option<Self> {
        if state.starts_with("connected") {
            Some(Self::Connected)
        } else if state.starts_with("connecting") {
            Some(Self::Connecting)
        } else if matches!(
            state.split(':').next().unwrap_or_default(),
            "closed" | "failed" | "disconnected"
        ) {
            Some(Self::Disconnected)
        } else {
            None
        }
    }
}

/// How the live link to a host is routed — shown next to the connected state so
/// the user can tell a LAN/p2p direct path from a relay-routed one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControllerLinkPath {
    /// Direct peer-to-peer path (LAN via mDNS, or a holepunched route).
    Direct,
    /// Routed through the relay.
    Relay,
}

impl ControllerLinkPath {
    /// Parse the path type from a transport state string. The controller transport
    /// publishes `connected:path=direct;addr=…` / `connected:path=relay;…` once a
    /// path is selected; other states (`connecting`, bare `connected`) carry none.
    fn from_transport_state(state: &str) -> Option<Self> {
        let detail = state.strip_prefix("connected:path=")?;
        match detail.split(';').next().unwrap_or(detail) {
            "direct" => Some(Self::Direct),
            "relay" => Some(Self::Relay),
            _ => None,
        }
    }
}

/// The peer address a `connected:path=direct;addr=…` state reports.
///
/// Only direct paths yield one worth keeping: a relay path's address is the
/// relay's own, which the saved candidate already names and which says nothing
/// about where the host is.
fn direct_addr_from_transport_state(state: &str) -> Option<&str> {
    let detail = state.strip_prefix("connected:path=")?;
    let mut parts = detail.split(';');
    if parts.next()? != "direct" {
        return None;
    }
    parts
        .find_map(|part| part.strip_prefix("addr="))
        .map(str::trim)
        .filter(|addr| !addr.is_empty())
}

/// Shared manager state captured (weakly) by transport callbacks and the
/// reconnect loop, so a dropped link can evict + reconnect without a reference
/// back to the owning `RuntimeService`.
struct ManagerShared {
    support_dir: PathBuf,
    store: RemoteControllerStore,
    connections: Mutex<HashMap<String, LiveConnection>>,
    connection_attempts: Mutex<HashMap<String, ConnectionAttempt>>,
    links: Mutex<HashMap<String, ControllerLinkState>>,
    // Selected path type (direct/relay) per connected device, for the UI badge.
    // Set when the transport reports a selected path; cleared when the link drops.
    paths: Mutex<HashMap<String, ControllerLinkPath>>,
    reconnecting: Mutex<HashSet<String>>,
    // Last failure from the background reconnect loop per device, so
    // `controller_for` can surface *why* a host stays "not ready" (offline host,
    // relay unreachable, dial timeout) instead of a generic message.
    last_errors: Mutex<HashMap<String, String>>,
    disconnected_devices: Mutex<HashSet<String>>,
}

struct LiveConnection {
    epoch: u64,
    controller: Arc<RemoteController>,
}

/// The device's current connect attempt, and the only authority on whether its
/// controller may still become the live link. `terminated` records that the
/// transport already reported this attempt's link closed — which can happen
/// before `connect_saved` even returns, since the state callback is wired
/// during the dial.
struct ConnectionAttempt {
    epoch: u64,
    terminated: bool,
}

impl ManagerShared {
    fn set_link(&self, device_id: &str, state: ControllerLinkState) {
        if let Ok(mut links) = self.links.lock() {
            links.insert(device_id.to_string(), state);
        }
    }

    fn set_connecting_link(&self, device_id: &str) {
        if self.last_error(device_id).is_none() {
            self.set_link(device_id, ControllerLinkState::Connecting);
        }
    }

    fn set_path(&self, device_id: &str, path: ControllerLinkPath) {
        if let Ok(mut paths) = self.paths.lock() {
            paths.insert(device_id.to_string(), path);
        }
    }

    fn clear_path(&self, device_id: &str) {
        if let Ok(mut paths) = self.paths.lock() {
            paths.remove(device_id);
        }
    }

    /// Record the address a live direct path is using, so a later dial can try
    /// it even after the host moved away from what its ticket recorded.
    ///
    /// Runs on every path report of a live link, so the common case is "already
    /// the most recent" — `with_direct_addr` reports that as `None` and nothing
    /// is written.
    fn remember_direct_addr(&self, device_id: &str, addr: &str) {
        // Read, change and write under the store's lock. Doing it here instead
        // would race other updates, and would let a callback holding a host it
        // read moments ago write it back after the user forgot the device.
        let result = self.store.update_existing(device_id, |host| {
            let mut transports = host.transports.clone();
            let slot = transports
                .iter_mut()
                .find(|transport| transport.kind == REMOTE_TRANSPORT_IROH)?;
            *slot = slot.with_direct_addr(addr)?;
            Some(SavedRemoteHost {
                transports,
                ..host.clone()
            })
        });
        self.report_address_write(device_id, "remembered_direct_addr", result);
    }

    /// Adopt the dial candidates the host reported in `host.info`.
    ///
    /// The host rebuilds these from its live endpoint on every reply, so this is
    /// where a saved host's addresses stop being a pairing-time snapshot. An
    /// empty report is not an instruction to forget — an older host omits the
    /// field entirely, and discarding a working candidate on the strength of
    /// silence would strand the device.
    ///
    /// The remembered direct addresses are carried across: the two sources are
    /// independent fallbacks and neither may erase the other.
    fn adopt_reported_transports(&self, device_id: &str, info: &serde_json::Value) {
        let Some(reported) = info.get("transports").and_then(|value| value.as_array()) else {
            return;
        };
        let reported: Vec<SavedRemoteTransport> =
            reported.iter().map(saved_transport_from_json).collect();
        let result = self.store.update_existing(device_id, |host| {
            // Carry the remembered addresses over before judging: they survive
            // the adoption, so a reported candidate with no relay and no ticket
            // is still dialable when we have an address we reached this host on.
            let next: Vec<SavedRemoteTransport> = reported
                .iter()
                .cloned()
                .map(|mut transport| {
                    if let Some(previous) = host
                        .transports
                        .iter()
                        .find(|item| item.kind == transport.kind)
                    {
                        transport.direct_addrs = previous.direct_addrs.clone();
                    }
                    transport
                })
                .filter(|transport| {
                    codux_remote_transport::is_dialable_iroh_transport(
                        &transport.kind,
                        &transport.ticket,
                        &transport.node_id,
                        &transport.relay_url,
                        !transport.direct_addrs.is_empty(),
                    )
                })
                .collect();
            if next.is_empty() || next == host.transports {
                return None;
            }
            Some(SavedRemoteHost {
                transports: next,
                ..host.clone()
            })
        });
        self.report_address_write(device_id, "adopted_host_transports", result);
    }

    /// Report the outcome of an address write, which is an optimisation rather
    /// than part of any connection.
    ///
    /// A failure here must not break the live link — the addresses in memory are
    /// still correct and the connection using them is unaffected — but it must
    /// not vanish either: the next cold start would silently fall back to stale
    /// addresses with no record of why.
    fn report_address_write(
        &self,
        device_id: &str,
        reason: &str,
        result: Result<Option<SavedRemoteHost>, String>,
    ) {
        if let Err(error) = result {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!(
                    "controller_addresses_not_persisted device={device_id} reason={reason} error={error}"
                ),
            );
        }
    }

    fn last_error(&self, device_id: &str) -> Option<String> {
        self.last_errors
            .lock()
            .ok()
            .and_then(|errors| errors.get(device_id).cloned())
    }

    fn next_connection_epoch(&self, device_id: &str) -> u64 {
        let mut attempts = self.connection_attempts.lock().unwrap();
        let attempt = attempts
            .entry(device_id.to_string())
            .or_insert(ConnectionAttempt {
                epoch: 0,
                terminated: false,
            });
        attempt.epoch = attempt.epoch.saturating_add(1);
        attempt.terminated = false;
        attempt.epoch
    }

    /// Run `update` only while `epoch` is still this device's current attempt,
    /// with the attempt held so `forget` cannot invalidate it midway. Returns
    /// `None` when the attempt is already stale.
    ///
    /// `connection_attempts` is the outermost lock: `update` may take `links`,
    /// `paths`, `connections` or `disconnected_devices`, never the other way
    /// round.
    fn with_current_connection_epoch<T>(
        &self,
        device_id: &str,
        epoch: u64,
        update: impl FnOnce() -> T,
    ) -> Option<T> {
        let attempts = self.connection_attempts.lock().ok()?;
        if attempts.get(device_id).map(|attempt| attempt.epoch) != Some(epoch) {
            return None;
        }
        Some(update())
    }

    /// Like [`with_current_connection_epoch`](Self::with_current_connection_epoch),
    /// but also refuses an attempt whose link the transport already closed.
    /// Publishing is the only operation that needs this: link-state bookkeeping
    /// still belongs to the current attempt after it dies, but its controller
    /// must never be handed out.
    fn with_live_connection_attempt<T>(
        &self,
        device_id: &str,
        epoch: u64,
        update: impl FnOnce() -> T,
    ) -> Option<T> {
        let attempts = self.connection_attempts.lock().ok()?;
        let attempt = attempts.get(device_id)?;
        if attempt.epoch != epoch || attempt.terminated {
            return None;
        }
        Some(update())
    }

    /// Record that this attempt's link is dead. The dial may still be in
    /// flight — the controller it eventually returns is already useless, and
    /// the reconnect loop starts a fresh attempt instead of publishing it.
    fn terminate_connection_attempt(&self, device_id: &str, epoch: u64) {
        if let Ok(mut attempts) = self.connection_attempts.lock()
            && let Some(attempt) = attempts.get_mut(device_id)
            && attempt.epoch == epoch
        {
            attempt.terminated = true;
        }
    }

    /// Publish `controller` as the live link for `device_id`.
    ///
    /// The project-id bridge is loaded from the persisted projections before the
    /// connection becomes visible. The catalog subscription refreshes it from
    /// the host's baseline moments later, but the desktop restores its projects
    /// as soon as a link reports connected, so a terminal, git, AI, or worktree
    /// request can be the first thing to cross a fresh connection — and a
    /// desktop id sent there would scope the host's work to a project it does
    /// not have. Every live connection is published here, so none can be
    /// reachable before its bridge is.
    ///
    /// Returns whether the controller became the live link; a refused publish
    /// leaves the device without a connection, so the caller must keep
    /// reconnecting rather than treat the dial as done.
    fn publish_connection(
        &self,
        device_id: &str,
        epoch: u64,
        controller: Arc<RemoteController>,
    ) -> bool {
        controller.set_project_id_pairs(
            crate::project_store::ProjectStore::new(self.support_dir.clone())
                .remote_project_id_pairs(device_id),
        );
        // Publishing the connection and its link state under one hold of the
        // attempt is what keeps a `forget` from landing between them and
        // leaving a forgotten device looking connected. A host that is already
        // gone — or an attempt whose link already closed — gets no connection.
        let published = self.with_live_connection_attempt(device_id, epoch, || {
            self.store.find(device_id)?;
            let mut connections = self.connections.lock().ok()?;
            let replaced = connections.insert(
                device_id.to_string(),
                LiveConnection {
                    epoch,
                    controller: Arc::clone(&controller),
                },
            );
            drop(connections);
            self.set_link(device_id, ControllerLinkState::Connected);
            Some(replaced)
        });
        match published.flatten() {
            Some(replaced) => {
                if let Some(replaced) = replaced {
                    Self::shutdown_controller(replaced.controller);
                }
                true
            }
            // Dead attempt, stale attempt, forgotten host, or a poisoned map:
            // this controller never becomes reachable, so it must not be left
            // running.
            None => {
                Self::shutdown_controller(controller);
                false
            }
        }
    }

    fn shutdown_controller(controller: Arc<RemoteController>) {
        crate::async_runtime::spawn(async move {
            controller.shutdown().await;
        });
    }

    /// Build the transport state handler for `device_id`: it records every link
    /// transition and, on a drop, evicts the dead controller and kicks off a
    /// background reconnect. Holds only a `Weak` so a forgotten manager lets the
    /// callback become a no-op.
    fn state_handler(self: &Arc<Self>, device_id: &str, epoch: u64) -> RemoteTransportStateHandler {
        let weak = Arc::downgrade(self);
        let device_id = device_id.to_string();
        Arc::new(move |_node: String, state: String| {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let Some(mapped) = ControllerLinkState::from_transport_state(&state) else {
                return;
            };
            if mapped == ControllerLinkState::Disconnected {
                // Kill the attempt before anything else: the dial that owns
                // this callback may still be in flight, and the controller it
                // returns is already dead.
                shared.terminate_connection_attempt(&device_id, epoch);
                let recorded = shared.with_current_connection_epoch(&device_id, epoch, || {
                    shared.set_link(&device_id, mapped);
                    shared.clear_path(&device_id);
                    if let Ok(mut devices) = shared.disconnected_devices.lock() {
                        devices.insert(device_id.clone());
                    }
                });
                if recorded.is_some() {
                    shared.handle_disconnect(&device_id);
                }
                return;
            }
            // Death is final for an attempt: the transport can still publish a
            // path or a connected marker for a link it already closed (the
            // stream task outlives the dial), and honouring that would show a
            // connected badge for a device the reconnect loop is still dialing.
            let _ = shared.with_live_connection_attempt(&device_id, epoch, || {
                if mapped == ControllerLinkState::Connecting {
                    shared.set_connecting_link(&device_id);
                } else {
                    shared.set_link(&device_id, mapped);
                }
                if let Some(path) = ControllerLinkPath::from_transport_state(&state) {
                    shared.set_path(&device_id, path);
                }
                if let Some(addr) = direct_addr_from_transport_state(&state) {
                    shared.remember_direct_addr(&device_id, addr);
                }
            });
        })
    }

    /// Ensure a background connect/reconnect loop is retrying this device.
    /// Idempotent: marks the device as reconnecting (so `controller_for`
    /// fast-fails instead of re-dialling on the calling thread) and spawns the
    /// loop only if one isn't already running. Returns `false` if the host has
    /// been forgotten — nothing to connect to.
    fn mark_reconnect_needed(self: &Arc<Self>, device_id: &str) -> Option<bool> {
        let mut reconnecting = self.reconnecting.lock().ok()?;
        // The saved host is the source of truth for "should we reconnect":
        // a forgotten device must not be resurrected.
        self.store.find(device_id)?;
        if reconnecting.contains(device_id) {
            return Some(false);
        }
        reconnecting.insert(device_id.to_string());
        Some(true)
    }

    fn spawn_reconnect_loop(self: &Arc<Self>, device_id: &str) {
        let shared = Arc::clone(self);
        let device_id = device_id.to_string();
        crate::async_runtime::spawn(async move {
            shared.reconnect_loop(device_id).await;
        });
    }

    fn ensure_reconnect_loop(self: &Arc<Self>, device_id: &str) -> bool {
        let Some(spawn_needed) = self.mark_reconnect_needed(device_id) else {
            return false;
        };
        if spawn_needed {
            self.spawn_reconnect_loop(device_id);
        }
        true
    }

    /// Hand back the reconnect ownership this worker holds, but only while
    /// `epoch` is still the live attempt *and* its connection is published: a
    /// drop can land between publishing and this teardown, and the callback
    /// that handles it sees the ownership still taken and starts no replacement
    /// worker. Returns `false` when the link is already gone again — the caller
    /// then keeps the ownership and keeps retrying, which is the only thing
    /// that can bring the host back.
    ///
    /// The attempt is what makes this airtight: the callback marks it
    /// terminated *before* it evicts the connection, so a handover that landed
    /// in between still sees a dead attempt.
    ///
    /// Locks are taken `reconnecting` → `connection_attempts` → `connections`;
    /// nothing takes `reconnecting` while holding either of the other two.
    fn release_reconnect_ownership(&self, device_id: &str, epoch: u64) -> bool {
        let Ok(mut reconnecting) = self.reconnecting.lock() else {
            return true;
        };
        let live = self
            .connection_attempts
            .lock()
            .map(|attempts| {
                attempts
                    .get(device_id)
                    .is_some_and(|attempt| attempt.epoch == epoch && !attempt.terminated)
            })
            .unwrap_or(false)
            && self
                .connections
                .lock()
                .map(|connections| connections.contains_key(device_id))
                .unwrap_or(false);
        // A forgotten host has nothing to retry, so the ownership goes back
        // even without a live link.
        if !live && self.store.find(device_id).is_some() {
            return false;
        }
        reconnecting.remove(device_id);
        true
    }

    /// React to a dropped link: drop the dead controller from the pool and
    /// ensure a reconnect loop is retrying.
    fn handle_disconnect(self: &Arc<Self>, device_id: &str) {
        // Mark reconnecting BEFORE evicting, so `controller_for` never observes
        // an evicted-but-not-yet-reconnecting host — which it would try to
        // re-dial synchronously, blocking the (possibly UI-thread) caller
        // against the offline host.
        let Some(spawn_needed) = self.mark_reconnect_needed(device_id) else {
            return;
        };
        let old = self
            .connections
            .lock()
            .ok()
            .and_then(|mut connections| connections.remove(device_id));
        if let Some(old) = old {
            Self::shutdown_controller(old.controller);
        }
        if spawn_needed {
            self.spawn_reconnect_loop(device_id);
        }
    }

    /// Retry `connect_saved` with capped exponential backoff until the link is
    /// re-established or the host is forgotten. The fresh controller is wired to
    /// the same state handler, so a later drop is caught again.
    async fn reconnect_loop(self: Arc<Self>, device_id: String) {
        let mut delay = Duration::from_secs(1);
        let mut attempt: u32 = 0;
        loop {
            // Claim the attempt before reading the saved host. If `forget`
            // follows, it invalidates this attempt; if it came first, the host
            // lookup fails and no callback can recreate forgotten link state.
            let epoch = self.next_connection_epoch(&device_id);
            let Some(host) = self.store.find(&device_id) else {
                if let Ok(mut attempts) = self.connection_attempts.lock()
                    && attempts.get(&device_id).map(|attempt| attempt.epoch) == Some(epoch)
                {
                    attempts.remove(&device_id);
                }
                // Forgotten while retrying: stop and forget the link state too.
                if let Ok(mut links) = self.links.lock() {
                    links.remove(&device_id);
                }
                self.clear_path(&device_id);
                break;
            };
            attempt += 1;
            let current = self.with_current_connection_epoch(&device_id, epoch, || {
                self.set_connecting_link(&device_id);
                // No path while dialing — clear the stale direct/relay marker so
                // the badge doesn't claim a route the dropped link no longer has.
                self.clear_path(&device_id);
            });
            if current.is_none() {
                break;
            }
            // Trace every attempt: a stuck/looping reconnect was previously
            // invisible in the runtime log (one hung dial looked identical to
            // "never tried"). The error string from `connect_saved` names the
            // real failure (relay online timeout, dial timeout, offline host).
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!("controller_connect attempt device={device_id} n={attempt}"),
            );
            let on_state = self.state_handler(&device_id, epoch);
            match RemoteController::connect_saved(&host, on_state).await {
                Ok(controller) => {
                    if self.publish_connection(&device_id, epoch, Arc::new(controller)) {
                        if let Ok(mut errors) = self.last_errors.lock() {
                            errors.remove(&device_id);
                        }
                        crate::runtime_trace::runtime_trace(
                            "remote",
                            &format!("controller_connect ok device={device_id} n={attempt}"),
                        );
                        if self.release_reconnect_ownership(&device_id, epoch) {
                            return;
                        }
                        // The link dropped again between publishing and the
                        // handover, so no replacement worker was started: this
                        // one keeps going, from the short delay a fresh link
                        // deserves.
                        crate::runtime_trace::runtime_trace(
                            "remote",
                            &format!("controller_connect relost device={device_id} n={attempt}"),
                        );
                        delay = Duration::from_secs(1);
                        continue;
                    }
                    // The dial succeeded but the link was already dead (the
                    // transport reported it closed before `connect_saved`
                    // returned) or the attempt was superseded. Nothing is
                    // published, so this loop is still the only thing that can
                    // bring the host back — keep retrying instead of exiting
                    // and leaving the device permanently unconnected.
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "controller_connect discarded device={device_id} n={attempt} retry_in_ms={}",
                            delay.as_millis()
                        ),
                    );
                    if self.store.find(&device_id).is_none() {
                        break;
                    }
                    tokio::time::sleep(delay).await;
                    delay = next_reconnect_delay(delay);
                }
                Err(error) => {
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "controller_connect failed device={device_id} n={attempt} retry_in_ms={} error={error}",
                            delay.as_millis()
                        ),
                    );
                    // Record why the reconnect failed so `controller_for` can
                    // surface it (offline host, relay unreachable, dial timeout);
                    // otherwise the UI only ever sees the generic "not ready".
                    let current = self.with_current_connection_epoch(&device_id, epoch, || {
                        if let Ok(mut errors) = self.last_errors.lock() {
                            errors.insert(device_id.clone(), error);
                        }
                        self.set_link(&device_id, ControllerLinkState::Disconnected);
                    });
                    if current.is_none() {
                        break;
                    }
                    tokio::time::sleep(delay).await;
                    delay = next_reconnect_delay(delay);
                }
            }
        }
        if let Ok(mut reconnecting) = self.reconnecting.lock() {
            reconnecting.remove(&device_id);
        }
    }
}

fn next_reconnect_delay(delay: Duration) -> Duration {
    (delay * 2).min(RECONNECT_MAX_DELAY)
}

pub struct RemoteControllerManager {
    shared: Arc<ManagerShared>,
}

impl RemoteControllerManager {
    pub fn new(support_dir: PathBuf) -> Self {
        Self {
            shared: Arc::new(ManagerShared {
                support_dir: support_dir.clone(),
                store: RemoteControllerStore::new(support_dir),
                connections: Mutex::new(HashMap::new()),
                connection_attempts: Mutex::new(HashMap::new()),
                links: Mutex::new(HashMap::new()),
                paths: Mutex::new(HashMap::new()),
                reconnecting: Mutex::new(HashSet::new()),
                last_errors: Mutex::new(HashMap::new()),
                disconnected_devices: Mutex::new(HashSet::new()),
            }),
        }
    }

    pub fn saved_hosts(&self) -> Vec<SavedRemoteHost> {
        self.shared.store.list()
    }

    /// Eagerly bring up — and keep alive — the link to every saved host, so a
    /// paired device connects on its own (on launch, on an explicit reconnect,
    /// or on the periodic safety-net) without needing a project opened on it to
    /// trigger the lazy first-use dial. Fully idempotent: skips hosts that
    /// already have a live controller (never tears down a working link) and
    /// joins an in-flight reconnect loop instead of starting a duplicate — so it
    /// is safe to call on a timer. Returns how many saved hosts were engaged.
    pub fn ensure_saved_hosts_connected(&self) -> usize {
        let connected: HashSet<String> = self
            .shared
            .connections
            .lock()
            .map(|connections| connections.keys().cloned().collect())
            .unwrap_or_default();
        let mut engaged = 0;
        for host in self.shared.store.list() {
            if connected.contains(&host.device_id) {
                continue;
            }
            if self.shared.ensure_reconnect_loop(&host.device_id) {
                engaged += 1;
            }
        }
        engaged
    }

    /// Per-device link states for the UI (project connection badge + terminal
    /// re-attach trigger). Only devices that have been connected at least once
    /// appear; a device absent here has not been reached yet.
    pub fn link_states(&self) -> HashMap<String, ControllerLinkState> {
        self.shared
            .links
            .lock()
            .map(|links| links.clone())
            .unwrap_or_default()
    }

    /// Per-device link path types (direct/relay) for the connection badge. Only
    /// present while a device is connected and the transport has selected a path.
    pub fn link_paths(&self) -> HashMap<String, ControllerLinkPath> {
        self.shared
            .paths
            .lock()
            .map(|paths| paths.clone())
            .unwrap_or_default()
    }

    /// Pair with a host from a pasted ticket, persist it, and cache the live
    /// connection (wired for liveness) so the new device is immediately usable.
    pub fn pair(&self, ticket_input: &str, device_name: &str) -> Result<SavedRemoteHost, String> {
        let ticket = parse_pairing_ticket(ticket_input)?;
        let device_id = new_device_id();
        let epoch = self.shared.next_connection_epoch(&device_id);
        let on_state = self.shared.state_handler(&device_id, epoch);
        let (controller, saved) = crate::async_runtime::block_on(RemoteController::pair(
            &ticket,
            device_name,
            device_id,
            on_state,
        ))?;
        self.shared.store.upsert(saved.clone())?;
        // A link that died during pairing leaves the host saved but unreachable;
        // hand it to the reconnect loop so it comes up on its own.
        if !self
            .shared
            .publish_connection(&saved.device_id, epoch, Arc::new(controller))
        {
            self.shared.ensure_reconnect_loop(&saved.device_id);
        }
        Ok(saved)
    }

    /// Get the live controller for a paired device, or report it unavailable.
    ///
    /// This NEVER dials synchronously. The connect can take seconds (offline
    /// host → dial timeout; slow holepunch), and callers run on the UI thread
    /// (a project switch loading worktrees/git) or the blocking pool — a
    /// synchronous dial there freezes the UI and exhausts the pool (the "busy"
    /// indicator). Instead we hand off to a background connect/reconnect loop and
    /// report unavailable now; the desktop's link-state poll refreshes the
    /// project once the loop establishes the link.
    /// Collect `terminal.status` pushes from every live controller link
    /// without dialing anything.
    pub fn drain_pushed_terminal_status(&self) -> Vec<(String, serde_json::Value)> {
        let controllers: Vec<(String, Arc<RemoteController>)> = self
            .shared
            .connections
            .lock()
            .map(|connections| {
                connections
                    .iter()
                    .map(|(device_id, connection)| {
                        (device_id.clone(), Arc::clone(&connection.controller))
                    })
                    .collect()
            })
            .unwrap_or_default();
        controllers
            .iter()
            .flat_map(|(device_id, controller)| {
                controller
                    .drain_pushed_terminal_status()
                    .into_iter()
                    .map(|payload| (device_id.clone(), payload))
            })
            .collect()
    }

    pub fn drain_hosted_workspace_updates(&self) -> Vec<(String, String, serde_json::Value)> {
        let controllers: Vec<(String, Arc<RemoteController>)> = self
            .shared
            .connections
            .lock()
            .map(|connections| {
                connections
                    .iter()
                    .map(|(device_id, connection)| {
                        (device_id.clone(), Arc::clone(&connection.controller))
                    })
                    .collect()
            })
            .unwrap_or_default();
        controllers
            .iter()
            .flat_map(|(device_id, controller)| {
                controller
                    .drain_hosted_workspace_updates()
                    .into_iter()
                    .map(|(kind, payload)| (device_id.clone(), kind, payload))
            })
            .collect()
    }

    /// Every live host link with the epoch of the connection that carries it.
    /// A reconnect bumps the epoch, which is how per-connection work (the
    /// project-catalog subscription) knows to run again.
    pub fn connected_controllers(&self) -> Vec<(String, u64, Arc<RemoteController>)> {
        let Ok(connections) = self.shared.connections.lock() else {
            return Vec::new();
        };
        connections
            .iter()
            .map(|(device_id, connection)| {
                (
                    device_id.clone(),
                    connection.epoch,
                    Arc::clone(&connection.controller),
                )
            })
            .collect()
    }

    /// Epoch of this exact controller while it remains the published live link.
    /// Device id plus epoch is insufficient when a stale caller accidentally
    /// pairs an old controller with the replacement controller's current epoch.
    pub(crate) fn live_connection_epoch(
        &self,
        device_id: &str,
        controller: &RemoteController,
    ) -> Option<u64> {
        let Ok(connections) = self.shared.connections.lock() else {
            return None;
        };
        let current = connections.get(device_id)?;
        if !std::ptr::eq(current.controller.as_ref(), controller) {
            return None;
        }
        Some(current.epoch)
    }

    pub fn drain_disconnected_devices(&self) -> Vec<String> {
        self.shared
            .disconnected_devices
            .lock()
            .map(|mut devices| devices.drain().collect())
            .unwrap_or_default()
    }

    /// Fetch a host's identity and capabilities, adopting the dial candidates it
    /// reports along the way.
    ///
    /// The adoption lives here, not at the call sites, because "saved addresses
    /// follow the host" is this manager's invariant: it owns the store. Every
    /// caller that asks a host who it is refreshes the record for free, and a
    /// caller added later cannot forget to.
    pub fn host_info(&self, device_id: &str) -> Result<serde_json::Value, String> {
        let controller = self.controller_for(device_id)?;
        let info = controller.host_info()?;
        self.adopt_if_current(device_id, &controller, &info);
        Ok(info)
    }

    /// [`Self::host_info`] for a caller that already holds the controller it
    /// wants to ask — an epoch-guarded path, where re-resolving the device could
    /// hand back a different connection than the one being validated.
    pub fn host_info_via(
        &self,
        device_id: &str,
        controller: &RemoteController,
    ) -> Result<serde_json::Value, String> {
        let info = controller.host_info()?;
        self.adopt_if_current(device_id, controller, &info);
        Ok(info)
    }

    /// Adopt reported addresses only when the connection that reported them is
    /// still this device's live link.
    ///
    /// A reply describes the connection that produced it. A request can outlive
    /// its connection — the link drops and a reconnect replaces it while the
    /// reply is still in flight — and the answer then describes a link nobody
    /// uses. Persisting it would overwrite the newer connection's addresses with
    /// older ones. Callers checked this, but only after the write had happened.
    fn adopt_if_current(
        &self,
        device_id: &str,
        controller: &RemoteController,
        info: &serde_json::Value,
    ) {
        if self.live_connection_epoch(device_id, controller).is_none() {
            return;
        }
        self.shared.adopt_reported_transports(device_id, info);
    }

    /// [`Self::host_info`] for callers that must block on the connect.
    pub fn host_info_blocking(
        &self,
        device_id: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let controller = self.controller_for_blocking(device_id, timeout)?;
        let info = controller.host_info()?;
        self.adopt_if_current(device_id, &controller, &info);
        Ok(info)
    }

    pub fn controller_for(&self, device_id: &str) -> Result<Arc<RemoteController>, String> {
        if let Ok(connections) = self.shared.connections.lock()
            && let Some(controller) = connections
                .get(device_id)
                .map(|connection| Arc::clone(&connection.controller))
        {
            return Ok(controller);
        }
        if self.shared.ensure_reconnect_loop(device_id) {
            match self.shared.last_error(device_id) {
                Some(error) => Err(format!(
                    "Remote host {device_id} is connecting; not ready yet (last attempt failed: {error})."
                )),
                None => Err(format!(
                    "Remote host {device_id} is connecting; not ready yet."
                )),
            }
        } else {
            Err(format!("No saved remote host for device {device_id}."))
        }
    }

    /// Like [`controller_for`](Self::controller_for), but waits (bounded) for a
    /// not-yet-connected host to come up instead of failing immediately.
    ///
    /// `controller_for` never blocks because it's called from latency-sensitive
    /// and UI-thread paths. But the add-project file browser and its "new
    /// folder"/create run on the blocking pool in response to an explicit user
    /// action, where a short wait (with the picker showing its loading state) is
    /// exactly right — it turns the "first click shows nothing, second click
    /// works" race (the first call only *kicks off* the background reconnect)
    /// into a single successful call. NEVER call this from the UI thread.
    pub fn controller_for_blocking(
        &self,
        device_id: &str,
        timeout: Duration,
    ) -> Result<Arc<RemoteController>, String> {
        // Already connected (re-browsing the same host): no wait.
        if let Ok(connections) = self.shared.connections.lock()
            && let Some(controller) = connections
                .get(device_id)
                .map(|connection| Arc::clone(&connection.controller))
        {
            return Ok(controller);
        }
        // Kick off (or join) the background reconnect loop, then poll the pool
        // until it lands the link or we hit the deadline.
        if !self.shared.ensure_reconnect_loop(device_id) {
            return Err(format!("No saved remote host for device {device_id}."));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(connections) = self.shared.connections.lock()
                && let Some(controller) = connections
                    .get(device_id)
                    .map(|connection| Arc::clone(&connection.controller))
            {
                return Ok(controller);
            }
            if Instant::now() >= deadline {
                // Surface the real reconnect failure (offline host, relay
                // unreachable, dial timeout) so the picker shows *why*.
                return Err(match self.shared.last_error(device_id) {
                    Some(error) => format!(
                        "Remote host {device_id} is not reachable yet (last attempt failed: {error})."
                    ),
                    None => format!("Remote host {device_id} is still connecting; try again."),
                });
            }
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    /// Publish a controller as the live link for `device_id` without pairing or
    /// dialing, so runtime-level tests can drive a fake host.
    #[cfg(test)]
    pub(crate) fn insert_test_connection(
        &self,
        device_id: &str,
        controller: Arc<RemoteController>,
    ) {
        // A published connection requires a saved host, the same as a real
        // pairing does; without one `publish_connection` would (correctly)
        // refuse the test link.
        self.shared
            .store
            .upsert(SavedRemoteHost {
                device_id: device_id.to_string(),
                host_id: format!("test-host-{device_id}"),
                host_name: "Test host".to_string(),
                device_token: "test-token".to_string(),
                platform: "linux".to_string(),
                transports: Vec::new(),
            })
            .expect("save test host");
        let epoch = self.shared.next_connection_epoch(device_id);
        assert!(self.shared.publish_connection(device_id, epoch, controller));
    }

    /// Drop a paired host and any live connection or link state for it.
    pub fn forget(&self, device_id: &str) -> Result<Vec<SavedRemoteHost>, String> {
        // Remove saved intent first, so no fresh reconnect can start while the
        // in-flight epoch is being invalidated.
        let hosts = self.shared.store.remove(device_id)?;
        if let Ok(mut devices) = self.shared.disconnected_devices.lock() {
            devices.insert(device_id.to_string());
        }
        let old = {
            let mut attempts = self.shared.connection_attempts.lock().ok();
            if let Some(attempts) = attempts.as_mut() {
                attempts.remove(device_id);
            }
            self.shared
                .connections
                .lock()
                .ok()
                .and_then(|mut connections| connections.remove(device_id))
        };
        if let Some(old) = old {
            ManagerShared::shutdown_controller(old.controller);
        }
        if let Ok(mut links) = self.shared.links.lock() {
            links.remove(device_id);
        }
        self.shared.clear_path(device_id);
        Ok(hosts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_host_with(transport: SavedRemoteTransport) -> SavedRemoteHost {
        SavedRemoteHost {
            device_id: "dev-1".to_string(),
            host_id: "host-1".to_string(),
            host_name: String::new(),
            device_token: "token".to_string(),
            platform: String::new(),
            transports: vec![transport],
        }
    }

    fn iroh_transport() -> SavedRemoteTransport {
        SavedRemoteTransport {
            kind: REMOTE_TRANSPORT_IROH.to_string(),
            node_id: "node-x".to_string(),
            relay_url: "https://relay.example".to_string(),
            relay_authentication: String::new(),
            ticket: "ticket-v1".to_string(),
            direct_addrs: Vec::new(),
        }
    }

    fn manager_with(host: SavedRemoteHost) -> RemoteControllerManager {
        let dir = std::env::temp_dir().join(format!(
            "codux-controller-manager-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("support dir");
        let manager = RemoteControllerManager::new(dir);
        manager.shared.store.upsert(host).expect("seed host");
        manager
    }

    #[test]
    fn direct_addr_is_read_only_from_a_direct_path() {
        assert_eq!(
            direct_addr_from_transport_state("connected:path=direct;addr=10.8.0.3:41234"),
            Some("10.8.0.3:41234")
        );
        // A relay path's address is the relay's own and says nothing about
        // where the host is.
        assert_eq!(
            direct_addr_from_transport_state(
                "connected:path=relay;addr=https://relay.example;relayUrl=https://relay.example"
            ),
            None
        );
        assert_eq!(direct_addr_from_transport_state("connected"), None);
        assert_eq!(
            direct_addr_from_transport_state("connected:path=direct"),
            None
        );
    }

    #[test]
    fn a_working_direct_path_is_remembered_and_promoted() {
        let manager = manager_with(saved_host_with(iroh_transport()));

        manager
            .shared
            .remember_direct_addr("dev-1", "10.8.0.3:41234");
        manager
            .shared
            .remember_direct_addr("dev-1", "192.168.1.5:41234");

        let saved = manager.shared.store.find("dev-1").expect("host");
        assert_eq!(
            saved.transports[0].direct_addrs,
            vec![
                "192.168.1.5:41234".to_string(),
                "10.8.0.3:41234".to_string()
            ],
            "most recent first, no duplicates"
        );
    }

    #[test]
    fn a_reported_candidate_set_is_adopted_and_keeps_remembered_addresses() {
        let mut transport = iroh_transport();
        transport.direct_addrs = vec!["10.8.0.3:41234".to_string()];
        let manager = manager_with(saved_host_with(transport));

        manager.shared.adopt_reported_transports(
            "dev-1",
            &serde_json::json!({
                "transports": [{
                    "kind": "iroh",
                    "nodeId": "node-x",
                    "relayUrl": "",
                    "ticket": "ticket-v2",
                }],
            }),
        );

        let saved = manager.shared.store.find("dev-1").expect("host");
        assert_eq!(saved.transports[0].ticket, "ticket-v2", "adopted");
        assert_eq!(
            saved.transports[0].direct_addrs,
            vec!["10.8.0.3:41234".to_string()],
            "the two address sources are independent; neither may erase the other"
        );
    }

    #[test]
    fn silence_and_undialable_reports_leave_the_record_alone() {
        let manager = manager_with(saved_host_with(iroh_transport()));

        // No field at all: an older host build.
        manager
            .shared
            .adopt_reported_transports("dev-1", &serde_json::json!({}));
        // Present but empty.
        manager
            .shared
            .adopt_reported_transports("dev-1", &serde_json::json!({ "transports": [] }));
        // Present but carrying nothing the dial path would accept — adopting
        // this would strand the device until it is re-paired.
        manager.shared.adopt_reported_transports(
            "dev-1",
            &serde_json::json!({ "transports": [{ "kind": "iroh", "nodeId": "node-x" }] }),
        );

        let saved = manager.shared.store.find("dev-1").expect("host");
        assert_eq!(saved.transports[0].ticket, "ticket-v1");
        assert_eq!(saved.transports[0].relay_url, "https://relay.example");
    }

    fn manager(name: &str) -> RemoteControllerManager {
        RemoteControllerManager::new(
            std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4())),
        )
    }

    fn test_controller(device_id: &str) -> Arc<RemoteController> {
        let hub = codux_remote_transport::LocalMemoryTransportHub::new();
        Arc::new(RemoteController::with_test_transport(
            device_id,
            move |route| {
                hub.connect(
                    "controller",
                    codux_protocol::RemoteTransportKind::Iroh,
                    Arc::new(move |_, data| route(&data)),
                    Arc::new(|_, _| {}),
                )
            },
        ))
    }

    fn save_test_host(manager: &RemoteControllerManager, device_id: &str) {
        manager
            .shared
            .store
            .upsert(SavedRemoteHost {
                device_id: device_id.to_string(),
                host_id: "host-1".to_string(),
                host_name: "Test host".to_string(),
                device_token: "token-1".to_string(),
                platform: "linux".to_string(),
                transports: Vec::new(),
            })
            .unwrap();
    }

    #[test]
    fn reconnect_delay_caps_at_background_ceiling() {
        let mut delay = Duration::from_secs(1);
        for _ in 0..10 {
            delay = next_reconnect_delay(delay);
        }
        assert_eq!(delay, RECONNECT_MAX_DELAY);
    }

    #[test]
    fn connected_controller_keeps_its_published_epoch_during_next_dial() {
        let manager = manager("codux-controller-published-epoch");
        save_test_host(&manager, "device-1");
        let old = test_controller("device-1");
        let old_epoch = manager.shared.next_connection_epoch("device-1");
        manager
            .shared
            .publish_connection("device-1", old_epoch, Arc::clone(&old));

        let next_epoch = manager.shared.next_connection_epoch("device-1");
        assert!(next_epoch > old_epoch);
        let connected = manager.connected_controllers();
        assert_eq!(connected.len(), 1);
        assert_eq!(connected[0].1, old_epoch);
        assert!(Arc::ptr_eq(&connected[0].2, &old));
        assert_eq!(
            manager.live_connection_epoch("device-1", &old),
            Some(old_epoch)
        );

        let new = test_controller("device-1");
        manager
            .shared
            .publish_connection("device-1", next_epoch, Arc::clone(&new));
        assert_eq!(manager.live_connection_epoch("device-1", &old), None);
        assert_eq!(
            manager.live_connection_epoch("device-1", &new),
            Some(next_epoch)
        );
    }

    #[test]
    fn a_superseded_connection_cannot_write_its_reply_into_the_store() {
        // A `host.info` request can outlive the connection that sent it: the
        // link drops, a reconnect replaces it, and the reply arrives describing
        // a link nobody uses. Persisting it would overwrite the live
        // connection's addresses with older ones.
        let manager = manager("codux-controller-superseded-adopt");
        save_test_host(&manager, "device-1");
        let superseded = test_controller("device-1");

        manager.adopt_if_current(
            "device-1",
            &superseded,
            &serde_json::json!({
                "transports": [{
                    "kind": "iroh",
                    "nodeId": "node-x",
                    "relayUrl": "https://relay.example",
                    "ticket": "ticket-from-a-dead-link",
                }],
            }),
        );

        let saved = manager.shared.store.find("device-1").expect("host");
        assert!(
            saved.transports.is_empty(),
            "a reply from a connection that is no longer live must not be stored"
        );
    }

    #[test]
    fn invalidated_connection_epoch_cannot_be_published() {
        let manager = manager("codux-controller-invalidated-epoch");
        save_test_host(&manager, "device-1");
        let epoch = manager.shared.next_connection_epoch("device-1");
        manager
            .shared
            .connection_attempts
            .lock()
            .unwrap()
            .remove("device-1");

        assert!(
            !manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );

        assert!(manager.connected_controllers().is_empty());
        assert!(!manager.link_states().contains_key("device-1"));
    }

    #[test]
    fn forgotten_device_cannot_be_republished_with_a_new_epoch() {
        let manager = manager("codux-controller-forgotten-epoch");
        save_test_host(&manager, "device-1");

        // A reconnect loop can hold this stale host after `forget` removes it.
        assert!(manager.shared.store.find("device-1").is_some());
        manager.forget("device-1").unwrap();
        let stale_epoch = manager.shared.next_connection_epoch("device-1");
        assert!(!manager.shared.publish_connection(
            "device-1",
            stale_epoch,
            test_controller("device-1")
        ));

        assert!(manager.connected_controllers().is_empty());
        assert!(!manager.link_states().contains_key("device-1"));
    }

    #[test]
    fn forgotten_device_callback_cannot_restore_link_state() {
        let manager = manager("codux-controller-forgotten-callback");
        save_test_host(&manager, "device-1");
        let epoch = manager.shared.next_connection_epoch("device-1");
        let on_state = manager.shared.state_handler("device-1", epoch);
        on_state("node-1".to_string(), "connected:path=direct".to_string());

        manager.forget("device-1").unwrap();
        on_state("node-1".to_string(), "connecting".to_string());
        on_state("node-1".to_string(), "connected:path=relay".to_string());
        on_state("node-1".to_string(), "closed".to_string());

        assert!(!manager.link_states().contains_key("device-1"));
        assert!(!manager.link_paths().contains_key("device-1"));
        assert_eq!(manager.drain_disconnected_devices(), ["device-1"]);
    }

    #[test]
    fn reconnect_without_saved_host_releases_its_epoch() {
        let manager = manager("codux-controller-missing-host-epoch");

        crate::async_runtime::block_on(
            Arc::clone(&manager.shared).reconnect_loop("device-1".to_string()),
        );

        assert!(
            !manager
                .shared
                .connection_attempts
                .lock()
                .unwrap()
                .contains_key("device-1")
        );
        assert!(!manager.link_states().contains_key("device-1"));
    }

    #[test]
    fn link_closed_before_connect_returns_refuses_the_dead_controller() {
        let manager = manager("codux-controller-dead-on-arrival");
        save_test_host(&manager, "device-1");
        // A reconnect loop already owns this device, exactly as it does when
        // the drop happens mid-dial.
        manager
            .shared
            .reconnecting
            .lock()
            .unwrap()
            .insert("device-1".to_string());
        let epoch = manager.shared.next_connection_epoch("device-1");
        let on_state = manager.shared.state_handler("device-1", epoch);

        // The transport reports the link closed while `connect_saved` is still
        // running, so the controller it returns is already dead.
        on_state("node-1".to_string(), "closed".to_string());
        assert!(
            !manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );

        assert!(manager.connected_controllers().is_empty());
        assert_eq!(
            manager.link_states().get("device-1"),
            Some(&ControllerLinkState::Disconnected)
        );
        // The stream task outlives the dial and can still publish a path for
        // the link it already closed; a dead attempt must not look connected.
        on_state("node-1".to_string(), "connected:path=direct".to_string());
        assert_eq!(
            manager.link_states().get("device-1"),
            Some(&ControllerLinkState::Disconnected)
        );
        assert!(!manager.link_paths().contains_key("device-1"));
        // The next attempt is publishable again.
        let next_epoch = manager.shared.next_connection_epoch("device-1");
        assert!(manager.shared.publish_connection(
            "device-1",
            next_epoch,
            test_controller("device-1")
        ));
    }

    #[test]
    fn drop_between_publish_and_teardown_keeps_the_reconnect_worker() {
        let manager = manager("codux-controller-teardown-race");
        save_test_host(&manager, "device-1");
        // This worker owns the device's reconnect, as the loop does while it
        // dials.
        manager
            .shared
            .reconnecting
            .lock()
            .unwrap()
            .insert("device-1".to_string());
        let epoch = manager.shared.next_connection_epoch("device-1");
        let on_state = manager.shared.state_handler("device-1", epoch);
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );

        // The exact interleave the callback opens: it marks the attempt dead
        // before it evicts the connection, and the worker's handover lands in
        // between — the connection is still published, but the link is gone and
        // the callback (seeing the ownership taken) starts no replacement.
        manager
            .shared
            .terminate_connection_attempt("device-1", epoch);
        assert_eq!(manager.connected_controllers().len(), 1);
        assert!(
            !manager
                .shared
                .release_reconnect_ownership("device-1", epoch)
        );
        assert!(
            manager
                .shared
                .reconnecting
                .lock()
                .unwrap()
                .contains("device-1"),
            "a dead attempt must not hand the reconnect ownership back"
        );

        // The rest of the callback runs: eviction, still no replacement worker.
        on_state("node-1".to_string(), "closed".to_string());
        assert!(manager.connected_controllers().is_empty());
        assert!(
            !manager
                .shared
                .release_reconnect_ownership("device-1", epoch)
        );
        assert!(
            manager
                .shared
                .reconnecting
                .lock()
                .unwrap()
                .contains("device-1"),
            "the worker keeps retrying instead of leaving the host unconnected"
        );

        // Once the link is really up, the ownership goes back.
        let epoch = manager.shared.next_connection_epoch("device-1");
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );
        assert!(
            manager
                .shared
                .release_reconnect_ownership("device-1", epoch)
        );
        assert!(
            !manager
                .shared
                .reconnecting
                .lock()
                .unwrap()
                .contains("device-1")
        );
    }

    #[test]
    fn disconnected_device_is_drained_once() {
        let manager = manager("codux-controller-disconnect-drain");
        manager
            .shared
            .set_link("device-1", ControllerLinkState::Disconnected);
        manager
            .shared
            .disconnected_devices
            .lock()
            .unwrap()
            .insert("device-1".to_string());

        assert_eq!(manager.drain_disconnected_devices(), ["device-1"]);
        assert!(manager.drain_disconnected_devices().is_empty());
    }

    #[test]
    fn reconnect_does_not_discard_undrained_disconnect_event() {
        let manager = manager("codux-controller-reconnect-drain");
        manager
            .shared
            .disconnected_devices
            .lock()
            .unwrap()
            .insert("device-1".to_string());
        manager
            .shared
            .set_link("device-1", ControllerLinkState::Connected);

        assert_eq!(manager.drain_disconnected_devices(), ["device-1"]);
    }

    #[test]
    fn forgetting_device_emits_status_cleanup_event() {
        let manager = manager("codux-controller-forget-cleanup");

        manager.forget("device-1").unwrap();

        assert_eq!(manager.drain_disconnected_devices(), ["device-1"]);
    }

    #[test]
    fn latency_telemetry_does_not_change_link_lifecycle() {
        assert_eq!(
            ControllerLinkState::from_transport_state("latency:rtt=12;path=direct"),
            None
        );
        assert_eq!(
            ControllerLinkState::from_transport_state("connected:path=relay"),
            Some(ControllerLinkState::Connected)
        );
        assert_eq!(
            ControllerLinkState::from_transport_state("closed"),
            Some(ControllerLinkState::Disconnected)
        );
    }
}
