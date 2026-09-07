//! Persisted dial targets for the desktop-as-controller: hosts this desktop has
//! paired with and can reconnect to without re-pairing. This is the OUTBOUND
//! counterpart to `RemoteDeviceSummary` (which records INBOUND devices paired
//! into this host). Stored as `remote-controllers.json` under the support dir.

use serde::{Deserialize, Deserializer, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

// Recover quickly from a transient rename/permission race, then cap retries so
// a persistent disk failure does not become a tight I/O and logging loop.
const STORE_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(100);
const STORE_RETRY_MAX_DELAY: Duration = Duration::from_secs(5);

/// One reconnectable transport candidate for a saved host. Its endpoint ticket,
/// reported relay and remembered direct addresses are complementary routes to
/// the same `node_id`; reconnect keeps every route that still parses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SavedRemoteTransport {
    pub kind: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub relay_url: String,
    #[serde(default)]
    pub relay_authentication: String,
    /// The host's endpoint ticket, which serialises its DIRECT addresses.
    ///
    /// Without this a saved host is reachable only through its relay, so a
    /// relay outage strands a desktop that could have dialled it straight over
    /// the LAN or a shared virtual network. Empty for hosts paired before this
    /// was recorded, and refreshed from every `host.info` reply thereafter.
    #[serde(default)]
    pub ticket: String,
    /// Socket addresses this desktop has actually reached the host on, most
    /// recent first.
    ///
    /// An endpoint ticket carries an address snapshot; these follow paths this
    /// controller actually reaches as the host moves. Dialled alongside it,
    /// never instead of it.
    #[serde(default, deserialize_with = "deserialize_direct_addrs")]
    pub direct_addrs: Vec<String>,
}

impl SavedRemoteTransport {
    /// How many previously-working addresses to keep. Matches the mobile
    /// controller: enough to cover a host reachable at one address at home and
    /// another over a VPN, without turning a hint list into a routing table.
    pub const MAX_DIRECT_ADDRS: usize = 3;

    /// This transport with `addr` promoted to most-recently-used, or `None` when
    /// nothing would change — so a caller can skip a pointless store write on
    /// the path reports that arrive throughout a live connection.
    pub fn with_direct_addr(&self, addr: &str) -> Option<Self> {
        let addr = addr.trim();
        if addr.is_empty() {
            return None;
        }
        let next = normalize_direct_addrs(
            std::iter::once(addr.to_string()).chain(self.direct_addrs.iter().cloned()),
        );
        if next == self.direct_addrs {
            return None;
        }
        Some(Self {
            direct_addrs: next,
            ..self.clone()
        })
    }

    fn normalize(&mut self) {
        self.direct_addrs = normalize_direct_addrs(std::mem::take(&mut self.direct_addrs));
    }
}

fn deserialize_direct_addrs<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer).map(normalize_direct_addrs)
}

fn normalize_direct_addrs(addresses: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(SavedRemoteTransport::MAX_DIRECT_ADDRS);
    for address in addresses {
        let address = address.trim();
        if address.is_empty() || normalized.iter().any(|item| item == address) {
            continue;
        }
        normalized.push(address.to_string());
        if normalized.len() == SavedRemoteTransport::MAX_DIRECT_ADDRS {
            break;
        }
    }
    normalized
}

/// A host this desktop has paired with, keyed by the device id we minted during
/// pairing (the host caches that id, so reconnect needs no fresh handshake).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedRemoteHost {
    pub device_id: String,
    pub host_id: String,
    #[serde(default)]
    pub host_name: String,
    #[serde(default)]
    pub device_token: String,
    /// The host's OS (`std::env::consts::OS`: "macos"/"linux"/"windows"/…),
    /// reported in `pairing.confirmed`. Empty for hosts paired before this was
    /// recorded. Used to label the device type in the UI.
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub transports: Vec<SavedRemoteTransport>,
}

impl SavedRemoteHost {
    fn normalize(&mut self) {
        for transport in &mut self.transports {
            transport.normalize();
        }
    }
}

pub struct RemoteControllerStore {
    inner: Arc<RemoteControllerStoreInner>,
}

struct RemoteControllerStoreInner {
    path: PathBuf,
    /// Serialises every mutation and disk replacement. No stale snapshot can
    /// finish after a newer one: the retry worker uses this same lock.
    write_lock: Mutex<()>,
    state: Mutex<RemoteControllerStoreState>,
}

/// Authoritative state while the process is alive. The file is its durable
/// projection; a failed derived write leaves this snapshot dirty until the
/// single retry worker persists the latest version.
struct RemoteControllerStoreState {
    hosts: Vec<SavedRemoteHost>,
    dirty: bool,
    retry_running: bool,
    last_write_error: Option<String>,
}

/// One authoritative in-process view of saved intent plus the outcome of
/// refreshing its durable projection. A read failure never rewrites known
/// hosts as absence, but lifecycle callers still receive the failure.
pub(crate) struct RemoteControllerStoreSnapshot {
    pub(crate) hosts: Vec<SavedRemoteHost>,
    pub(crate) refresh_error: Option<String>,
}

#[derive(Clone, Copy)]
enum MutationFailure {
    /// Required user intent: report failure and leave both memory and disk at
    /// the state the caller still believes is current.
    RollBack,
    /// Derived live metadata: keep latest intent and retry its projection.
    RetainAndRetry,
}

impl RemoteControllerStore {
    pub fn new(support_dir: PathBuf) -> Self {
        let path = support_dir.join("remote-controllers.json");
        let hosts = Self::load_path(&path).unwrap_or_default();
        Self {
            inner: Arc::new(RemoteControllerStoreInner {
                path,
                write_lock: Mutex::new(()),
                state: Mutex::new(RemoteControllerStoreState {
                    hosts,
                    dirty: false,
                    retry_running: false,
                    last_write_error: None,
                }),
            }),
        }
    }

    /// Saved hosts for display. A failed refresh keeps the last good snapshot
    /// visible and emits a trace; it never makes every paired device disappear.
    pub fn list(&self) -> Vec<SavedRemoteHost> {
        match self.lifecycle_snapshot() {
            Ok(snapshot) => {
                if let Some(error) = snapshot.refresh_error.as_ref() {
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "controller_store_read_failed phase=display path={} error={error}",
                            self.inner.path.display()
                        ),
                    );
                }
                snapshot.hosts
            }
            Err(error) => {
                crate::runtime_trace::runtime_trace(
                    "remote",
                    &format!(
                        "controller_store_read_failed phase=display path={} error={error}",
                        self.inner.path.display()
                    ),
                );
                Vec::new()
            }
        }
    }

    /// Snapshot used to decide which host lifecycles must converge. Known hosts
    /// and a failed disk refresh travel together, so callers can keep workers
    /// alive without hiding the failure or guessing that hosts vanished.
    pub(crate) fn lifecycle_snapshot(&self) -> Result<RemoteControllerStoreSnapshot, String> {
        let _write = self.lock()?;
        let mut state = self.state()?;
        let refresh_error = self.refresh_clean_state(&mut state).err();
        Ok(RemoteControllerStoreSnapshot {
            hosts: state.hosts.clone(),
            refresh_error,
        })
    }

    /// Authoritative lifecycle lookup.
    ///
    /// Unlike [`Self::list`], this never turns an unreadable projection into
    /// confirmed absence. The last good in-memory snapshot stays intact, while
    /// the caller receives the read error and can keep reconciliation alive.
    pub fn find(&self, device_id: &str) -> Result<Option<SavedRemoteHost>, String> {
        let snapshot = self.lifecycle_snapshot()?;
        if let Some(error) = snapshot.refresh_error {
            return Err(error);
        }
        Ok(snapshot
            .hosts
            .iter()
            .find(|host| host.device_id == device_id)
            .cloned())
    }

    /// Insert or replace a saved host (keyed by `device_id`). Pairing is not
    /// acknowledged until this required write succeeds, so unlike derived
    /// address refreshes this mutation is not retained after an error.
    pub fn upsert(&self, mut host: SavedRemoteHost) -> Result<Vec<SavedRemoteHost>, String> {
        host.normalize();
        self.mutate(MutationFailure::RollBack, move |hosts| {
            if let Some(existing) = hosts
                .iter_mut()
                .find(|existing| existing.device_id == host.device_id)
            {
                *existing = host;
            } else {
                hosts.push(host);
            }
            (hosts.clone(), true)
        })
    }

    /// Remove a host durably. On failure both memory and disk keep the host, so
    /// the caller cannot report a forget that a restart would undo.
    pub fn remove(&self, device_id: &str) -> Result<Vec<SavedRemoteHost>, String> {
        self.mutate(MutationFailure::RollBack, |hosts| {
            let previous_len = hosts.len();
            hosts.retain(|host| host.device_id != device_id);
            (hosts.clone(), hosts.len() != previous_len)
        })
    }

    /// Change a host that is still saved, and only if it is.
    ///
    /// The read, the change and the write happen under one lock, so concurrent
    /// updates to different devices cannot discard each other — which a caller
    /// doing `find` then `upsert` around its own edit cannot avoid.
    ///
    /// Absent means absent: a caller holding a host it read moments ago must not
    /// be able to write it back after the user forgot the device. A failed save
    /// keeps the changed in-memory snapshot dirty and starts one retry worker;
    /// later changes replace that pending intent rather than racing it.
    ///
    /// Returns the stored host when `change` produced one, `None` when the
    /// device is gone or `change` declined. The initial write error is returned
    /// for observability even though the store retains and retries the intent.
    pub fn update_existing(
        &self,
        device_id: &str,
        change: impl FnOnce(&SavedRemoteHost) -> Option<SavedRemoteHost>,
    ) -> Result<Option<SavedRemoteHost>, String> {
        self.mutate(MutationFailure::RetainAndRetry, |hosts| {
            let Some(slot) = hosts.iter_mut().find(|host| host.device_id == device_id) else {
                return (None, false);
            };
            let Some(mut updated) = change(slot) else {
                return (None, false);
            };
            updated.normalize();
            *slot = updated.clone();
            (Some(updated), true)
        })
    }

    /// Sole read-modify-write entry point. Every mutation uses the same latest
    /// snapshot and writer lock; only failure policy differs between required
    /// user intent and refresh metadata that can safely converge in background.
    fn mutate<T>(
        &self,
        failure: MutationFailure,
        change: impl FnOnce(&mut Vec<SavedRemoteHost>) -> (T, bool),
    ) -> Result<T, String> {
        let guard = self.lock()?;
        let mut state = self.state()?;
        let mut hosts = self.load_for_write(&mut state)?;
        let (output, changed) = change(&mut hosts);
        if !changed {
            return Ok(output);
        }
        let (result, start_retry) = self.persist_snapshot(&mut state, hosts, failure);
        drop(state);
        drop(guard);
        if start_retry {
            self.spawn_retry_worker();
        }
        result.map(|()| output)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.inner
            .write_lock
            .lock()
            .map_err(|_| "remote controller store lock is poisoned".to_string())
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, RemoteControllerStoreState>, String> {
        self.inner
            .state
            .lock()
            .map_err(|_| "remote controller store state is poisoned".to_string())
    }

    fn load_path(path: &std::path::Path) -> Result<Vec<SavedRemoteHost>, String> {
        match Self::read_path(path) {
            Ok(Some(hosts)) => Ok(hosts),
            Ok(None) => Ok(Self::read_path(&Self::backup_path(path))?.unwrap_or_default()),
            Err(error) => Err(error),
        }
    }

    fn read_path(path: &std::path::Path) -> Result<Option<Vec<SavedRemoteHost>>, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            // Nothing saved yet is not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "remote controller store could not be read: {error}"
                ));
            }
        };
        let mut hosts: Vec<SavedRemoteHost> = serde_json::from_str(&text)
            .map_err(|error| format!("remote controller store could not be read: {error}"))?;
        for host in &mut hosts {
            host.normalize();
        }
        Ok(Some(hosts))
    }

    /// The saved hosts, refusing to guess when the file cannot be read.
    ///
    /// A write starts from what is already stored, so treating an unreadable
    /// file as "no devices" would save an empty list over it and destroy every
    /// pairing on the machine. Failing leaves the file untouched for recovery.
    fn load_for_write(
        &self,
        state: &mut RemoteControllerStoreState,
    ) -> Result<Vec<SavedRemoteHost>, String> {
        self.refresh_clean_state(state)?;
        Ok(state.hosts.clone())
    }

    /// Sole durable refresh of the authoritative in-process snapshot. Dirty
    /// state already contains newer intent and must never be replaced from disk.
    fn refresh_clean_state(&self, state: &mut RemoteControllerStoreState) -> Result<(), String> {
        if !state.dirty {
            state.hosts = Self::load_path(&self.inner.path)?;
        }
        Ok(())
    }

    fn save(&self, hosts: &[SavedRemoteHost]) -> Result<(), String> {
        Self::save_path(&self.inner.path, hosts)
    }

    fn save_path(path: &std::path::Path, hosts: &[SavedRemoteHost]) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = serde_json::to_string_pretty(hosts).map_err(|error| error.to_string())?;
        // Write beside the target and rename over it. A truncating write leaves
        // a half-written file if the process dies mid-save, and that file is
        // every pairing this machine has.
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, text).map_err(|error| error.to_string())?;

        // Windows rename does not replace an existing file. Keep one known-good
        // backup across the remove+rename gap, and restore it if replacement
        // fails. This matches the durable operation-store mechanism.
        #[cfg(windows)]
        {
            let backup = Self::backup_path(path);
            if path.exists() {
                let backup_temporary = path.with_extension("json.bak.tmp");
                let _ = fs::remove_file(&backup_temporary);
                fs::copy(path, &backup_temporary).map_err(|error| error.to_string())?;
                if backup.exists() {
                    fs::remove_file(&backup).map_err(|error| error.to_string())?;
                }
                fs::rename(&backup_temporary, &backup).map_err(|error| error.to_string())?;
                fs::remove_file(path).map_err(|error| error.to_string())?;
                if let Err(error) = fs::rename(&temporary, path) {
                    let _ = fs::copy(&backup, path);
                    let _ = fs::remove_file(&temporary);
                    return Err(error.to_string());
                }
                return Ok(());
            }
        }

        fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            error.to_string()
        })
    }

    fn backup_path(path: &std::path::Path) -> PathBuf {
        path.with_extension("json.bak")
    }

    fn persist_snapshot(
        &self,
        state: &mut RemoteControllerStoreState,
        mut hosts: Vec<SavedRemoteHost>,
        failure: MutationFailure,
    ) -> (Result<(), String>, bool) {
        for host in &mut hosts {
            host.normalize();
        }
        let previous_hosts = std::mem::replace(&mut state.hosts, hosts);
        let previous_dirty = state.dirty;
        let previous_error = state.last_write_error.take();
        state.dirty = true;

        let result = self.save(&state.hosts);
        match (&result, failure) {
            (Ok(()), _) => {
                state.dirty = false;
                state.last_write_error = None;
                (result, false)
            }
            (Err(error), MutationFailure::RetainAndRetry) => {
                state.last_write_error = Some(error.clone());
                let start_retry = !state.retry_running;
                state.retry_running = true;
                (result, start_retry)
            }
            (Err(_), MutationFailure::RollBack) => {
                state.hosts = previous_hosts;
                state.dirty = previous_dirty;
                state.last_write_error = previous_error;
                let start_retry = state.dirty && !state.retry_running;
                if start_retry {
                    state.retry_running = true;
                }
                (result, start_retry)
            }
        }
    }

    fn spawn_retry_worker(&self) {
        let weak = Arc::downgrade(&self.inner);
        let weak_for_error = Weak::clone(&weak);
        let spawn = std::thread::Builder::new()
            .name("codux-controller-store".to_string())
            .spawn(move || Self::retry_dirty_store(weak));
        if let Err(error) = spawn {
            if let Some(inner) = weak_for_error.upgrade()
                && let Ok(mut state) = inner.state.lock()
            {
                state.retry_running = false;
            }
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!("controller_store_retry_not_started error={error}"),
            );
        }
    }

    fn retry_dirty_store(weak: Weak<RemoteControllerStoreInner>) {
        let mut delay = STORE_RETRY_INITIAL_DELAY;
        loop {
            std::thread::sleep(delay);
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let Ok(_write) = inner.write_lock.lock() else {
                crate::runtime_trace::runtime_trace(
                    "remote",
                    "controller_store_retry_stopped error=write_lock_poisoned",
                );
                return;
            };
            let Ok(mut state) = inner.state.lock() else {
                crate::runtime_trace::runtime_trace(
                    "remote",
                    "controller_store_retry_stopped error=state_lock_poisoned",
                );
                return;
            };
            if !state.dirty {
                state.retry_running = false;
                return;
            }
            let result = Self::save_path(&inner.path, &state.hosts);
            match result {
                Ok(()) => {
                    state.dirty = false;
                    state.retry_running = false;
                    state.last_write_error = None;
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!("controller_store_retry_ok path={}", inner.path.display()),
                    );
                    return;
                }
                Err(error) => {
                    delay = (delay * 2).min(STORE_RETRY_MAX_DELAY);
                    state.last_write_error = Some(error.clone());
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "controller_store_retry_failed path={} retry_in_ms={} error={error}",
                            inner.path.display(),
                            delay.as_millis()
                        ),
                    );
                }
            }
            drop(state);
            drop(_write);
            drop(inner);
        }
    }

    #[cfg(test)]
    pub(crate) fn hold_writes_for_test(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner.write_lock.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_support() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codux-controller-store-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn saved_host(device_id: &str) -> SavedRemoteHost {
        SavedRemoteHost {
            device_id: device_id.to_string(),
            host_id: "host-1".to_string(),
            host_name: String::new(),
            device_token: "token".to_string(),
            platform: String::new(),
            transports: vec![SavedRemoteTransport {
                kind: "iroh".to_string(),
                node_id: "node-abc".to_string(),
                relay_url: "https://relay.example".to_string(),
                relay_authentication: String::new(),
                ticket: "ticket".to_string(),
                direct_addrs: Vec::new(),
            }],
        }
    }

    fn wait_for_clean(store: &RemoteControllerStore, message: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while store.inner.state.lock().unwrap().dirty {
            assert!(std::time::Instant::now() < deadline, "{message}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_corrupt_file_is_never_silently_replaced_with_an_empty_one() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        store.upsert(saved_host("dev-1")).unwrap();
        let path = dir.join("remote-controllers.json");
        std::fs::write(&path, "{ this is not the array we wrote").unwrap();

        // Reading degrades to empty for display, but writing must not: treating
        // an unreadable file as "no devices" and saving over it destroys every
        // pairing on the machine.
        let error = store
            .upsert(saved_host("dev-2"))
            .expect_err("a write over an unreadable store must fail");
        assert!(error.contains("could not be read"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not the array we wrote",
            "the unreadable file must be left for recovery, not overwritten"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lifecycle_lookup_distinguishes_read_error_from_confirmed_absence() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        let host = saved_host("dev-1");
        store.upsert(host.clone()).unwrap();
        let path = dir.join("remote-controllers.json");
        let valid_store = std::fs::read_to_string(&path).unwrap();

        std::fs::write(&path, "{ unreadable").unwrap();
        let error = store
            .find("dev-1")
            .expect_err("an unreadable store is not confirmed absence");
        assert!(error.contains("could not be read"), "{error}");
        assert_eq!(
            store.inner.state.lock().unwrap().hosts,
            vec![host.clone()],
            "a read error must preserve the last good in-memory snapshot"
        );

        std::fs::write(&path, valid_store).unwrap();
        assert_eq!(store.find("dev-1").unwrap(), Some(host));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(store.find("dev-1").unwrap(), None);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_refresh_preserves_one_last_good_snapshot_for_all_consumers() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        let host = saved_host("dev-1");
        store.upsert(host.clone()).unwrap();
        let path = dir.join("remote-controllers.json");
        std::fs::write(&path, "{ unreadable").unwrap();

        assert_eq!(
            store.list(),
            vec![host.clone()],
            "display must not silently erase the last-good device list"
        );
        let snapshot = store.lifecycle_snapshot().unwrap();
        assert_eq!(snapshot.hosts, vec![host]);
        assert!(
            snapshot
                .refresh_error
                .is_some_and(|error| error.contains("could not be read")),
            "lifecycle enumeration needs hosts and refresh failure together"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_primary_recovers_the_last_known_good_backup() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        store.upsert(saved_host("dev-1")).unwrap();
        let path = dir.join("remote-controllers.json");
        std::fs::copy(&path, RemoteControllerStore::backup_path(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();

        let recovered = RemoteControllerStore::new(dir.clone());
        assert_eq!(recovered.find("dev-1").unwrap().unwrap().device_id, "dev-1");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_direct_addresses_are_trimmed_deduplicated_and_capped() {
        let transport: SavedRemoteTransport = serde_json::from_value(serde_json::json!({
            "kind": "iroh",
            "directAddrs": [
                " 10.8.0.3:41234 ",
                "",
                "10.8.0.3:41234",
                " 192.168.1.5:41234",
                "100.64.0.7:41234",
                "172.16.0.9:41234"
            ]
        }))
        .unwrap();

        assert_eq!(
            transport.direct_addrs,
            vec![
                "10.8.0.3:41234".to_string(),
                "192.168.1.5:41234".to_string(),
                "100.64.0.7:41234".to_string(),
            ]
        );
    }

    #[test]
    fn every_persisted_host_is_normalized_at_the_store_boundary() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        let mut host = saved_host("dev-1");
        host.transports[0].direct_addrs = vec![
            " 10.8.0.3:41234 ".to_string(),
            "10.8.0.3:41234".to_string(),
            "192.168.1.5:41234".to_string(),
            "100.64.0.7:41234".to_string(),
        ];

        let saved = store.upsert(host).unwrap();
        assert_eq!(saved[0].transports[0].direct_addrs.len(), 3);
        let reloaded = RemoteControllerStore::new(dir.clone());
        assert_eq!(
            reloaded.find("dev-1").unwrap().unwrap().transports[0].direct_addrs,
            saved[0].transports[0].direct_addrs
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn updating_a_forgotten_device_does_not_resurrect_it() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        store.upsert(saved_host("dev-1")).unwrap();
        store.remove("dev-1").unwrap();

        // A path callback holding a host read before the removal must not write
        // it back. Address bookkeeping is not a reason to re-pair a device the
        // user just forgot.
        let updated = store
            .update_existing("dev-1", |host| {
                let mut host = host.clone();
                host.host_name = "resurrected".to_string();
                Some(host)
            })
            .expect("no error");

        assert!(updated.is_none());
        assert!(store.find("dev-1").unwrap().is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_updates_to_different_devices_do_not_overwrite_each_other() {
        let dir = temp_support();
        let store = Arc::new(RemoteControllerStore::new(dir.clone()));
        store.upsert(saved_host("dev-1")).unwrap();
        store.upsert(saved_host("dev-2")).unwrap();

        let handles: Vec<_> = ["dev-1", "dev-2"]
            .into_iter()
            .map(|device_id| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for index in 0..25 {
                        store
                            .update_existing(device_id, |host| {
                                let mut host = host.clone();
                                host.host_name = format!("{device_id}-{index}");
                                Some(host)
                            })
                            .expect("update");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        // Read-modify-write outside a lock loses whichever update lands second.
        assert_eq!(store.find("dev-1").unwrap().unwrap().host_name, "dev-1-24");
        assert_eq!(store.find("dev-2").unwrap().unwrap().host_name, "dev-2-24");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn newer_update_cannot_be_overwritten_by_an_older_write() {
        let dir = temp_support();
        let store = Arc::new(RemoteControllerStore::new(dir.clone()));
        store.upsert(saved_host("dev-1")).unwrap();
        let (old_started_tx, old_started_rx) = std::sync::mpsc::sync_channel(0);
        let (release_old_tx, release_old_rx) = std::sync::mpsc::sync_channel(0);

        let old_store = Arc::clone(&store);
        let old = std::thread::spawn(move || {
            old_store
                .update_existing("dev-1", |host| {
                    old_started_tx.send(()).unwrap();
                    release_old_rx.recv().unwrap();
                    let mut host = host.clone();
                    host.host_name = "old".to_string();
                    Some(host)
                })
                .unwrap();
        });
        old_started_rx.recv().unwrap();

        let (new_started_tx, new_started_rx) = std::sync::mpsc::sync_channel(0);
        let new_store = Arc::clone(&store);
        let new = std::thread::spawn(move || {
            new_started_tx.send(()).unwrap();
            new_store
                .update_existing("dev-1", |host| {
                    let mut host = host.clone();
                    host.host_name = "new".to_string();
                    Some(host)
                })
                .unwrap();
        });
        new_started_rx.recv().unwrap();
        release_old_tx.send(()).unwrap();
        old.join().unwrap();
        new.join().unwrap();

        let reloaded = RemoteControllerStore::new(dir.clone());
        assert_eq!(reloaded.find("dev-1").unwrap().unwrap().host_name, "new");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_derived_write_retries_the_latest_snapshot() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        store.upsert(saved_host("dev-1")).unwrap();
        let blocked_temporary = dir.join("remote-controllers.json.tmp");
        std::fs::create_dir(&blocked_temporary).unwrap();

        let first_error = store
            .update_existing("dev-1", |host| {
                let mut host = host.clone();
                host.host_name = "stale-pending".to_string();
                Some(host)
            })
            .expect_err("the temporary-path directory must reject the write");
        assert!(!first_error.is_empty());
        let second_error = store
            .update_existing("dev-1", |host| {
                let mut host = host.clone();
                host.host_name = "latest-pending".to_string();
                Some(host)
            })
            .expect_err("the second write is still blocked");
        assert!(!second_error.is_empty());
        assert_eq!(
            store.find("dev-1").unwrap().unwrap().host_name,
            "latest-pending",
            "the in-process owner must expose the retained latest intent"
        );
        {
            let state = store.inner.state.lock().unwrap();
            assert!(state.dirty);
            assert!(state.retry_running);
            assert!(state.last_write_error.is_some());
        }

        std::fs::remove_dir(&blocked_temporary).unwrap();
        wait_for_clean(
            &store,
            "dirty store was not retried after the write path recovered",
        );

        let reloaded = RemoteControllerStore::new(dir.clone());
        assert_eq!(
            reloaded.find("dev-1").unwrap().unwrap().host_name,
            "latest-pending",
            "retry must persist the latest snapshot, never the first failed one"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn required_write_failure_rolls_back_without_losing_pending_refresh() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        store.upsert(saved_host("dev-1")).unwrap();
        store.upsert(saved_host("dev-2")).unwrap();
        let blocked_temporary = dir.join("remote-controllers.json.tmp");
        std::fs::create_dir(&blocked_temporary).unwrap();

        store
            .update_existing("dev-1", |host| {
                let mut host = host.clone();
                host.host_name = "pending-refresh".to_string();
                Some(host)
            })
            .expect_err("refresh write must be retained while blocked");
        store
            .remove("dev-2")
            .expect_err("forget must fail closed while it cannot be persisted");
        store
            .upsert(saved_host("dev-3"))
            .expect_err("pairing must fail closed while it cannot be persisted");

        assert_eq!(
            store.find("dev-1").unwrap().unwrap().host_name,
            "pending-refresh"
        );
        assert!(
            store.find("dev-2").unwrap().is_some(),
            "failed forget must roll back"
        );
        assert!(
            store.find("dev-3").unwrap().is_none(),
            "failed pairing must not appear later as a ghost success"
        );

        std::fs::remove_dir(&blocked_temporary).unwrap();
        wait_for_clean(
            &store,
            "retained refresh did not converge after required writes rolled back",
        );

        let reloaded = RemoteControllerStore::new(dir.clone());
        assert_eq!(
            reloaded.find("dev-1").unwrap().unwrap().host_name,
            "pending-refresh"
        );
        assert!(reloaded.find("dev-2").unwrap().is_some());
        assert!(reloaded.find("dev-3").unwrap().is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upsert_find_remove_round_trip() {
        let dir = temp_support();
        let store = RemoteControllerStore::new(dir.clone());
        assert!(store.list().is_empty());

        let host = SavedRemoteHost {
            device_id: "dev-1".to_string(),
            host_id: "host-1".to_string(),
            host_name: "Studio".to_string(),
            device_token: "device-token-1".to_string(),
            platform: "macos".to_string(),
            transports: vec![SavedRemoteTransport {
                kind: "iroh".to_string(),
                node_id: "node-abc".to_string(),
                relay_url: "https://relay.example".to_string(),
                relay_authentication: String::new(),
                ticket: "endpoint-ticket-bytes".to_string(),
                direct_addrs: vec!["10.8.0.3:41234".to_string()],
            }],
        };
        store.upsert(host.clone()).unwrap();

        // Reloaded from disk by a fresh store instance.
        let reloaded = RemoteControllerStore::new(dir.clone());
        assert_eq!(reloaded.find("dev-1").unwrap(), Some(host.clone()));

        // Upsert replaces in place (no duplicate).
        let mut renamed = host.clone();
        renamed.host_name = "Studio Renamed".to_string();
        let hosts = reloaded.upsert(renamed.clone()).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host_name, "Studio Renamed");

        reloaded.remove("dev-1").unwrap();
        assert!(reloaded.list().is_empty());

        std::fs::remove_dir_all(dir).ok();
    }
}
