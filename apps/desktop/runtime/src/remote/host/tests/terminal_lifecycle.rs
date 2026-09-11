use super::*;

#[test]
fn remote_terminal_plan_uses_device_project_scope_without_desktop_ui_selection() {
    let support_dir = temp_support_dir("codux-remote-scope-terminal");
    write_two_project_state(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    runtime.set_remote_project_scope(Some("device-1"), "project-b");
    let layout_key = terminal_layout_storage_key("project-b", "worktree-b");
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &layout_key,
            Vec::new(),
            vec![TerminalPaneSummary {
                title: "Mobile".to_string(),
                terminal_id: "terminal-b".to_string(),
            }],
            vec![1.0],
            0.24,
        )
        .expect("save layout");

    let plan = runtime
        .remote_terminal_plan_from_envelope(
            &RemoteEnvelope {
                kind: "terminal.buffer".to_string(),
                device_id: Some("device-1".to_string()),
                session_id: Some("terminal-b".to_string()),
                request_id: None,
                seq: None,
                payload: json!({}),
            },
            None,
            true,
        )
        .expect("terminal plan");

    assert_eq!(plan.scope.project_id, "project-b");
    assert_eq!(plan.scope.worktree_id, "worktree-b");
    assert_eq!(plan.config.project_id.as_deref(), Some("worktree-b"));
    assert_eq!(
        plan.config.session_key.as_deref(),
        Some("gpui:worktree-b:terminal-b")
    );
    assert_eq!(plan.config.terminal_id.as_deref(), Some("terminal-b"));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_list_indexes_all_project_worktree_layouts() {
    let support_dir = temp_support_dir("codux-remote-terminal-all-worktrees");
    write_two_project_state(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    );
    let default_session = terminals
        .create(
            TerminalPtyConfig {
                command: Some("printf default".to_string()),
                project_id: Some("project-b".to_string()),
                terminal_id: Some("terminal-default".to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create default terminal");
    let worktree_session = terminals
        .create(
            TerminalPtyConfig {
                command: Some("printf worktree".to_string()),
                project_id: Some("project-b".to_string()),
                terminal_id: Some("terminal-worktree".to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create worktree terminal");
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &terminal_layout_storage_key("project-b", "project-b"),
            Vec::new(),
            vec![TerminalPaneSummary {
                title: "Default".to_string(),
                terminal_id: default_session.clone(),
            }],
            vec![1.0],
            0.24,
        )
        .expect("save default layout");
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &terminal_layout_storage_key("project-b", "worktree-b"),
            Vec::new(),
            vec![TerminalPaneSummary {
                title: "Worktree".to_string(),
                terminal_id: worktree_session.clone(),
            }],
            vec![1.0],
            0.24,
        )
        .expect("save worktree layout");

    let terminal_worktrees = runtime
        .remote_terminals()
        .into_iter()
        .filter_map(|terminal| {
            Some((
                terminal.get("id")?.as_str()?.to_string(),
                terminal.get("worktreeId")?.as_str()?.to_string(),
            ))
        })
        .collect::<HashMap<_, _>>();

    assert_eq!(
        terminal_worktrees.get(&default_session).map(String::as_str),
        Some("project-b")
    );
    assert_eq!(
        terminal_worktrees
            .get(&worktree_session)
            .map(String::as_str),
        Some("worktree-b")
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_list_reports_all_worktree_splits_under_root_project() {
    let support_dir = temp_support_dir("codux-remote-terminal-worktree-splits");
    write_two_project_state(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    );
    let sessions = (0..3)
        .map(|index| {
            terminals
                .create(
                    TerminalPtyConfig {
                        command: Some(format!("printf split-{index}")),
                        project_id: Some("worktree-b".to_string()),
                        terminal_id: Some(format!("terminal-worktree-{index}")),
                        ..Default::default()
                    },
                    |_| {},
                )
                .expect("create worktree terminal")
        })
        .collect::<Vec<_>>();
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &terminal_layout_storage_key("project-b", "project-b"),
            Vec::new(),
            vec![TerminalPaneSummary {
                title: "Stale".to_string(),
                terminal_id: sessions[0].clone(),
            }],
            vec![1.0],
            0.24,
        )
        .expect("save stale default layout");
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &terminal_layout_storage_key("project-b", "worktree-b"),
            Vec::new(),
            sessions
                .iter()
                .enumerate()
                .map(|(index, session)| TerminalPaneSummary {
                    title: format!("Split {}", index + 1),
                    terminal_id: session.clone(),
                })
                .collect(),
            vec![0.33, 0.34, 0.33],
            0.24,
        )
        .expect("save worktree split layout");

    let mut worktree_terminals = runtime
        .remote_terminals()
        .into_iter()
        .filter(|terminal| terminal.get("projectId").and_then(Value::as_str) == Some("project-b"))
        .filter(|terminal| terminal.get("worktreeId").and_then(Value::as_str) == Some("worktree-b"))
        .collect::<Vec<_>>();
    worktree_terminals.sort_by_key(|terminal| {
        terminal
            .get("layoutOrder")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    });

    assert_eq!(worktree_terminals.len(), 3);
    assert_eq!(
        worktree_terminals
            .iter()
            .filter_map(|terminal| terminal.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        sessions.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(
        worktree_terminals
            .iter()
            .filter_map(|terminal| terminal.get("layoutOrder").and_then(Value::as_u64))
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_create_plan_does_not_reuse_saved_layout_terminal() {
    let support_dir = temp_support_dir("codux-remote-create-new-terminal");
    write_two_project_state(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    runtime.set_remote_project_scope(Some("device-1"), "project-b");
    let layout_key = terminal_layout_storage_key("project-b", "worktree-b");
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &layout_key,
            Vec::new(),
            vec![TerminalPaneSummary {
                title: "Mobile".to_string(),
                terminal_id: "terminal-b".to_string(),
            }],
            vec![1.0],
            0.24,
        )
        .expect("save layout");

    let create_plan = runtime
        .remote_terminal_plan_from_envelope(
            &RemoteEnvelope {
                kind: "terminal.create".to_string(),
                device_id: Some("device-1".to_string()),
                session_id: None,
                request_id: None,
                seq: None,
                payload: json!({}),
            },
            None,
            false,
        )
        .expect("create terminal plan");
    let create_terminal_id = create_plan
        .config
        .terminal_id
        .as_deref()
        .expect("create plan terminal id");
    assert!(create_terminal_id.starts_with("gpui-term-worktree-b-"));
    assert_ne!(create_terminal_id, "terminal-b");
    assert_eq!(create_plan.config.project_id.as_deref(), Some("worktree-b"));
    let expected_worktree_path = support_dir.join("project-b");
    let expected_worktree_path = expected_worktree_path.to_string_lossy();
    assert_eq!(
        create_plan.config.cwd.as_deref(),
        Some(expected_worktree_path.as_ref())
    );

    let restore_plan = runtime
        .remote_terminal_plan_from_envelope(
            &RemoteEnvelope {
                kind: "terminal.buffer".to_string(),
                device_id: Some("device-1".to_string()),
                session_id: None,
                request_id: None,
                seq: None,
                payload: json!({}),
            },
            None,
            true,
        )
        .expect("restore terminal plan");
    assert_eq!(
        restore_plan.config.terminal_id.as_deref(),
        Some("terminal-b")
    );
    assert_eq!(
        restore_plan.config.project_id.as_deref(),
        Some("worktree-b")
    );
    assert_eq!(
        restore_plan.config.session_key.as_deref(),
        Some("gpui:worktree-b:terminal-b")
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_layout_is_persisted_to_project_worktree_scope() {
    let support_dir = temp_support_dir("codux-remote-layout-persist");
    write_two_project_state(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let layout_key = terminal_layout_storage_key("project-b", "worktree-b");

    runtime.persist_remote_terminal_layout(&layout_key, "terminal-mobile-b", "Mobile");

    let layout = TerminalLayoutService::new(support_dir.clone()).load(Some(&layout_key));
    assert_eq!(layout.active_terminal_id, "");
    assert_eq!(layout.top_panes.len(), 1);
    assert_eq!(layout.top_panes[0].terminal_id, "terminal-mobile-b");

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_create_plan_carries_project_env() {
    let support_dir = temp_support_dir("codux-remote-project-env");
    write_two_project_state(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    runtime.set_remote_project_scope(Some("device-1"), "project-b");

    let plan = runtime
        .remote_terminal_plan_from_envelope(
            &RemoteEnvelope {
                kind: "terminal.create".to_string(),
                device_id: Some("device-1".to_string()),
                session_id: None,
                request_id: None,
                seq: None,
                payload: json!({
                    "projectEnv": {
                        "API_BASE": "https://example.test",
                        "EMPTY": 42
                    }
                }),
            },
            None,
            false,
        )
        .expect("terminal plan");

    assert_eq!(
        plan.config
            .project_env
            .as_ref()
            .and_then(|env| env.get("API_BASE"))
            .map(String::as_str),
        Some("https://example.test")
    );
    assert!(
        plan.config
            .project_env
            .as_ref()
            .is_none_or(|env| !env.contains_key("EMPTY"))
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_create_emits_layout_changed_event() {
    let support_dir = temp_support_dir("codux-remote-create-layout-event");
    write_two_project_state(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    runtime.set_remote_project_scope(Some("device-1"), "project-b");
    runtime.drain_events();

    runtime.handle_terminal_create(&RemoteEnvelope {
        kind: "terminal.create".to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: None,
        seq: None,
        payload: json!({
            "projectId": "project-b",
            "worktreeId": "worktree-b",
        }),
    });

    let layout_key = terminal_layout_storage_key("project-b", "worktree-b");
    let layout = TerminalLayoutService::new(support_dir.clone()).load(Some(&layout_key));
    // First terminal in an empty scope seeds the main split.
    assert_eq!(layout.top_panes.len(), 1);
    assert!(layout.tabs.is_empty());
    assert!(
        runtime
            .drain_events()
            .iter()
            .any(|event| matches!(event, RemoteHostEvent::TerminalLayoutChanged(_)))
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_close_clears_layout_when_last_terminal_is_removed() {
    let support_dir = temp_support_dir("codux-remote-close-last-layout");
    write_two_project_state(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let layout_key = terminal_layout_storage_key("project-b", "worktree-b");
    // The controller owns the only terminal in this worktree layout. The PTY is
    // already gone (killed elsewhere), so no shell is spawned in this test.
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &layout_key,
            Vec::new(),
            vec![TerminalPaneSummary {
                title: "Only".to_string(),
                terminal_id: "terminal-only".to_string(),
            }],
            vec![1.0],
            0.24,
        )
        .expect("save layout");
    runtime.drain_events();

    runtime.handle_terminal_close(&RemoteEnvelope {
        kind: "terminal.close".to_string(),
        device_id: Some("device-1".to_string()),
        session_id: Some("terminal-only".to_string()),
        request_id: None,
        seq: None,
        payload: json!({ "projectId": "project-b", "worktreeId": "worktree-b" }),
    });

    let layout = TerminalLayoutService::new(support_dir.clone()).load(Some(&layout_key));
    assert!(
        layout.top_panes.is_empty(),
        "persisted layout still lists the closed terminal: {:?}",
        layout.top_panes
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_close_removes_layout_entry_and_kills_last_terminal() {
    let support_dir = temp_support_dir("codux-remote-close-layout-entry");
    write_two_project_state(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let layout_key = terminal_layout_storage_key("project-b", "worktree-b");
    let session_a = terminals
        .create(
            TerminalPtyConfig {
                command: Some("printf a".to_string()),
                project_id: Some("worktree-b".to_string()),
                terminal_id: Some("terminal-a".to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal a");
    let session_b = terminals
        .create(
            TerminalPtyConfig {
                command: Some("printf b".to_string()),
                project_id: Some("worktree-b".to_string()),
                terminal_id: Some("terminal-b".to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal b");
    TerminalLayoutService::new(support_dir.clone())
        .save_from_gpui(
            &layout_key,
            Vec::new(),
            vec![
                TerminalPaneSummary {
                    title: "A".to_string(),
                    terminal_id: session_a.clone(),
                },
                TerminalPaneSummary {
                    title: "B".to_string(),
                    terminal_id: session_b.clone(),
                },
            ],
            vec![0.5, 0.5],
            0.24,
        )
        .expect("save layout");
    runtime.drain_events();

    runtime.handle_terminal_close(&RemoteEnvelope {
        kind: "terminal.close".to_string(),
        device_id: Some("device-1".to_string()),
        session_id: Some(session_b.clone()),
        request_id: None,
        seq: None,
        payload: json!({ "projectId": "project-b", "worktreeId": "worktree-b" }),
    });

    let layout = TerminalLayoutService::new(support_dir.clone()).load(Some(&layout_key));
    assert_eq!(layout.top_panes.len(), 1);
    assert_eq!(layout.top_panes[0].terminal_id, session_a);
    assert!(terminals.snapshot(&session_b).is_err());
    assert!(
        runtime
            .drain_events()
            .iter()
            .any(|event| matches!(event, RemoteHostEvent::TerminalLayoutChanged(_)))
    );

    runtime.handle_terminal_close(&RemoteEnvelope {
        kind: "terminal.close".to_string(),
        device_id: Some("device-1".to_string()),
        session_id: Some(session_a.clone()),
        request_id: None,
        seq: None,
        payload: json!({ "projectId": "project-b", "worktreeId": "worktree-b" }),
    });

    let layout = TerminalLayoutService::new(support_dir.clone()).load(Some(&layout_key));
    // Closing the last terminal tears it down on both ends: the dead session is
    // gone AND the persisted layout no longer lists it, so the desktop reconcile
    // can drop the stale pane instead of leaving the takeover placeholder behind.
    assert!(
        layout.top_panes.is_empty(),
        "closing the last terminal left a stale layout entry: {:?}",
        layout.top_panes
    );
    assert!(terminals.snapshot(&session_a).is_err());

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn remote_terminal_list_excludes_exited_sessions() {
    let support_dir = temp_support_dir("codux-remote-terminal-exited-list");
    let terminals = Arc::new(TerminalManager::new());
    let runtime = RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    );
    let session_id = terminals
        .create(
            TerminalPtyConfig {
                shell: Some("sh".to_string()),
                command: Some("exit 0".to_string()),
                cwd: Some(support_dir.to_string_lossy().to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal");

    for _ in 0..200 {
        if terminals
            .list()
            .iter()
            .any(|terminal| terminal.id == session_id && !terminal.is_running)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        terminals
            .list()
            .iter()
            .any(|terminal| terminal.id == session_id && !terminal.is_running)
    );
    assert!(runtime.remote_terminals().iter().all(|terminal| {
        terminal.get("id").and_then(Value::as_str) != Some(session_id.as_str())
    }));

    fs::remove_dir_all(support_dir).ok();
}

#[cfg(unix)]
#[test]
fn last_remote_viewer_unsubscribe_preserves_scrollback() {
    let support_dir = temp_support_dir("codux-remote-terminal-unsubscribe-scrollback");
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let command = "read ready; i=0; while [ \"$i\" -lt 800 ]; do printf 'line-%04d\\r\\n' \"$i\"; i=$((i + 1)); done; printf '__DONE__\\r\\n'; read hold";
    let (session, output) = terminals
        .attach_or_create_with_context(
            TerminalPtyConfig {
                shell: Some("/bin/sh".to_string()),
                command: Some(command.to_string()),
                cwd: Some(support_dir.to_string_lossy().to_string()),
                cols: Some(80),
                rows: Some(24),
                scrollback_lines: Some(2_000),
                ..Default::default()
            },
            None,
            Arc::new(|_| true),
        )
        .expect("create terminal");

    runtime.register_terminal_viewer(session.id(), Some("device-1"));
    session.write(b"\n").expect("start output");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = String::new();
    while Instant::now() < deadline && !received.contains("__DONE__") {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Ok(bytes) = output.recv_timeout(remaining.min(Duration::from_millis(100))) {
            received.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    assert!(received.contains("__DONE__"));

    let before = terminals
        .screen_snapshot(session.id())
        .expect("snapshot before unsubscribe")
        .total_lines;
    assert!(
        before > 500,
        "expected retained scrollback, got {before} lines"
    );

    runtime.remove_terminal_viewer_for_session(session.id(), Some("device-1"));

    assert!(runtime.terminal_output_viewers(session.id()).is_empty());
    let after = terminals
        .screen_snapshot(session.id())
        .expect("snapshot after unsubscribe")
        .total_lines;
    assert_eq!(after, before);

    let _ = session.kill();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn device_disconnect_releases_owned_terminal_viewport() {
    let support_dir = temp_support_dir("codux-remote-terminal-viewport-disconnect");
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let session_id = terminals
        .create(
            TerminalPtyConfig {
                shell: Some("sh".to_string()),
                command: Some("printf ready".to_string()),
                cwd: Some(support_dir.to_string_lossy().to_string()),
                cols: Some(100),
                rows: Some(32),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal");

    runtime.handle_terminal_viewport_resize(&RemoteEnvelope {
        kind: "terminal.viewport.resize".to_string(),
        device_id: Some("device-1".to_string()),
        session_id: Some(session_id.clone()),
        request_id: None,
        seq: None,
        payload: json!({
            "cols": 72,
            "rows": 18,
        }),
    });
    assert_eq!(
        terminals
            .viewport_state(&session_id)
            .expect("viewport state")
            .owner,
        "remote:device-1"
    );

    runtime.handle_remote_envelope(RemoteEnvelope {
        kind: "device.disconnected".to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: None,
        seq: None,
        payload: json!({}),
    });

    let state = terminals
        .viewport_state(&session_id)
        .expect("viewport state");
    assert_eq!(state.owner, "remote:device-1");
    assert_eq!((state.cols, state.rows), (72, 18));

    let expired = terminals
        .expire_viewport_lease_for_test(&session_id)
        .expect("expire viewport lease")
        .expect("expired viewport state");
    assert_eq!(expired.owner, "desktop");
    assert_eq!((expired.cols, expired.rows), (72, 18));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn device_transport_disconnect_keeps_viewport_until_lease_expires() {
    let support_dir = temp_support_dir("codux-remote-terminal-viewport-transport-disconnect");
    write_paired_remote_settings(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let start =
        runtime.set_transport_lifecycle_for_test(7, Arc::new(CapturingTransport::default()));
    let session_id = terminals
        .create(
            TerminalPtyConfig {
                shell: Some("sh".to_string()),
                command: Some("printf ready".to_string()),
                cwd: Some(support_dir.to_string_lossy().to_string()),
                cols: Some(100),
                rows: Some(32),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal");
    terminals
        .claim_viewport(&session_id, "remote:device-1")
        .expect("remote claim");

    runtime.handle_transport_state(start, "device-1".to_string(), "disconnected".to_string());

    let state = terminals
        .viewport_state(&session_id)
        .expect("viewport state");
    assert_eq!(state.owner, "remote:device-1");

    let expired = terminals
        .expire_viewport_lease_for_test(&session_id)
        .expect("expire viewport lease")
        .expect("expired viewport state");
    assert_eq!(expired.owner, "desktop");

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn host_transport_disconnect_releases_remote_terminal_viewports() {
    let support_dir = temp_support_dir("codux-remote-terminal-viewport-host-disconnect");
    write_paired_remote_settings(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let start =
        runtime.set_transport_lifecycle_for_test(7, Arc::new(CapturingTransport::default()));
    let session_id = terminals
        .create(
            TerminalPtyConfig {
                shell: Some("sh".to_string()),
                command: Some("printf ready".to_string()),
                cwd: Some(support_dir.to_string_lossy().to_string()),
                cols: Some(100),
                rows: Some(32),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal");
    terminals
        .claim_viewport(&session_id, "remote:device-1")
        .expect("remote claim");

    runtime.handle_transport_state(start, String::new(), "closed".to_string());

    let state = terminals
        .viewport_state(&session_id)
        .expect("viewport state");
    assert_eq!(state.owner, "desktop");

    fs::remove_dir_all(support_dir).ok();
}
