impl RuntimeService {
    pub fn project_open_applications(
        &self,
    ) -> Vec<crate::project_open::ProjectOpenApplicationSummary> {
        crate::project_open::project_open_applications()
    }

    pub fn project_open_in_application(
        &self,
        project_path: String,
        application_id: String,
    ) -> Result<(), String> {
        crate::project_open::project_open_in_application(
            crate::project_open::ProjectOpenApplicationRequest {
                project_path,
                application_id,
            },
        )
    }

    pub fn project_reveal_in_file_manager(&self, project_path: &str) -> Result<(), String> {
        crate::project_open::project_reveal_in_file_manager(project_path)
    }

    pub fn project_list(&self) -> ProjectListSnapshot {
        ProjectStore::new(self.support_dir.clone()).list_snapshot()
    }

    pub fn project_root_missing(&self, project_id: &str) -> bool {
        ProjectStore::new(self.support_dir.clone()).project_root_missing(project_id)
    }

    pub fn project_create(
        &self,
        request: ProjectCreateRequest,
    ) -> Result<ProjectListSnapshot, String> {
        ProjectStore::validate_project_environment_variables(&request.environment_variables)?;
        // A project on a host that owns its catalog is the host's to create:
        // commit there first, then adopt the catalog it committed. An
        // unreachable owner fails instead of writing locally — that record could
        // never be uploaded afterwards (migration has already run for that host)
        // and no snapshot would ever mention it, leaving a project only this
        // desktop can see. Hosts that own no catalog keep the previous
        // controller-local behaviour.
        if let Some(device_id) = request.runtime_target.remote_device_id().map(str::to_string) {
            match self.project_catalog_owner(&device_id) {
                ProjectCatalogOwner::NotACatalog => {}
                owner => {
                    let controller = owner.require(&device_id)?;
                    return self.create_remote_project(&device_id, &controller, request);
                }
            }
        }
        let snapshot = ProjectStore::new(self.support_dir.clone()).create_project(request)?;
        if let Some(project_id) = snapshot.selected_project_id.as_deref() {
            let _ = self.mark_project_active_with_watch(project_id);
        }
        self.sync_pet_project_memberships();
        Ok(snapshot)
    }

    pub fn project_update(
        &self,
        request: ProjectUpdateRequest,
    ) -> Result<ProjectListSnapshot, String> {
        ProjectStore::validate_project_environment_variables(&request.environment_variables)?;
        let store = ProjectStore::new(self.support_dir.clone());
        let target_remote_device_id = request
            .runtime_target
            .remote_device_id()
            .map(str::to_string);
        // A legacy remote record can become a projection asynchronously. Finish
        // that host's migration before deciding whether this edit is local;
        // otherwise edit/close can race registration and diverge from the host.
        let existing_remote_device_id = store
            .projects_snapshot()
            .into_iter()
            .find(|project| project.id == request.project_id)
            .and_then(|project| {
                project
                    .runtime_target
                    .remote_device_id()
                    .map(str::to_string)
            });
        let existing_catalog_controller = match existing_remote_device_id.as_deref() {
            Some(device_id) => self.remote_project_catalog_host_if_any(device_id)?,
            None => None,
        };
        // A projection of a host's catalog is the host's to rename or re-path:
        // it commits there first, and the snapshot it publishes is what moves
        // the shared fields here and on every other connected controller. An
        // edit that changes owners creates an independent project through the
        // new target's normal flow. Rewriting this projection in place would
        // orphan the original host's catalog entry, sessions, and bridge state.
        if let Some((device_id, host_project_id)) =
            store.remote_catalog_projection(&request.project_id)
        {
            if target_remote_device_id.as_deref() == Some(device_id.as_str()) {
                let controller = existing_catalog_controller.as_ref().ok_or_else(|| {
                    format!("Remote host {device_id} is unavailable.")
                })?;
                return self.edit_remote_project(
                    &device_id,
                    &host_project_id,
                    controller.as_ref(),
                    request,
                );
            }
            return self.project_create(ProjectCreateRequest {
                name: request.name,
                path: request.path,
                badge_text: request.badge_text,
                badge_symbol: request.badge_symbol,
                badge_color_hex: request.badge_color_hex,
                environment_variables: request.environment_variables,
                runtime_target: request.runtime_target,
            });
        }
        if let Some(device_id) = target_remote_device_id {
            let catalog_controller = if existing_remote_device_id.as_deref()
                == Some(device_id.as_str())
            {
                existing_catalog_controller
            } else {
                self.remote_project_catalog_host_if_any(&device_id)?
            };
            if let Some(controller) = catalog_controller {
                return self.create_remote_project(
                    &device_id,
                    &controller,
                    ProjectCreateRequest {
                        name: request.name,
                        path: request.path,
                        badge_text: request.badge_text,
                        badge_symbol: request.badge_symbol,
                        badge_color_hex: request.badge_color_hex,
                        environment_variables: request.environment_variables,
                        runtime_target: request.runtime_target,
                    },
                );
            }
        }
        let snapshot = store.update_project_from_request(request)?;
        if let Some(project_id) = snapshot.selected_project_id.as_deref() {
            let _ = self.mark_project_active_with_watch(project_id);
        }
        self.sync_pet_project_memberships();
        Ok(snapshot)
    }

    pub fn project_reorder(
        &self,
        request: ProjectReorderRequest,
    ) -> Result<ProjectListSnapshot, String> {
        ProjectStore::new(self.support_dir.clone()).reorder_projects(request)
    }

    pub fn project_close(
        &self,
        request: ProjectCloseRequest,
    ) -> Result<ProjectListSnapshot, String> {
        self.close_project(&request.project_id)?;
        Ok(self.project_list())
    }

    pub fn project_select_worktree(
        &self,
        request: ProjectSelectWorktreeRequest,
    ) -> Result<(), String> {
        let store = ProjectStore::new(self.support_dir.clone());
        let worktree_id = request.worktree_id.clone();
        store.select_worktree(request)?;
        let project_id = store
            .root_project_summary_for_workspace_id(&worktree_id)
            .map(|project| project.id)
            .or_else(|| store.list_snapshot().selected_project_id);
        if let Some(project_id) = project_id {
            let _ = self.mark_project_active_with_watch(&project_id);
        }
        Ok(())
    }

    pub fn project_set_default_push_remote(
        &self,
        request: ProjectDefaultPushRemoteRequest,
    ) -> Result<ProjectListSnapshot, String> {
        ProjectStore::new(self.support_dir.clone()).set_default_push_remote(request)
    }

    pub fn terminal_layout_record(&self, project_id: &str) -> Option<TerminalLayoutRecord> {
        ProjectStore::new(self.support_dir.clone()).terminal_layout(project_id)
    }

    pub fn terminal_layout_records(&self) -> TerminalLayoutsSnapshot {
        ProjectStore::new(self.support_dir.clone()).terminal_layouts_snapshot()
    }

    pub fn save_terminal_layout_record(
        &self,
        project_id: String,
        layout: TerminalLayoutRecord,
    ) -> Result<TerminalLayoutRecord, String> {
        ProjectStore::new(self.support_dir.clone()).save_terminal_layout(project_id, layout)
    }

    pub fn select_project(&self, project_id: &str) -> Result<(), String> {
        ProjectStore::new(self.support_dir.clone()).select_project(project_id)?;
        self.mark_project_active_with_watch(project_id).map(|_| ())
    }

    pub fn create_or_select_project(&self, name: &str, path: &str) -> Result<String, String> {
        let project_id = ProjectStore::new(self.support_dir.clone()).create_or_select_project(name, path)?;
        self.mark_project_active_with_watch(&project_id)?;
        self.sync_pet_project_memberships();
        Ok(project_id)
    }

    pub fn update_project(&self, project_id: &str, name: &str, path: &str) -> Result<(), String> {
        ProjectStore::new(self.support_dir.clone()).update_project(project_id, name, path)?;
        self.mark_project_active_with_watch(project_id)?;
        self.sync_pet_project_memberships();
        Ok(())
    }

    pub fn move_project_up(&self, project_id: &str) -> Result<(), String> {
        ProjectStore::new(self.support_dir.clone())
            .move_project(project_id, ProjectMoveDirection::Up)
    }

    pub fn move_project_down(&self, project_id: &str) -> Result<(), String> {
        ProjectStore::new(self.support_dir.clone())
            .move_project(project_id, ProjectMoveDirection::Down)
    }

    pub fn close_project(&self, project_id: &str) -> Result<Option<String>, String> {
        let store = ProjectStore::new(self.support_dir.clone());
        let remote_device_id = store
            .projects_snapshot()
            .into_iter()
            .find(|project| project.id == project_id)
            .and_then(|project| {
                project
                    .runtime_target
                    .remote_device_id()
                    .map(str::to_string)
            });
        let catalog_controller = match remote_device_id.as_deref() {
            Some(device_id) => self.remote_project_catalog_host_if_any(device_id)?,
            None => None,
        };
        // Removing a projection removes catalog membership, which only the host
        // can do; the snapshot it publishes prunes this projection and every
        // other controller's. Reconciling that snapshot runs the lifecycle
        // below, so this path stops here. The host's directory and sessions are
        // untouched.
        if let Some((device_id, host_project_id)) = store.remote_catalog_projection(project_id) {
            let controller = catalog_controller
                .ok_or_else(|| format!("Remote host {device_id} is unavailable."))?;
            return self.remove_remote_project(&device_id, &host_project_id, &controller);
        }
        let workspace_ids = self.project_workspace_ids_for_root(project_id);
        let next_project_id = store.close_project(project_id)?;
        self.finish_project_removal(
            std::slice::from_ref(&project_id.to_string()),
            &workspace_ids,
            next_project_id.as_deref(),
        );
        Ok(next_project_id)
    }

    /// Everything that must follow projects leaving this desktop, whichever way
    /// they left: the user closed one, or the host that owns them dropped them
    /// from its catalog. `workspace_ids` are the workspaces they owned, read
    /// before the store write that removed them.
    pub(crate) fn finish_project_removal(
        &self,
        project_ids: &[String],
        workspace_ids: &[String],
        next_project_id: Option<&str>,
    ) {
        for project_id in project_ids {
            self.remote_host.remove_project_state(project_id);
            self.project_activity.remove_project(project_id);
        }
        self.cleanup_project_workspace_data(workspace_ids);
        match next_project_id {
            Some(next_project_id) => {
                let _ = self.mark_project_active_with_watch(next_project_id);
            }
            None => self.stop_active_project_watches(),
        }
        self.sync_pet_project_memberships();
    }

    pub fn read_project_file_edit_buffer(
        &self,
        project_path: &str,
        relative_path: &str,
    ) -> Result<(String, bool), String> {
        if let Some(runtime) = self.hosted_runtime_for_project_path(project_path) {
            let absolute = crate::path::join_path(
                project_path,
                relative_path.trim_start_matches(['/', '\\']),
            );
            return match runtime.and_then(|runtime| runtime.read_file(&absolute)) {
                Ok(payload) => Ok((
                    payload
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    true,
                )),
                // The host errors on binary/oversized files; surface it
                // read-only, mirroring the local "message" path.
                Err(message) => Ok((message, false)),
            };
        }
        let result = FilesService::read_text(project_path, relative_path)?;
        if let Some(message) = result.message {
            return Ok((message, false));
        }
        Ok((
            result.content,
            !result.is_binary && !result.is_large && !result.is_truncated,
        ))
    }

    fn project_workspace_ids_for_root(&self, project_id: &str) -> Vec<String> {
        ProjectStore::new(self.support_dir.clone())
            .project_workspaces_snapshot()
            .into_iter()
            .filter(|workspace| workspace.root_project_id == project_id)
            .map(|workspace| workspace.id)
            .collect()
    }

    fn cleanup_project_workspace_data(&self, workspace_ids: &[String]) {
        if workspace_ids.is_empty() {
            return;
        }

        let terminal_layout = TerminalLayoutService::new(self.support_dir.clone());
        let file_editor_layout = FileEditorLayoutService::new(self.support_dir.clone());
        let obsolete_cache =
            crate::persistent_cache::PersistentCacheStore::for_support_dir(self.support_dir.clone())
                .ok();

        for workspace_id in workspace_ids {
            let _ = terminal_layout.delete(workspace_id);
            let _ = file_editor_layout.delete(workspace_id);
            if let Some(cache) = obsolete_cache.as_ref() {
                let _ = cache.delete_json("file-tree-state", workspace_id);
                let _ = cache.delete_json("git-ui-state", workspace_id);
            }
        }
    }
}
