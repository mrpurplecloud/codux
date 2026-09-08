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
use codux_remote_transport::{RemoteTransportCandidate, RemoteTransportStateHandler};
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
    controllers: Mutex<ControllerLifecycle>,
    links: Mutex<HashMap<String, ControllerLinkState>>,
    // Selected path type (direct/relay) per connected device, for the UI badge.
    // Set when the transport reports a selected path; cleared when the link drops.
    paths: Mutex<HashMap<String, ControllerLinkPath>>,
    // Last failure from the background reconnect loop per device, so
    // `controller_for` can surface *why* a host stays "not ready" (offline host,
    // relay unreachable, dial timeout) instead of a generic message.
    last_errors: Mutex<HashMap<String, String>>,
    disconnected_devices: Mutex<HashSet<String>>,
    #[cfg(test)]
    disconnect_pause: Mutex<Option<DisconnectPause>>,
}

#[derive(Default)]
struct ControllerLifecycle {
    next_owner: u64,
    slots: HashMap<String, ControllerSlot>,
}

#[derive(Default)]
struct ControllerSlot {
    attempt: Option<ConnectionAttempt>,
    live: Option<LiveConnection>,
    reconnect_worker: Option<ReconnectWorker>,
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
    worker: Option<u64>,
    terminated: bool,
    pending_path: Option<ControllerLinkPath>,
    pending_direct_addr: Option<String>,
}

#[derive(Clone, Copy)]
struct ReconnectWorker {
    owner: u64,
}

#[cfg(test)]
struct DisconnectPause {
    accepted: std::sync::mpsc::SyncSender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
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
                    codux_remote_transport::build_iroh_dial_target(
                        &RemoteTransportCandidate {
                            kind: transport.kind.clone(),
                            url: String::new(),
                            node_id: transport.node_id.clone(),
                            relay_url: transport.relay_url.clone(),
                            ticket: transport.ticket.clone(),
                            relay_authentication: transport.relay_authentication.clone(),
                        },
                        &transport.direct_addrs,
                    )
                    .is_ok()
                })
                // Only now, on candidates the dial path would already accept:
                // the host publishes the relay it is *connected to* and omits
                // the key when it has none, so a host that started while its
                // relay was down reports no relay at all, and its ticket
                // carries none either. Keeping the one we already know leaves a
                // relay route for the case direct dialling cannot cover.
                //
                // Deliberately after the filter, not before. A report carrying
                // nothing else usable must still be rejected -- lending it a
                // remembered relay would let it pass and then overwrite the
                // ticket it did not bring, stranding the device until it is
                // re-paired.
                .map(|mut transport| {
                    if transport.relay_url.trim().is_empty()
                        && let Some(previous) = host
                            .transports
                            .iter()
                            .find(|item| item.kind == transport.kind)
                    {
                        transport.relay_url = previous.relay_url.clone();
                    }
                    transport
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
    /// A failure here must not break the live link. The store retains the latest
    /// in-memory snapshot and owns retry; this helper only makes the first failed
    /// projection observable with the device and semantic reason attached.
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
                    "controller_addresses_persist_pending device={device_id} reason={reason} error={error}"
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

    fn allocate_owner(lifecycle: &mut ControllerLifecycle) -> u64 {
        lifecycle.next_owner = lifecycle
            .next_owner
            .checked_add(1)
            .expect("remote controller owner space exhausted");
        lifecycle.next_owner
    }

    /// Remove an explicit attempt that never became publishable. Exact epoch
    /// matching keeps late error cleanup from touching a replacement owner.
    fn abort_unpublished_attempt(&self, device_id: &str, epoch: u64) -> bool {
        let Ok(mut lifecycle) = self.controllers.lock() else {
            return false;
        };
        let Some(slot) = lifecycle.slots.get_mut(device_id) else {
            return false;
        };
        let owns_unpublished_attempt = slot
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.epoch == epoch && attempt.worker.is_none())
            && !slot.live.as_ref().is_some_and(|live| live.epoch == epoch);
        if !owns_unpublished_attempt {
            return false;
        }
        slot.attempt = None;
        if slot.live.is_some() {
            return true;
        }
        slot.reconnect_worker = None;
        lifecycle.slots.remove(device_id);
        if let Ok(mut links) = self.links.lock() {
            links.remove(device_id);
        }
        self.clear_path(device_id);
        if let Ok(mut errors) = self.last_errors.lock() {
            errors.remove(device_id);
        }
        if let Ok(mut devices) = self.disconnected_devices.lock() {
            devices.remove(device_id);
        }
        true
    }

    #[cfg(test)]
    fn pause_next_disconnect_for_test(
        &self,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (accepted, accepted_rx) = std::sync::mpsc::sync_channel(0);
        let (resume, resume_rx) = std::sync::mpsc::sync_channel(0);
        *self.disconnect_pause.lock().unwrap() = Some(DisconnectPause {
            accepted,
            resume: Mutex::new(resume_rx),
        });
        (accepted_rx, resume)
    }

    #[cfg(test)]
    fn pause_accepted_disconnect_for_test(&self) {
        let pause = self
            .disconnect_pause
            .lock()
            .ok()
            .and_then(|mut pause| pause.take());
        if let Some(pause) = pause {
            let _ = pause.accepted.send(());
            let _ = pause.resume.lock().map(|resume| resume.recv());
        }
    }

    fn next_connection_epoch(&self, device_id: &str) -> u64 {
        let mut lifecycle = self.controllers.lock().unwrap();
        let epoch = Self::allocate_owner(&mut lifecycle);
        let slot = lifecycle.slots.entry(device_id.to_string()).or_default();
        // An explicit pairing/test attempt supersedes any background worker.
        // That worker retains its token locally, but every later transition
        // will reject it against this slot.
        slot.reconnect_worker = None;
        slot.attempt = Some(ConnectionAttempt {
            epoch,
            worker: None,
            terminated: false,
            pending_path: None,
            pending_direct_addr: None,
        });
        epoch
    }

    fn begin_reconnect_attempt(&self, device_id: &str, worker: u64) -> Option<u64> {
        let mut lifecycle = self.controllers.lock().ok()?;
        let slot = lifecycle.slots.get_mut(device_id)?;
        if slot.reconnect_worker.map(|owner| owner.owner) != Some(worker) {
            return None;
        }
        if slot.live.is_some() {
            slot.reconnect_worker = None;
            return None;
        }
        let epoch = Self::allocate_owner(&mut lifecycle);
        lifecycle.slots.get_mut(device_id)?.attempt = Some(ConnectionAttempt {
            epoch,
            worker: Some(worker),
            terminated: false,
            pending_path: None,
            pending_direct_addr: None,
        });
        Some(epoch)
    }

    /// Run `update` only while `epoch` is still this device's current attempt,
    /// with the attempt held so `forget` cannot invalidate it midway. Returns
    /// `None` when the attempt is already stale.
    ///
    /// The controller-lifecycle lock stays held through `update`, so ownership
    /// cannot change between the precondition and the side effect it protects.
    fn with_current_connection_epoch<T>(
        &self,
        device_id: &str,
        epoch: u64,
        update: impl FnOnce() -> T,
    ) -> Option<T> {
        let lifecycle = self.controllers.lock().ok()?;
        if lifecycle
            .slots
            .get(device_id)
            .and_then(|slot| slot.attempt.as_ref())
            .map(|attempt| attempt.epoch)
            != Some(epoch)
        {
            return None;
        }
        Some(update())
    }

    /// Reconcile a non-terminal transport state with its attempt. Transport
    /// readiness arrives before `connect` returns, so path metadata stays on the
    /// attempt until the controller becomes the live owner. Later path changes
    /// from that live owner apply immediately under the same lifecycle hold.
    fn handle_transport_state(
        &self,
        device_id: &str,
        epoch: u64,
        mapped: ControllerLinkState,
        state: &str,
    ) {
        let path = ControllerLinkPath::from_transport_state(state);
        let direct_addr = direct_addr_from_transport_state(state).map(str::to_string);
        let Ok(mut lifecycle) = self.controllers.lock() else {
            return;
        };
        let Some(slot) = lifecycle.slots.get_mut(device_id) else {
            return;
        };
        let Some(attempt) = slot.attempt.as_mut() else {
            return;
        };
        if attempt.epoch != epoch || attempt.terminated {
            return;
        }

        let owns_live = slot.live.as_ref().is_some_and(|live| live.epoch == epoch);
        if mapped == ControllerLinkState::Connecting {
            if slot.live.is_none() {
                self.set_connecting_link(device_id);
            }
            return;
        }
        if !owns_live {
            if let Some(path) = path {
                attempt.pending_path = Some(path);
            }
            if let Some(addr) = direct_addr {
                attempt.pending_direct_addr = Some(addr);
            }
            return;
        }

        self.set_link(device_id, ControllerLinkState::Connected);
        if let Some(path) = path {
            self.set_path(device_id, path);
        }
        if let Some(addr) = direct_addr.as_deref() {
            self.remember_direct_addr(device_id, addr);
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
        match self.store.find(device_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                Self::shutdown_controller(controller);
                return false;
            }
            Err(error) => {
                let _ = self.with_current_connection_epoch(device_id, epoch, || {
                    if let Ok(mut errors) = self.last_errors.lock() {
                        errors.insert(device_id.to_string(), error.clone());
                    }
                });
                crate::runtime_trace::runtime_trace(
                    "remote",
                    &format!(
                        "controller_store_read_failed device={device_id} phase=publish error={error}"
                    ),
                );
                Self::shutdown_controller(controller);
                return false;
            }
        }

        // Saved intent was checked before this hold. A concurrent `forget`
        // invalidates the attempt in this same lifecycle map, so a stale dial
        // still cannot publish after removal.
        let published = self.controllers.lock().ok().and_then(|mut lifecycle| {
            let slot = lifecycle.slots.get_mut(device_id)?;
            let attempt = slot.attempt.as_mut()?;
            if attempt.epoch != epoch || attempt.terminated {
                return None;
            }
            let pending_path = attempt.pending_path.take();
            let pending_direct_addr = attempt.pending_direct_addr.take();
            let replaced = slot.live.replace(LiveConnection {
                epoch,
                controller: Arc::clone(&controller),
            });
            if let Ok(mut errors) = self.last_errors.lock() {
                errors.remove(device_id);
            }
            self.set_link(device_id, ControllerLinkState::Connected);
            self.clear_path(device_id);
            if let Some(path) = pending_path {
                self.set_path(device_id, path);
            }
            if let Some(addr) = pending_direct_addr.as_deref() {
                self.remember_direct_addr(device_id, addr);
            }
            Some(replaced)
        });
        match published {
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
                shared.handle_disconnect(&device_id, epoch);
                return;
            }
            // `connected:path=…` is transport readiness, not publication. The
            // attempt retains it until its controller owns the live slot.
            shared.handle_transport_state(&device_id, epoch, mapped, &state);
        })
    }

    /// Ensure a background connect/reconnect loop is retrying this device.
    /// Idempotent: marks the device as reconnecting (so `controller_for`
    /// fast-fails instead of re-dialling on the calling thread) and spawns the
    /// loop only if one isn't already running. Returns `false` if the host has
    /// been forgotten — nothing to connect to.
    fn spawn_reconnect_loop(self: &Arc<Self>, device_id: &str, worker: u64) {
        let shared = Arc::clone(self);
        let device_id = device_id.to_string();
        crate::async_runtime::spawn(async move {
            shared.reconnect_loop(device_id, worker).await;
        });
    }

    fn ensure_reconnect_loop(self: &Arc<Self>, device_id: &str) -> bool {
        let lookup = self.store.find(device_id);
        let lookup_error = lookup.as_ref().err().cloned();
        let confirmed_absent = matches!(lookup, Ok(None));
        let Ok(mut lifecycle) = self.controllers.lock() else {
            return false;
        };
        if lifecycle
            .slots
            .get(device_id)
            .and_then(|slot| slot.live.as_ref())
            .is_some()
        {
            return true;
        }
        if let Some(error) = lookup_error.as_ref() {
            if let Ok(mut errors) = self.last_errors.lock() {
                errors.insert(device_id.to_string(), error.clone());
            }
            self.set_link(device_id, ControllerLinkState::Disconnected);
        }
        if lifecycle
            .slots
            .get(device_id)
            .and_then(|slot| slot.reconnect_worker)
            .is_some()
        {
            return true;
        }
        if confirmed_absent {
            return false;
        }
        let worker = Self::allocate_owner(&mut lifecycle);
        lifecycle
            .slots
            .entry(device_id.to_string())
            .or_default()
            .reconnect_worker = Some(ReconnectWorker { owner: worker });
        drop(lifecycle);
        if let Some(error) = lookup_error {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!(
                    "controller_store_read_failed device={device_id} phase=ensure error={error}"
                ),
            );
        }
        self.spawn_reconnect_loop(device_id, worker);
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
    fn release_reconnect_ownership(&self, device_id: &str, worker: u64, epoch: u64) -> bool {
        let Ok(mut lifecycle) = self.controllers.lock() else {
            return true;
        };
        let Some(slot) = lifecycle.slots.get_mut(device_id) else {
            return true;
        };
        if slot.reconnect_worker.map(|owner| owner.owner) != Some(worker) {
            return true;
        }
        let live = slot.attempt.as_ref().is_some_and(|attempt| {
            attempt.epoch == epoch && attempt.worker == Some(worker) && !attempt.terminated
        }) && slot.live.as_ref().is_some_and(|live| live.epoch == epoch);
        if !live {
            return false;
        }
        slot.reconnect_worker = None;
        true
    }

    /// React to a dropped link: drop the dead controller from the pool and
    /// ensure a reconnect loop is retrying.
    fn handle_disconnect(self: &Arc<Self>, device_id: &str, epoch: u64) {
        let Ok(mut lifecycle) = self.controllers.lock() else {
            return;
        };
        let Some(slot) = lifecycle.slots.get(device_id) else {
            return;
        };
        let owns_attempt = slot
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.epoch == epoch);
        let owns_live = slot.live.as_ref().is_some_and(|live| live.epoch == epoch);
        if !owns_attempt && !owns_live {
            return;
        }
        #[cfg(test)]
        self.pause_accepted_disconnect_for_test();
        let newer_attempt_exists = slot
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.epoch != epoch);
        let needs_worker = !newer_attempt_exists
            && (owns_live || slot.live.is_none())
            && slot.reconnect_worker.is_none();
        let worker = needs_worker.then(|| Self::allocate_owner(&mut lifecycle));
        let slot = lifecycle.slots.get_mut(device_id).unwrap();
        if owns_attempt {
            let attempt = slot.attempt.as_mut().unwrap();
            attempt.terminated = true;
            attempt.pending_path = None;
            attempt.pending_direct_addr = None;
        }
        let old = owns_live.then(|| slot.live.take()).flatten();
        if let Some(worker) = worker {
            slot.reconnect_worker = Some(ReconnectWorker { owner: worker });
        }
        if slot.live.is_none() {
            self.set_link(device_id, ControllerLinkState::Disconnected);
            self.clear_path(device_id);
        }
        if let Ok(mut devices) = self.disconnected_devices.lock() {
            devices.insert(device_id.to_string());
        }
        drop(lifecycle);
        if let Some(old) = old {
            Self::shutdown_controller(old.controller);
        }
        if let Some(worker) = worker {
            self.spawn_reconnect_loop(device_id, worker);
        }
    }

    fn worker_is_current(&self, device_id: &str, worker: u64) -> bool {
        self.controllers
            .lock()
            .ok()
            .and_then(|lifecycle| {
                lifecycle
                    .slots
                    .get(device_id)
                    .and_then(|slot| slot.reconnect_worker)
            })
            .is_some_and(|owner| owner.owner == worker)
    }

    fn record_worker_error(
        &self,
        device_id: &str,
        worker: u64,
        epoch: Option<u64>,
        error: String,
    ) -> bool {
        let Ok(lifecycle) = self.controllers.lock() else {
            return false;
        };
        let Some(slot) = lifecycle.slots.get(device_id) else {
            return false;
        };
        if slot.reconnect_worker.map(|owner| owner.owner) != Some(worker)
            || epoch.is_some_and(|epoch| {
                !slot
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.epoch == epoch && attempt.worker == Some(worker))
            })
        {
            return false;
        }
        if let Ok(mut errors) = self.last_errors.lock() {
            errors.insert(device_id.to_string(), error);
        }
        self.set_link(device_id, ControllerLinkState::Disconnected);
        true
    }

    /// Give up this worker's claim on the slot. Two callers reach it: the host
    /// disappeared from the store, and the worker's attempt was replaced while
    /// it was setting up. Both mean "stop, and leave nothing claimed behind" --
    /// every check here is against `worker`, so a replacement that already took
    /// the slot keeps it.
    ///
    /// The slot must not survive with `reconnect_worker` set and no worker
    /// running: `ensure_reconnect_loop` would see a claimed worker, decline to
    /// spawn, and the device would stay "connecting" until it is forgotten and
    /// re-paired.
    fn abandon_reconnect_worker(&self, device_id: &str, worker: u64) {
        let Ok(mut lifecycle) = self.controllers.lock() else {
            return;
        };
        let Some(slot) = lifecycle.slots.get_mut(device_id) else {
            return;
        };
        if slot.reconnect_worker.map(|owner| owner.owner) != Some(worker) {
            return;
        }
        slot.reconnect_worker = None;
        if slot
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.worker == Some(worker))
        {
            slot.attempt = None;
        }
        if slot.live.is_none() && slot.attempt.is_none() {
            lifecycle.slots.remove(device_id);
            if let Ok(mut links) = self.links.lock() {
                links.remove(device_id);
            }
            self.clear_path(device_id);
            if let Ok(mut errors) = self.last_errors.lock() {
                errors.remove(device_id);
            }
        }
    }

    /// Retry `connect_saved` with capped exponential backoff until the link is
    /// re-established or the host is forgotten. The fresh controller is wired to
    /// the same state handler, so a later drop is caught again.
    async fn reconnect_loop(self: Arc<Self>, device_id: String, worker: u64) {
        let mut delay = Duration::from_secs(1);
        let mut attempt: u32 = 0;
        loop {
            let host = match self.store.find(&device_id) {
                Ok(Some(host)) => host,
                Ok(None) => {
                    self.abandon_reconnect_worker(&device_id, worker);
                    return;
                }
                Err(error) => {
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "controller_store_read_failed device={device_id} phase=reconnect retry_in_ms={} error={error}",
                            delay.as_millis()
                        ),
                    );
                    if !self.record_worker_error(&device_id, worker, None, error) {
                        return;
                    }
                    tokio::time::sleep(delay).await;
                    delay = next_reconnect_delay(delay);
                    continue;
                }
            };
            let Some(epoch) = self.begin_reconnect_attempt(&device_id, worker) else {
                return;
            };
            attempt += 1;
            let current = self.with_current_connection_epoch(&device_id, epoch, || {
                self.set_connecting_link(&device_id);
                // No path while dialing — clear the stale direct/relay marker so
                // the badge doesn't claim a route the dropped link no longer has.
                self.clear_path(&device_id);
            });
            if current.is_none() {
                // A replacement took the attempt while this worker was setting
                // up. Leaving now without disclaiming would strand the slot:
                // claimed by a worker that no longer exists.
                self.abandon_reconnect_worker(&device_id, worker);
                return;
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
                        crate::runtime_trace::runtime_trace(
                            "remote",
                            &format!("controller_connect ok device={device_id} n={attempt}"),
                        );
                        if self.release_reconnect_ownership(&device_id, worker, epoch) {
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
                    if !self.worker_is_current(&device_id, worker) {
                        return;
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
                    if !self.record_worker_error(&device_id, worker, Some(epoch), error) {
                        return;
                    }
                    tokio::time::sleep(delay).await;
                    delay = next_reconnect_delay(delay);
                }
            }
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
                controllers: Mutex::new(ControllerLifecycle::default()),
                links: Mutex::new(HashMap::new()),
                paths: Mutex::new(HashMap::new()),
                last_errors: Mutex::new(HashMap::new()),
                disconnected_devices: Mutex::new(HashSet::new()),
                #[cfg(test)]
                disconnect_pause: Mutex::new(None),
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
        let snapshot = match self.shared.store.lifecycle_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                crate::runtime_trace::runtime_trace(
                    "remote",
                    &format!("controller_store_read_failed phase=ensure error={error}"),
                );
                return 0;
            }
        };
        if let Some(error) = snapshot.refresh_error.as_ref() {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!("controller_store_read_failed phase=ensure error={error}"),
            );
        }
        let connected: HashSet<String> = self
            .shared
            .controllers
            .lock()
            .map(|lifecycle| {
                lifecycle
                    .slots
                    .iter()
                    .filter(|(_, slot)| slot.live.is_some())
                    .map(|(device_id, _)| device_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut engaged = 0;
        for host in snapshot.hosts {
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
        let paired = crate::async_runtime::block_on(RemoteController::pair(
            &ticket,
            device_name,
            device_id.clone(),
            on_state,
        ));
        let (controller, saved) = match paired {
            Ok(paired) => paired,
            Err(error) => {
                self.shared.abort_unpublished_attempt(&device_id, epoch);
                return Err(error);
            }
        };
        self.finish_pairing(epoch, controller, saved)
    }

    /// Commit confirmed pairing intent before publishing its controller. A
    /// failed required write aborts exact attempt and closes unowned transport;
    /// success may then hand a dead-on-arrival link to normal reconciliation.
    fn finish_pairing(
        &self,
        epoch: u64,
        controller: RemoteController,
        saved: SavedRemoteHost,
    ) -> Result<SavedRemoteHost, String> {
        if let Err(error) = self.shared.store.upsert(saved.clone()) {
            self.shared
                .abort_unpublished_attempt(&saved.device_id, epoch);
            crate::async_runtime::block_on(controller.shutdown());
            return Err(error);
        }
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
            .controllers
            .lock()
            .map(|lifecycle| {
                lifecycle
                    .slots
                    .iter()
                    .filter_map(|(device_id, slot)| {
                        slot.live.as_ref().map(|connection| {
                            (device_id.clone(), Arc::clone(&connection.controller))
                        })
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
            .controllers
            .lock()
            .map(|lifecycle| {
                lifecycle
                    .slots
                    .iter()
                    .filter_map(|(device_id, slot)| {
                        slot.live.as_ref().map(|connection| {
                            (device_id.clone(), Arc::clone(&connection.controller))
                        })
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
        let Ok(lifecycle) = self.shared.controllers.lock() else {
            return Vec::new();
        };
        lifecycle
            .slots
            .iter()
            .filter_map(|(device_id, slot)| {
                slot.live.as_ref().map(|connection| {
                    (
                        device_id.clone(),
                        connection.epoch,
                        Arc::clone(&connection.controller),
                    )
                })
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
        let Ok(lifecycle) = self.shared.controllers.lock() else {
            return None;
        };
        let current = lifecycle.slots.get(device_id)?.live.as_ref()?;
        if !std::ptr::eq(current.controller.as_ref(), controller) {
            return None;
        }
        Some(current.epoch)
    }

    /// Run `update` only while this exact `(device, epoch, controller)` still
    /// owns the published live-link slot. Ownership cannot change between the
    /// check and the local side effect because both share one lifecycle hold.
    /// This lifecycle hold is outermost: `update` must not re-enter this manager
    /// or wait on remote I/O.
    pub(crate) fn with_live_connection_owner<T>(
        &self,
        device_id: &str,
        epoch: u64,
        controller: &RemoteController,
        update: impl FnOnce() -> T,
    ) -> Result<Option<T>, String> {
        let lifecycle = self
            .shared
            .controllers
            .lock()
            .map_err(|_| "Remote controller lifecycle lock is poisoned.".to_string())?;
        let Some(current) = lifecycle
            .slots
            .get(device_id)
            .and_then(|slot| slot.live.as_ref())
        else {
            return Ok(None);
        };
        if current.epoch != epoch || !std::ptr::eq(current.controller.as_ref(), controller) {
            return Ok(None);
        }
        Ok(Some(update()))
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
        self.adopt_if_owner(device_id, &controller, &info);
        Ok(info)
    }

    /// [`Self::host_info`] for a caller that already holds the controller it
    /// wants to ask. Re-resolving the device could hand back a different
    /// connection, so adoption checks ownership of this exact controller.
    pub fn host_info_via(
        &self,
        device_id: &str,
        controller: &RemoteController,
    ) -> Result<serde_json::Value, String> {
        let info = controller.host_info()?;
        self.adopt_if_owner(device_id, controller, &info);
        Ok(info)
    }

    /// Adopt reported addresses only while the connection that reported them
    /// still owns this device's live-link slot.
    ///
    /// A reply describes the connection that produced it. A request can outlive
    /// its connection — the link drops and a reconnect replaces it while the
    /// reply is still in flight — and the answer then describes a link nobody
    /// uses. Persisting it would overwrite the newer connection's addresses with
    /// older ones. The ownership check and store mutation share one
    /// lifecycle critical section, so replacement cannot land between the
    /// precondition and the write it protects.
    ///
    /// The store is innermost while this lifecycle hold is active and never
    /// acquires a manager lock itself.
    fn adopt_if_owner(
        &self,
        device_id: &str,
        controller: &RemoteController,
        info: &serde_json::Value,
    ) {
        let Ok(lifecycle) = self.shared.controllers.lock() else {
            return;
        };
        let Some(current) = lifecycle
            .slots
            .get(device_id)
            .and_then(|slot| slot.live.as_ref())
        else {
            return;
        };
        if !std::ptr::eq(current.controller.as_ref(), controller) {
            return;
        }
        self.shared.adopt_reported_transports(device_id, info);
        drop(lifecycle);
    }

    /// [`Self::host_info`] for callers that must block on the connect.
    pub fn host_info_blocking(
        &self,
        device_id: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let controller = self.controller_for_blocking(device_id, timeout)?;
        let info = controller.host_info()?;
        self.adopt_if_owner(device_id, &controller, &info);
        Ok(info)
    }

    pub fn controller_for(&self, device_id: &str) -> Result<Arc<RemoteController>, String> {
        if let Ok(lifecycle) = self.shared.controllers.lock()
            && let Some(controller) = lifecycle
                .slots
                .get(device_id)
                .and_then(|slot| slot.live.as_ref())
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
        if let Ok(lifecycle) = self.shared.controllers.lock()
            && let Some(controller) = lifecycle
                .slots
                .get(device_id)
                .and_then(|slot| slot.live.as_ref())
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
            if let Ok(lifecycle) = self.shared.controllers.lock()
                && let Some(controller) = lifecycle
                    .slots
                    .get(device_id)
                    .and_then(|slot| slot.live.as_ref())
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
        let old = if let Ok(mut lifecycle) = self.shared.controllers.lock() {
            let old = lifecycle.slots.remove(device_id).and_then(|slot| slot.live);
            if let Ok(mut devices) = self.shared.disconnected_devices.lock() {
                devices.insert(device_id.to_string());
            }
            if let Ok(mut links) = self.shared.links.lock() {
                links.remove(device_id);
            }
            self.shared.clear_path(device_id);
            if let Ok(mut errors) = self.shared.last_errors.lock() {
                errors.remove(device_id);
            }
            old
        } else {
            None
        };
        if let Some(old) = old {
            ManagerShared::shutdown_controller(old.controller);
        }
        Ok(hosts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TEST_ENDPOINT_ID: &str =
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

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
            node_id: TEST_ENDPOINT_ID.to_string(),
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

        let saved = manager
            .shared
            .store
            .find("dev-1")
            .expect("read host")
            .expect("host");
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
    fn host_info_candidate_is_adopted_persisted_and_keeps_remembered_addresses() {
        let mut transport = iroh_transport();
        transport.direct_addrs = vec!["10.8.0.3:41234".to_string()];
        let manager = manager_with(saved_host_with(transport));
        let support_dir = manager.shared.support_dir.clone();
        let info = codux_runtime_core::host::host_info_payload(
            codux_runtime_core::host::HostInfoPayload {
                host_id: "host-1".to_string(),
                runtime_instance_id: "runtime-2".to_string(),
                name: "Host".to_string(),
                platform: "macos".to_string(),
                app: "Codux".to_string(),
                resource_subscriptions: Vec::new(),
                transports: vec![
                    codux_protocol::iroh_transport_candidate_with_ticket_and_authentication(
                        "",
                        TEST_ENDPOINT_ID,
                        "",
                        "ticket-v2",
                        "",
                    ),
                ],
            },
        );

        manager.shared.adopt_reported_transports("dev-1", &info);

        let saved = RemoteControllerStore::new(support_dir.clone())
            .find("dev-1")
            .expect("read persisted host")
            .expect("persisted host");
        assert_eq!(saved.transports[0].ticket, "ticket-v2", "adopted");
        assert_eq!(
            saved.transports[0].direct_addrs,
            vec!["10.8.0.3:41234".to_string()],
            "the two address sources are independent; neither may erase the other"
        );

        let _ = std::fs::remove_dir_all(support_dir);
    }

    #[test]
    fn a_report_without_a_relay_keeps_the_one_already_known() {
        // A host publishes the relay it is connected to and omits the key when
        // it has none, so a host that started while its relay was unreachable
        // reports a relay-less candidate. Adopting that verbatim would leave
        // this controller with only direct routes, which is exactly the case a
        // relay exists to cover.
        // The remembered address is what makes the relay-less report dialable
        // at all; without it the report is rejected before inheritance and this
        // would be testing the filter instead.
        let mut saved = iroh_transport();
        saved.direct_addrs = vec!["10.8.0.3:41234".to_string()];
        let manager = manager_with(saved_host_with(saved));

        manager.shared.adopt_reported_transports(
            "dev-1",
            &serde_json::json!({
                "transports": [{
                    "kind": "iroh",
                    "nodeId": TEST_ENDPOINT_ID,
                    "ticket": "ticket-v2"
                }]
            }),
        );

        let saved = manager
            .shared
            .store
            .find("dev-1")
            .expect("read host")
            .expect("host");
        assert_eq!(
            saved.transports[0].ticket, "ticket-v2",
            "the fresh report still wins on everything it carries"
        );
        assert_eq!(
            saved.transports[0].relay_url, "https://relay.example",
            "a report that omits the relay must not erase the one we know"
        );
    }

    #[test]
    fn a_reported_relay_replaces_the_one_already_known() {
        // Only one relay can be the one the host is homed on, so a report that
        // names one supersedes the stored guess rather than joining it.
        let manager = manager_with(saved_host_with(iroh_transport()));

        manager.shared.adopt_reported_transports(
            "dev-1",
            &serde_json::json!({
                "transports": [{
                    "kind": "iroh",
                    "nodeId": TEST_ENDPOINT_ID,
                    "ticket": "ticket-v2",
                    "relayUrl": "https://moved-relay.example"
                }]
            }),
        );

        let saved = manager
            .shared
            .store
            .find("dev-1")
            .expect("read host")
            .expect("host");
        assert_eq!(saved.transports[0].relay_url, "https://moved-relay.example");
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
            &serde_json::json!({
                "transports": [{ "kind": "iroh", "nodeId": TEST_ENDPOINT_ID }]
            }),
        );

        let saved = manager
            .shared
            .store
            .find("dev-1")
            .expect("read host")
            .expect("host");
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

    fn install_reconnect_worker(manager: &RemoteControllerManager, device_id: &str) -> u64 {
        let mut lifecycle = manager.shared.controllers.lock().unwrap();
        let worker = ManagerShared::allocate_owner(&mut lifecycle);
        lifecycle
            .slots
            .entry(device_id.to_string())
            .or_default()
            .reconnect_worker = Some(ReconnectWorker { owner: worker });
        worker
    }

    fn reconnect_worker_owner(manager: &RemoteControllerManager, device_id: &str) -> Option<u64> {
        manager
            .shared
            .controllers
            .lock()
            .unwrap()
            .slots
            .get(device_id)
            .and_then(|slot| slot.reconnect_worker)
            .map(|worker| worker.owner)
    }

    fn reconnect_worker_is_claimed(manager: &RemoteControllerManager, device_id: &str) -> bool {
        reconnect_worker_owner(manager, device_id).is_some()
    }

    struct ShutdownRecordingTransport {
        stopped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl codux_remote_transport::RemoteTransport for ShutdownRecordingTransport {
        fn kind(&self) -> codux_protocol::RemoteTransportKind {
            codux_protocol::RemoteTransportKind::Iroh
        }

        fn send(&self, _data: Vec<u8>, _device_id: Option<&str>) -> bool {
            false
        }

        async fn shutdown(&self) {
            self.stopped.store(true, Ordering::SeqCst);
        }
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
    fn connected_transport_state_waits_for_live_publication() {
        let manager = manager_with(saved_host_with(iroh_transport()));
        let epoch = manager.shared.next_connection_epoch("dev-1");
        let on_state = manager.shared.state_handler("dev-1", epoch);
        on_state("node-1".to_string(), "connecting".to_string());
        on_state(
            "node-1".to_string(),
            "connected:path=direct;addr=10.8.0.3:41234".to_string(),
        );

        assert_eq!(
            manager.link_states().get("dev-1"),
            Some(&ControllerLinkState::Connecting),
            "transport readiness is pending metadata until its controller is published"
        );
        assert!(!manager.link_paths().contains_key("dev-1"));
        assert!(
            manager
                .shared
                .store
                .find("dev-1")
                .unwrap()
                .unwrap()
                .transports[0]
                .direct_addrs
                .is_empty(),
            "an unpublished attempt must not write its path outside the lifecycle slot"
        );

        assert!(
            manager
                .shared
                .publish_connection("dev-1", epoch, test_controller("dev-1"))
        );
        assert_eq!(
            manager.link_states().get("dev-1"),
            Some(&ControllerLinkState::Connected)
        );
        assert_eq!(
            manager.link_paths().get("dev-1"),
            Some(&ControllerLinkPath::Direct)
        );
        assert_eq!(
            manager
                .shared
                .store
                .find("dev-1")
                .unwrap()
                .unwrap()
                .transports[0]
                .direct_addrs,
            ["10.8.0.3:41234"]
        );
    }

    #[test]
    fn failed_pair_attempt_leaves_no_lifecycle_state() {
        let manager = manager("codux-controller-failed-pair-cleanup");
        let payload = serde_json::json!({
            "code": "1234",
            "secret": "secret",
            "pairingId": "pair-1",
            "transports": [{
                "kind": REMOTE_TRANSPORT_IROH,
                "nodeId": "not-a-node-id",
                "relayUrl": "https://relay.example",
            }],
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());

        manager
            .pair(&encoded, "desktop")
            .expect_err("the malformed dial target must fail before network I/O");

        assert!(manager.shared.controllers.lock().unwrap().slots.is_empty());
        assert!(manager.link_states().is_empty());
        assert!(manager.link_paths().is_empty());
    }

    #[test]
    fn failed_required_pair_save_aborts_and_shuts_down_unpublished_controller() {
        let manager = manager("codux-controller-failed-pair-save");
        std::fs::create_dir_all(&manager.shared.support_dir).unwrap();
        let temporary = manager
            .shared
            .support_dir
            .join("remote-controllers.json.tmp");
        std::fs::create_dir(&temporary).unwrap();
        let epoch = manager.shared.next_connection_epoch("dev-1");
        let on_state = manager.shared.state_handler("dev-1", epoch);
        on_state("node-1".to_string(), "connecting".to_string());
        on_state(
            "node-1".to_string(),
            "connected:path=relay;relayUrl=https://relay.example".to_string(),
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_for_transport = Arc::clone(&stopped);
        let controller = RemoteController::with_test_transport("dev-1", move |_| {
            Arc::new(ShutdownRecordingTransport {
                stopped: stopped_for_transport,
            })
        });

        manager
            .finish_pairing(epoch, controller, saved_host_with(iroh_transport()))
            .expect_err("blocked temporary path must fail required save");

        assert!(stopped.load(Ordering::SeqCst));
        assert!(manager.shared.controllers.lock().unwrap().slots.is_empty());
        assert!(manager.link_states().is_empty());
        assert!(manager.link_paths().is_empty());
        assert!(manager.shared.store.find("dev-1").unwrap().is_none());
        std::fs::remove_dir(&temporary).unwrap();
    }

    #[test]
    fn stale_pair_abort_cannot_remove_replacement() {
        let manager = manager("codux-controller-stale-pair-abort");
        save_test_host(&manager, "device-1");
        let stale_epoch = manager.shared.next_connection_epoch("device-1");
        let replacement_epoch = manager.shared.next_connection_epoch("device-1");
        let replacement = test_controller("device-1");
        assert!(manager.shared.publish_connection(
            "device-1",
            replacement_epoch,
            Arc::clone(&replacement)
        ));

        manager
            .shared
            .abort_unpublished_attempt("device-1", stale_epoch);

        assert_eq!(
            manager.live_connection_epoch("device-1", &replacement),
            Some(replacement_epoch)
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
        let superseded_epoch = manager.shared.next_connection_epoch("device-1");
        assert!(manager.shared.publish_connection(
            "device-1",
            superseded_epoch,
            Arc::clone(&superseded)
        ));
        let replacement = test_controller("device-1");
        let replacement_epoch = manager.shared.next_connection_epoch("device-1");
        assert!(
            manager
                .shared
                .publish_connection("device-1", replacement_epoch, replacement)
        );

        manager.adopt_if_owner(
            "device-1",
            &superseded,
            &serde_json::json!({
                "transports": [{
                    "kind": "iroh",
                    "nodeId": TEST_ENDPOINT_ID,
                    "relayUrl": "https://relay.example",
                    "ticket": "ticket-from-a-dead-link",
                }],
            }),
        );

        let saved = manager
            .shared
            .store
            .find("device-1")
            .expect("read host")
            .expect("host");
        assert!(
            saved.transports.is_empty(),
            "a reply from a connection that is no longer live must not be stored"
        );
    }

    #[test]
    fn ownership_cannot_change_between_validation_and_store_mutation() {
        let manager = Arc::new(manager_with(saved_host_with(iroh_transport())));
        let owner = test_controller("dev-1");
        let owner_epoch = manager.shared.next_connection_epoch("dev-1");
        assert!(
            manager
                .shared
                .publish_connection("dev-1", owner_epoch, Arc::clone(&owner))
        );

        // Hold the store immediately after the adoption's ownership check. A
        // correct adoption keeps lifecycle locked while it waits here, so
        // a replacement cannot take the slot before the protected mutation.
        let write_gate = manager.shared.store.hold_writes_for_test();
        let adopting_manager = Arc::clone(&manager);
        let (adopt_started_tx, adopt_started_rx) = std::sync::mpsc::sync_channel(0);
        let adoption = std::thread::spawn(move || {
            adopt_started_tx.send(()).unwrap();
            adopting_manager.adopt_if_owner(
                "dev-1",
                &owner,
                &serde_json::json!({
                    "transports": [{
                        "kind": "iroh",
                        "nodeId": TEST_ENDPOINT_ID,
                        "relayUrl": "https://relay.example",
                        "ticket": "ticket-from-owner",
                    }],
                }),
            );
        });
        adopt_started_rx.recv().unwrap();
        let lock_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match manager.shared.controllers.try_lock() {
                Err(std::sync::TryLockError::WouldBlock) => break,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("controller lifecycle lock poisoned")
                }
                Ok(guard) => drop(guard),
            }
            assert!(
                Instant::now() < lock_deadline,
                "adoption released ownership before reaching the store mutation"
            );
            std::thread::yield_now();
        }

        let replacing_manager = Arc::clone(&manager);
        let replacement = test_controller("dev-1");
        let (replace_started_tx, replace_started_rx) = std::sync::mpsc::sync_channel(0);
        let (replaced_tx, replaced_rx) = std::sync::mpsc::sync_channel(0);
        let replacement_thread = std::thread::spawn(move || {
            replace_started_tx.send(()).unwrap();
            replacing_manager
                .shared
                .controllers
                .lock()
                .unwrap()
                .slots
                .entry("dev-1".to_string())
                .or_default()
                .live = Some(LiveConnection {
                epoch: owner_epoch + 1,
                controller: replacement,
            });
            replaced_tx.send(()).unwrap();
        });
        replace_started_rx.recv().unwrap();
        assert!(
            replaced_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "replacement took ownership between validation and mutation"
        );

        drop(write_gate);
        adoption.join().unwrap();
        replaced_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        replacement_thread.join().unwrap();
        assert_eq!(
            manager
                .shared
                .store
                .find("dev-1")
                .unwrap()
                .unwrap()
                .transports[0]
                .ticket,
            "ticket-from-owner"
        );
    }

    #[test]
    fn invalidated_connection_epoch_cannot_be_published() {
        let manager = manager("codux-controller-invalidated-epoch");
        save_test_host(&manager, "device-1");
        let epoch = manager.shared.next_connection_epoch("device-1");
        manager
            .shared
            .controllers
            .lock()
            .unwrap()
            .slots
            .get_mut("device-1")
            .unwrap()
            .attempt = None;

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
        assert!(manager.shared.store.find("device-1").unwrap().is_some());
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
        let worker = install_reconnect_worker(&manager, "device-1");

        crate::async_runtime::block_on(
            Arc::clone(&manager.shared).reconnect_loop("device-1".to_string(), worker),
        );

        assert!(
            !manager
                .shared
                .controllers
                .lock()
                .unwrap()
                .slots
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
        let worker = install_reconnect_worker(&manager, "device-1");
        let epoch = manager
            .shared
            .begin_reconnect_attempt("device-1", worker)
            .unwrap();
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
        let worker = install_reconnect_worker(&manager, "device-1");
        let epoch = manager
            .shared
            .begin_reconnect_attempt("device-1", worker)
            .unwrap();
        let on_state = manager.shared.state_handler("device-1", epoch);
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );

        // Drop before the worker's success handover. The callback evicts its
        // exact live owner but leaves this same worker responsible for retry.
        on_state("node-1".to_string(), "closed".to_string());
        assert!(manager.connected_controllers().is_empty());
        assert!(
            !manager
                .shared
                .release_reconnect_ownership("device-1", worker, epoch)
        );
        assert!(
            reconnect_worker_is_claimed(&manager, "device-1"),
            "the worker keeps retrying instead of leaving the host unconnected"
        );

        // Once the link is really up, the ownership goes back.
        let epoch = manager
            .shared
            .begin_reconnect_attempt("device-1", worker)
            .unwrap();
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );
        assert!(
            manager
                .shared
                .release_reconnect_ownership("device-1", worker, epoch)
        );
        assert!(!reconnect_worker_is_claimed(&manager, "device-1"));
    }

    #[test]
    fn stale_disconnect_cannot_evict_a_replacement() {
        let manager = Arc::new(manager("codux-controller-stale-disconnect"));
        save_test_host(&manager, "device-1");
        let first = test_controller("device-1");
        let first_epoch = manager.shared.next_connection_epoch("device-1");
        let on_first_state = manager.shared.state_handler("device-1", first_epoch);
        assert!(
            manager
                .shared
                .publish_connection("device-1", first_epoch, Arc::clone(&first))
        );

        // Stop A after its callback accepted ownership but before eviction.
        // B must be unable to publish in that interval: ownership validation
        // and the protected transition are one lifecycle critical section.
        let (disconnect_accepted, resume_disconnect) =
            manager.shared.pause_next_disconnect_for_test();
        let disconnect = std::thread::spawn(move || {
            on_first_state("node-1".to_string(), "closed".to_string());
        });
        disconnect_accepted
            .recv_timeout(Duration::from_secs(1))
            .expect("disconnect callback did not accept A ownership");

        let replacement = test_controller("device-1");
        let replacement_for_thread = Arc::clone(&replacement);
        let manager_for_thread = Arc::clone(&manager);
        let (replacement_done_tx, replacement_done_rx) = std::sync::mpsc::sync_channel(0);
        let replace = std::thread::spawn(move || {
            let replacement_epoch = manager_for_thread.shared.next_connection_epoch("device-1");
            let published = manager_for_thread.shared.publish_connection(
                "device-1",
                replacement_epoch,
                replacement_for_thread,
            );
            replacement_done_tx
                .send((replacement_epoch, published))
                .unwrap();
        });
        assert!(
            replacement_done_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "replacement published between disconnect ownership check and eviction"
        );

        resume_disconnect.send(()).unwrap();
        disconnect.join().unwrap();
        let (replacement_epoch, published) = replacement_done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        replace.join().unwrap();
        assert!(published);

        assert_eq!(
            manager.live_connection_epoch("device-1", &replacement),
            Some(replacement_epoch),
            "a stale disconnect may evict only the controller that emitted it"
        );
    }

    #[test]
    fn ensure_does_not_claim_a_worker_for_an_already_published_connection() {
        let manager = manager("codux-controller-ensure-publication-gap");
        save_test_host(&manager, "device-1");
        let controller = test_controller("device-1");
        let epoch = manager.shared.next_connection_epoch("device-1");
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, Arc::clone(&controller))
        );

        assert!(manager.shared.ensure_reconnect_loop("device-1"));
        assert!(
            !reconnect_worker_is_claimed(&manager, "device-1"),
            "ensure must observe publication and leave the live owner alone"
        );
        assert_eq!(
            manager.live_connection_epoch("device-1", &controller),
            Some(epoch)
        );
    }

    #[test]
    fn ensure_uses_last_good_hosts_while_store_refresh_is_unreadable() {
        let manager = manager("codux-controller-ensure-last-good");
        save_test_host(&manager, "device-1");
        let path = manager.shared.support_dir.join("remote-controllers.json");
        let valid_store = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, "{ unreadable").unwrap();

        assert_eq!(manager.ensure_saved_hosts_connected(), 1);
        assert!(reconnect_worker_is_claimed(&manager, "device-1"));
        assert!(
            manager
                .shared
                .last_error("device-1")
                .is_some_and(|error| error.contains("could not be read")),
            "refresh error must stay observable while last-good intent reconnects"
        );

        std::fs::write(&path, valid_store).unwrap();
        manager.forget("device-1").unwrap();
    }

    #[test]
    fn a_worker_that_gives_up_leaves_nothing_claimed() {
        // The reconnect loop reaches this when a replacement takes its attempt
        // while it is setting up. A slot left claimed by a worker that has
        // exited is permanently stuck: `ensure_reconnect_loop` sees an owner,
        // declines to spawn, and the device reports "connecting" until it is
        // forgotten and re-paired.
        let manager = manager("codux-controller-worker-disown");
        save_test_host(&manager, "device-1");
        // A published connection keeps the slot alive past the disown. Without
        // one the slot is empty and gets evicted whole, so the assertion below
        // holds whether or not the worker was released.
        let epoch = manager.shared.next_connection_epoch("device-1");
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, test_controller("device-1"))
        );
        let worker = install_reconnect_worker(&manager, "device-1");
        assert!(reconnect_worker_is_claimed(&manager, "device-1"));

        manager.shared.abandon_reconnect_worker("device-1", worker);

        assert!(
            !reconnect_worker_is_claimed(&manager, "device-1"),
            "a worker that stops must not leave the slot claimed"
        );
    }

    #[test]
    fn stale_worker_cleanup_cannot_release_a_replacement_worker() {
        let manager = manager("codux-controller-stale-worker-cleanup");
        save_test_host(&manager, "device-1");
        let stale_worker = install_reconnect_worker(&manager, "device-1");

        manager.forget("device-1").unwrap();
        save_test_host(&manager, "device-1");
        let replacement_worker = install_reconnect_worker(&manager, "device-1");
        assert!(
            replacement_worker > stale_worker,
            "A-B-A ownership must not reuse a stale callback's token"
        );

        manager
            .shared
            .abandon_reconnect_worker("device-1", stale_worker);
        assert_eq!(
            reconnect_worker_owner(&manager, "device-1"),
            Some(replacement_worker),
            "worker cleanup may release only its own ownership"
        );
    }

    #[test]
    fn store_read_error_does_not_leave_the_dead_controller_published() {
        let manager = manager("codux-controller-store-read-error");
        save_test_host(&manager, "device-1");
        let path = manager.shared.support_dir.join("remote-controllers.json");
        let valid_store = std::fs::read_to_string(&path).unwrap();
        let controller = test_controller("device-1");
        let epoch = manager.shared.next_connection_epoch("device-1");
        let on_state = manager.shared.state_handler("device-1", epoch);
        assert!(
            manager
                .shared
                .publish_connection("device-1", epoch, Arc::clone(&controller))
        );

        std::fs::write(&path, "{ unreadable").unwrap();
        on_state("node-1".to_string(), "closed".to_string());

        assert!(
            manager.connected_controllers().is_empty(),
            "dead-owner eviction must not depend on reading reconnect intent"
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if manager
                .shared
                .last_error("device-1")
                .is_some_and(|error| error.contains("could not be read"))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "store read failure was not made observable"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Repairing the projection must let the same worker leave the store
        // error behind and resume normal dial attempts.
        std::fs::write(&path, valid_store).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager
            .shared
            .last_error("device-1")
            .is_some_and(|error| error.contains("could not be read"))
        {
            assert!(
                Instant::now() < deadline,
                "reconnect loop did not resume after the store recovered"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        manager.forget("device-1").unwrap();
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
