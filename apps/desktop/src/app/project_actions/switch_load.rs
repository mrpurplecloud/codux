use super::*;
use crate::app::app_state::ProjectSessionLoad;

impl ProjectSessionLoad {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            result: None,
        }
    }

    fn published(&mut self, generation: u64, result: Result<(), String>) {
        if self.generation == generation {
            self.result = Some(result);
        }
    }

    pub(in crate::app) fn readiness(&self, generation: u64) -> Result<bool, &str> {
        if self.generation != generation {
            return Ok(true);
        }
        match &self.result {
            None => Ok(false),
            Some(Ok(())) => Ok(true),
            Some(Err(error)) => Err(error),
        }
    }
}

impl CoduxApp {
    pub(in crate::app) fn spawn_project_switch_load(
        &mut self,
        project_id: String,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            return;
        };
        let projects = self.state.projects.clone();
        let runtime_service = self.runtime_service.clone();
        let terminal_layout_service = runtime_service.clone();
        let task_runtime_service = runtime_service.clone();
        let task_project = project.clone();
        let primary_runtime_service = runtime_service.clone();
        let primary_project = project.clone();
        let primary_worktree = super::ai_runtime_status::selected_worktree_info(&self.state);
        let include_cached = self.state.settings.statistics_mode == "includingCache";
        let primary_scope_key = WorktreeScopeKey {
            project_id: project.id.clone(),
            worktree_id: primary_worktree
                .as_ref()
                .map(|worktree| worktree.id.clone())
                .unwrap_or_else(|| project.id.clone()),
        };
        let stats_runtime_service = runtime_service.clone();
        let stats_project = project.clone();
        self.runtime_trace(
            "project-switch",
            &format!(
                "spawn_loads project={} generation={}",
                project_id, generation
            ),
        );

        // Directory and layout must describe the same load. Publish both in
        // one callback, then allow the current session navigation to continue.
        self.project_session_load = Some(ProjectSessionLoad::new(generation));
        let task_project_id = task_project.id.clone();
        let task_project_path = task_project.path.clone();
        let task_queued_at = Instant::now();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let session_load = codux_runtime::async_runtime::run_limited_blocking_with_priority(
                codux_runtime::async_runtime::BLOCKING_PRIORITY_FOREGROUND + generation,
                move || {
                    let worker_started_at = Instant::now();
                    codux_runtime::runtime_trace::runtime_trace(
                        "project-switch",
                        &format!(
                            "session_load worker_start project={} generation={} queue_wait_ms={}",
                            task_project_id,
                            generation,
                            task_queued_at.elapsed().as_millis()
                        ),
                    );
                    let worktrees = task_runtime_service.reload_worktrees_from_state(
                        Some(&task_project_id),
                        Some(&task_project_path),
                    );
                    let terminal_owner_id = worktrees
                        .selected_worktree_id
                        .clone()
                        .unwrap_or_else(|| task_project_id.clone());
                    let terminal_storage_key =
                        super::ai_runtime_status::terminal_layout_storage_key(
                            &task_project_id,
                            &terminal_owner_id,
                        );
                    let terminal_layout =
                        terminal_layout_service.reload_terminal_layout(Some(&terminal_storage_key));
                    let terminal = ProjectSwitchTerminalLoad {
                        project_id: task_project_id.clone(),
                        generation,
                        scope_key: WorktreeScopeKey {
                            project_id: task_project_id.clone(),
                            worktree_id: terminal_owner_id,
                        },
                        terminal_layout,
                        terminal_runtime: TerminalRuntimeSummary::default(),
                    };
                    codux_runtime::runtime_trace::runtime_trace(
                        "project-switch",
                        &format!(
                            "session_load worker_done project={} generation={} elapsed_ms={}",
                            task_project_id,
                            generation,
                            worker_started_at.elapsed().as_millis()
                        ),
                    );
                    (
                        ProjectSwitchTaskLoad {
                            project_id: task_project_id,
                            generation,
                            worktrees,
                        },
                        terminal,
                    )
                },
            )
            .await
            .map_err(|error| error.to_string());
            let _ = this.update_in(cx, |app, window, cx| {
                if app.project_switch_generation != generation {
                    return;
                }
                let result = match session_load {
                    Ok((catalog, terminal)) => {
                        let error = catalog
                            .worktrees
                            .error
                            .clone()
                            .or_else(|| terminal.terminal_layout.error.clone());
                        app.apply_project_switch_task_load(catalog, cx);
                        match error {
                            Some(error) => Err(error),
                            None => {
                                app.apply_project_switch_terminal_load(terminal, window, cx);
                                Ok(())
                            }
                        }
                    }
                    Err(error) => Err(error),
                };
                // Publication, not worker completion, releases navigation.
                if let Some(load) = &mut app.project_session_load {
                    load.published(generation, result);
                }
            });
        })
        .detach();

        let primary_queued_at = Instant::now();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let primary = codux_runtime::async_runtime::run_limited_blocking_with_priority(
                codux_runtime::async_runtime::BLOCKING_PRIORITY_FOREGROUND + generation,
                move || {
                    let worker_started_at = Instant::now();
                    codux_runtime::runtime_trace::runtime_trace(
                        "project-switch",
                        &format!(
                            "primary_load worker_start project={} generation={} queue_wait_ms={}",
                            primary_project.id,
                            generation,
                            primary_queued_at.elapsed().as_millis()
                        ),
                    );
                    let request =
                        ai_history_worktree_request(&primary_project, primary_worktree.as_ref());
                    let ai_history = primary_runtime_service
                        .indexed_project_ai_history_summary(request)
                        .ok()
                        .map(|state| {
                            ai_history_summary_from_state_or_status(
                                &AIHistorySummary::default(),
                                &state,
                            )
                        })
                        .unwrap_or_else(|| AIHistorySummary {
                            is_loading: true,
                            detail: "loading".to_string(),
                            ..AIHistorySummary::default()
                        });
                    let remote_ai_current_sessions = primary_runtime_service
                        .remote_ai_current_sessions(
                            &primary_project.path,
                            &primary_scope_key.worktree_id,
                            include_cached,
                        )
                        .and_then(Result::ok)
                        .unwrap_or_default();
                    codux_runtime::runtime_trace::runtime_trace(
                        "project-switch",
                        &format!(
                            "primary_load worker_done project={} generation={} elapsed_ms={}",
                            primary_project.id,
                            generation,
                            worker_started_at.elapsed().as_millis()
                        ),
                    );
                    ProjectSwitchPrimaryLoad {
                        project_id: primary_project.id,
                        generation,
                        scope_key: primary_scope_key,
                        ai_history,
                        remote_ai_current_sessions,
                    }
                },
            )
            .await
            .ok();
            let _ = this.update(cx, |app, cx| {
                if let Some(primary) = primary {
                    app.apply_project_switch_primary_load(primary, cx);
                }
            });
        })
        .detach();

        let stats_queued_at = Instant::now();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let load = codux_runtime::async_runtime::run_limited_blocking_with_priority(
                codux_runtime::async_runtime::BLOCKING_PRIORITY_FOREGROUND + generation,
                move || {
                    let worker_started_at = Instant::now();
                    codux_runtime::runtime_trace::runtime_trace(
                        "project-switch",
                        &format!(
                            "full_load worker_start project={} generation={} queue_wait_ms={}",
                            stats_project.id,
                            generation,
                            stats_queued_at.elapsed().as_millis()
                        ),
                    );
                    let memory = stats_runtime_service.reload_memory(Some(&stats_project.id));
                    let memory_manager = stats_runtime_service.reload_memory_manager(
                        &projects,
                        "project",
                        Some(&stats_project.id),
                        "active",
                    );
                    codux_runtime::runtime_trace::runtime_trace(
                        "project-switch",
                        &format!(
                            "full_load worker_done project={} generation={} elapsed_ms={}",
                            stats_project.id,
                            generation,
                            worker_started_at.elapsed().as_millis()
                        ),
                    );
                    ProjectSwitchLoad {
                        project_id: stats_project.id,
                        generation,
                        memory,
                        memory_manager,
                    }
                },
            )
            .await
            .ok();
            let _ = this.update(cx, |app, cx| {
                if let Some(load) = load {
                    app.apply_project_switch_load(load, cx);
                }
            });
        })
        .detach();
    }

    pub(in crate::app) fn apply_project_switch_terminal_load(
        &mut self,
        load: ProjectSwitchTerminalLoad,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .state
            .selected_project
            .as_ref()
            .map(|project| project.id.as_str())
            != Some(load.project_id.as_str())
            || self.project_switch_generation != load.generation
        {
            return;
        }
        let selected_terminal_key = current_worktree_scope_key(&self.state);
        let is_selected_terminal_owner = selected_terminal_key
            .as_ref()
            .is_some_and(|key| key == &load.scope_key);
        self.runtime_trace(
            "project-switch",
            &format!(
                "terminal_load apply project={} worktree={} generation={} selected={}",
                load.project_id,
                load.scope_key.worktree_id,
                load.generation,
                is_selected_terminal_owner
            ),
        );
        if !is_selected_terminal_owner {
            self.runtime_trace(
                "project-switch",
                &format!(
                    "terminal_load stale_scope project={} generation={}",
                    load.project_id, load.generation
                ),
            );
        } else {
            let navigation_needs_layout =
                self.session_navigation_needs_layout(&load.scope_key, &load.terminal_layout);
            if !navigation_needs_layout
                && self
                    .terminal_restored_generation
                    .as_ref()
                    .is_some_and(|token| token == &(load.generation, load.scope_key.clone()))
            {
                self.runtime_trace(
                    "project-switch",
                    &format!(
                        "terminal_load skip_already_restored project={} worktree={} generation={}",
                        load.project_id, load.scope_key.worktree_id, load.generation
                    ),
                );
                return;
            }
            if navigation_needs_layout {
                self.terminal_layout_cache.remove(&load.scope_key);
            }
            self.schedule_terminal_layout_restore(
                load.terminal_layout,
                load.terminal_runtime,
                load.generation,
                window,
                cx,
            );
            self.invalidate_terminal_workspace(cx);
        }
    }

    pub(in crate::app) fn apply_project_switch_task_load(
        &mut self,
        load: ProjectSwitchTaskLoad,
        cx: &mut Context<Self>,
    ) {
        if self
            .state
            .selected_project
            .as_ref()
            .map(|project| project.id.as_str())
            != Some(load.project_id.as_str())
            || self.project_switch_generation != load.generation
        {
            return;
        }
        self.state.worktrees = load.worktrees;
        self.reset_current_worktree_ui_state(cx);
        self.spawn_worktree_sidebar_load(load.generation, cx);
        self.runtime_trace(
            "project-switch",
            &format!(
                "task_load apply project={} generation={} worktrees={} tasks={}",
                load.project_id,
                load.generation,
                self.state.worktrees.worktrees.len(),
                self.state.worktrees.tasks.len()
            ),
        );
        self.invalidate_worktree_context(cx);
    }

    pub(in crate::app) fn apply_project_switch_primary_load(
        &mut self,
        load: ProjectSwitchPrimaryLoad,
        cx: &mut Context<Self>,
    ) {
        if self
            .state
            .selected_project
            .as_ref()
            .map(|project| project.id.as_str())
            != Some(load.project_id.as_str())
            || self.project_switch_generation != load.generation
            || current_worktree_scope_key(&self.state).as_ref() != Some(&load.scope_key)
        {
            return;
        }
        if merge_ai_history_summary(&mut self.state.ai_history, load.ai_history) {
            self.selected_ai_session_id = None;
            self.state.ai_session_detail = None;
        }
        self.state.remote_ai_current_sessions = load.remote_ai_current_sessions;
        self.state.refresh_ai_history_stats();
        self.runtime_trace(
            "project-switch",
            &format!(
                "primary_load apply project={} generation={} worktrees={} tasks={} sessions={}",
                load.project_id,
                load.generation,
                self.state.worktrees.worktrees.len(),
                self.state.worktrees.tasks.len(),
                self.state.ai_history.sessions.len()
            ),
        );
        self.normalize_selected_ai_session();
        self.invalidate_ui(
            cx,
            [
                UiRegion::TaskColumn,
                UiRegion::AIStatsSidebar,
                UiRegion::StatusBar,
            ],
        );
    }

    pub(in crate::app) fn apply_project_switch_load(
        &mut self,
        load: ProjectSwitchLoad,
        cx: &mut Context<Self>,
    ) {
        if self
            .state
            .selected_project
            .as_ref()
            .map(|project| project.id.as_str())
            != Some(load.project_id.as_str())
            || self.project_switch_generation != load.generation
        {
            return;
        }
        self.state.memory = load.memory;
        self.state.memory_manager = load.memory_manager;
        self.runtime_trace(
            "project-switch",
            &format!(
                "full_load apply project={} generation={} worktrees={} tasks={}",
                load.project_id,
                load.generation,
                self.state.worktrees.worktrees.len(),
                self.state.worktrees.tasks.len()
            ),
        );
        self.invalidate_ui(
            cx,
            [
                UiRegion::WorkspaceAssistant,
                UiRegion::AIStatsSidebar,
                UiRegion::StatusBar,
            ],
        );
    }

    pub(in crate::app) fn merge_selected_project_worktrees(&mut self, worktrees: WorktreeSummary) {
        if worktree_summary_has_rows(&worktrees)
            || !worktree_summary_has_rows(&self.state.worktrees)
        {
            self.state.worktrees = worktrees;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_overview_waits_for_project_publication_not_cached_layout() {
        let mut load = ProjectSessionLoad::new(1);
        // The cached skeleton may already be mounted. Only the owner's single
        // callback publishing both fresh results can release navigation.
        assert_eq!(load.readiness(1), Ok(false));
        load.published(1, Ok(()));
        assert_eq!(load.readiness(1), Ok(true));
    }

    #[test]
    fn session_overview_project_a_b_a_ignores_superseded_completions() {
        let mut load = ProjectSessionLoad::new(1);
        assert_eq!(load.readiness(1), Ok(false));
        load = ProjectSessionLoad::new(2);
        load.published(1, Ok(()));
        assert_eq!(load.readiness(2), Ok(false));
        load = ProjectSessionLoad::new(3);
        load.published(1, Err("old A failed".into()));
        load.published(2, Ok(()));
        assert_eq!(load.readiness(3), Ok(false));
        load.published(3, Ok(()));
        load.published(2, Err("old B failed".into()));
        assert_eq!(load.readiness(3), Ok(true));
    }

    #[test]
    fn session_overview_project_load_failure_remains_visible_until_new_load() {
        let mut load = ProjectSessionLoad::new(4);
        load.published(4, Err("remote directory unavailable".into()));
        assert_eq!(load.readiness(4), Err("remote directory unavailable"));
        load.published(3, Ok(()));
        assert_eq!(load.readiness(4), Err("remote directory unavailable"));
        load = ProjectSessionLoad::new(5);
        assert_eq!(load.readiness(5), Ok(false));
        load.published(5, Err("terminal layout unavailable".into()));
        assert_eq!(load.readiness(5), Err("terminal layout unavailable"));
    }

    #[test]
    fn session_overview_superseded_project_load_does_not_block_worktree_load() {
        let load = ProjectSessionLoad::new(1);
        // A worktree selection owns its newer generation and uses the existing
        // terminal-layout loading/attach checks, not the old project worker.
        assert_eq!(load.readiness(2), Ok(true));
    }
}
