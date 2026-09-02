//! A tiny project store for the headless agent: a JSON list of
//! `{id, name, path}` the host serves via `project.list/add/remove`. The desktop
//! has a richer ProjectStore (worktrees, badges, …); the agent only needs the
//! list so a controller can pick a project to run terminals/files against.

use codux_runtime_core::project::ProjectListItem;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// The agent's persistent data directory (projects list, AI usage cache, …).
/// `CODUX_AGENT_DATA_DIR` overrides the default `~/.codux-agent`.
pub fn agent_data_dir() -> PathBuf {
    std::env::var("CODUX_AGENT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".codux-agent")
        })
}

/// One committed catalog change: the catalog as saved, plus the entry the
/// mutation settled on.
pub struct ProjectCommit {
    pub items: Vec<ProjectListItem>,
    pub project_id: String,
}

pub struct AgentProjectStore {
    path: PathBuf,
    /// Held across load → validate → save so two controllers mutating at once
    /// cannot each save a catalog built from the same stale read.
    mutation: Mutex<()>,
}

impl AgentProjectStore {
    pub fn new() -> Self {
        Self::with_path(agent_data_dir().join("projects.json"))
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            mutation: Mutex::new(()),
        }
    }

    /// The stored catalog. A missing file is an empty catalog; an unreadable or
    /// malformed one is an error, so a mutation refuses to overwrite the last
    /// valid catalog with a fresh empty list.
    pub fn list(&self) -> Result<Vec<ProjectListItem>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "Project catalog {} is unreadable: {error}",
                    self.path.display()
                ));
            }
        };
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<ProjectListItem>>(&text).map_err(|error| {
            format!(
                "Project catalog {} is not valid JSON: {error}. Repair or move the file, then retry.",
                self.path.display()
            )
        })
    }

    /// Register `path`. Idempotent by host-native path: an existing entry keeps
    /// its id and name, so a retry or a second controller adding the same folder
    /// does not rename it.
    pub fn add(&self, path: &str, name: Option<&str>) -> Result<ProjectCommit, String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("Project path is required.".to_string());
        }
        let _mutation = self.lock_mutation();
        let mut items = self.list()?;
        if let Some(existing) = items.iter().find(|item| paths_equal(&item.path, path)) {
            return Ok(ProjectCommit {
                project_id: existing.id.clone(),
                items,
            });
        }
        let project_id = project_id_for_path(path);
        items.push(ProjectListItem {
            id: project_id.clone(),
            name: clean_name(name).unwrap_or_else(|| default_project_name(path)),
            path: path.to_string(),
        });
        self.save(&items)?;
        Ok(ProjectCommit { items, project_id })
    }

    /// Rename or re-point an entry, keeping its host project id stable so
    /// project-scoped history and requests keep working.
    pub fn edit(
        &self,
        project_id: &str,
        path: &str,
        name: Option<&str>,
    ) -> Result<ProjectCommit, String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("Project path is required.".to_string());
        }
        let _mutation = self.lock_mutation();
        let mut items = self.list()?;
        if !items.iter().any(|item| item.id == project_id) {
            return Err("Project not found.".to_string());
        }
        if items
            .iter()
            .any(|item| item.id != project_id && paths_equal(&item.path, path))
        {
            return Err("Another project already uses that path.".to_string());
        }
        let name = clean_name(name).unwrap_or_else(|| default_project_name(path));
        if let Some(entry) = items.iter_mut().find(|item| item.id == project_id) {
            entry.name = name;
            entry.path = path.to_string();
        }
        self.save(&items)?;
        Ok(ProjectCommit {
            items,
            project_id: project_id.to_string(),
        })
    }

    /// Drop catalog membership. Idempotent, and it never touches the project
    /// directory on disk.
    pub fn remove(&self, project_id: &str) -> Result<ProjectCommit, String> {
        let _mutation = self.lock_mutation();
        let mut items = self.list()?;
        let before = items.len();
        items.retain(|item| item.id != project_id);
        if items.len() != before {
            self.save(&items)?;
        }
        Ok(ProjectCommit {
            items,
            project_id: project_id.to_string(),
        })
    }

    fn lock_mutation(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.mutation.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Write through a same-directory temporary file and replace the catalog in
    /// one step, so a reader sees either the old or the new complete JSON.
    fn save(&self, items: &[ProjectListItem]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = serde_json::to_string_pretty(items).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension(format!(
            "json.tmp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        fs::write(&temporary, text).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            error.to_string()
        })
    }
}

fn paths_equal(left: &str, right: &str) -> bool {
    codux_runtime_core::path::local_paths_equal(Path::new(left), Path::new(right))
}

fn clean_name(name: Option<&str>) -> Option<String> {
    name.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn project_id_for_path(path: &str) -> String {
    let mut hasher = DefaultHasher::new();
    codux_runtime_core::path::local_path_identity_key(Path::new(path))
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("p-{:016x}", hasher.finish())
}

fn default_project_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Project")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids_and_names_follow_target_path_syntax() {
        #[cfg(windows)]
        assert_eq!(
            project_id_for_path(r"C:\Users\Dux\Project"),
            project_id_for_path("c:/users/dux/project/")
        );
        #[cfg(windows)]
        assert_eq!(default_project_name(r"C:\Users\Dux\Project"), "Project");
        #[cfg(not(windows))]
        assert_eq!(default_project_name("/home/dux/Project"), "Project");
        #[cfg(not(windows))]
        assert_ne!(
            project_id_for_path("/repo/Project"),
            project_id_for_path("/repo/project")
        );
    }
}
