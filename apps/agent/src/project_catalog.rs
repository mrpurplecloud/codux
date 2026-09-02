//! The headless host's shared project catalog: one authoritative store plus the
//! `projects` resource subscription, so every connected controller sees the same
//! catalog. Mutations commit here first, then the committed snapshot fans out to
//! every subscriber and to the mutation source.
//!
//! Kept out of `host.rs` so the catalog contract is drivable without a transport
//! or a terminal runtime: `handle` takes an envelope kind plus payload and
//! returns the messages the host should send.

use crate::projects::{AgentProjectStore, ProjectCommit};
use codux_protocol::{
    REMOTE_ERROR, REMOTE_PROJECT_ADD, REMOTE_PROJECT_EDIT, REMOTE_PROJECT_LIST,
    REMOTE_PROJECT_REMOVE, REMOTE_PROJECT_UPDATED, REMOTE_RESOURCE_PROJECTS,
    REMOTE_RESOURCE_SUBSCRIBE, REMOTE_RESOURCE_UNSUBSCRIBE,
};
use codux_runtime_core::{
    project::project_list_payload,
    subscription::{RuntimeSubscriptionRouter, with_resource_version},
};
use serde_json::{Value, json};

/// One outbound message. `reply` marks the answer to the request being handled
/// (it carries the caller's `requestId`); everything else is an unsolicited push
/// addressed to `device_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMessage {
    pub device_id: Option<String>,
    pub kind: &'static str,
    pub payload: Value,
    pub reply: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectCatalogOutcome {
    pub messages: Vec<ProjectMessage>,
    /// Set when a project left the catalog, so the host can drop the terminal
    /// fan-out and AI-stats watchers scoped to it.
    pub removed_project_id: Option<String>,
}

pub struct ProjectCatalogService {
    store: AgentProjectStore,
    subscriptions: RuntimeSubscriptionRouter,
}

impl ProjectCatalogService {
    pub fn new(store: AgentProjectStore) -> Self {
        Self {
            store,
            subscriptions: RuntimeSubscriptionRouter::new(),
        }
    }

    /// Whether this service answers `kind`. Subscription envelopes are shared
    /// with other resources, so the host routes those by resource name instead.
    pub fn handles(kind: &str) -> bool {
        matches!(
            kind,
            REMOTE_PROJECT_LIST | REMOTE_PROJECT_ADD | REMOTE_PROJECT_EDIT | REMOTE_PROJECT_REMOVE
        )
    }

    pub fn handle(
        &self,
        kind: &str,
        payload: &Value,
        device_id: Option<&str>,
    ) -> ProjectCatalogOutcome {
        match kind {
            REMOTE_RESOURCE_SUBSCRIBE if is_projects_resource(payload) => self.subscribe(device_id),
            REMOTE_RESOURCE_UNSUBSCRIBE if is_projects_resource(payload) => {
                if let Some(device_id) = clean_device_id(device_id) {
                    self.subscriptions
                        .unsubscribe(REMOTE_RESOURCE_PROJECTS, None, None, device_id);
                }
                ProjectCatalogOutcome::default()
            }
            REMOTE_PROJECT_LIST => match self.snapshot_payload() {
                Ok(payload) => reply(REMOTE_PROJECT_LIST, payload),
                Err(message) => error(&message),
            },
            REMOTE_PROJECT_ADD => {
                let Some(path) = payload.get("path").and_then(Value::as_str) else {
                    return error("Project path is required.");
                };
                let name = payload.get("name").and_then(Value::as_str);
                self.commit("add", self.store.add(path, name), device_id)
            }
            REMOTE_PROJECT_EDIT => {
                let Some(project_id) = project_id_of(payload) else {
                    return error("Project id is required.");
                };
                let Some(path) = payload.get("path").and_then(Value::as_str) else {
                    return error("Project path is required.");
                };
                let name = payload.get("name").and_then(Value::as_str);
                self.commit("edit", self.store.edit(project_id, path, name), device_id)
            }
            REMOTE_PROJECT_REMOVE => {
                let Some(project_id) = project_id_of(payload) else {
                    return error("Project id is required.");
                };
                let removed = self.store.remove(project_id);
                if removed.is_ok() {
                    self.subscriptions.remove_project(project_id);
                }
                let mut outcome = self.commit("remove", removed, device_id);
                if outcome
                    .messages
                    .iter()
                    .all(|message| message.kind != REMOTE_ERROR)
                {
                    outcome.removed_project_id = Some(project_id.to_string());
                }
                outcome
            }
            _ => ProjectCatalogOutcome::default(),
        }
    }

    /// Drop a disconnected device's subscriptions so later mutations stop
    /// fanning out to it.
    pub fn remove_device(&self, device_id: &str) {
        if let Some(device_id) = clean_device_id(Some(device_id)) {
            self.subscriptions.remove_device(device_id);
        }
    }

    fn subscribe(&self, device_id: Option<&str>) -> ProjectCatalogOutcome {
        let Some(device_id) = clean_device_id(device_id) else {
            return error("Device id is required.");
        };
        self.subscriptions
            .subscribe(REMOTE_RESOURCE_PROJECTS, None, None, device_id);
        // Always answer with the current catalog, whether or not the subscriber
        // asked for a baseline. The desktop host has always replied to a
        // `projects` subscribe with its list, and the mobile client subscribes
        // without `baseline` and waits for exactly that reply; a baseline gate
        // here would leave it with an empty project list until someone else
        // mutated the catalog.
        match self.snapshot_payload() {
            Ok(payload) => reply(REMOTE_PROJECT_LIST, payload),
            Err(message) => error(&message),
        }
    }

    /// Acknowledge the mutation to its source, then publish one newer snapshot
    /// to every subscriber and to the source itself, so a controller that never
    /// subscribed still learns the committed catalog.
    ///
    /// The version comes from the commit, not from a counter read here: the
    /// store minted the two together under one lock, so this fan-out describes
    /// the catalog it actually carries even when a concurrent mutation commits
    /// and publishes first.
    fn commit(
        &self,
        action: &str,
        commit: Result<ProjectCommit, String>,
        device_id: Option<&str>,
    ) -> ProjectCatalogOutcome {
        let commit = match commit {
            Ok(commit) => commit,
            Err(message) => return error(&message),
        };
        let snapshot = with_resource_version(
            project_list_payload(commit.items, None, None),
            commit.version,
        );
        let mut messages = vec![ProjectMessage {
            device_id: None,
            kind: REMOTE_PROJECT_UPDATED,
            payload: json!({ "action": action, "projectId": commit.project_id }),
            reply: true,
        }];
        let mut recipients = self
            .subscriptions
            .devices_for(REMOTE_RESOURCE_PROJECTS, None, None);
        if let Some(device_id) = clean_device_id(device_id) {
            recipients.insert(device_id.to_string());
        }
        let mut recipients = recipients.into_iter().collect::<Vec<_>>();
        recipients.sort();
        for recipient in recipients {
            messages.push(ProjectMessage {
                device_id: Some(recipient),
                kind: REMOTE_PROJECT_LIST,
                payload: snapshot.clone(),
                reply: false,
            });
        }
        ProjectCatalogOutcome {
            messages,
            removed_project_id: None,
        }
    }

    fn snapshot_payload(&self) -> Result<Value, String> {
        let (items, version) = self.store.snapshot()?;
        Ok(with_resource_version(
            project_list_payload(items, None, None),
            version,
        ))
    }
}

fn is_projects_resource(payload: &Value) -> bool {
    payload.get("resource").and_then(Value::as_str) == Some(REMOTE_RESOURCE_PROJECTS)
}

fn project_id_of(payload: &Value) -> Option<&str> {
    payload
        .get("projectId")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn clean_device_id(device_id: Option<&str>) -> Option<&str> {
    device_id.map(str::trim).filter(|value| !value.is_empty())
}

fn reply(kind: &'static str, payload: Value) -> ProjectCatalogOutcome {
    ProjectCatalogOutcome {
        messages: vec![ProjectMessage {
            device_id: None,
            kind,
            payload,
            reply: true,
        }],
        removed_project_id: None,
    }
}

fn error(message: &str) -> ProjectCatalogOutcome {
    reply(REMOTE_ERROR, json!({ "message": message }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codux-agent-catalog-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service(dir: &std::path::Path) -> ProjectCatalogService {
        ProjectCatalogService::new(AgentProjectStore::with_path(dir.join("projects.json")))
    }

    fn subscribe(service: &ProjectCatalogService, device_id: &str) -> ProjectCatalogOutcome {
        service.handle(
            REMOTE_RESOURCE_SUBSCRIBE,
            &json!({ "resource": REMOTE_RESOURCE_PROJECTS, "baseline": true }),
            Some(device_id),
        )
    }

    fn project_dir(root: &std::path::Path, name: &str) -> String {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path.to_string_lossy().to_string()
    }

    fn reply_of<'a>(outcome: &'a ProjectCatalogOutcome, kind: &str) -> Option<&'a ProjectMessage> {
        outcome
            .messages
            .iter()
            .find(|message| message.reply && message.kind == kind)
    }

    fn pushes_to<'a>(
        outcome: &'a ProjectCatalogOutcome,
        device_id: &str,
        kind: &str,
    ) -> Vec<&'a ProjectMessage> {
        outcome
            .messages
            .iter()
            .filter(|message| {
                !message.reply
                    && message.kind == kind
                    && message.device_id.as_deref() == Some(device_id)
            })
            .collect()
    }

    fn project_paths(payload: &Value) -> Vec<String> {
        payload["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|project| project["path"].as_str().unwrap().to_string())
            .collect()
    }

    fn project_names(payload: &Value) -> Vec<String> {
        payload["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|project| project["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn version(payload: &Value) -> u64 {
        payload["version"].as_u64().unwrap()
    }

    #[test]
    fn add_from_one_device_reaches_every_subscribed_device() {
        let dir = temp_dir("fanout");
        let service = service(&dir);
        let project = project_dir(&dir, "repo");

        let baseline = subscribe(&service, "device-a");
        subscribe(&service, "device-b");
        let baseline_reply = reply_of(&baseline, REMOTE_PROJECT_LIST)
            .expect("subscribe with baseline replies with a project.list");
        assert!(project_paths(&baseline_reply.payload).is_empty());
        let baseline_version = version(&baseline_reply.payload);

        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Repo" }),
            Some("device-a"),
        );

        let acknowledgement =
            reply_of(&added, REMOTE_PROJECT_UPDATED).expect("the mutation source is acknowledged");
        assert_eq!(acknowledgement.payload["action"], "add");

        for device_id in ["device-a", "device-b"] {
            let snapshots = pushes_to(&added, device_id, REMOTE_PROJECT_LIST);
            assert_eq!(
                snapshots.len(),
                1,
                "{device_id} receives exactly one authoritative snapshot"
            );
            assert_eq!(project_paths(&snapshots[0].payload), vec![project.clone()]);
            assert!(
                version(&snapshots[0].payload) > baseline_version,
                "{device_id} receives a newer resource version"
            );
        }

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn edit_and_remove_fan_out_and_keep_the_host_project_id() {
        let dir = temp_dir("edit-remove");
        let service = service(&dir);
        let project = project_dir(&dir, "repo");
        subscribe(&service, "device-a");
        subscribe(&service, "device-b");

        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Repo" }),
            Some("device-a"),
        );
        let project_id = reply_of(&added, REMOTE_PROJECT_UPDATED).unwrap().payload["projectId"]
            .as_str()
            .unwrap()
            .to_string();
        let added_version = version(&pushes_to(&added, "device-b", REMOTE_PROJECT_LIST)[0].payload);

        let edited = service.handle(
            REMOTE_PROJECT_EDIT,
            &json!({ "projectId": project_id, "path": project, "name": "Renamed" }),
            Some("device-b"),
        );
        assert_eq!(
            reply_of(&edited, REMOTE_PROJECT_UPDATED).unwrap().payload["projectId"],
            json!(project_id),
            "edit keeps the host project id"
        );
        let seen_by_a = pushes_to(&edited, "device-a", REMOTE_PROJECT_LIST);
        assert_eq!(seen_by_a.len(), 1);
        assert_eq!(project_names(&seen_by_a[0].payload), vec!["Renamed"]);
        assert!(version(&seen_by_a[0].payload) > added_version);

        let removed = service.handle(
            REMOTE_PROJECT_REMOVE,
            &json!({ "projectId": project_id }),
            Some("device-b"),
        );
        assert_eq!(
            removed.removed_project_id.as_deref(),
            Some(project_id.as_str())
        );
        assert!(
            project_paths(&pushes_to(&removed, "device-a", REMOTE_PROJECT_LIST)[0].payload)
                .is_empty()
        );
        assert!(
            std::path::Path::new(&project).exists(),
            "removal drops catalog membership, not the directory"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_subscribe_without_baseline_still_receives_the_catalog() {
        // The mobile client subscribes to `projects` without asking for a
        // baseline and waits for the `project.list` the desktop host has always
        // answered with. Gating the reply on `baseline` would leave it showing
        // an empty catalog until some other controller mutated it.
        let dir = temp_dir("subscribe-no-baseline");
        let service = service(&dir);
        let project = project_dir(&dir, "repo");
        service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Repo" }),
            Some("device-a"),
        );

        let subscribed = service.handle(
            REMOTE_RESOURCE_SUBSCRIBE,
            &json!({ "resource": REMOTE_RESOURCE_PROJECTS }),
            Some("device-b"),
        );

        let payload = &reply_of(&subscribed, REMOTE_PROJECT_LIST)
            .expect("a subscribe answers with the current catalog")
            .payload;
        assert_eq!(project_paths(payload), vec![project]);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_client_that_never_subscribes_still_uses_the_one_shot_project_api() {
        // An already-shipped client predates the `projects` resource: it never
        // subscribes, and the messages this host added — the `project.updated`
        // acknowledgement and the source-inclusive snapshot — are additive, so
        // dropping them on the floor leaves its one-shot calls working.
        let dir = temp_dir("one-shot");
        let service = service(&dir);
        let project = project_dir(&dir, "repo");

        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Repo" }),
            Some("old-client"),
        );
        assert!(reply_of(&added, REMOTE_ERROR).is_none());
        let project_id = reply_of(&added, REMOTE_PROJECT_UPDATED).unwrap().payload["projectId"]
            .as_str()
            .unwrap()
            .to_string();

        let listed = service.handle(REMOTE_PROJECT_LIST, &json!({}), Some("old-client"));
        assert_eq!(
            project_paths(&reply_of(&listed, REMOTE_PROJECT_LIST).unwrap().payload),
            vec![project]
        );
        assert!(
            listed.messages.iter().all(|message| message.reply),
            "a one-shot list answers the caller and pushes nothing"
        );

        let removed = service.handle(
            REMOTE_PROJECT_REMOVE,
            &json!({ "projectId": project_id }),
            Some("old-client"),
        );
        assert!(reply_of(&removed, REMOTE_ERROR).is_none());
        let listed = service.handle(REMOTE_PROJECT_LIST, &json!({}), Some("old-client"));
        assert!(project_paths(&reply_of(&listed, REMOTE_PROJECT_LIST).unwrap().payload).is_empty());

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unsubscribed_and_disconnected_devices_stop_receiving_snapshots() {
        let dir = temp_dir("unsubscribe");
        let service = service(&dir);
        subscribe(&service, "device-a");
        subscribe(&service, "device-b");

        service.handle(
            REMOTE_RESOURCE_UNSUBSCRIBE,
            &json!({ "resource": REMOTE_RESOURCE_PROJECTS }),
            Some("device-a"),
        );
        service.remove_device("device-b");

        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project_dir(&dir, "repo") }),
            Some("device-c"),
        );

        assert!(pushes_to(&added, "device-a", REMOTE_PROJECT_LIST).is_empty());
        assert!(pushes_to(&added, "device-b", REMOTE_PROJECT_LIST).is_empty());
        assert_eq!(
            pushes_to(&added, "device-c", REMOTE_PROJECT_LIST).len(),
            1,
            "the mutation source still receives the committed snapshot"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adding_the_same_path_twice_keeps_one_entry_with_a_stable_id() {
        let dir = temp_dir("idempotent");
        let service = service(&dir);
        let project = project_dir(&dir, "repo");

        let first = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Repo" }),
            Some("device-a"),
        );
        let second = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Different" }),
            Some("device-b"),
        );

        let first_id =
            reply_of(&first, REMOTE_PROJECT_UPDATED).unwrap().payload["projectId"].clone();
        let second_id =
            reply_of(&second, REMOTE_PROJECT_UPDATED).unwrap().payload["projectId"].clone();
        assert_eq!(first_id, second_id);
        let snapshot = &pushes_to(&second, "device-b", REMOTE_PROJECT_LIST)[0].payload;
        assert_eq!(project_paths(snapshot), vec![project]);
        assert_eq!(
            project_names(snapshot),
            vec!["Repo"],
            "a repeated add does not rename the existing entry"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn edit_rejects_a_path_owned_by_another_project_without_broadcasting() {
        let dir = temp_dir("collision");
        let service = service(&dir);
        let first = project_dir(&dir, "first");
        let second = project_dir(&dir, "second");
        subscribe(&service, "device-a");
        service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": first }),
            Some("device-a"),
        );
        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": second }),
            Some("device-a"),
        );
        let second_id = reply_of(&added, REMOTE_PROJECT_UPDATED).unwrap().payload["projectId"]
            .as_str()
            .unwrap()
            .to_string();
        let settled_version =
            version(&pushes_to(&added, "device-a", REMOTE_PROJECT_LIST)[0].payload);

        let rejected = service.handle(
            REMOTE_PROJECT_EDIT,
            &json!({ "projectId": second_id, "path": first }),
            Some("device-a"),
        );

        assert!(reply_of(&rejected, REMOTE_ERROR).is_some());
        assert!(pushes_to(&rejected, "device-a", REMOTE_PROJECT_LIST).is_empty());
        let listed = service.handle(REMOTE_PROJECT_LIST, &json!({}), Some("device-a"));
        assert_eq!(
            version(&reply_of(&listed, REMOTE_PROJECT_LIST).unwrap().payload),
            settled_version,
            "a rejected edit does not bump the resource version"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn concurrent_adds_publish_each_catalog_under_its_own_version() {
        let dir = temp_dir("concurrent");
        let service = std::sync::Arc::new(service(&dir));
        let paths: Vec<String> = (0..8)
            .map(|index| project_dir(&dir, &format!("repo-{index}")))
            .collect();

        let handles: Vec<_> = paths
            .iter()
            .cloned()
            .map(|path| {
                let service = std::sync::Arc::clone(&service);
                std::thread::spawn(move || {
                    service.handle(
                        REMOTE_PROJECT_ADD,
                        &json!({ "path": path }),
                        Some("device-a"),
                    )
                })
            })
            .collect();
        let mut published = handles
            .into_iter()
            .map(|handle| {
                let outcome = handle.join().unwrap();
                let snapshot = &pushes_to(&outcome, "device-a", REMOTE_PROJECT_LIST)[0].payload;
                (version(snapshot), project_paths(snapshot))
            })
            .collect::<Vec<_>>();

        let listed = service.handle(REMOTE_PROJECT_LIST, &json!({}), Some("device-a"));
        let mut stored = project_paths(&reply_of(&listed, REMOTE_PROJECT_LIST).unwrap().payload);
        stored.sort();
        let mut expected = paths;
        expected.sort();
        assert_eq!(stored, expected);

        // Every published catalog carries the version its own commit minted, so
        // the n-th version describes exactly n entries. This is the pairing a
        // client's latest-wins guard depends on: with the version minted outside
        // the store lock, one commit's catalog could go out under another's
        // version and a client would lose an entry that is on disk. (The window
        // is too narrow to provoke on demand, so this pins the invariant rather
        // than reproducing the interleaving.)
        published.sort_by_key(|(version, _)| *version);
        for (index, (version, paths)) in published.iter().enumerate() {
            assert_eq!(*version, index as u64 + 1, "one version per commit");
            assert_eq!(
                paths.len(),
                index + 1,
                "version {version} describes the catalog of another commit"
            );
        }
        assert_eq!(published.len(), 8);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_malformed_catalog_is_reported_and_never_overwritten() {
        let dir = temp_dir("malformed");
        let catalog = dir.join("projects.json");
        std::fs::write(&catalog, "{ not json").unwrap();
        let service = service(&dir);

        let listed = service.handle(REMOTE_PROJECT_LIST, &json!({}), Some("device-a"));
        assert!(
            reply_of(&listed, REMOTE_ERROR).unwrap().payload["message"]
                .as_str()
                .unwrap()
                .contains("not valid JSON")
        );

        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project_dir(&dir, "repo") }),
            Some("device-a"),
        );
        assert!(reply_of(&added, REMOTE_ERROR).is_some());
        assert!(added.messages.iter().all(|message| message.reply));
        assert_eq!(std::fs::read_to_string(&catalog).unwrap(), "{ not json");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_failed_save_acknowledges_nothing_and_publishes_no_snapshot() {
        let dir = temp_dir("save-failure");
        // The catalog path is a directory, so the atomic replacement fails.
        std::fs::create_dir_all(dir.join("projects.json")).unwrap();
        let service = service(&dir);
        subscribe(&service, "device-b");

        let added = service.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project_dir(&dir, "repo") }),
            Some("device-a"),
        );

        assert!(reply_of(&added, REMOTE_ERROR).is_some());
        assert!(reply_of(&added, REMOTE_PROJECT_UPDATED).is_none());
        assert!(pushes_to(&added, "device-b", REMOTE_PROJECT_LIST).is_empty());

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_restarted_store_serves_the_persisted_catalog_from_a_fresh_version_epoch() {
        let dir = temp_dir("restart");
        let project = project_dir(&dir, "repo");
        let first = service(&dir);
        subscribe(&first, "device-a");
        let added = first.handle(
            REMOTE_PROJECT_ADD,
            &json!({ "path": project, "name": "Repo" }),
            Some("device-a"),
        );
        assert_eq!(
            version(&pushes_to(&added, "device-a", REMOTE_PROJECT_LIST)[0].payload),
            1
        );

        let restarted = service(&dir);
        let baseline = subscribe(&restarted, "device-a");
        let payload = &reply_of(&baseline, REMOTE_PROJECT_LIST).unwrap().payload;

        assert_eq!(project_paths(payload), vec![project]);
        assert_eq!(project_names(payload), vec!["Repo"]);
        assert_eq!(version(payload), 0, "a restart starts a new version epoch");

        std::fs::remove_dir_all(dir).ok();
    }
}
