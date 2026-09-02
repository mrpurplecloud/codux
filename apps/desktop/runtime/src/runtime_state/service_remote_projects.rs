// Desktop-as-controller project catalog sync: subscribe to each connected
// host's `projects` resource, reconcile its authoritative snapshots into the
// local projection, and keep the transport's project-id bridge current.
//
// The host owns the catalog, so nothing here creates membership locally. A host
// that does not advertise the resource keeps the pre-change behaviour: its
// projects stay whatever the desktop already has.

/// Bumped when every paired host must run a new migration step. A store that
/// records this version for a host has already handed its legacy remote records
/// over, and the host's catalog is authoritative from then on.
const REMOTE_PROJECT_MIGRATION_VERSION: u64 = 1;

/// Per-host sync state, keyed by device id and pinned to one connection.
///
/// `connection_epoch` scopes the whole record, and is what keeps the version
/// guard tied to one host runtime instance: resource versions restart at zero
/// when the host process does, and a host restart always drops the link, so a
/// new epoch — with a cleared guard and a fresh capability probe — is exactly
/// what a new runtime instance produces.
#[derive(Clone, Debug, Default)]
pub(crate) struct RemoteProjectSyncState {
    connection_epoch: u64,
    /// Whether a capability probe for this connection has been taken on. Held
    /// separately from the record's existence, because a mutation can create the
    /// record first and must not cancel the subscription.
    probe_claimed: bool,
    /// The host's reported runtime instance; recorded once the capability probe
    /// answers, which is also how a caller can tell the probe finished.
    runtime_instance_id: String,
    /// Whether this host shares its project catalog with its controllers.
    shares_catalog: bool,
    /// Highest catalog version applied on this connection.
    version: Option<u64>,
}

impl RuntimeService {
    /// Bring every connected host's project catalog up to date. Called on the
    /// runtime tick: cheap when nothing changed, and the one blocking step
    /// (subscribe on a fresh connection) runs off the caller's thread.
    pub(crate) fn sync_remote_project_catalogs(&self) -> Vec<RemoteHostEvent> {
        let mut events = Vec::new();
        for (device_id, epoch, controller) in self.remote_controllers.connected_controllers() {
            if self.claim_remote_project_subscription(&device_id, epoch) {
                self.spawn_remote_project_subscription(device_id, epoch, controller);
                continue;
            }
            for payload in controller.drain_pushed_project_lists() {
                match self.apply_remote_project_snapshot(&device_id, epoch, &payload, &controller) {
                    Ok(true) => events.push(RemoteHostEvent::ProjectsChanged),
                    Ok(false) => {}
                    Err(error) => crate::runtime_trace::runtime_trace(
                        "remote-projects",
                        &format!("ignored snapshot from {device_id}: {error}"),
                    ),
                }
            }
        }
        events
    }

    /// Reserve the once-per-connection subscription for `device_id`. Returns
    /// `true` to the caller that must run it; every later tick on the same
    /// connection gets `false`, so exactly one probe is ever in flight per host.
    fn claim_remote_project_subscription(&self, device_id: &str, epoch: u64) -> bool {
        let Ok(mut states) = self.remote_project_sync.lock() else {
            return false;
        };
        let state = sync_state_for(&mut states, device_id, epoch);
        if state.probe_claimed {
            return false;
        }
        state.probe_claimed = true;
        true
    }

    /// Release a failed claim so the next tick tries again. Without this a
    /// single unlucky `host.info` would leave the host unsynchronized until it
    /// reconnected; the claim keeps the retry to one attempt at a time.
    fn release_remote_project_subscription(&self, device_id: &str, epoch: u64) {
        if let Ok(mut states) = self.remote_project_sync.lock()
            && let Some(state) = states
                .get_mut(device_id)
                .filter(|state| state.connection_epoch == epoch)
        {
            state.probe_claimed = false;
        }
    }

    fn spawn_remote_project_subscription(
        &self,
        device_id: String,
        epoch: u64,
        controller: Arc<crate::remote::RemoteController>,
    ) {
        let service = self.clone();
        crate::async_runtime::spawn_blocking(move || {
            if let Err(error) =
                service.subscribe_remote_project_catalog(&device_id, epoch, &controller)
            {
                crate::runtime_trace::runtime_trace(
                    "remote-projects",
                    &format!("subscribe failed device={device_id} error={error}"),
                );
                service.release_remote_project_subscription(&device_id, epoch);
            }
        });
    }

    fn subscribe_remote_project_catalog(
        &self,
        device_id: &str,
        epoch: u64,
        controller: &crate::remote::RemoteController,
    ) -> Result<(), String> {
        let info = controller.host_info()?;
        let runtime_instance_id = info
            .get("runtimeInstanceId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let shares_catalog = host_info_shares_projects(&info);
        crate::runtime_trace::runtime_trace(
            "remote-projects",
            &format!(
                "probe device={device_id} runtime_instance={runtime_instance_id} shares_catalog={shares_catalog}"
            ),
        );
        {
            let mut states = self
                .remote_project_sync
                .lock()
                .map_err(|_| "Remote project sync state is poisoned.".to_string())?;
            let state = sync_state_for(&mut states, device_id, epoch);
            state.runtime_instance_id = runtime_instance_id;
            state.shares_catalog = shares_catalog;
        }
        if !shares_catalog {
            // Pre-change or desktop host: keep whatever projects this desktop
            // already holds for it.
            return Ok(());
        }
        let baseline = controller.subscribe_projects()?;
        let store = ProjectStore::new(self.support_dir.clone());
        let pending_migration =
            store.remote_project_migration_version(device_id) < REMOTE_PROJECT_MIGRATION_VERSION;
        let snapshot = if pending_migration {
            self.register_legacy_remote_projects(device_id, controller, &store, baseline)?
        } else {
            baseline
        };
        let changed = self.apply_remote_project_snapshot(device_id, epoch, &snapshot, controller)?;
        if pending_migration {
            // Only now: a marker stored before the reconciliation lands would
            // leave a legacy record unmigrated and never looked at again.
            store.set_remote_project_migration_version(
                device_id,
                REMOTE_PROJECT_MIGRATION_VERSION,
            )?;
        }
        if changed && let Ok(mut events) = self.hosted_runtime_events.lock() {
            events.push_back(RemoteHostEvent::ProjectsChanged);
        }
        Ok(())
    }

    /// Hand this desktop's legacy remote records — the ones it created back when
    /// it was the only place they existed — to the host that can serve them,
    /// once per host. Returns the catalog to reconcile: the one the host
    /// committed when anything was registered, otherwise the baseline.
    ///
    /// `project.add` is idempotent by the host's normalized path, so a path the
    /// host already knows keeps that entry's name and host id; migration adds
    /// membership and never renames shared state. Attaching the host ids before
    /// reconciling is what keeps a migrated record on its desktop id — a
    /// reconciliation that ran first would build a second projection for the
    /// same catalog entry.
    ///
    /// A failed registration returns the error with no marker stored and no
    /// record pruned, so the next tick retries the whole set; re-registering an
    /// already-registered path is a no-op on the host.
    fn register_legacy_remote_projects(
        &self,
        device_id: &str,
        controller: &crate::remote::RemoteController,
        store: &ProjectStore,
        baseline: Value,
    ) -> Result<Value, String> {
        let legacy = store.legacy_remote_projects(device_id);
        if legacy.is_empty() {
            return Ok(baseline);
        }
        crate::runtime_trace::runtime_trace(
            "remote-projects",
            &format!(
                "migrating {} legacy record(s) to device={device_id}",
                legacy.len()
            ),
        );
        let mut assignments = Vec::with_capacity(legacy.len());
        for project in &legacy {
            let host_project_id = controller.project_add(&project.name, &project.path)?;
            assignments.push((project.project_id.clone(), host_project_id));
        }
        store.attach_host_project_ids(device_id, &assignments)?;
        controller.project_list()
    }

    /// Reconcile one `project.list` snapshot and refresh the transport's id
    /// bridge. Returns whether the local store changed.
    fn apply_remote_project_snapshot(
        &self,
        device_id: &str,
        epoch: u64,
        payload: &Value,
        controller: &crate::remote::RemoteController,
    ) -> Result<bool, String> {
        if !self.accept_remote_project_version(device_id, epoch, payload)? {
            return Ok(false);
        }
        let catalog = remote_catalog_from_payload(payload)?;
        let store = ProjectStore::new(self.support_dir.clone());
        let changed = store.reconcile_remote_projects(device_id, &catalog)?;
        controller.set_project_id_pairs(store.remote_project_id_pairs(device_id));
        if changed {
            self.sync_pet_project_memberships();
        }
        Ok(changed)
    }

    /// Latest-wins within one host runtime instance: a duplicate or older
    /// version cannot roll this desktop back.
    fn accept_remote_project_version(
        &self,
        device_id: &str,
        epoch: u64,
        payload: &Value,
    ) -> Result<bool, String> {
        let version = payload.get("version").and_then(Value::as_u64);
        let mut states = self
            .remote_project_sync
            .lock()
            .map_err(|_| "Remote project sync state is poisoned.".to_string())?;
        let state = sync_state_for(&mut states, device_id, epoch);
        let Some(version) = version else {
            // An unversioned snapshot comes from a host without the resource
            // version stamp; apply it and keep the guard where it is.
            return Ok(true);
        };
        if state.version.is_some_and(|applied| version <= applied) {
            return Ok(false);
        }
        state.version = Some(version);
        Ok(true)
    }

    /// The live controller of a host that owns its project catalog, if this is
    /// such a host and it is reachable right now.
    ///
    /// The answer comes from the host itself rather than from the cached probe:
    /// a mutation can be raised before the connection's probe has answered, and
    /// routing it locally then would create exactly the phantom project the
    /// host-first flow exists to prevent. An unreachable host answers nothing,
    /// and the caller keeps the pre-change local behaviour.
    pub(crate) fn remote_project_catalog_host(
        &self,
        device_id: &str,
    ) -> Option<Arc<crate::remote::RemoteController>> {
        let controller = self
            .remote_controllers
            .controller_for_blocking(device_id, REMOTE_CONNECT_TIMEOUT)
            .inspect_err(|error| {
                crate::runtime_trace::runtime_trace(
                    "remote-projects",
                    &format!("catalog host unreachable device={device_id} error={error}"),
                )
            })
            .ok()?;
        host_info_shares_projects(&controller.host_info().ok()?).then_some(controller)
    }

    /// Register a project in a host's catalog, then adopt the catalog the host
    /// committed. Nothing is written locally until the host says yes.
    pub(crate) fn create_remote_project(
        &self,
        device_id: &str,
        controller: &crate::remote::RemoteController,
        request: ProjectCreateRequest,
    ) -> Result<ProjectListSnapshot, String> {
        let host_project_id = controller.project_add(&request.name, &request.path)?;
        let snapshot = controller.project_list()?;
        let epoch = self
            .remote_controllers
            .connection_epoch(device_id)
            .ok_or_else(|| format!("Remote host {device_id} disconnected during project add."))?;
        self.apply_remote_project_snapshot(device_id, epoch, &snapshot, controller)?;
        let store = ProjectStore::new(self.support_dir.clone());
        // Read the projection id back rather than deriving it: an add that the
        // host deduplicated onto an existing entry lands on whatever desktop id
        // that projection already had.
        let project_id = store
            .remote_project_id_pairs(device_id)
            .into_iter()
            .find(|(_, host_id)| host_id == &host_project_id)
            .map(|(project_id, _)| project_id)
            .ok_or_else(|| {
                format!("Remote host {device_id} did not list the project it just committed.")
            })?;
        store.apply_local_project_metadata(
            &project_id,
            request.badge_text.as_deref(),
            request.badge_symbol.as_deref(),
            request.badge_color_hex.as_deref(),
            request.environment_variables,
        )?;
        store.select_project(&project_id)?;
        let _ = self.mark_project_active_with_watch(&project_id);
        self.sync_pet_project_memberships();
        Ok(store.list_snapshot())
    }
}

/// This connection's sync state, reset when the device reconnected: a new
/// connection is a new host runtime instance, so its version guard and its
/// capability answer both start over.
fn sync_state_for<'a>(
    states: &'a mut HashMap<String, RemoteProjectSyncState>,
    device_id: &str,
    epoch: u64,
) -> &'a mut RemoteProjectSyncState {
    let state = states.entry(device_id.to_string()).or_default();
    if state.connection_epoch != epoch {
        *state = RemoteProjectSyncState {
            connection_epoch: epoch,
            ..RemoteProjectSyncState::default()
        };
    }
    state
}

/// A shared catalog is the headless host's contract. A desktop host also
/// advertises the `projects` resource, but its projects are its own controller
/// state — adopting them would silently import another desktop's project list,
/// so desktop-hosted projects keep behaving exactly as before.
fn host_info_shares_projects(info: &Value) -> bool {
    if info.get("app").and_then(Value::as_str) != Some(codux_protocol::REMOTE_APP_AGENT) {
        return false;
    }
    info.get("capabilities")
        .and_then(|capabilities| capabilities.get("resourceSubscriptions"))
        .and_then(Value::as_array)
        .is_some_and(|resources| {
            resources
                .iter()
                .filter_map(Value::as_str)
                .any(|resource| resource == codux_protocol::REMOTE_RESOURCE_PROJECTS)
        })
}

fn remote_catalog_from_payload(
    payload: &Value,
) -> Result<Vec<crate::project_store::RemoteCatalogProject>, String> {
    let projects = payload
        .get("projects")
        .and_then(Value::as_array)
        .ok_or_else(|| "Remote project snapshot is missing projects.".to_string())?;
    projects
        .iter()
        .map(|project| {
            let field = |key: &str| {
                project
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            };
            let host_project_id = field("id")
                .ok_or_else(|| "Remote project entry is missing id.".to_string())?
                .to_string();
            let path = field("path")
                .ok_or_else(|| "Remote project entry is missing path.".to_string())?
                .to_string();
            let name = field("name")
                .map(str::to_string)
                .unwrap_or_else(|| {
                    crate::path::file_name(&path).unwrap_or_else(|| "Project".to_string())
                });
            Ok(crate::project_store::RemoteCatalogProject {
                host_project_id,
                name,
                path,
            })
        })
        .collect()
}
