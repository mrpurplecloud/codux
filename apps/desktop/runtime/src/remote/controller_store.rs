//! Persisted dial targets for the desktop-as-controller: hosts this desktop has
//! paired with and can reconnect to without re-pairing. This is the OUTBOUND
//! counterpart to `RemoteDeviceSummary` (which records INBOUND devices paired
//! into this host). Stored as `remote-controllers.json` under the support dir.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// One reconnectable transport candidate for a saved host. The iroh pairing
/// ticket is single-use, so reconnect relies on `node_id` + `relay_url`.
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
    /// The ticket is a snapshot from pairing time; these follow the host as it
    /// moves. Dialled alongside it, never instead of it.
    #[serde(default)]
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
        if addr.is_empty() || self.direct_addrs.first().is_some_and(|first| first == addr) {
            return None;
        }
        let mut next = Vec::with_capacity(Self::MAX_DIRECT_ADDRS);
        next.push(addr.to_string());
        next.extend(
            self.direct_addrs
                .iter()
                .filter(|item| item.as_str() != addr)
                .take(Self::MAX_DIRECT_ADDRS - 1)
                .cloned(),
        );
        Some(Self {
            direct_addrs: next,
            ..self.clone()
        })
    }
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

pub struct RemoteControllerStore {
    path: PathBuf,
    /// Serialises read-modify-write. Every mutation reads the whole file,
    /// changes one host, and writes it all back, so two of them interleaving
    /// silently discards one — and they arrive from transport callbacks on
    /// whatever thread the link happens to be running on.
    write_lock: Mutex<()>,
}

impl RemoteControllerStore {
    pub fn new(support_dir: PathBuf) -> Self {
        Self {
            path: support_dir.join("remote-controllers.json"),
            write_lock: Mutex::new(()),
        }
    }

    /// Saved hosts for display. An unreadable file reads as empty here — the UI
    /// has nothing better to show — but see [`Self::load_for_write`]: emptiness
    /// from a failed read must never reach a write.
    pub fn list(&self) -> Vec<SavedRemoteHost> {
        self.load().unwrap_or_default()
    }

    pub fn find(&self, device_id: &str) -> Option<SavedRemoteHost> {
        self.list()
            .into_iter()
            .find(|host| host.device_id == device_id)
    }

    /// Insert or replace a saved host (keyed by `device_id`).
    pub fn upsert(&self, host: SavedRemoteHost) -> Result<Vec<SavedRemoteHost>, String> {
        let _guard = self.lock()?;
        let mut hosts = self.load_for_write()?;
        if let Some(existing) = hosts
            .iter_mut()
            .find(|existing| existing.device_id == host.device_id)
        {
            *existing = host;
        } else {
            hosts.push(host);
        }
        self.save(&hosts)?;
        Ok(hosts)
    }

    pub fn remove(&self, device_id: &str) -> Result<Vec<SavedRemoteHost>, String> {
        let _guard = self.lock()?;
        let mut hosts = self.load_for_write()?;
        hosts.retain(|host| host.device_id != device_id);
        self.save(&hosts)?;
        Ok(hosts)
    }

    /// Change a host that is still saved, and only if it is.
    ///
    /// The read, the change and the write happen under one lock, so concurrent
    /// updates to different devices cannot discard each other — which a caller
    /// doing `find` then `upsert` around its own edit cannot avoid.
    ///
    /// Absent means absent: a caller holding a host it read moments ago must not
    /// be able to write it back after the user forgot the device. Returns the
    /// stored host when `change` produced one, `None` when the device is gone or
    /// `change` declined.
    pub fn update_existing(
        &self,
        device_id: &str,
        change: impl FnOnce(&SavedRemoteHost) -> Option<SavedRemoteHost>,
    ) -> Result<Option<SavedRemoteHost>, String> {
        let _guard = self.lock()?;
        let mut hosts = self.load_for_write()?;
        let Some(slot) = hosts.iter_mut().find(|host| host.device_id == device_id) else {
            return Ok(None);
        };
        let Some(updated) = change(slot) else {
            return Ok(None);
        };
        *slot = updated.clone();
        self.save(&hosts)?;
        Ok(Some(updated))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.write_lock
            .lock()
            .map_err(|_| "remote controller store lock is poisoned".to_string())
    }

    fn load(&self) -> Result<Vec<SavedRemoteHost>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            // Nothing saved yet is not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "remote controller store could not be read: {error}"
                ));
            }
        };
        serde_json::from_str(&text)
            .map_err(|error| format!("remote controller store could not be read: {error}"))
    }

    /// The saved hosts, refusing to guess when the file cannot be read.
    ///
    /// A write starts from what is already stored, so treating an unreadable
    /// file as "no devices" would save an empty list over it and destroy every
    /// pairing on the machine. Failing leaves the file untouched for recovery.
    fn load_for_write(&self) -> Result<Vec<SavedRemoteHost>, String> {
        self.load()
    }

    fn save(&self, hosts: &[SavedRemoteHost]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = serde_json::to_string_pretty(hosts).map_err(|error| error.to_string())?;
        // Write beside the target and rename over it. A truncating write leaves
        // a half-written file if the process dies mid-save, and that file is
        // every pairing this machine has.
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, text).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            error.to_string()
        })
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
        assert!(store.find("dev-1").is_none());

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
        assert_eq!(store.find("dev-1").unwrap().host_name, "dev-1-24");
        assert_eq!(store.find("dev-2").unwrap().host_name, "dev-2-24");

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
        assert_eq!(reloaded.find("dev-1"), Some(host.clone()));

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
