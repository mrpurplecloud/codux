//! Desktop projections of a Headless host's authoritative project catalog.
//!
//! The host owns membership, host project id, name, and path; the desktop keeps
//! a local record per catalog entry so the existing UI (selection, ordering,
//! badges, layouts) keeps working. Reconciliation applies one snapshot for one
//! device in a single state transaction: matched projections keep their desktop
//! id and controller-only fields, missing ones are created, and projections the
//! host no longer lists are pruned. Records for local, WSL, another device, or a
//! not-yet-migrated legacy remote target are never touched.

use super::{ProjectRecord, ProjectRuntimeTarget, ProjectStore};
use crate::project_store::helpers::project_uuid_from;
use crate::project_store::mutations::normalized_runtime_target;
use crate::project_store::raw_state::{
    ensure_array, project_record, prune_project_state, select_project_after_removal,
    update_default_worktree_record,
};
use serde_json::{Map, Value};

/// One entry of a host's `project.list` snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCatalogProject {
    pub host_project_id: String,
    pub name: String,
    pub path: String,
}

impl ProjectStore {
    /// Apply one authoritative catalog snapshot from the Headless host
    /// `device_id`. Returns whether the local store changed.
    pub fn reconcile_remote_projects(
        &self,
        device_id: &str,
        catalog: &[RemoteCatalogProject],
    ) -> Result<bool, String> {
        let target = normalized_runtime_target(ProjectRuntimeTarget::Remote {
            device_id: device_id.to_string(),
        })?;
        let device_id = target.remote_device_id().unwrap_or_default();
        let mut raw = self.raw_snapshot();
        let selected_project_id = raw
            .get("selectedProjectId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut changed = false;
        let mut removed = Vec::new();
        let mut selected_removed_at = None;
        // Desktop id of every projection whose shared fields moved, so the
        // default worktree record follows the rename. A migrated projection
        // keeps its old desktop id, so this cannot be derived from the host id.
        let mut renamed = Vec::new();

        {
            let projects = ensure_array(&mut raw, "projects")?;
            let mut index = 0;
            while index < projects.len() {
                let Some(managed) = managed_projection(&projects[index], device_id) else {
                    index += 1;
                    continue;
                };
                match catalog
                    .iter()
                    .find(|item| item.host_project_id == managed.host_project_id)
                {
                    Some(item) => {
                        if apply_shared_fields(&mut projects[index], item) {
                            renamed.push((managed.project_id, item));
                            changed = true;
                        }
                        index += 1;
                    }
                    None => {
                        projects.remove(index);
                        if selected_project_id.as_deref() == Some(managed.project_id.as_str()) {
                            selected_removed_at = Some(index);
                        }
                        removed.push(managed.project_id);
                        changed = true;
                    }
                }
            }

            for item in catalog {
                if projects.iter().any(|project| {
                    managed_projection(project, device_id)
                        .is_some_and(|managed| managed.host_project_id == item.host_project_id)
                }) {
                    continue;
                }
                projects.push(Value::Object(new_projection(device_id, item, &target)?));
                changed = true;
            }
        }

        if !changed {
            return Ok(false);
        }

        for project_id in &removed {
            prune_project_state(&mut raw, project_id);
        }
        for (project_id, item) in renamed {
            update_default_worktree_record(&mut raw, &project_id, &item.name, &item.path);
        }
        if let Some(removed_index) = selected_removed_at {
            let next = raw
                .get("projects")
                .and_then(Value::as_array)
                .and_then(|projects| select_project_after_removal(projects, removed_index));
            match next {
                Some(next) => {
                    raw.insert("selectedProjectId".to_string(), Value::String(next));
                }
                None => {
                    raw.remove("selectedProjectId");
                }
            }
        }
        self.save_raw_snapshot(&raw)?;
        Ok(true)
    }

    /// Every `(desktop id, host project id)` pair this desktop holds for
    /// `device_id`. The remote transport uses it to translate project-scoped ids
    /// in both directions.
    pub fn remote_project_id_pairs(&self, device_id: &str) -> Vec<(String, String)> {
        self.projects_snapshot()
            .into_iter()
            .filter(|project| project.runtime_target.remote_device_id() == Some(device_id))
            .filter_map(|project| {
                project
                    .host_project_id
                    .map(|host_project_id| (project.id, host_project_id))
            })
            .collect()
    }
}

/// The desktop id for a catalog entry. Derived from `(device id, host project
/// id)` so two hosts that mint the same host id stay distinct projections, and
/// so a removed-then-re-added entry lands back on its previous local state.
pub fn remote_projection_id(device_id: &str, host_project_id: &str) -> String {
    project_uuid_from(&format!("remote:{device_id}:{host_project_id}"))
}

struct ManagedProjection {
    project_id: String,
    host_project_id: String,
}

/// A record this device's catalog owns: a remote projection for `device_id`
/// that already carries a host project id. Legacy remote records without one
/// are migrated separately and must survive reconciliation untouched.
fn managed_projection(project: &Value, device_id: &str) -> Option<ManagedProjection> {
    let record = serde_json::from_value::<ProjectRecord>(project.clone()).ok()?;
    if record.runtime_target.remote_device_id() != Some(device_id) {
        return None;
    }
    Some(ManagedProjection {
        project_id: record.id,
        host_project_id: record.host_project_id?,
    })
}

/// Overwrite the host-owned fields, leaving every controller-only field (badge,
/// environment, push remote, ordering, and any field a newer build wrote) in
/// place. Returns whether anything actually changed.
fn apply_shared_fields(project: &mut Value, item: &RemoteCatalogProject) -> bool {
    let Some(project) = project.as_object_mut() else {
        return false;
    };
    if project.get("name").and_then(Value::as_str) == Some(item.name.as_str())
        && project.get("path").and_then(Value::as_str) == Some(item.path.as_str())
    {
        return false;
    }
    project.insert("name".to_string(), Value::String(item.name.clone()));
    project.insert("path".to_string(), Value::String(item.path.clone()));
    true
}

fn new_projection(
    device_id: &str,
    item: &RemoteCatalogProject,
    target: &ProjectRuntimeTarget,
) -> Result<Map<String, Value>, String> {
    let project_id = remote_projection_id(device_id, &item.host_project_id);
    let mut record = project_record(&project_id, &item.name, &item.path);
    record.insert(
        "runtimeTarget".to_string(),
        serde_json::to_value(target)
            .map_err(|error| format!("Failed to serialize project runtime target: {error}"))?,
    );
    record.insert(
        "hostProjectId".to_string(),
        Value::String(item.host_project_id.clone()),
    );
    Ok(record)
}
