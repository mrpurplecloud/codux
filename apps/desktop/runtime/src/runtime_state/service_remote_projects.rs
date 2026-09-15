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

/// Who owns the projects of one paired device.
pub(crate) enum ProjectCatalogOwner {
    /// A headless host that owns its catalog and can be reached right now.
    Live(Arc<crate::remote::RemoteController>),
    /// A host this desktop has already synced a catalog with, unreachable now.
    /// Its projects are still its own: writing one here would diverge silently
    /// and be undone by its next snapshot.
    Unreachable,
    /// No shared catalog: a pre-change agent, a desktop host, or a device this
    /// desktop has never synced with. These keep the controller-local flows.
    NotACatalog,
}

impl ProjectCatalogOwner {
    fn require(self, device_id: &str) -> Result<Arc<crate::remote::RemoteController>, String> {
        match self {
            Self::Live(controller) => Ok(controller),
            _ => Err(format!("Remote host {device_id} is unavailable.")),
        }
    }
}

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
    /// Capability probe and, for a shared catalog, baseline/migration are done.
    /// Project pushes stay queued until this becomes true so they cannot race
    /// legacy ID attachment or bypass the host-type gate.
    sync_ready: bool,
    /// Highest catalog version applied on this connection.
    version: Option<u64>,
    #[cfg(test)]
    commit_pause: Option<RemoteProjectCommitPause>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteProjectCommitPoint {
    Snapshot,
    LegacyIds,
    MigrationMarker,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct RemoteProjectCommitPause {
    point: RemoteProjectCommitPoint,
    accepted: std::sync::mpsc::SyncSender<()>,
    resume: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl RuntimeService {
    #[cfg(test)]
    fn pause_next_remote_project_commit_for_test(
        &self,
        device_id: &str,
        epoch: u64,
        point: RemoteProjectCommitPoint,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (accepted, accepted_rx) = std::sync::mpsc::sync_channel(0);
        let (resume, resume_rx) = std::sync::mpsc::sync_channel(0);
        let mut states = self.remote_project_sync.lock().unwrap();
        let state = sync_state_for(&mut states, device_id, epoch).unwrap();
        state.commit_pause = Some(RemoteProjectCommitPause {
            point,
            accepted,
            resume: Arc::new(Mutex::new(resume_rx)),
        });
        (accepted_rx, resume)
    }

    #[cfg(test)]
    fn pause_remote_project_commit_for_test(
        &self,
        device_id: &str,
        epoch: u64,
        point: RemoteProjectCommitPoint,
    ) {
        let pause = self.remote_project_sync.lock().ok().and_then(|mut states| {
            states
                .get_mut(device_id)
                .filter(|state| state.connection_epoch == epoch)
                .and_then(|state| {
                    if state
                        .commit_pause
                        .as_ref()
                        .is_some_and(|pause| pause.point == point)
                    {
                        state.commit_pause.take()
                    } else {
                        None
                    }
                })
        });
        if let Some(pause) = pause {
            let _ = pause.accepted.send(());
            let _ = pause.resume.lock().map(|resume| resume.recv());
        }
    }

    /// Bring every connected host's project catalog up to date. Called on the
    /// runtime tick: cheap when nothing changed, and the one blocking step
    /// (subscribe on a fresh connection) runs off the caller's thread. Project
    /// changes enqueue their event in the same guarded commit that applies them.
    pub(crate) fn sync_remote_project_catalogs(&self) {
        for (device_id, epoch, controller) in self.remote_controllers.connected_controllers() {
            if self.claim_remote_project_subscription(&device_id, epoch) {
                self.spawn_remote_project_subscription(device_id, epoch, controller);
                continue;
            }
            match self.remote_project_push_policy(&device_id, epoch) {
                Some(true) => {}
                Some(false) => {
                    controller.drain_pushed_project_lists();
                    continue;
                }
                None => continue,
            }
            for payload in controller.drain_pushed_project_lists() {
                match self.apply_remote_project_snapshot(&device_id, epoch, &payload, &controller) {
                    Ok(true) => {
                        // Membership or paths changed; newly added projections
                        // need their own project-scoped terminal subscription.
                        self.subscribe_remote_terminal_projects(&device_id, &controller);
                    }
                    Ok(false) => {}
                    Err(error) => crate::runtime_trace::runtime_trace(
                        "remote-projects",
                        &format!("ignored snapshot from {device_id}: {error}"),
                    ),
                }
            }
        }
    }

    /// Reserve the once-per-connection subscription for `device_id`. Returns
    /// `true` to the caller that must run it; every later tick on the same
    /// connection gets `false`, so exactly one probe is ever in flight per host.
    fn claim_remote_project_subscription(&self, device_id: &str, epoch: u64) -> bool {
        let Ok(mut states) = self.remote_project_sync.lock() else {
            return false;
        };
        let Some(state) = sync_state_for(&mut states, device_id, epoch) else {
            return false;
        };
        if state.probe_claimed {
            return false;
        }
        state.probe_claimed = true;
        true
    }

    /// `None` while capability/baseline work is in flight, `Some(false)` for a
    /// host whose project lists are not authoritative, `Some(true)` once this
    /// Headless connection is ready to apply queued pushes.
    fn remote_project_push_policy(&self, device_id: &str, epoch: u64) -> Option<bool> {
        let states = self.remote_project_sync.lock().ok()?;
        let state = states
            .get(device_id)
            .filter(|state| state.connection_epoch == epoch && state.sync_ready)?;
        Some(state.shares_catalog)
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
            state.sync_ready = false;
        }
    }

    /// A catalog mutation cannot overtake first-connection migration. Otherwise
    /// a same-path add can create a fresh projection before the legacy Desktop
    /// id is attached, and migration then drops the legacy id and its metadata.
    fn ensure_remote_project_catalog_ready(
        &self,
        device_id: &str,
        controller: &crate::remote::RemoteController,
    ) -> Result<(), String> {
        let epoch = self
            .remote_controllers
            .live_connection_epoch(device_id, controller)
            .ok_or_else(|| format!("Remote host {device_id} disconnected."))?;
        let deadline = std::time::Instant::now() + REMOTE_CONNECT_TIMEOUT;
        loop {
            match self.remote_project_push_policy(device_id, epoch) {
                Some(true) => return Ok(()),
                Some(false) => {
                    return Err(format!(
                        "Remote host {device_id} no longer shares a project catalog."
                    ));
                }
                None => {}
            }
            if self.claim_remote_project_subscription(device_id, epoch) {
                if let Err(error) =
                    self.subscribe_remote_project_catalog(device_id, epoch, controller)
                {
                    self.release_remote_project_subscription(device_id, epoch);
                    return Err(error);
                }
                continue;
            }
            if self
                .remote_controllers
                .live_connection_epoch(device_id, controller)
                != Some(epoch)
            {
                return Err(format!("Remote host {device_id} disconnected."));
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "Remote host {device_id} project catalog is still synchronizing."
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
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
        let info = self.remote_controllers.host_info_via(device_id, controller)?;
        if self
            .remote_controllers
            .live_connection_epoch(device_id, controller)
            != Some(epoch)
        {
            return Ok(());
        }
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
            let Some(state) = sync_state_for(&mut states, device_id, epoch) else {
                return Ok(());
            };
            state.runtime_instance_id = runtime_instance_id;
            state.shares_catalog = shares_catalog;
            state.sync_ready = !shares_catalog;
        }
        if !shares_catalog {
            // Pre-change or desktop host: keep whatever projects this desktop
            // already holds for it.
            return Ok(());
        }
        // Remote reads stay outside the lifecycle hold. If this request outlives
        // its controller, each local commit below rejects that stale owner.
        let baseline = controller.subscribe_projects()?;
        let store = ProjectStore::new(self.support_dir.clone());
        let pending_migration =
            store.remote_project_migration_version(device_id) < REMOTE_PROJECT_MIGRATION_VERSION;
        let snapshot = if pending_migration {
            self.register_legacy_remote_projects(device_id, epoch, controller, &store, baseline)?
        } else {
            baseline
        };
        self.apply_remote_project_snapshot(device_id, epoch, &snapshot, controller)?;
        #[cfg(test)]
        self.pause_remote_project_commit_for_test(
            device_id,
            epoch,
            RemoteProjectCommitPoint::MigrationMarker,
        );
        let Some(completed) = self.remote_controllers.with_live_connection_owner(
            device_id,
            epoch,
            controller,
            || -> Result<(), String> {
                let mut states = self
                    .remote_project_sync
                    .lock()
                    .map_err(|_| "Remote project sync state is poisoned.".to_string())?;
                let Some(state) = sync_state_for(&mut states, device_id, epoch) else {
                    return Ok(());
                };
                if pending_migration {
                    // Only now: a marker stored before the reconciliation lands
                    // would leave a legacy record unmigrated forever.
                    store.set_remote_project_migration_version(
                        device_id,
                        REMOTE_PROJECT_MIGRATION_VERSION,
                    )?;
                }
                state.sync_ready = true;
                Ok(())
            },
        )?
        else {
            return Ok(());
        };
        completed?;
        // The project catalog baseline can be empty of changes for a host whose
        // projections were already local from a previous session. Subscribe for
        // every current projection so its terminal list still converges.
        self.subscribe_remote_terminal_projects(device_id, controller);
        Ok(())
    }

    /// Send one fire-and-forget project-scoped `terminals` subscription per
    /// remote projection on this device. The host answers with an unsolicited
    /// `terminal.list` (and pushes later changes), which the existing hosted
    /// workspace update path reconciles into the desktop terminal layout.
    fn subscribe_remote_terminal_projects(
        &self,
        device_id: &str,
        controller: &crate::remote::RemoteController,
    ) {
        let store = ProjectStore::new(self.support_dir.clone());
        for (local_project_id, _) in store.remote_project_id_pairs(device_id) {
            controller.subscribe_terminal_project(&local_project_id);
        }
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
        epoch: u64,
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
        // `project.add` is remote and idempotent by normalized path, so never
        // hold the lifecycle lock across it. If cancellation loses the race and
        // a stale request completes, the guarded local attachment below is
        // rejected; the next owner can safely retry registration.
        for project in &legacy {
            let host_project_id = controller.project_add(&project.name, &project.path)?;
            assignments.push((project.project_id.clone(), host_project_id));
        }
        #[cfg(test)]
        self.pause_remote_project_commit_for_test(
            device_id,
            epoch,
            RemoteProjectCommitPoint::LegacyIds,
        );
        let Some(attached) = self.remote_controllers.with_live_connection_owner(
            device_id,
            epoch,
            controller,
            || -> Result<(), String> {
                // Reserve publication before mutating the authoritative local
                // projection, the same ordering the catalog commit uses: a
                // poisoned queue must not leave a committed attachment that can
                // never produce its one generic refresh. The migration version
                // advances after this returns, so a dropped refresh here would
                // never be redone.
                let mut events = self
                    .hosted_runtime_events
                    .lock()
                    .map_err(|_| "Remote runtime event queue is poisoned.".to_string())?;
                let change = store.attach_host_project_ids(device_id, &assignments)?;
                if !change.is_empty() {
                    self.follow_remote_catalog_change(&change);
                    events.push_back(RemoteHostEvent::ProjectsChanged);
                }
                Ok(())
            },
        )?
        else {
            return Ok(baseline);
        };
        attached?;
        // Keep the follow-up remote read out of the lifecycle hold too. Its
        // snapshot still passes through the exact-owner commit guard.
        controller.project_list()
    }

    /// Reconcile one `project.list` snapshot and refresh the transport's id
    /// bridge. Returns whether the local store changed.
    ///
    /// Exact controller ownership is the outer hold; version check, store write,
    /// version record, local lifecycle, and generic publication complete before
    /// it is released. Latest-wins needs the applied version and catalog to be
    /// the same fact: recording a version around a failed write would make the
    /// host's next re-delivery a duplicate and ignore it forever, while releasing
    /// before publication could let a newer snapshot's effects land first.
    fn apply_remote_project_snapshot(
        &self,
        device_id: &str,
        epoch: u64,
        payload: &Value,
        controller: &crate::remote::RemoteController,
    ) -> Result<bool, String> {
        // Parsing is the caller's error to report, and it needs no lock.
        let catalog = remote_catalog_from_payload(payload)?;
        let version = match payload.get("version") {
            None => None,
            Some(version) => Some(version.as_u64().ok_or_else(|| {
                "Remote project snapshot has invalid version.".to_string()
            })?),
        };
        let store = ProjectStore::new(self.support_dir.clone());
        #[cfg(test)]
        self.pause_remote_project_commit_for_test(
            device_id,
            epoch,
            RemoteProjectCommitPoint::Snapshot,
        );
        let Some(change) = self.remote_controllers.with_live_connection_owner(
            device_id,
            epoch,
            controller,
            || -> Result<Option<_>, String> {
                let mut states = self
                    .remote_project_sync
                    .lock()
                    .map_err(|_| "Remote project sync state is poisoned.".to_string())?;
                let Some(state) = sync_state_for(&mut states, device_id, epoch) else {
                    return Ok(None);
                };
                // An unversioned snapshot comes from a host without the resource
                // version stamp; apply it and leave the guard where it is.
                if version.is_some_and(|version| {
                    state.version.is_some_and(|applied| version <= applied)
                }) {
                    return Ok(None);
                }
                // Reserve publication before mutating the authoritative local
                // projection. A poisoned queue must not leave a committed
                // change that can never produce its one generic refresh.
                let mut events = self
                    .hosted_runtime_events
                    .lock()
                    .map_err(|_| "Remote runtime event queue is poisoned.".to_string())?;
                let change = store.reconcile_remote_projects(device_id, &catalog)?;
                controller.set_project_id_pairs(store.remote_project_id_pairs(device_id));
                if let Some(version) = version {
                    state.version = Some(version);
                }
                if change.is_empty() {
                    return Ok(Some(false));
                }
                drop(states);
                self.follow_remote_catalog_change(&change);
                events.push_back(RemoteHostEvent::ProjectsChanged);
                Ok(Some(true))
            },
        )?
        else {
            return Ok(false);
        };
        Ok(change?.unwrap_or(false))
    }

    /// Follow a catalog change with the runtime lifecycle the local paths run.
    ///
    /// A projection removed on another controller leaves this desktop exactly
    /// like one closed here. Added and moved projections also join or refresh
    /// the activity index, and a newly selected or re-pathed projection starts
    /// its watchers.
    fn follow_remote_catalog_change(
        &self,
        change: &crate::project_store::RemoteCatalogChange,
    ) {
        let snapshot = ProjectStore::new(self.support_dir.clone()).list_snapshot();
        let selected = snapshot.selected_project_id;
        let removed_projects = !change.removed_project_ids.is_empty();
        if removed_projects {
            self.finish_project_removal(
                &change.removed_project_ids,
                &change.removed_workspace_ids,
                selected.as_deref(),
            );
        }
        for project in &snapshot.projects {
            if change.added_project_ids.contains(&project.id)
                || change.moved_project_ids.contains(&project.id)
            {
                self.project_activity.mark_project_summary(project);
            }
        }
        if !removed_projects {
            if let Some(selected) = selected.filter(|selected| {
                change.added_project_ids.contains(selected)
                    || change.moved_project_ids.contains(selected)
            }) {
                let _ = self.mark_project_active_with_watch(&selected);
            }
            self.sync_pet_project_memberships();
        }
    }

    /// Who owns the projects of `device_id`.
    ///
    /// Liveness is asked of the host rather than of the cached probe: a mutation
    /// can be raised before the connection's probe has answered, and deciding
    /// from a missing answer would create exactly the phantom project the
    /// host-first flow exists to prevent. A host that cannot answer is not
    /// therefore a host without a catalog — this desktop's own record of having
    /// synced with it says which of the two it is.
    pub(crate) fn project_catalog_owner(&self, device_id: &str) -> ProjectCatalogOwner {
        let was_catalog = ProjectStore::new(self.support_dir.clone())
            .remote_project_migration_version(device_id)
            > 0;
        let controller = self
            .remote_controllers
            .controller_for_blocking(device_id, REMOTE_CONNECT_TIMEOUT)
            .inspect_err(|error| {
                crate::runtime_trace::runtime_trace(
                    "remote-projects",
                    &format!("catalog host unreachable device={device_id} error={error}"),
                )
            })
            .ok();
        match controller {
            Some(controller) => match self
                .remote_controllers
                .host_info_via(device_id, &controller)
            {
                Ok(info) if host_info_shares_projects(&info) => match self
                    .ensure_remote_project_catalog_ready(device_id, &controller)
                {
                    Ok(()) => ProjectCatalogOwner::Live(controller),
                    Err(error) => {
                        crate::runtime_trace::runtime_trace(
                            "remote-projects",
                            &format!(
                                "catalog not ready device={device_id} error={error}"
                            ),
                        );
                        ProjectCatalogOwner::Unreachable
                    }
                },
                Ok(_) if was_catalog => ProjectCatalogOwner::Unreachable,
                Ok(_) => ProjectCatalogOwner::NotACatalog,
                Err(error) => {
                    crate::runtime_trace::runtime_trace(
                        "remote-projects",
                        &format!(
                            "catalog capability unavailable device={device_id} error={error}"
                        ),
                    );
                    // A live link whose host contract could not be read is not
                    // evidence that the host has no catalog. A local write here
                    // could become a permanent controller-only phantom.
                    ProjectCatalogOwner::Unreachable
                }
            },
            // The migration marker is this desktop's record that the host once
            // served it a catalog; from then on that host owns its projects
            // whether or not it can be reached.
            None if was_catalog => ProjectCatalogOwner::Unreachable,
            None => ProjectCatalogOwner::NotACatalog,
        }
    }

    /// Resolve a target that may still use the pre-catalog controller-local
    /// model. A live catalog result also means first-connection migration has
    /// completed, so callers can safely classify a legacy record afterwards.
    pub(crate) fn remote_project_catalog_host_if_any(
        &self,
        device_id: &str,
    ) -> Result<Option<Arc<crate::remote::RemoteController>>, String> {
        match self.project_catalog_owner(device_id) {
            ProjectCatalogOwner::NotACatalog => Ok(None),
            owner => owner.require(device_id).map(Some),
        }
    }

    /// Rename or re-path a project in a host's catalog, then adopt the catalog
    /// the host committed. The host owns name and path; the badge and
    /// environment the user typed stay this desktop's own.
    pub(crate) fn edit_remote_project(
        &self,
        device_id: &str,
        host_project_id: &str,
        controller: &crate::remote::RemoteController,
        request: ProjectUpdateRequest,
    ) -> Result<ProjectListSnapshot, String> {
        controller.project_edit(host_project_id, &request.name, &request.path)?;
        self.adopt_committed_catalog(device_id, controller)?;
        let store = ProjectStore::new(self.support_dir.clone());
        store.apply_local_project_metadata(
            &request.project_id,
            request.badge_text.as_deref(),
            request.badge_symbol.as_deref(),
            request.badge_color_hex.as_deref(),
            request.environment_variables,
        )?;
        store.select_project(&request.project_id)?;
        let _ = self.mark_project_active_with_watch(&request.project_id);
        self.sync_pet_project_memberships();
        Ok(store.list_snapshot())
    }

    /// Drop a project from a host's catalog. The authoritative snapshot is what
    /// removes the local projection, so a rejected removal changes nothing.
    /// Returns the projection the reconciliation selected next.
    pub(crate) fn remove_remote_project(
        &self,
        device_id: &str,
        host_project_id: &str,
        controller: &crate::remote::RemoteController,
    ) -> Result<Option<String>, String> {
        controller.project_remove(host_project_id)?;
        self.adopt_committed_catalog(device_id, controller)?;
        Ok(ProjectStore::new(self.support_dir.clone())
            .list_snapshot()
            .selected_project_id)
    }

    /// Reconcile the catalog a host just committed. Asked for directly rather
    /// than waited for: the same snapshot also arrives as a push, and the
    /// version guard drops whichever of the two lands second.
    fn adopt_committed_catalog(
        &self,
        device_id: &str,
        controller: &crate::remote::RemoteController,
    ) -> Result<(), String> {
        let disconnected = || {
            format!("Remote host {device_id} disconnected before its committed catalog arrived.")
        };
        let epoch = self
            .remote_controllers
            .live_connection_epoch(device_id, controller)
            .ok_or_else(&disconnected)?;
        let snapshot = controller.project_list()?;
        self.apply_remote_project_snapshot(device_id, epoch, &snapshot, controller)?;
        if self
            .remote_controllers
            .live_connection_epoch(device_id, controller)
            != Some(epoch)
        {
            return Err(disconnected());
        }
        Ok(())
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
        self.adopt_committed_catalog(device_id, controller)?;
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
) -> Option<&'a mut RemoteProjectSyncState> {
    let state = states.entry(device_id.to_string()).or_default();
    if state.connection_epoch > epoch {
        return None;
    }
    if state.connection_epoch < epoch {
        *state = RemoteProjectSyncState {
            connection_epoch: epoch,
            ..RemoteProjectSyncState::default()
        };
    }
    Some(state)
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
    let mut catalog = Vec::with_capacity(projects.len());
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for project in projects {
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
        if !ids.insert(host_project_id.clone()) {
            return Err(format!(
                "Remote project snapshot has duplicate id {host_project_id:?}."
            ));
        }
        let path = field("path")
            .ok_or_else(|| "Remote project entry is missing path.".to_string())?
            .to_string();
        let path_key = codux_runtime_core::path::path_identity_key(&path)
            .ok_or_else(|| "Remote project entry has invalid path.".to_string())?;
        if !paths.insert(path_key) {
            return Err(format!(
                "Remote project snapshot has duplicate path {path:?}."
            ));
        }
        let name = field("name").map(str::to_string).unwrap_or_else(|| {
            crate::path::file_name(&path).unwrap_or_else(|| "Project".to_string())
        });
        catalog.push(crate::project_store::RemoteCatalogProject {
            host_project_id,
            name,
            path,
        });
    }
    Ok(catalog)
}
