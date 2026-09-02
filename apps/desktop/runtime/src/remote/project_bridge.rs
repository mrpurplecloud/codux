//! Translation between this desktop's project ids and the ids the host keeps.
//!
//! A desktop projection of a host catalog entry keeps its own controller-local
//! id (UI state, layouts, and selection point at it), while the host routes
//! terminal, Git, AI, file, and worktree work by the id in ITS catalog. Passing
//! the desktop id over the wire relies on the host's path fallback and misroutes
//! any id-only request.
//!
//! The bridge sits at the transport boundary of one controller — the single
//! place every project-scoped envelope passes through — so no UI caller has to
//! remember which id it holds. Outbound envelopes get the host id, inbound ones
//! get the desktop id back.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

/// Payload fields that carry a project identity. The worktree scopes are
/// included because a project's default worktree id IS its project id.
const PROJECT_ID_FIELDS: [&str; 3] = ["projectId", "worktreeId", "selectedWorktreeId"];

#[derive(Default)]
pub(super) struct ProjectIdBridge {
    tables: Mutex<Tables>,
}

#[derive(Default)]
struct Tables {
    to_host: HashMap<String, String>,
    to_local: HashMap<String, String>,
}

impl ProjectIdBridge {
    /// Replace the whole table from the reconciled projections of this device.
    /// Reconciliation is a full snapshot, so the bridge is too — a projection
    /// the host dropped must stop translating.
    pub(super) fn replace(&self, pairs: Vec<(String, String)>) {
        let Ok(mut tables) = self.tables.lock() else {
            return;
        };
        tables.to_local = pairs
            .iter()
            .map(|(local, host)| (host.clone(), local.clone()))
            .collect();
        tables.to_host = pairs.into_iter().collect();
    }

    pub(super) fn to_host(&self, payload: &mut Value) {
        let Ok(tables) = self.tables.lock() else {
            return;
        };
        translate(payload, &tables.to_host);
    }

    pub(super) fn to_local(&self, payload: &mut Value) {
        let Ok(tables) = self.tables.lock() else {
            return;
        };
        translate(payload, &tables.to_local);
    }
}

/// Rewrite every project-identity field in `payload`, at any depth: a terminal
/// config nests its scope, and list payloads carry one entry per project.
fn translate(payload: &mut Value, table: &HashMap<String, String>) {
    if table.is_empty() {
        return;
    }
    match payload {
        Value::Object(fields) => {
            // A record listed under a project can be identified BY that project:
            // the default worktree's own `id` is the project id. Only such a
            // record's `id` is a candidate, so a session or file id listed
            // beside a `projectId` is left alone by the lookup below.
            let identifies_a_project = fields.contains_key("projectId");
            for (key, value) in fields.iter_mut() {
                let is_identity = PROJECT_ID_FIELDS.contains(&key.as_str())
                    || (identifies_a_project && key == "id");
                if is_identity && let Some(translated) = value.as_str().and_then(|id| table.get(id))
                {
                    *value = Value::String(translated.clone());
                    continue;
                }
                translate(value, table);
            }
        }
        Value::Array(items) => {
            for item in items {
                translate(item, table);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bridge() -> ProjectIdBridge {
        let bridge = ProjectIdBridge::default();
        bridge.replace(vec![("desktop-1".to_string(), "p-host-1".to_string())]);
        bridge
    }

    #[test]
    fn outbound_project_and_worktree_scope_use_the_host_id() {
        let mut payload = json!({
            "projectId": "desktop-1",
            "worktreeId": "desktop-1",
            "cwd": "/srv/alpha",
        });

        bridge().to_host(&mut payload);

        assert_eq!(payload["projectId"], json!("p-host-1"));
        assert_eq!(payload["worktreeId"], json!("p-host-1"));
        assert_eq!(payload["cwd"], json!("/srv/alpha"));
    }

    #[test]
    fn nested_and_listed_scopes_are_translated() {
        let mut payload = json!({
            "terminals": [
                { "id": "session-1", "projectId": "p-host-1", "worktreeId": "host-worktree" },
                { "id": "session-2", "projectId": "unknown" }
            ],
            "config": { "projectId": "p-host-1" }
        });

        bridge().to_local(&mut payload);

        assert_eq!(payload["terminals"][0]["projectId"], json!("desktop-1"));
        assert_eq!(
            payload["terminals"][0]["worktreeId"],
            json!("host-worktree")
        );
        assert_eq!(payload["terminals"][1]["projectId"], json!("unknown"));
        assert_eq!(payload["config"]["projectId"], json!("desktop-1"));
    }

    #[test]
    fn the_default_worktree_identity_follows_the_project() {
        // The host names a project's default worktree after the project, so an
        // untranslated `id` would store a worktree the desktop cannot select.
        let mut payload = json!({
            "projectId": "p-host-1",
            "selectedWorktreeId": "p-host-1",
            "worktrees": [
                { "id": "p-host-1", "projectId": "p-host-1", "isDefault": true },
                { "id": "worktree-2", "projectId": "p-host-1", "isDefault": false }
            ],
            "terminals": [{ "id": "session-1", "projectId": "p-host-1" }]
        });

        bridge().to_local(&mut payload);

        assert_eq!(payload["projectId"], json!("desktop-1"));
        assert_eq!(payload["selectedWorktreeId"], json!("desktop-1"));
        assert_eq!(payload["worktrees"][0]["id"], json!("desktop-1"));
        assert_eq!(payload["worktrees"][1]["id"], json!("worktree-2"));
        assert_eq!(payload["terminals"][0]["id"], json!("session-1"));
    }

    #[test]
    fn replacing_the_table_drops_stale_projections() {
        let bridge = bridge();
        bridge.replace(Vec::new());
        let mut payload = json!({ "projectId": "desktop-1" });

        bridge.to_host(&mut payload);

        assert_eq!(payload["projectId"], json!("desktop-1"));
    }
}
