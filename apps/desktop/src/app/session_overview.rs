use super::agent_display::agent_lifecycle_status_dot;
use super::ai_runtime_status::AgentLifecycleState;
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionTarget {
    device: Option<String>,
    terminal: String,
    instance: Option<String>,
    project: String,
    worktree: Option<String>,
}

impl SessionTarget {
    fn in_layout(&self, layout: &TerminalLayoutSummary) -> bool {
        layout
            .top_panes
            .iter()
            .chain(&layout.collapsed_panes)
            .map(|pane| pane.terminal_id.as_str())
            .chain(layout.tabs.iter().map(|tab| tab.terminal_id.as_str()))
            .any(|id| id == self.terminal)
    }

    fn needs_layout<'a>(
        &self,
        scope: &WorktreeScopeKey,
        layout: &TerminalLayoutSummary,
        mut shown: impl Iterator<Item = &'a str>,
    ) -> bool {
        self.project == scope.project_id
            && self.worktree.as_deref().unwrap_or(&self.project) == scope.worktree_id
            && self.in_layout(layout)
            && !shown.any(|id| id == self.terminal)
    }

    fn matches(&self, entry: &codux_runtime::ai_runtime::AgentActivityRecord) -> bool {
        self.terminal == entry.terminal_id
            && self.instance == entry.terminal_instance_id
            && self.project == entry.project_id
            && self.worktree == entry.worktree_id
            && entry.state != AgentLifecycleState::Absent
    }
}

pub(in crate::app) struct SessionNavigation {
    target: SessionTarget,
    window: AnyWindowHandle,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionRow {
    target: SessionTarget,
    title: String,
    subtitle: String,
    state: AgentLifecycleState,
    output: Option<String>,
    available: bool,
}

fn attention_rank(state: AgentLifecycleState) -> u8 {
    match state {
        AgentLifecycleState::NeedsInput => 0,
        AgentLifecycleState::Idle => 1,
        AgentLifecycleState::Unknown => 2,
        AgentLifecycleState::Working => 3,
        AgentLifecycleState::Absent => 4,
    }
}

fn reconcile_rows(
    mut current: Vec<SessionRow>,
    previous: &[SessionRow],
    known: &HashSet<Option<String>>,
    projects: &[ProjectInfo],
) -> Vec<SessionRow> {
    // Keep only previously observed Agents when their source is unavailable.
    // Cached output is deliberately removed instead of presented as live text.
    for row in previous {
        if !known.contains(&row.target.device)
            && projects.iter().any(|project| {
                project.id == row.target.project
                    && project.remote_device_id() == row.target.device.as_deref()
            })
        {
            let mut row = row.clone();
            row.state = AgentLifecycleState::Unknown;
            row.output = None;
            row.available = false;
            current.push(row);
        }
    }
    current.sort_by(|a, b| {
        attention_rank(a.state)
            .cmp(&attention_rank(b.state))
            .then_with(|| a.target.project.cmp(&b.target.project))
            .then_with(|| a.target.device.cmp(&b.target.device))
            .then_with(|| a.target.terminal.cmp(&b.target.terminal))
    });
    current
}

pub(in crate::app) struct SessionOverview {
    rows: Vec<SessionRow>,
    parent: gpui::WeakEntity<CoduxApp>,
    main_window: AnyWindowHandle,
    chinese: bool,
}

impl Render for SessionOverview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let chinese = self.chinese;
        let mut background = color(theme::BG_COLUMN);
        background.a = 0.94;
        div()
            .size_full()
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .bg(background)
            .border_1()
            .border_color(color(theme::BORDER))
            .text_color(color(theme::TEXT))
            .text_size(px(13.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .h(px(38.0))
                    .child(super::ui_helpers::titlebar_drag_area(
                        "session-overview-drag",
                        div()
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .child(if chinese {
                                "Agent 会话总览"
                            } else {
                                "Agent sessions"
                            }),
                    ))
                    .child(
                        div()
                            .id("session-overview-close")
                            .px_2()
                            .cursor_pointer()
                            .child("×")
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_size(px(11.0))
                    .text_color(color(theme::TEXT_MUTED))
                    .child(if chinese {
                        "最近终端输出 · 点击定位会话"
                    } else {
                        "Recent terminal output · Click to open session"
                    }),
            )
            .child(
                div()
                    .id("session-overview-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(self.rows.is_empty(), |view| {
                        view.child(div().p_3().child(if chinese {
                            "暂无已识别的 Agent 会话"
                        } else {
                            "No identified Agent sessions"
                        }))
                    })
                    .children(self.rows.iter().enumerate().map(|(index, row)| {
                        let target = row.target.clone();
                        let parent = self.parent.clone();
                        let main_window = self.main_window;
                        let available = row.available;
                        let state = match row.state {
                            AgentLifecycleState::NeedsInput => {
                                if chinese {
                                    "需要输入"
                                } else {
                                    "Needs input"
                                }
                            }
                            AgentLifecycleState::Working => {
                                if chinese {
                                    "工作中"
                                } else {
                                    "Working"
                                }
                            }
                            AgentLifecycleState::Idle => {
                                if chinese {
                                    "空闲"
                                } else {
                                    "Idle"
                                }
                            }
                            _ => {
                                if chinese {
                                    "状态未知"
                                } else {
                                    "Unknown"
                                }
                            }
                        };
                        div()
                            .id(("session-overview-row", index))
                            .p_3()
                            .border_t_1()
                            .border_color(color(theme::BORDER_SOFT))
                            .when(row.state == AgentLifecycleState::NeedsInput, |view| {
                                view.border_l_4().border_color(color(0xB45309))
                            })
                            .when(available, |view| {
                                view.cursor_pointer()
                                    .hover(|style| style.bg(color(theme::BG_PANEL)))
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(agent_lifecycle_status_dot(row.state))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::BOLD)
                                            .child(row.title.clone()),
                                    )
                                    .child(div().text_size(px(12.0)).child(state)),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(px(11.0))
                                    .truncate()
                                    .text_color(color(theme::TEXT_MUTED))
                                    .child(row.subtitle.clone()),
                            )
                            .child(div().mt_2().whitespace_normal().line_clamp(3).child(
                                row.output.clone().unwrap_or_else(|| {
                                    if !available {
                                        if chinese {
                                            "来源不可用，请等待重新连接"
                                        } else {
                                            "Source unavailable; waiting for reconnection"
                                        }
                                    } else if chinese {
                                        "暂无最近输出"
                                    } else {
                                        "No recent output"
                                    }
                                    .to_string()
                                }),
                            ))
                            .on_click(move |_, _, cx| {
                                if !available {
                                    return;
                                }
                                let target = target.clone();
                                let _ = main_window.update(cx, |_, window, cx| {
                                    let _ = parent.update(cx, |app, cx| {
                                        app.session_navigation = Some(SessionNavigation {
                                            target,
                                            window: main_window,
                                            generation: app.project_switch_generation,
                                        });
                                        app.reconcile_session_navigation(window, cx);
                                    });
                                });
                            })
                    })),
            )
    }
}

impl CoduxApp {
    fn session_target_is_current(&self, target: &SessionTarget) -> bool {
        let current = match &target.device {
            None => self.runtime_service.agent_activity_snapshot().ok(),
            Some(device) => self
                .runtime_service
                .remote_agent_activity_snapshots()
                .into_iter()
                .find(|(id, _)| id == device)
                .and_then(|(_, snapshot)| snapshot),
        };
        current.is_some_and(|snapshot| {
            snapshot
                .terminals
                .iter()
                .any(|record| target.matches(record))
        })
    }

    pub(in crate::app) fn session_navigation_needs_layout(
        &self,
        scope: &WorktreeScopeKey,
        layout: &TerminalLayoutSummary,
    ) -> bool {
        let Some(navigation) = &self.session_navigation else {
            return false;
        };
        navigation.generation == self.project_switch_generation
            && self.session_target_is_current(&navigation.target)
            && navigation.target.needs_layout(
                scope,
                layout,
                self.terminals
                    .iter()
                    .flat_map(|tab| &tab.panes)
                    .chain(&self.collapsed_terminal_panes)
                    .filter_map(|slot| slot.terminal_id.as_deref()),
            )
    }

    fn session_overview_rows(&self) -> Vec<SessionRow> {
        self.pane_agent_lifecycle
            .iter()
            .filter_map(|((device, terminal), entry)| {
                // An unknown plain shell is not evidence of an Agent session.
                if entry.state == AgentLifecycleState::Unknown && entry.tool.is_none() {
                    return None;
                }
                let project = self.state.projects.iter().find(|project| {
                    project.id == entry.project_id
                        && project.remote_device_id() == device.as_deref()
                })?;
                let device_label = device
                    .as_ref()
                    .map(|id| {
                        self.state
                            .remote
                            .device_list
                            .iter()
                            .find(|device| &device.id == id)
                            .map(|device| device.name.as_str())
                            .unwrap_or(id.as_str())
                    })
                    .unwrap_or(
                        if locale_from_language_setting(&self.state.settings.language)
                            .starts_with("zh")
                        {
                            "本机"
                        } else {
                            "Local"
                        },
                    );
                Some(SessionRow {
                    target: SessionTarget {
                        device: device.clone(),
                        terminal: terminal.clone(),
                        instance: entry.terminal_instance_id.clone(),
                        project: entry.project_id.clone(),
                        worktree: entry.worktree_id.clone(),
                    },
                    title: format!("{} · {}", project.name, device_label),
                    subtitle: format!(
                        "{} · {}",
                        entry.tool.as_deref().unwrap_or("Agent"),
                        terminal
                    ),
                    state: entry.state,
                    output: entry.recent_output.clone(),
                    available: true,
                })
            })
            .collect()
    }

    pub(in crate::app) fn toggle_session_overview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.session_overview_window.take()
            && handle
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
        {
            return;
        }
        let rows = reconcile_rows(
            self.session_overview_rows(),
            &[],
            &self.agent_activity_known_sources,
            &self.state.projects,
        );
        let parent = cx.entity().downgrade();
        let main_window = window.window_handle();
        let chinese = locale_from_language_setting(&self.state.settings.language).starts_with("zh");
        let bounds = Bounds::centered(None, size(px(360.0), px(520.0)), cx);
        let result = cx.open_window(
            WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(320.0), px(220.0))),
                kind: WindowKind::PopUp,
                focus: false,
                is_resizable: true,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| SessionOverview {
                    rows,
                    parent,
                    main_window,
                    chinese,
                })
            },
        );
        match result {
            Ok(handle) => self.session_overview_window = Some(handle),
            Err(error) => self.show_toast(format!("Failed to open Agent sessions: {error}"), cx),
        }
    }

    pub(in crate::app) fn close_session_overview(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.session_overview_window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.session_navigation = None;
    }

    pub(in crate::app) fn publish_session_overview(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.session_overview_window else {
            return;
        };
        let rows = self.session_overview_rows();
        let chinese = locale_from_language_setting(&self.state.settings.language).starts_with("zh");
        if handle
            .update(cx, |view, _, cx| {
                let rows = reconcile_rows(
                    rows,
                    &view.rows,
                    &self.agent_activity_known_sources,
                    &self.state.projects,
                );
                if view.rows != rows || view.chinese != chinese {
                    view.rows = rows;
                    view.chinese = chinese;
                    cx.notify();
                }
            })
            .is_err()
        {
            self.session_overview_window = None;
        }
    }

    pub(in crate::app) fn schedule_session_navigation(&self, cx: &mut Context<Self>) {
        let Some(navigation) = &self.session_navigation else {
            return;
        };
        let handle = navigation.window;
        let app = cx.entity().downgrade();
        // Use the existing main tick; deferred closures read current intent,
        // never a copied target from an earlier click.
        cx.defer(move |cx| {
            let result = handle.update(cx, |_, window, cx| {
                let _ = app.update(cx, |app, cx| app.reconcile_session_navigation(window, cx));
            });
            if result.is_err() {
                let _ = app.update(cx, |app, _| app.session_navigation = None);
            }
        });
    }

    fn reconcile_session_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut navigation) = self.session_navigation.take() else {
            return;
        };
        let target = &navigation.target;
        if navigation.generation != self.project_switch_generation {
            return;
        }
        // Recheck the authoritative current connection/instance at event time;
        // a row displayed by the previous UI tick cannot authorize navigation.
        if !self.session_target_is_current(target) {
            self.show_toast(
                if locale_from_language_setting(&self.state.settings.language).starts_with("zh") {
                    "会话已退出、替换或连接不可用"
                } else {
                    "Session ended, replaced, or disconnected"
                }
                .to_string(),
                cx,
            );
            return;
        }
        if self
            .state
            .selected_project
            .as_ref()
            .map(|project| project.id.as_str())
            != Some(target.project.as_str())
        {
            self.select_project(target.project.clone(), window, cx);
            navigation.generation = self.project_switch_generation;
            self.session_navigation = Some(navigation);
            return;
        }
        match self
            .project_session_load
            .as_ref()
            .map(|load| load.readiness(self.project_switch_generation))
            .unwrap_or(Ok(true))
        {
            Ok(false) => {
                self.session_navigation = Some(navigation);
                return;
            }
            Err(error) => {
                let message = if locale_from_language_setting(&self.state.settings.language)
                    .starts_with("zh")
                {
                    format!("会话定位失败：{error}")
                } else {
                    format!("Session navigation failed: {error}")
                };
                self.show_toast(message, cx);
                return;
            }
            Ok(true) => {}
        }
        let owner = target.worktree.as_deref().unwrap_or(&target.project);
        if super::ai_runtime_status::terminal_layout_owner_id(&self.state).as_deref() != Some(owner)
        {
            if self
                .state
                .worktrees
                .worktrees
                .iter()
                .any(|worktree| worktree.id == owner)
            {
                // A cached layout that predates this live target must not
                // override the selected worktree's freshly loaded layout.
                let scope = WorktreeScopeKey {
                    project_id: target.project.clone(),
                    worktree_id: owner.to_string(),
                };
                if self
                    .terminal_layout_cache
                    .get(&scope)
                    .is_some_and(|cache| !target.in_layout(&cache.layout))
                {
                    self.terminal_layout_cache.remove(&scope);
                }
                self.select_worktree(owner.to_string(), window, cx);
                navigation.generation = self.project_switch_generation;
                self.session_navigation = Some(navigation);
            } else {
                self.show_toast(
                    if locale_from_language_setting(&self.state.settings.language).starts_with("zh") {
                        "无法定位会话工作区，请刷新项目后重试"
                    } else {
                        "Session worktree unavailable; refresh project and retry"
                    }
                    .to_string(),
                    cx,
                );
            }
            return;
        }
        if self.terminal_layout_loading {
            self.session_navigation = Some(navigation);
            return;
        }
        if let Some(index) = self
            .collapsed_terminal_panes
            .iter()
            .position(|slot| slot.terminal_id.as_deref() == Some(&target.terminal))
        {
            if self.collapsed_terminal_panes[index]
                .pane
                .as_ref()
                .is_some_and(|pane| {
                    target.instance.is_some() && pane.terminal_instance_id() != target.instance
                })
            {
                return;
            }
            self.restore_collapsed_terminal(index, window, cx);
        }
        let position = self
            .terminals
            .iter()
            .enumerate()
            .find_map(|(tab, terminal)| {
                terminal
                    .panes
                    .iter()
                    .enumerate()
                    .find(|(_, slot)| slot.terminal_id.as_deref() == Some(&target.terminal))
                    .map(|(pane, _)| (tab, pane))
            });
        let Some((tab, pane)) = position else {
            self.show_toast(
                if locale_from_language_setting(&self.state.settings.language).starts_with("zh") {
                    "会话不在当前终端布局中"
                } else {
                    "Session is not in current terminal layout"
                }
                .to_string(),
                cx,
            );
            return;
        };
        if let Err(error) = self.ensure_terminal_slot_mounted(tab, pane, cx) {
            self.show_toast(error, cx);
            return;
        }
        if self.terminal_attach_in_flight.contains(&target.terminal) {
            self.session_navigation = Some(navigation);
            return;
        }
        let Some(mounted) = self.terminals[tab].panes[pane].pane.as_ref() else {
            return;
        };
        if target.instance.is_some() && mounted.terminal_instance_id() != target.instance {
            return;
        }
        self.workspace_view = WorkspaceView::Terminal;
        self.active_terminal_id = self.terminals[tab].id;
        self.select_active_terminal_runtime_id(Some(&target.terminal));
        window.activate_window();
        if self.focus_active_terminal(window, cx) {
            self.invalidate_terminal_workspace(cx);
        } else {
            self.session_navigation = Some(navigation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(terminal: &str, state: AgentLifecycleState) -> SessionRow {
        SessionRow {
            target: SessionTarget {
                device: None,
                terminal: terminal.into(),
                instance: Some("instance".into()),
                project: "project".into(),
                worktree: None,
            },
            title: "Project · Local".into(),
            subtitle: terminal.into(),
            state,
            output: Some("previous output".into()),
            available: true,
        }
    }

    fn project() -> ProjectInfo {
        ProjectInfo {
            id: "project".into(),
            name: "Project".into(),
            path: "/project".into(),
            exists: true,
            badge: String::new(),
            badge_symbol: None,
            badge_color_hex: None,
            git_default_push_remote_name: None,
            environment_variables: Default::default(),
            runtime_target: ProjectRuntimeTarget::Local,
        }
    }

    #[test]
    fn session_overview_prioritizes_attention_and_keeps_text_changes_from_reordering_rows() {
        use AgentLifecycleState::*;
        let rows = vec![
            row("working", Working),
            row("idle-b", Idle),
            row("question", NeedsInput),
            row("unknown", Unknown),
            row("idle-a", Idle),
        ];
        let known = HashSet::from([None]);
        let ordered = reconcile_rows(rows, &[], &known, &[project()]);
        assert_eq!(
            ordered
                .iter()
                .map(|r| r.target.terminal.as_str())
                .collect::<Vec<_>>(),
            vec!["question", "idle-a", "idle-b", "unknown", "working"]
        );
        let mut changed = ordered.clone();
        changed[2].output = Some("new output".into());
        changed.reverse();
        let changed = reconcile_rows(changed, &ordered, &known, &[project()]);
        assert_eq!(
            changed.iter().map(|r| &r.target).collect::<Vec<_>>(),
            ordered.iter().map(|r| &r.target).collect::<Vec<_>>()
        );
        assert_eq!(changed[2].output.as_deref(), Some("new output"));
    }

    #[test]
    fn session_overview_disconnection_clears_live_text_and_reconnect_removes_closed_sessions() {
        let previous = vec![row("terminal", AgentLifecycleState::NeedsInput)];
        let disconnected = reconcile_rows(vec![], &previous, &HashSet::new(), &[project()]);
        assert_eq!(disconnected.len(), 1);
        assert_eq!(disconnected[0].state, AgentLifecycleState::Unknown);
        assert_eq!(disconnected[0].output, None);
        assert!(!disconnected[0].available);
        assert!(
            reconcile_rows(vec![], &disconnected, &HashSet::from([None]), &[project()]).is_empty()
        );
        assert!(reconcile_rows(vec![], &previous, &HashSet::new(), &[]).is_empty());
        let live = row("terminal", AgentLifecycleState::Working);
        assert_eq!(
            reconcile_rows(
                vec![live.clone()],
                &disconnected,
                &HashSet::from([None]),
                &[project()]
            ),
            vec![live]
        );
    }

    #[test]
    fn session_overview_navigation_rejects_replaced_instance_and_wrong_scope() {
        let target = row("terminal", AgentLifecycleState::Idle).target;
        let mut record = codux_runtime::ai_runtime::AgentActivityRecord {
            terminal_id: target.terminal.clone(),
            terminal_instance_id: target.instance.clone(),
            project_id: target.project.clone(),
            worktree_id: target.worktree.clone(),
            state: AgentLifecycleState::Idle,
            tool: None,
            recent_output: None,
        };
        assert!(target.matches(&record));
        record.terminal_instance_id = Some("replacement".into());
        assert!(!target.matches(&record));
        record.terminal_instance_id = target.instance.clone();
        record.worktree_id = Some("other-worktree".into());
        assert!(!target.matches(&record));
        record.worktree_id = target.worktree.clone();
        record.state = AgentLifecycleState::Absent;
        assert!(!target.matches(&record));
    }

    #[test]
    fn session_overview_fresh_layout_can_reveal_target_missing_from_cached_skeleton() {
        let target = row("terminal", AgentLifecycleState::Idle).target;
        let scope = WorktreeScopeKey {
            project_id: target.project.clone(),
            worktree_id: target.project.clone(),
        };
        let mut fresh = TerminalLayoutSummary::default();
        fresh.top_panes.push(TerminalPaneSummary {
            terminal_id: target.terminal.clone(),
            title: "Agent".into(),
        });
        assert!(target.needs_layout(&scope, &fresh, ["old-terminal"].into_iter()));
        assert!(!target.needs_layout(&scope, &fresh, ["terminal"].into_iter()));
        assert!(!target.needs_layout(
            &scope,
            &TerminalLayoutSummary::default(),
            ["old-terminal"].into_iter()
        ));
        let other = WorktreeScopeKey {
            project_id: "other-project".into(),
            worktree_id: "other-project".into(),
        };
        assert!(!target.needs_layout(&other, &fresh, ["old-terminal"].into_iter()));
    }

    #[test]
    fn session_overview_target_worktree_cache_must_contain_live_target() {
        let mut target = row("terminal", AgentLifecycleState::Idle).target;
        target.worktree = Some("branch".into());
        let scope = WorktreeScopeKey {
            project_id: target.project.clone(),
            worktree_id: "branch".into(),
        };
        let mut layout = TerminalLayoutSummary::default();
        layout.collapsed_panes.push(TerminalPaneSummary {
            terminal_id: target.terminal.clone(),
            title: "Agent".into(),
        });
        assert!(target.in_layout(&layout));
        assert!(target.needs_layout(&scope, &layout, std::iter::empty()));
        assert!(!target.in_layout(&TerminalLayoutSummary::default()));
    }
}
