use super::super::*;

#[derive(Default)]
struct HostedTestController {
    actions: std::sync::Mutex<Vec<String>>,
    terminals: Vec<String>,
    fail_open: bool,
    fail_list: bool,
    identity: Option<String>,
    failures: std::sync::Mutex<HashSet<&'static str>>,
    streams: std::sync::Mutex<
        Vec<codux_runtime::runtime_terminal::RuntimeTerminalStreamForwarder>,
    >,
}

impl HostedTestController {
    fn with_terminals(terminals: &[&str]) -> Self {
        Self {
            terminals: terminals.iter().map(|value| value.to_string()).collect(),
            ..Default::default()
        }
    }

    fn managed() -> Self {
        Self {
            identity: Some("remote:desktop".to_string()),
            ..Default::default()
        }
    }

    fn record(&self, action: impl Into<String>) {
        self.actions.lock().unwrap().push(action.into());
    }

    fn succeeds(&self, action: &'static str) -> bool {
        self.record(action);
        !self.failures.lock().unwrap().contains(action)
    }

    fn fail(&self, action: &'static str) {
        self.failures.lock().unwrap().insert(action);
    }

    fn emit(
        &self,
        index: usize,
        event: codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent,
    ) {
        self.streams.lock().unwrap()[index](event);
    }
}

impl RuntimeTerminalController for HostedTestController {
    fn open_terminal(&self, config: &TerminalPtyConfig) -> Result<String, String> {
        if self.fail_open {
            return Err("open failed".to_string());
        }
        let session_id = config
            .terminal_id
            .clone()
            .ok_or_else(|| "terminal id missing".to_string())?;
        self.record(format!("open:{session_id}"));
        Ok(session_id)
    }

    fn list_terminals(&self) -> Result<serde_json::Value, String> {
        self.record("list");
        if self.fail_list {
            return Err("list failed".to_string());
        }
        Ok(serde_json::json!({
            "terminals": self
                .terminals
                .iter()
                .map(|id| serde_json::json!({ "id": id }))
                .collect::<Vec<_>>()
        }))
    }

    fn terminal_input(&self, session_id: &str, bytes: &[u8]) -> bool {
        self.record(format!("input:{session_id}:{}", String::from_utf8_lossy(bytes)));
        !self.failures.lock().unwrap().contains("input")
    }

    fn terminal_resize(&self, session_id: &str, cols: u16, rows: u16) -> bool {
        self.record(format!("resize:{session_id}:{cols}x{rows}"));
        !self.failures.lock().unwrap().contains("resize")
    }

    fn close_terminal(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn close_terminal_fire(&self, _session_id: &str) -> bool {
        true
    }

    fn register_terminal_output(
        &self,
        session_id: &str,
        _forwarder: codux_runtime::runtime_terminal::RuntimeTerminalOutputForwarder,
    ) {
        self.record(format!("output:{session_id}"));
    }

    fn unregister_terminal_output(&self, _session_id: &str) {}

    fn register_terminal_events(
        &self,
        session_id: &str,
        _forwarder: codux_runtime::runtime_terminal::RuntimeTerminalEventForwarder,
    ) {
        self.record(format!("events:{session_id}"));
    }

    fn register_terminal_stream(
        &self,
        session_id: &str,
        forwarder: codux_runtime::runtime_terminal::RuntimeTerminalStreamForwarder,
    ) {
        self.record(format!("stream:{session_id}"));
        self.streams.lock().unwrap().push(forwarder);
    }

    fn terminal_viewport_identity(&self) -> Option<String> {
        self.identity.clone()
    }

    fn terminal_viewport_auto(&self, _session_id: &str) -> bool {
        self.succeeds("auto")
    }

    fn terminal_viewport_force(&self, _session_id: &str) -> bool {
        self.succeeds("force")
    }

    fn terminal_viewport_renew(&self, _session_id: &str) -> bool {
        self.succeeds("renew")
    }

    fn terminal_viewport_release(&self, _session_id: &str) -> bool {
        self.succeeds("release")
    }
}

fn managed_binding(
    controller: Arc<HostedTestController>,
) -> (
    TerminalSessionBinding,
    flume::Receiver<Vec<u8>>,
    flume::Receiver<TerminalUiEvent>,
) {
    let config = TerminalPtyConfig {
        terminal_id: Some("terminal-1".to_string()),
        ..Default::default()
    };
    let (binding, _) = TerminalSessionBinding::pending(config.clone());
    let (output_tx, output_rx) = flume::unbounded();
    let (event_tx, event_rx) = flume::unbounded();
    let viewport = Arc::new(Mutex::new(awaiting_hosted_viewport(
        "remote:desktop".to_string(),
        1,
        false,
    )));
    binding.attach_hosted(
        controller,
        "terminal-1".to_string(),
        output_tx,
        event_tx,
        flume::unbounded().0,
        config,
        Some(viewport),
    );
    (binding, output_rx, event_rx)
}

fn viewport_event(owner: &str, generation: u64) -> codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent {
    codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Event(TerminalEvent::Viewport {
        session_id: "terminal-1".to_string(),
        owner: owner.to_string(),
        cols: 80,
        rows: 24,
        generation,
    })
}

fn wait_for_stream(controller: &HostedTestController) {
    for _ in 0..100 {
        if !controller.streams.lock().unwrap().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal stream was not registered");
}

fn recv_error(events: &flume::Receiver<TerminalUiEvent>) -> String {
    loop {
        match events.recv_timeout(Duration::from_secs(1)).unwrap() {
            TerminalUiEvent::Error(error) => return error,
            _ => {}
        }
    }
}

#[test]
fn pending_terminal_binding_matches_requested_config_before_attach() {
    let config = terminal_pty_config_with_view(
        TerminalPtyConfig {
            cwd: Some("/tmp/project".to_string()),
            project_id: Some("project-1".to_string()),
            terminal_id: Some("terminal-1".to_string()),
            session_key: Some("gpui:project-1:terminal-1".to_string()),
            ..Default::default()
        },
        &terminal_config(),
    );

    let (binding, _initial_layout_rx) = TerminalSessionBinding::pending(config.clone());

    assert!(binding.matches_pty_config(&config));

    let mut different_terminal = config;
    different_terminal.terminal_id = Some("terminal-2".to_string());
    assert!(!binding.matches_pty_config(&different_terminal));
}

#[test]
fn pending_terminal_binding_rejects_different_runtime_target() {
    let config = TerminalPtyConfig {
        cwd: Some("/tmp/project".to_string()),
        project_id: Some("project-1".to_string()),
        terminal_id: Some("terminal-1".to_string()),
        session_key: Some("gpui:project-1:terminal-1".to_string()),
        ..Default::default()
    };
    let (binding, _initial_layout_rx) = TerminalSessionBinding::pending(config.clone());
    let mut wsl_config = config;
    wsl_config.runtime_target = ProjectRuntimeTarget::Wsl {
        distribution: "Ubuntu-24.04".to_string(),
    };

    assert!(!binding.matches_pty_config(&wsl_config));
}

#[test]
fn failed_hosted_attach_stops_pending_binding_from_matching() {
    let config = TerminalPtyConfig {
        terminal_id: Some("terminal-1".to_string()),
        ..Default::default()
    };
    let terminal_config = terminal_config();
    let matching_config = terminal_pty_config_with_view(config.clone(), &terminal_config);
    let (binding, initial_layout_rx) = TerminalSessionBinding::pending(matching_config.clone());
    binding.resize(80, 24).unwrap();
    let (output_tx, _) = flume::unbounded();
    let (session_event_tx, session_event_rx) = flume::unbounded();
    let (session_event_wake_tx, _) = flume::unbounded();
    let pending = PendingTerminalAttach {
        session: binding.clone(),
        output_tx,
        session_event_tx,
        session_event_wake_tx,
        terminal_id: config.terminal_id.clone(),
        initial_layout_rx,
    };

    assert!(
        TerminalPane::attach_pending_session_hosted(
            Arc::new(HostedTestController {
                fail_open: true,
                ..Default::default()
            }),
            config,
            terminal_config,
            pending,
        )
        .is_err()
    );
    assert!(!binding.matches_pty_config(&matching_config));
    assert!(recv_error(&session_event_rx).contains("open failed"));
}

#[test]
fn hosted_terminal_binding_rejects_different_distribution() {
    let config = TerminalPtyConfig {
        terminal_id: Some("terminal-1".to_string()),
        runtime_target: ProjectRuntimeTarget::Wsl {
            distribution: "Ubuntu-24.04".to_string(),
        },
        ..Default::default()
    };
    let (binding, _initial_layout_rx) = TerminalSessionBinding::pending(config.clone());
    let controller = Arc::new(HostedTestController::default());
    binding.attach_hosted(
        controller.clone(),
        "terminal-1".to_string(),
        flume::unbounded().0,
        flume::unbounded().0,
        flume::unbounded().0,
        config.clone(),
        None,
    );
    let mut other_distribution = config;
    other_distribution.runtime_target = ProjectRuntimeTarget::Wsl {
        distribution: "Debian".to_string(),
    };

    assert!(!binding.matches_pty_config(&other_distribution));
    assert!(binding.local_viewport_owns());
    binding.write(b"x").unwrap();
    binding.resize(80, 24).unwrap();
    let actions = controller.actions.lock().unwrap();
    assert!(actions.iter().any(|action| action.starts_with("input:")));
    assert!(actions.iter().any(|action| action.starts_with("resize:")));
}

#[test]
fn hosted_restore_recreates_missing_session_after_registering_forwarders() {
    let controller = HostedTestController::default();
    let config = TerminalPtyConfig {
        terminal_id: Some("terminal-1".to_string()),
        ..Default::default()
    };
    let (output_tx, _) = flume::unbounded();
    let (event_tx, _) = flume::unbounded();
    let (wake_tx, _) = flume::unbounded();

    let session_id = restore_hosted_session(
        &controller,
        "terminal-1",
        &config,
        None,
        0,
        &output_tx,
        &event_tx,
        &wake_tx,
    )
    .unwrap();

    assert_eq!(session_id, "terminal-1");
    assert_eq!(
        *controller.actions.lock().unwrap(),
        ["list", "stream:terminal-1", "open:terminal-1"]
    );
}

#[test]
fn hosted_restore_reuses_existing_session_without_reopening() {
    let controller = HostedTestController::with_terminals(&["terminal-1"]);
    let config = TerminalPtyConfig {
        terminal_id: Some("terminal-1".to_string()),
        ..Default::default()
    };
    let (output_tx, _) = flume::unbounded();
    let (event_tx, _) = flume::unbounded();
    let (wake_tx, _) = flume::unbounded();

    let session_id = restore_hosted_session(
        &controller,
        "terminal-1",
        &config,
        None,
        0,
        &output_tx,
        &event_tx,
        &wake_tx,
    )
    .unwrap();

    assert_eq!(session_id, "terminal-1");
    assert_eq!(*controller.actions.lock().unwrap(), ["list"]);
}

#[test]
fn managed_hosted_viewport_gates_io_and_obeys_host_generation() {
    let controller = Arc::new(HostedTestController::managed());
    let (binding, _, events) = managed_binding(controller.clone());
    controller.actions.lock().unwrap().clear();

    assert!(!binding.local_viewport_owns());
    binding.write(b"x").unwrap();
    binding.resize(100, 30).unwrap();
    assert!(controller.actions.lock().unwrap().is_empty());

    controller.emit(0, viewport_event("remote:phone", 2));
    assert!(!binding.local_viewport_owns());
    controller.emit(0, viewport_event("remote:desktop", 2));
    assert!(binding.local_viewport_owns());
    controller.emit(
        0,
        codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Event(TerminalEvent::Error {
            session_id: "terminal-1".to_string(),
            message: "lost owner".to_string(),
        }),
    );
    assert!(!binding.local_viewport_owns());
    controller.emit(0, viewport_event("remote:desktop", 2));
    assert!(binding.local_viewport_owns());
    controller.emit(0, viewport_event("remote:phone", 1));
    assert!(binding.local_viewport_owns());

    binding.write(b"x").unwrap();
    binding.resize(100, 30).unwrap();
    assert_eq!(
        *controller.actions.lock().unwrap(),
        ["input:terminal-1:x", "resize:terminal-1:100x30"]
    );
    binding.restore_local_viewport().unwrap();
    assert!(!binding.local_viewport_owns());
    assert_eq!(controller.actions.lock().unwrap().last().unwrap(), "force");
    assert!(events.try_iter().any(|event| matches!(
        event,
        TerminalUiEvent::Viewport { remote_owner: false, generation: 2, .. }
    )));
}

#[test]
fn hosted_local_viewport_refresh_does_not_deadlock() {
    let controller = Arc::new(HostedTestController::managed());
    let (binding, _, _) = managed_binding(controller.clone());
    binding.resize(100, 30).unwrap();
    controller.emit(0, viewport_event("remote:desktop", 1));
    controller.actions.lock().unwrap().clear();

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(binding.refresh_local_viewport_if_current_owner());
    });

    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("local viewport refresh deadlocked")
        .unwrap();
    assert_eq!(
        *controller.actions.lock().unwrap(),
        ["resize:terminal-1:100x30"]
    );
}

#[test]
fn managed_hosted_viewport_lifecycle_and_failures_are_visible() {
    let controller = Arc::new(HostedTestController::managed());
    let (binding, _, events) = managed_binding(controller.clone());

    controller.fail("auto");
    assert!(binding.set_hosted_render_visible(true));
    controller.failures.lock().unwrap().clear();
    controller.emit(0, viewport_event("remote:desktop", 1));
    controller.fail("renew");
    assert!(binding.renew_hosted_viewport());
    controller.failures.lock().unwrap().clear();
    controller.fail("release");
    assert!(binding.set_hosted_render_visible(false));
    controller.failures.lock().unwrap().clear();
    controller.emit(0, viewport_event("remote:desktop", 2));
    controller.fail("force");
    binding.restore_local_viewport().unwrap();
    controller.failures.lock().unwrap().clear();
    controller.emit(0, viewport_event("remote:desktop", 3));
    controller.fail("input");
    binding.write(b"x").unwrap();
    controller.failures.lock().unwrap().clear();
    controller.emit(0, viewport_event("remote:desktop", 4));
    controller.fail("resize");
    binding.resize(90, 25).unwrap();

    assert_eq!(
        events
            .try_iter()
            .filter(|event| matches!(event, TerminalUiEvent::Error(_)))
            .count(),
        6
    );
    assert!(!binding.local_viewport_owns());
    let actions = controller.actions.lock().unwrap();
    for action in ["auto", "renew", "release", "force"] {
        assert!(actions.iter().any(|recorded| recorded == action));
    }
}

#[test]
fn managed_hosted_viewport_reconnect_drops_old_epoch_and_respects_visibility() {
    let first = Arc::new(HostedTestController::managed());
    let (binding, output, _) = managed_binding(first.clone());
    let mut next_controller = HostedTestController::with_terminals(&["terminal-1"]);
    next_controller.identity = Some("remote:desktop".to_string());
    let next = Arc::new(next_controller);

    assert!(binding.rebind_hosted(next.clone()));
    first.emit(
        0,
        codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Output(b"old".to_vec()),
    );
    first.emit(0, viewport_event("remote:desktop", 99));
    wait_for_stream(&next);
    next.emit(0, viewport_event("remote:desktop", 0));
    next.emit(
        0,
        codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Output(b"new".to_vec()),
    );
    assert_eq!(output.recv_timeout(Duration::from_secs(1)).unwrap(), b"new");
    assert!(output.try_recv().is_err());
    assert!(binding.local_viewport_owns());
    assert!(!next.actions.lock().unwrap().iter().any(|action| action == "auto"));

    assert!(binding.set_hosted_render_visible(true));
    let mut third_controller = HostedTestController::with_terminals(&["terminal-1"]);
    third_controller.identity = Some("remote:desktop".to_string());
    let third = Arc::new(third_controller);
    assert!(binding.rebind_hosted(third.clone()));
    wait_for_stream(&third);
    assert!(third.actions.lock().unwrap().iter().any(|action| action == "auto"));
    assert!(!binding.local_viewport_owns());
}

#[test]
fn managed_hosted_viewport_reconnect_surfaces_list_and_open_failures() {
    let first = Arc::new(HostedTestController::managed());
    let (binding, _, events) = managed_binding(first);
    let list_failure = Arc::new(HostedTestController {
        fail_list: true,
        identity: Some("remote:desktop".to_string()),
        ..Default::default()
    });
    assert!(binding.rebind_hosted(list_failure));
    assert!(recv_error(&events).contains("list failed"));

    let open_failure = Arc::new(HostedTestController {
        fail_open: true,
        identity: Some("remote:desktop".to_string()),
        ..Default::default()
    });
    assert!(binding.rebind_hosted(open_failure));
    assert!(recv_error(&events).contains("open failed"));
    assert!(!binding.local_viewport_owns());
}

#[test]
fn reconnect_event_clears_terminal_failure_state() {
    let mut model = TerminalModel::new_for_test(80, 24, 100);

    assert!(model.apply_ui_event(TerminalUiEvent::Exit));
    assert!(model.apply_ui_event(TerminalUiEvent::Error("runtime exited".to_string())));
    assert!(model.exited);
    assert!(model.title.is_some());

    assert!(model.apply_ui_event(TerminalUiEvent::Reconnected));
    assert!(!model.exited);
    assert!(model.title.is_none());
}
