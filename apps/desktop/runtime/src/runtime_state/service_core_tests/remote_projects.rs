// Desktop-as-controller against a host that shares its project catalog.
//
// The fake host below speaks the same contract the headless agent implements:
// it advertises the `projects` resource, answers a baseline subscribe with a
// versioned `project.list`, commits `project.add` before acknowledging it, and
// fans the committed snapshot out to every subscriber. Its project ids are the
// host's own — deliberately unlike the desktop ids — so a request that carries
// the wrong identity is visible.

/// One fake host: a project catalog plus the controllers subscribed to it.
#[derive(Default)]
struct FakeHostCatalog {
    projects: Vec<crate::project_store::RemoteCatalogProject>,
    version: u64,
    /// Where a committed snapshot fans out — one entry per subscribed
    /// controller, so a second device sees a mutation without asking for it.
    subscribers: Vec<crate::remote::TestHostRoute>,
    /// Every envelope a controller sent, for asserting outbound identity.
    sent: Vec<serde_json::Value>,
    reject_adds: bool,
    shares_catalog: bool,
}

impl FakeHostCatalog {
    fn snapshot(&self) -> serde_json::Value {
        json!({
            "projects": self
                .projects
                .iter()
                .map(|project| json!({
                    "id": project.host_project_id,
                    "name": project.name,
                    "path": project.path,
                }))
                .collect::<Vec<_>>(),
            "worktrees": [],
            "version": self.version,
        })
    }

    /// A mutation raised on another client — the phone in the reported bug:
    /// it commits on the host, which then fans the committed catalog out.
    fn mutate_elsewhere(
        &mut self,
        mutate: impl FnOnce(&mut Vec<crate::project_store::RemoteCatalogProject>),
    ) {
        mutate(&mut self.projects);
        self.version += 1;
        self.publish();
    }

    /// Publish the committed catalog to every subscriber, like the headless
    /// host's source-inclusive fan-out.
    fn publish(&self) {
        let message = serde_json::to_vec(&json!({
            "type": codux_protocol::REMOTE_PROJECT_LIST,
            "payload": self.snapshot(),
        }))
        .unwrap();
        for subscriber in &self.subscribers {
            subscriber(&message);
        }
    }
}

struct FakeHostTransport {
    route: crate::remote::TestHostRoute,
    catalog: Arc<Mutex<FakeHostCatalog>>,
    /// `host.info.app`; only the headless agent shares its catalog.
    app: String,
}

#[async_trait::async_trait]
impl codux_remote_transport::RemoteTransport for FakeHostTransport {
    fn kind(&self) -> codux_protocol::RemoteTransportKind {
        codux_protocol::RemoteTransportKind::Iroh
    }

    fn send(&self, data: Vec<u8>, _device_id: Option<&str>) -> bool {
        let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(&data) else {
            return false;
        };
        let request_id = envelope.get("requestId").cloned();
        let payload = envelope.get("payload").cloned().unwrap_or(json!({}));
        let mut catalog = self.catalog.lock().unwrap();
        catalog.sent.push(envelope.clone());
        let route = |kind: &str, payload: serde_json::Value, request_id: Option<&serde_json::Value>| {
            let mut message = json!({ "type": kind, "payload": payload });
            if let Some(request_id) = request_id {
                message["requestId"] = request_id.clone();
            }
            (self.route)(&serde_json::to_vec(&message).unwrap());
        };
        // Every committed mutation answers its source and then fans the
        // committed snapshot out to every subscriber and to the source itself,
        // exactly like the headless host.
        let commit = |action: &str, host_project_id: &str, catalog: &FakeHostCatalog| {
            route(
                codux_protocol::REMOTE_PROJECT_UPDATED,
                json!({ "action": action, "projectId": host_project_id }),
                request_id.as_ref(),
            );
            catalog.publish();
            route(codux_protocol::REMOTE_PROJECT_LIST, catalog.snapshot(), None);
        };
        match envelope.get("type").and_then(serde_json::Value::as_str) {
            Some(codux_protocol::REMOTE_HOST_INFO) => {
                let resources = if catalog.shares_catalog {
                    json!(["projects", "terminals"])
                } else {
                    json!(["terminals"])
                };
                route(
                    codux_protocol::REMOTE_HOST_INFO,
                    json!({
                        "hostId": "fake-host",
                        "runtimeInstanceId": "runtime-1",
                        "app": self.app.clone(),
                        "capabilities": { "resourceSubscriptions": resources },
                    }),
                    request_id.as_ref(),
                );
            }
            Some(codux_protocol::REMOTE_RESOURCE_SUBSCRIBE) => {
                catalog.subscribers.push(Arc::clone(&self.route));
                let snapshot = catalog.snapshot();
                route(
                    codux_protocol::REMOTE_PROJECT_LIST,
                    snapshot,
                    request_id.as_ref(),
                );
            }
            Some(codux_protocol::REMOTE_PROJECT_LIST) => {
                let snapshot = catalog.snapshot();
                route(
                    codux_protocol::REMOTE_PROJECT_LIST,
                    snapshot,
                    request_id.as_ref(),
                );
            }
            Some(codux_protocol::REMOTE_PROJECT_ADD) => {
                if catalog.reject_adds {
                    route(
                        codux_protocol::REMOTE_ERROR,
                        json!({ "message": "Project path is required." }),
                        request_id.as_ref(),
                    );
                    return true;
                }
                let path = payload_field(&payload, "path");
                let name = payload_field(&payload, "name");
                // Idempotent by path, and a repeat never renames the entry.
                let host_project_id = match catalog
                    .projects
                    .iter()
                    .find(|project| project.path == path)
                {
                    Some(project) => project.host_project_id.clone(),
                    None => {
                        let host_project_id = format!("p-host-{}", catalog.projects.len() + 1);
                        catalog.projects.push(catalog_project(
                            &host_project_id,
                            &name,
                            &path,
                        ));
                        catalog.version += 1;
                        host_project_id
                    }
                };
                commit("add", &host_project_id, &catalog);
            }
            Some(codux_protocol::REMOTE_PROJECT_EDIT) => {
                let host_project_id = payload_field(&payload, "projectId");
                let path = payload_field(&payload, "path");
                let name = payload_field(&payload, "name");
                if catalog.projects.iter().any(|project| {
                    project.path == path && project.host_project_id != host_project_id
                }) {
                    route(
                        codux_protocol::REMOTE_ERROR,
                        json!({ "message": "Another project already uses this path." }),
                        request_id.as_ref(),
                    );
                    return true;
                }
                let Some(index) = catalog
                    .projects
                    .iter()
                    .position(|project| project.host_project_id == host_project_id)
                else {
                    route(
                        codux_protocol::REMOTE_ERROR,
                        json!({ "message": "Project not found." }),
                        request_id.as_ref(),
                    );
                    return true;
                };
                // The entry keeps its id through a rename or a re-path.
                catalog.projects[index] = catalog_project(&host_project_id, &name, &path);
                catalog.version += 1;
                commit("edit", &host_project_id, &catalog);
            }
            Some(codux_protocol::REMOTE_PROJECT_REMOVE) => {
                let host_project_id = payload_field(&payload, "projectId");
                // Removal is idempotent on the host, and never touches the
                // directory the entry pointed at.
                catalog
                    .projects
                    .retain(|project| project.host_project_id != host_project_id);
                catalog.version += 1;
                commit("remove", &host_project_id, &catalog);
            }
            // Answer the project-scoped domains so the caller does not sit out
            // its request timeout; the assertions read the envelope, not this.
            Some(kind @ codux_protocol::REMOTE_GIT_STATUS) => {
                route(kind, json!({}), request_id.as_ref());
            }
            Some(kind @ codux_protocol::REMOTE_AI_STATS) => {
                route(kind, json!({}), request_id.as_ref());
            }
            _ => {}
        }
        true
    }

    async fn shutdown(&self) {}
}

fn payload_field(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn catalog_project(
    host_project_id: &str,
    name: &str,
    path: &str,
) -> crate::project_store::RemoteCatalogProject {
    crate::project_store::RemoteCatalogProject {
        host_project_id: host_project_id.to_string(),
        name: name.to_string(),
        path: path.to_string(),
    }
}

fn remote_project_service(
    label: &str,
    state: serde_json::Value,
) -> (RuntimeService, PathBuf, Arc<Mutex<FakeHostCatalog>>) {
    remote_project_service_for_app(label, state, codux_protocol::REMOTE_APP_AGENT)
}

fn remote_project_service_for_app(
    label: &str,
    state: serde_json::Value,
    app: &str,
) -> (RuntimeService, PathBuf, Arc<Mutex<FakeHostCatalog>>) {
    let catalog = Arc::new(Mutex::new(FakeHostCatalog {
        shares_catalog: true,
        ..FakeHostCatalog::default()
    }));
    let (service, support_dir) =
        remote_project_service_on(label, state, app, Arc::clone(&catalog));
    (service, support_dir, catalog)
}

/// A desktop whose only paired host is `catalog`. Two services can share one
/// catalog, which is how a second controller observes another's mutation.
fn remote_project_service_on(
    label: &str,
    state: serde_json::Value,
    app: &str,
    catalog: Arc<Mutex<FakeHostCatalog>>,
) -> (RuntimeService, PathBuf) {
    let support_dir = std::env::temp_dir().join(format!("codux-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&support_dir).expect("create support dir");
    fs::write(support_dir.join("state.json"), state.to_string()).expect("write state");
    let service = RuntimeService::new(support_dir.clone());
    connect_test_host(&service, app, catalog);
    (service, support_dir)
}

/// Attach a fresh connection to `catalog`. Called again on the same service to
/// reconnect: the manager mints a new connection epoch, exactly like a host
/// that restarted.
fn connect_test_host(service: &RuntimeService, app: &str, catalog: Arc<Mutex<FakeHostCatalog>>) {
    let app = app.to_string();
    let controller = crate::remote::RemoteController::with_test_transport("device-1", |route| {
        Arc::new(FakeHostTransport {
            route,
            catalog,
            app,
        })
    });
    service
        .remote_controllers
        .insert_test_connection("device-1", Arc::new(controller));
}

/// The tick spawns the capability probe and subscription on the blocking pool.
/// Wait for the probe to answer instead of sleeping a fixed amount; the host's
/// runtime instance id is recorded once it has.
fn wait_for_catalog_probe(service: &RuntimeService, device_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        service.drain_remote_events();
        let probed = service
            .remote_project_sync
            .lock()
            .unwrap()
            .get(device_id)
            .is_some_and(|state| !state.runtime_instance_id.is_empty());
        if probed {
            // The baseline reconcile follows the probe on the same task.
            service.drain_remote_events();
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the host capability probe"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_projects(service: &RuntimeService, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        service.drain_remote_events();
        if service.project_list().projects.len() == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected} projects"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Drain the runtime tick until `ready`, so a test never sleeps a fixed amount
/// for work that runs on the blocking pool.
fn wait_until(service: &RuntimeService, label: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        service.drain_remote_events();
        if ready() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn migration_done(support_dir: &std::path::Path) -> bool {
    crate::project_store::ProjectStore::new(support_dir.to_path_buf())
        .remote_project_migration_version("device-1")
        > 0
}

fn host_project_id_of(support_dir: &std::path::Path, project_id: &str) -> Option<String> {
    crate::project_store::ProjectStore::new(support_dir.to_path_buf())
        .remote_project_id_pairs("device-1")
        .into_iter()
        .find(|(desktop_id, _)| desktop_id == project_id)
        .map(|(_, host_project_id)| host_project_id)
}

fn empty_state() -> serde_json::Value {
    json!({ "projects": [], "selectedProjectId": null })
}

/// A remote project this desktop created before the host owned the catalog: it
/// has no `hostProjectId`, and the host has never heard of its path.
fn legacy_state() -> serde_json::Value {
    json!({
        "projects": [{
            "id": "legacy-desktop-id",
            "name": "Alpha",
            "path": "/srv/alpha",
            "badgeText": "AL",
            "environmentVariables": { "FOO": "bar" },
            "runtimeTarget": { "kind": "remote", "deviceId": "device-1" }
        }],
        "selectedProjectId": "legacy-desktop-id"
    })
}

#[test]
fn host_baseline_creates_projections_with_desktop_ids() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-baseline", empty_state());
    catalog
        .lock()
        .unwrap()
        .projects
        .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
    catalog.lock().unwrap().version = 3;

    wait_for_projects(&service, 1);

    let project = service.project_list().projects.remove(0);
    assert_eq!(project.name, "Alpha");
    assert_eq!(project.path, "/srv/alpha");
    assert_ne!(project.id, "p-host-1");
    assert_eq!(
        project.runtime_target,
        ProjectRuntimeTarget::Remote {
            device_id: "device-1".to_string()
        }
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_add_commits_on_the_host_before_the_local_projection_appears() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-add", empty_state());
    wait_for_catalog_probe(&service, "device-1");

    let snapshot = service
        .project_create(ProjectCreateRequest {
            name: "Alpha".to_string(),
            path: "/srv/alpha".to_string(),
            badge_text: Some("AL".to_string()),
            badge_symbol: None,
            badge_color_hex: Some("#123456".to_string()),
            environment_variables: [("FOO".to_string(), "bar".to_string())]
                .into_iter()
                .collect(),
            runtime_target: ProjectRuntimeTarget::Remote {
                device_id: "device-1".to_string(),
            },
        })
        .expect("remote add");

    let host = catalog.lock().unwrap();
    assert_eq!(
        host.projects,
        [catalog_project("p-host-1", "Alpha", "/srv/alpha")]
    );
    drop(host);
    assert_eq!(snapshot.projects.len(), 1);
    let project = &snapshot.projects[0];
    assert_eq!(
        project.id,
        crate::project_store::remote_projection_id("device-1", "p-host-1")
    );
    assert_eq!(project.badge, "AL");
    assert_eq!(project.badge_color_hex.as_deref(), Some("#123456"));
    assert_eq!(
        project.environment_variables.get("FOO").map(String::as_str),
        Some("bar")
    );
    assert_eq!(snapshot.selected_project_id.as_deref(), Some(&project.id[..]));

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_add_before_the_capability_probe_answers_still_commits_on_the_host() {
    // No `wait_for_catalog_probe`: the user can add a project in the first
    // moments of a connection. Deciding from a not-yet-answered probe would
    // create a local-only project the host never heard about.
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-add-early", empty_state());

    service
        .project_create(ProjectCreateRequest {
            name: "Alpha".to_string(),
            path: "/srv/alpha".to_string(),
            badge_text: None,
            badge_symbol: None,
            badge_color_hex: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Remote {
                device_id: "device-1".to_string(),
            },
        })
        .expect("remote add");

    assert_eq!(
        catalog.lock().unwrap().projects,
        [catalog_project("p-host-1", "Alpha", "/srv/alpha")]
    );
    let project = service.project_list().projects.remove(0);
    assert_eq!(
        project.id,
        crate::project_store::remote_projection_id("device-1", "p-host-1")
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn rejected_remote_add_leaves_the_local_store_unchanged() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-add-rejected", empty_state());
    wait_for_catalog_probe(&service, "device-1");
    catalog.lock().unwrap().reject_adds = true;

    let error = service
        .project_create(ProjectCreateRequest {
            name: "Alpha".to_string(),
            path: "/srv/alpha".to_string(),
            badge_text: None,
            badge_symbol: None,
            badge_color_hex: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Remote {
                device_id: "device-1".to_string(),
            },
        })
        .expect_err("host rejects the add");

    assert_eq!(error, "Project path is required.");
    assert!(service.project_list().projects.is_empty());
    assert!(catalog.lock().unwrap().projects.is_empty());

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn desktop_added_project_reaches_the_other_subscribed_device() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-fanout", empty_state());
    // A second controller on the same host, with its own store — the phone in
    // the reported bug. It never asks for the catalog again after subscribing.
    let (other, other_support_dir) = remote_project_service_on(
        "remote-project-fanout-other",
        empty_state(),
        codux_protocol::REMOTE_APP_AGENT,
        Arc::clone(&catalog),
    );
    wait_for_catalog_probe(&service, "device-1");
    wait_for_catalog_probe(&other, "device-1");

    service
        .project_create(ProjectCreateRequest {
            name: "Alpha".to_string(),
            path: "/srv/alpha".to_string(),
            badge_text: None,
            badge_symbol: None,
            badge_color_hex: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Remote {
                device_id: "device-1".to_string(),
            },
        })
        .expect("remote add");

    wait_for_projects(&other, 1);
    let seen = other.project_list().projects.remove(0);
    assert_eq!(seen.path, "/srv/alpha");
    assert_eq!(seen.name, "Alpha");

    drop(other);
    drop(service);
    fs::remove_dir_all(other_support_dir).ok();
    fs::remove_dir_all(support_dir).ok();
}

/// A desktop already projecting one catalog entry, `p-host-1`. Returned as
/// (service, its support dir, the host catalog, the desktop id of `p-host-1`).
fn one_projection(label: &str) -> (RuntimeService, PathBuf, Arc<Mutex<FakeHostCatalog>>, String) {
    let (service, support_dir, catalog) = remote_project_service(label, empty_state());
    catalog
        .lock()
        .unwrap()
        .projects
        .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
    wait_for_projects(&service, 1);
    let project_id = service.project_list().projects.remove(0).id;
    (service, support_dir, catalog, project_id)
}

/// Two desktops on one host, each already projecting `p-host-1`. Returned as
/// (mutating service, its support dir, observing service, its support dir).
fn two_controllers_on_one_project(
    label: &str,
) -> (RuntimeService, PathBuf, RuntimeService, PathBuf) {
    let (service, support_dir, catalog) = remote_project_service(label, empty_state());
    catalog
        .lock()
        .unwrap()
        .projects
        .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
    let (other, other_support_dir) = remote_project_service_on(
        &format!("{label}-other"),
        empty_state(),
        codux_protocol::REMOTE_APP_AGENT,
        Arc::clone(&catalog),
    );
    wait_for_projects(&service, 1);
    wait_for_projects(&other, 1);
    (service, support_dir, other, other_support_dir)
}

fn edit_request(project_id: &str, name: &str, path: &str) -> ProjectUpdateRequest {
    ProjectUpdateRequest {
        project_id: project_id.to_string(),
        name: name.to_string(),
        path: path.to_string(),
        badge_text: None,
        badge_symbol: None,
        badge_color_hex: None,
        environment_variables: Default::default(),
        runtime_target: ProjectRuntimeTarget::Remote {
            device_id: "device-1".to_string(),
        },
    }
}

#[test]
fn desktop_edit_commits_on_the_host_and_reaches_the_other_device() {
    let (service, support_dir, other, other_support_dir) =
        two_controllers_on_one_project("remote-project-edit");
    let project_id = service.project_list().projects.remove(0).id;

    let snapshot = service
        .project_update(edit_request(&project_id, "Renamed", "/srv/renamed"))
        .expect("remote edit");

    // The desktop id and the host id both survive a rename, so layouts,
    // selection, and every project-scoped request keep working.
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].id, project_id);
    assert_eq!(snapshot.projects[0].name, "Renamed");
    assert_eq!(snapshot.projects[0].path, "/srv/renamed");
    assert_eq!(
        host_project_id_of(&support_dir, &project_id).as_deref(),
        Some("p-host-1")
    );

    wait_until(&other, "the renamed projection", || {
        other.project_list().projects[0].name == "Renamed"
    });
    let seen = other.project_list().projects.remove(0);
    assert_eq!(seen.path, "/srv/renamed");

    drop(other);
    drop(service);
    fs::remove_dir_all(other_support_dir).ok();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn desktop_remove_commits_on_the_host_and_reaches_the_other_device() {
    let (service, support_dir, other, other_support_dir) =
        two_controllers_on_one_project("remote-project-remove");
    let project_id = service.project_list().projects.remove(0).id;

    let snapshot = service
        .project_close(crate::project_store::ProjectCloseRequest {
            project_id: project_id.clone(),
        })
        .expect("remote remove");

    assert!(snapshot.projects.is_empty());
    wait_for_projects(&other, 0);

    drop(other);
    drop(service);
    fs::remove_dir_all(other_support_dir).ok();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_mutation_from_another_client_reaches_the_desktop_projection() {
    let (service, support_dir, catalog, project_id) =
        one_projection("remote-project-inbound-mutation");

    // The phone renames the project…
    catalog.lock().unwrap().mutate_elsewhere(|projects| {
        projects[0] = catalog_project("p-host-1", "Renamed", "/srv/renamed");
    });
    wait_until(&service, "the renamed projection", || {
        service.project_list().projects[0].name == "Renamed"
    });
    let renamed = service.project_list().projects.remove(0);
    // …onto the same desktop record, so nothing local is orphaned.
    assert_eq!(renamed.id, project_id);
    assert_eq!(renamed.path, "/srv/renamed");

    // …and then removes it.
    catalog
        .lock()
        .unwrap()
        .mutate_elsewhere(|projects| projects.clear());
    wait_for_projects(&service, 0);

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn an_edit_onto_a_path_another_entry_owns_changes_nothing() {
    let (service, support_dir, catalog, project_id) =
        one_projection("remote-project-edit-rejected");
    catalog
        .lock()
        .unwrap()
        .mutate_elsewhere(|projects| projects.push(catalog_project("p-host-2", "Beta", "/srv/beta")));
    wait_for_projects(&service, 2);

    let error = service
        .project_update(edit_request(&project_id, "Alpha", "/srv/beta"))
        .expect_err("the host owns this path already");

    assert_eq!(error, "Another project already uses this path.");
    let projects = service.project_list().projects;
    let alpha = projects
        .iter()
        .find(|project| project.id == project_id)
        .expect("the projection survives a rejected edit");
    assert_eq!(alpha.name, "Alpha");
    assert_eq!(alpha.path, "/srv/alpha");
    assert_eq!(
        catalog.lock().unwrap().projects,
        [
            catalog_project("p-host-1", "Alpha", "/srv/alpha"),
            catalog_project("p-host-2", "Beta", "/srv/beta"),
        ]
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn retargeting_a_projection_leaves_the_catalog_and_takes_the_host_id_with_it() {
    let (service, support_dir, catalog, project_id) = one_projection("remote-project-retarget");
    let local_path = support_dir.join("alpha");
    fs::create_dir_all(&local_path).expect("local project dir");

    // Moving the record to another runtime target is not a catalog edit: the
    // record stops projecting this host, so it must stop carrying its id too.
    service
        .project_update(ProjectUpdateRequest {
            project_id: project_id.clone(),
            name: "Alpha".to_string(),
            path: local_path.display().to_string(),
            badge_text: None,
            badge_symbol: None,
            badge_color_hex: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Local,
        })
        .expect("retarget to local");

    assert!(host_project_id_of(&support_dir, &project_id).is_none());
    assert_eq!(
        catalog.lock().unwrap().projects,
        [catalog_project("p-host-1", "Alpha", "/srv/alpha")],
        "the host's catalog membership is not this desktop's to drop"
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn re_adding_a_committed_path_keeps_one_projection() {
    let (service, support_dir, catalog, project_id) = one_projection("remote-project-add-retry");

    let snapshot = service
        .project_create(ProjectCreateRequest {
            name: "Alpha again".to_string(),
            path: "/srv/alpha".to_string(),
            badge_text: None,
            badge_symbol: None,
            badge_color_hex: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Remote {
                device_id: "device-1".to_string(),
            },
        })
        .expect("repeated remote add");

    // The host deduplicated by path and did not rename its entry, so the retry
    // lands back on the projection that already existed.
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].id, project_id);
    assert_eq!(snapshot.projects[0].name, "Alpha");
    assert_eq!(catalog.lock().unwrap().projects.len(), 1);

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_duplicate_snapshot_version_cannot_empty_the_projection() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-duplicate", empty_state());
    {
        let mut host = catalog.lock().unwrap();
        host.projects
            .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
        host.version = 7;
    }
    wait_for_projects(&service, 1);

    let controller = service
        .remote_controller_for_device("device-1")
        .expect("live controller");
    // Same version as the applied snapshot: a re-delivery, not a new catalog.
    controller.route_test_message(
        &serde_json::to_vec(&json!({
            "type": codux_protocol::REMOTE_PROJECT_LIST,
            "payload": { "projects": [], "worktrees": [], "version": 7 }
        }))
        .unwrap(),
    );
    service.drain_remote_events();

    assert_eq!(service.project_list().projects.len(), 1);

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn each_controller_keeps_its_own_selection_through_a_shared_removal() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-selection", empty_state());
    {
        let mut host = catalog.lock().unwrap();
        host.projects
            .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
        host.projects
            .push(catalog_project("p-host-2", "Beta", "/srv/beta"));
    }
    let (other, other_support_dir) = remote_project_service_on(
        "remote-project-selection-other",
        empty_state(),
        codux_protocol::REMOTE_APP_AGENT,
        Arc::clone(&catalog),
    );
    wait_for_projects(&service, 2);
    wait_for_projects(&other, 2);

    let alpha = crate::project_store::remote_projection_id("device-1", "p-host-1");
    let beta_on_other = crate::project_store::remote_projection_id("device-1", "p-host-2");
    service.select_project(&alpha).expect("select alpha");
    other
        .select_project(&beta_on_other)
        .expect("select beta elsewhere");

    service
        .project_close(crate::project_store::ProjectCloseRequest {
            project_id: alpha.clone(),
        })
        .expect("remove alpha");

    wait_for_projects(&other, 1);
    // The other controller loses the removed entry and keeps its own selection.
    assert_eq!(
        other.project_list().selected_project_id.as_deref(),
        Some(beta_on_other.as_str())
    );
    // Membership only: the host still serves the path it kept.
    assert_eq!(
        catalog.lock().unwrap().projects,
        [catalog_project("p-host-2", "Beta", "/srv/beta")]
    );

    drop(other);
    drop(service);
    fs::remove_dir_all(other_support_dir).ok();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_projection_is_not_removed_locally_when_its_host_stops_sharing() {
    let (service, support_dir, catalog, project_id) =
        one_projection("remote-project-remove-offline");
    catalog.lock().unwrap().shares_catalog = false;

    let error = service
        .project_close(crate::project_store::ProjectCloseRequest {
            project_id: project_id.clone(),
        })
        .expect_err("the catalog owner cannot commit the removal");

    assert_eq!(error, "Remote host device-1 is unavailable.");
    // A local-only removal would be undone by the next snapshot anyway.
    assert_eq!(service.project_list().projects.len(), 1);
    assert_eq!(catalog.lock().unwrap().projects.len(), 1);

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn project_scoped_requests_carry_the_host_project_id() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-id-bridge", empty_state());
    catalog
        .lock()
        .unwrap()
        .projects
        .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
    wait_for_projects(&service, 1);
    let desktop_project_id = service.project_list().projects.remove(0).id;
    assert_ne!(desktop_project_id, "p-host-1");

    let controller = service
        .remote_controller_for_device("device-1")
        .expect("live controller");
    let _ = controller.git_status(&desktop_project_id, "/srv/alpha");
    let _ = controller.ai_stats(&desktop_project_id, &desktop_project_id);

    let host = catalog.lock().unwrap();
    let scoped = host
        .sent
        .iter()
        .filter(|envelope| {
            matches!(
                envelope.get("type").and_then(serde_json::Value::as_str),
                Some(codux_protocol::REMOTE_GIT_STATUS) | Some(codux_protocol::REMOTE_AI_STATS)
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(scoped.len(), 2);
    for envelope in scoped {
        assert_eq!(envelope["payload"]["projectId"], json!("p-host-1"));
    }
    assert_eq!(
        host.sent
            .iter()
            .find(|envelope| envelope.get("type").and_then(serde_json::Value::as_str)
                == Some(codux_protocol::REMOTE_AI_STATS))
            .expect("ai.stats envelope")["payload"]["worktreeId"],
        json!("p-host-1")
    );

    drop(host);
    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn host_worktree_update_maps_back_to_the_desktop_projection() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-inbound", empty_state());
    catalog
        .lock()
        .unwrap()
        .projects
        .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
    wait_for_projects(&service, 1);
    let desktop_project_id = service.project_list().projects.remove(0).id;

    let controller = service
        .remote_controller_for_device("device-1")
        .expect("live controller");
    controller.route_test_message(
        &serde_json::to_vec(&json!({
            "type": codux_protocol::REMOTE_WORKTREE_UPDATED,
            "payload": {
                "projectId": "p-host-1",
                "selectedWorktreeId": "p-host-1",
                "worktrees": [],
                "tasks": [],
                "error": null,
                "defaultBaseBranch": "main"
            }
        }))
        .unwrap(),
    );

    let events = service.drain_remote_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RemoteHostEvent::WorktreesChanged { project_id, .. }
                if project_id == &desktop_project_id
        )),
        "expected a worktree change for the desktop projection, got {events:?}"
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_stale_snapshot_cannot_roll_the_projection_back() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-stale", empty_state());
    {
        let mut host = catalog.lock().unwrap();
        host.projects
            .push(catalog_project("p-host-1", "Alpha", "/srv/alpha"));
        host.version = 5;
    }
    wait_for_projects(&service, 1);

    let controller = service
        .remote_controller_for_device("device-1")
        .expect("live controller");
    controller.route_test_message(
        &serde_json::to_vec(&json!({
            "type": codux_protocol::REMOTE_PROJECT_LIST,
            "payload": { "projects": [], "worktrees": [], "version": 4 }
        }))
        .unwrap(),
    );
    service.drain_remote_events();

    assert_eq!(service.project_list().projects.len(), 1);

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_desktop_host_catalog_is_not_adopted() {
    let (service, support_dir, catalog) =
        remote_project_service_for_app("remote-project-desktop-host", empty_state(), "Codux");
    catalog.lock().unwrap().projects.push(catalog_project(
        "host-desktop-project",
        "Alpha",
        "/srv/alpha",
    ));
    wait_for_catalog_probe(&service, "device-1");

    // A desktop host advertises `projects` too, but its list is its own
    // controller state: importing it would add projects the user never opened.
    assert!(service.project_list().projects.is_empty());
    assert!(service.remote_project_catalog_host("device-1").is_none());

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn legacy_records_register_on_the_host_and_keep_their_desktop_ids() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-migrate", legacy_state());

    wait_until(&service, "the legacy migration", || {
        migration_done(&support_dir)
    });

    // The host now owns the entry the desktop used to hold alone.
    assert_eq!(
        catalog.lock().unwrap().projects,
        [catalog_project("p-host-1", "Alpha", "/srv/alpha")]
    );
    let projects = service.project_list().projects;
    assert_eq!(projects.len(), 1);
    // The desktop id survives, so layouts, selection, and local metadata do too.
    assert_eq!(projects[0].id, "legacy-desktop-id");
    assert_eq!(projects[0].badge, "AL");
    assert_eq!(
        projects[0]
            .environment_variables
            .get("FOO")
            .map(String::as_str),
        Some("bar")
    );
    assert_eq!(
        host_project_id_of(&support_dir, "legacy-desktop-id").as_deref(),
        Some("p-host-1")
    );

    let controller = service
        .remote_controller_for_device("device-1")
        .expect("live controller");
    let _ = controller.git_status("legacy-desktop-id", "/srv/alpha");
    let host = catalog.lock().unwrap();
    let git = host
        .sent
        .iter()
        .find(|envelope| {
            envelope.get("type").and_then(serde_json::Value::as_str)
                == Some(codux_protocol::REMOTE_GIT_STATUS)
        })
        .expect("git.status envelope");
    assert_eq!(git["payload"]["projectId"], json!("p-host-1"));

    drop(host);
    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn an_existing_host_entry_keeps_its_name_and_host_id_through_migration() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-migrate-existing", legacy_state());
    catalog
        .lock()
        .unwrap()
        .projects
        .push(catalog_project("p-host-9", "Alpha on host", "/srv/alpha"));

    wait_until(&service, "the legacy migration", || {
        migration_done(&support_dir)
    });

    // One catalog entry per path: the host deduplicated the legacy path onto
    // the entry it already had, and did not rename it.
    assert_eq!(
        catalog.lock().unwrap().projects,
        [catalog_project("p-host-9", "Alpha on host", "/srv/alpha")]
    );
    let projects = service.project_list().projects;
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, "legacy-desktop-id");
    assert_eq!(projects[0].name, "Alpha on host");
    assert_eq!(
        host_project_id_of(&support_dir, "legacy-desktop-id").as_deref(),
        Some("p-host-9")
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_rejected_registration_retries_without_pruning_the_legacy_record() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-migrate-retry", legacy_state());
    catalog.lock().unwrap().reject_adds = true;

    wait_for_catalog_probe(&service, "device-1");
    service.drain_remote_events();

    // Partial failure leaves the marker unset and the legacy record intact —
    // never a projection the host never accepted.
    assert!(!migration_done(&support_dir));
    let projects = service.project_list().projects;
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, "legacy-desktop-id");
    assert!(host_project_id_of(&support_dir, "legacy-desktop-id").is_none());

    catalog.lock().unwrap().reject_adds = false;
    wait_until(&service, "the retried legacy migration", || {
        migration_done(&support_dir)
    });

    assert_eq!(
        catalog.lock().unwrap().projects,
        [catalog_project("p-host-1", "Alpha", "/srv/alpha")]
    );
    assert_eq!(
        host_project_id_of(&support_dir, "legacy-desktop-id").as_deref(),
        Some("p-host-1")
    );

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_migrated_record_removed_on_the_host_is_not_resurrected() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-migrate-once", legacy_state());
    wait_until(&service, "the legacy migration", || {
        migration_done(&support_dir)
    });

    {
        // The user removes the project on another controller.
        let mut host = catalog.lock().unwrap();
        host.projects.clear();
        host.version += 1;
    }
    connect_test_host(&service, codux_protocol::REMOTE_APP_AGENT, Arc::clone(&catalog));

    wait_for_projects(&service, 0);
    assert!(catalog.lock().unwrap().projects.is_empty());

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn two_controllers_migrate_their_legacy_records_into_one_catalog() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-migrate-union", legacy_state());
    let (other, other_support_dir) = remote_project_service_on(
        "remote-project-migrate-union-other",
        json!({
            "projects": [{
                "id": "other-desktop-id",
                "name": "Beta",
                "path": "/srv/beta",
                "runtimeTarget": { "kind": "remote", "deviceId": "device-1" }
            }],
            "selectedProjectId": "other-desktop-id"
        }),
        codux_protocol::REMOTE_APP_AGENT,
        Arc::clone(&catalog),
    );

    wait_until(&service, "the first legacy migration", || {
        migration_done(&support_dir)
    });
    wait_until(&other, "the second legacy migration", || {
        migration_done(&other_support_dir)
    });

    let mut paths = catalog
        .lock()
        .unwrap()
        .projects
        .iter()
        .map(|project| project.path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths, ["/srv/alpha", "/srv/beta"]);
    // Each controller keeps its own record and adopts the other's.
    wait_for_projects(&service, 2);
    wait_for_projects(&other, 2);
    assert!(
        service
            .project_list()
            .projects
            .iter()
            .any(|project| project.id == "legacy-desktop-id")
    );
    assert!(
        other
            .project_list()
            .projects
            .iter()
            .any(|project| project.id == "other-desktop-id")
    );

    drop(other);
    drop(service);
    fs::remove_dir_all(other_support_dir).ok();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn a_host_without_the_capability_keeps_local_project_creation() {
    let (service, support_dir, catalog) =
        remote_project_service("remote-project-legacy-host", empty_state());
    catalog.lock().unwrap().shares_catalog = false;
    wait_for_catalog_probe(&service, "device-1");

    let snapshot = service
        .project_create(ProjectCreateRequest {
            name: "Alpha".to_string(),
            path: "/srv/alpha".to_string(),
            badge_text: None,
            badge_symbol: None,
            badge_color_hex: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Remote {
                device_id: "device-1".to_string(),
            },
        })
        .expect("local remote-target create");

    assert_eq!(snapshot.projects.len(), 1);
    assert!(catalog.lock().unwrap().projects.is_empty());

    drop(service);
    fs::remove_dir_all(support_dir).ok();
}
