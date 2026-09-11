#[derive(Clone)]
pub struct TerminalPane {
    pub view: Entity<TerminalView>,
    session: TerminalSessionBinding,
}

#[derive(Clone)]
pub struct HostedTerminalCloseTarget {
    controller: Arc<dyn RuntimeTerminalController>,
    session_id: String,
}

impl HostedTerminalCloseTarget {
    pub fn close(self) -> Result<(), String> {
        self.controller.close_terminal(&self.session_id)?;
        self.controller.unregister_terminal_stream(&self.session_id);
        Ok(())
    }
}

fn terminal_ui_event_sink(
    session_event_tx: flume::Sender<TerminalUiEvent>,
    session_event_wake_tx: flume::Sender<()>,
) -> EventSink {
    Arc::new(move |event| match event {
        TerminalEvent::Exit { .. } => {
            let sent = session_event_tx.send(TerminalUiEvent::Exit).is_ok();
            let _ = session_event_wake_tx.try_send(());
            sent
        }
        TerminalEvent::Error { message, .. } => {
            let sent = session_event_tx
                .send(TerminalUiEvent::Error(message))
                .is_ok();
            let _ = session_event_wake_tx.try_send(());
            sent
        }
        TerminalEvent::Output { .. } => true,
        TerminalEvent::Viewport {
            owner,
            generation,
            cols,
            rows,
            ..
        } => {
            let sent = session_event_tx
                .send(TerminalUiEvent::Viewport {
                    remote_owner: owner != terminal_viewport_local_owner(),
                    generation,
                    cols,
                    rows,
                })
                .is_ok();
            let _ = session_event_wake_tx.try_send(());
            sent
        }
    })
}

impl TerminalPane {
    pub fn spawn_with_pty_config<C>(
        cx: &mut C,
        terminal_manager: Arc<TerminalManager>,
        pty_config: TerminalPtyConfig,
        terminal_config: TerminalConfig,
    ) -> Result<Self>
    where
        C: AppContext,
    {
        let config = terminal_pty_config_with_view(pty_config, &terminal_config);
        let (session_event_tx, session_event_rx) = flume::unbounded();
        let (session_event_wake_tx, session_event_wake_rx) = flume::bounded(1);
        let emit = terminal_ui_event_sink(session_event_tx, session_event_wake_tx);
        let terminal_id = config.terminal_id.clone();
        let attach_started_at = Instant::now();
        let (session, output_rx) =
            terminal_manager.attach_or_create_with_context(config, None, emit)?;
        codux_runtime::runtime_trace::runtime_trace(
            "terminal-restore",
            &format!(
                "pty_attach elapsed_ms={} terminal_id={}",
                attach_started_at.elapsed().as_millis(),
                terminal_id.as_deref().unwrap_or("none")
            ),
        );
        let session = TerminalSessionBinding::attached(session);
        let writer = TerminalSessionWriter::new(session.clone());
        let view_started_at = Instant::now();
        let view = cx.new(|cx| {
            TerminalView::new(
                TerminalViewInput {
                    stdin_writer: writer,
                    output_rx: TerminalOutputReceiver::Bytes(output_rx),
                    session_event_rx,
                    session_event_wake_rx,
                    session: session.clone(),
                    config: terminal_config,
                    restored_output: None,
                },
                cx,
            )
        });
        codux_runtime::runtime_trace::runtime_trace(
            "terminal-restore",
            &format!(
                "view_create elapsed_ms={} terminal_id={}",
                view_started_at.elapsed().as_millis(),
                terminal_id.as_deref().unwrap_or("none")
            ),
        );

        Ok(Self { view, session })
    }

    pub fn pending_with_pty_config<C>(
        cx: &mut C,
        pty_config: TerminalPtyConfig,
        terminal_config: TerminalConfig,
    ) -> (Self, PendingTerminalAttach)
    where
        C: AppContext,
    {
        Self::pending_with_restored_output(cx, pty_config, terminal_config, None)
    }

    pub fn pending_with_restored_output<C>(
        cx: &mut C,
        pty_config: TerminalPtyConfig,
        terminal_config: TerminalConfig,
        restored_output: Option<TerminalOutputSnapshot>,
    ) -> (Self, PendingTerminalAttach)
    where
        C: AppContext,
    {
        let config = terminal_pty_config_with_view(pty_config, &terminal_config);
        let terminal_id = config.terminal_id.clone();
        let (session_event_tx, session_event_rx) = flume::unbounded();
        let (session_event_wake_tx, session_event_wake_rx) = flume::bounded(1);
        let (output_tx, output_rx) = flume::unbounded();
        let (session, initial_layout_rx) = TerminalSessionBinding::pending(config.clone());
        let writer = TerminalSessionWriter::new(session.clone());
        let restored_output_bytes = restored_output
            .as_ref()
            .map(|output| output.bytes)
            .unwrap_or_default();
        let restored_tail_bytes = restored_output
            .as_ref()
            .map(|output| output.tail.len())
            .unwrap_or_default();
        let view_started_at = Instant::now();
        let view = cx.new(|cx| {
            TerminalView::new(
                TerminalViewInput {
                    stdin_writer: writer,
                    output_rx: TerminalOutputReceiver::Events(output_rx),
                    session_event_rx,
                    session_event_wake_rx,
                    session: session.clone(),
                    config: terminal_config,
                    restored_output,
                },
                cx,
            )
        });
        codux_runtime::runtime_trace::runtime_trace(
            "terminal-restore",
            &format!(
                "pending_view elapsed_ms={} terminal_id={} restored_bytes={} restored_tail_bytes={}",
                view_started_at.elapsed().as_millis(),
                terminal_id.as_deref().unwrap_or("none"),
                restored_output_bytes,
                restored_tail_bytes
            ),
        );
        (
            Self {
                view,
                session: session.clone(),
            },
            PendingTerminalAttach {
                session,
                output_tx,
                session_event_tx,
                session_event_wake_tx,
                terminal_id,
                initial_layout_rx,
            },
        )
    }

    pub fn attach_pending_session(
        terminal_manager: Arc<TerminalManager>,
        pty_config: TerminalPtyConfig,
        terminal_config: TerminalConfig,
        pending: PendingTerminalAttach,
    ) -> Result<String> {
        let attach_total_started_at = Instant::now();
        let layout_started_at = Instant::now();
        let initial_layout = pending.wait_for_initial_layout();
        let layout_wait_ms = layout_started_at.elapsed().as_millis();
        let config = terminal_pty_config_with_view(pty_config, &terminal_config);
        let terminal_id = config.terminal_id.clone();
        let session_event_tx = pending.session_event_tx.clone();
        let session_event_wake_tx = pending.session_event_wake_tx.clone();
        let emit = terminal_ui_event_sink(session_event_tx, session_event_wake_tx);
        let attach_started_at = Instant::now();
        let (session, output_rx) =
            terminal_manager.attach_or_create_with_context(config, None, emit)?;
        codux_runtime::runtime_trace::runtime_trace(
            "terminal-restore",
            &format!(
                "pty_attach elapsed_ms={} terminal_id={}",
                attach_started_at.elapsed().as_millis(),
                terminal_id.as_deref().unwrap_or("none")
            ),
        );
        let attached_id = session.id().to_string();
        pending.session.attach(session)?;
        match initial_layout {
            Some((cols, rows)) => codux_runtime::runtime_trace::runtime_trace(
                "terminal-restore",
                &format!(
                    "initial_layout_ready terminal_id={} cols={} rows={} wait_ms={}",
                    terminal_id.as_deref().unwrap_or("none"),
                    cols,
                    rows,
                    layout_wait_ms
                ),
            ),
            None => codux_runtime::runtime_trace::runtime_trace(
                "terminal-restore",
                &format!(
                    "initial_layout_timeout terminal_id={} wait_ms={}",
                    terminal_id.as_deref().unwrap_or("none"),
                    layout_wait_ms
                ),
            ),
        }
        let output_tx = pending.output_tx.clone();
        let output_terminal_id = terminal_id.clone().unwrap_or_else(|| attached_id.clone());
        codux_runtime::async_runtime::spawn(async move {
            let mut first_output = true;
            while let Ok(bytes) = output_rx.recv_async().await {
                if first_output {
                    first_output = false;
                    codux_runtime::runtime_trace::runtime_trace(
                        "terminal-restore",
                        &format!(
                            "first_output terminal_id={} bytes={} attach_total_ms={}",
                            output_terminal_id,
                            bytes.len(),
                            attach_total_started_at.elapsed().as_millis()
                        ),
                    );
                }
                if output_tx.send_async(TerminalOutputEvent::Append(bytes)).await.is_err() {
                    break;
                }
            }
        });
        Ok(attached_id)
    }

    /// Attach a pending pane to a hosted terminal over the controller. The
    /// runtime's output bytes and lifecycle events are forwarded into the pane
    /// channel (the model parses them itself, like a local PTY).
    pub fn attach_pending_session_hosted(
        controller: Arc<dyn RuntimeTerminalController>,
        pty_config: TerminalPtyConfig,
        terminal_config: TerminalConfig,
        pending: PendingTerminalAttach,
    ) -> Result<String> {
        let initial_layout = pending.wait_for_initial_layout();
        let config = terminal_pty_config_with_view(pty_config, &terminal_config);
        let mut remote_config = config.clone();
        if let Some((cols, rows)) = initial_layout {
            remote_config.cols = Some(cols);
            remote_config.rows = Some(rows);
        }
        let viewport = controller.terminal_viewport_identity().map(|identity| {
            Arc::new(Mutex::new(awaiting_hosted_viewport(identity, 1, false)))
        });
        // Wire the output forwarder under our stable terminal id (== the host's
        // session id) BEFORE creating. The host sends the seed buffer (history +
        // current screen, so a re-attached terminal repaints its last content)
        // immediately after `terminal.created`; registering only after the reply
        // races that send on another thread and drops the seed.
        let pre_registered_terminal_id = config.terminal_id.clone();
        if let Some(terminal_id) = pre_registered_terminal_id.as_deref() {
            register_hosted_terminal(
                controller.as_ref(),
                terminal_id,
                viewport.as_ref(),
                1,
                &pending.output_tx,
                &pending.session_event_tx,
                &pending.session_event_wake_tx,
            );
        }
        let session_id = match controller.open_terminal(&remote_config) {
            Ok(session_id) => session_id,
            Err(error) => {
                if let Some(terminal_id) = pre_registered_terminal_id.as_deref() {
                    unregister_hosted_terminal(controller.as_ref(), terminal_id);
                }
                let _ = pending
                    .session_event_tx
                    .send(TerminalUiEvent::Error(error.clone()));
                let _ = pending.session_event_wake_tx.try_send(());
                return Err(anyhow::Error::msg(error));
            }
        };
        // Register the live-session forwarder BEFORE dropping the stale
        // pre-registration. If the host assigned a different id than we proposed,
        // unregistering first would leave a window with NO forwarder for the live
        // session, and the host's baseline (sent right after `terminal.created`,
        // keyed by session_id) would be dropped — the "sometimes blank" outcome.
        pending.session.attach_hosted(
            controller.clone(),
            session_id.clone(),
            pending.output_tx.clone(),
            pending.session_event_tx.clone(),
            pending.session_event_wake_tx.clone(),
            remote_config,
            viewport,
        );
        if let Some(terminal_id) = pre_registered_terminal_id
            .as_deref()
            .filter(|terminal_id| *terminal_id != session_id)
        {
            unregister_hosted_terminal(controller.as_ref(), terminal_id);
        }
        codux_runtime::runtime_trace::runtime_trace(
            "terminal-restore",
            &format!(
                "hosted_attach terminal_id={} session_id={session_id} layout={}",
                pre_registered_terminal_id.as_deref().unwrap_or("none"),
                match initial_layout {
                    Some((cols, rows)) => format!("{cols}x{rows}"),
                    None => "none".to_string(),
                },
            ),
        );
        // The host is the single seed authority: on reattach it pushes the
        // baseline right after `terminal.created` (caught by the forwarder we
        // registered above); on a fresh create it sends nothing and the shell's
        // live prompt is the content. We must NOT also request `terminal_buffer_tail`
        // here — that raced the host baseline (both replay the same snapshot tail
        // into our emulator's live stream, which has no seq-dedup), so a switch
        // showed the prompt twice, once, or not at all depending on timing.
        Ok(session_id)
    }

    pub fn send_text(&self, text: &str) -> Result<()> {
        self.session.write(text.as_bytes())
    }

    pub fn input_snapshot(&self) -> TerminalInputSnapshot {
        self.session.input_snapshot()
    }

    pub fn output_snapshot(&self) -> TerminalOutputSnapshot {
        self.session.output_snapshot()
    }

    pub fn matches_pty_config(&self, config: &TerminalPtyConfig) -> bool {
        self.session.matches_pty_config(config)
    }

    pub fn rebind_hosted_controller(
        &self,
        controller: Arc<dyn RuntimeTerminalController>,
    ) -> bool {
        self.session.rebind_hosted(controller)
    }

    pub fn hosted_runtime_target(&self) -> Option<ProjectRuntimeTarget> {
        self.session.hosted_runtime_target()
    }

    pub fn terminal_instance_id(&self) -> Option<String> {
        self.session.terminal_instance_id()
    }

    /// Reap the runtime PTY for a hosted pane on a user-initiated close. Local
    /// panes are killed separately. Project switching does not call this, so the
    /// hosted shell remains available for re-attachment.
    pub fn close_hosted_session(&self) -> bool {
        self.session.close_hosted()
    }

    pub fn hosted_close_target(&self) -> Option<HostedTerminalCloseTarget> {
        self.session.hosted_close_target()
    }
}

pub struct PendingTerminalAttach {
    session: TerminalSessionBinding,
    output_tx: flume::Sender<TerminalOutputEvent>,
    session_event_tx: flume::Sender<TerminalUiEvent>,
    session_event_wake_tx: flume::Sender<()>,
    terminal_id: Option<String>,
    initial_layout_rx: mpsc::Receiver<(u16, u16)>,
}

impl PendingTerminalAttach {
    pub fn terminal_id(&self) -> Option<&str> {
        self.terminal_id.as_deref()
    }

    fn wait_for_initial_layout(&self) -> Option<(u16, u16)> {
        self.initial_layout_rx
            .recv_timeout(TERMINAL_INITIAL_LAYOUT_WAIT)
            .ok()
    }
}

impl Drop for PendingTerminalAttach {
    fn drop(&mut self) {
        self.session.inner.lock().pending_match_config = None;
    }
}

pub fn terminal_pty_config_with_view(
    mut config: TerminalPtyConfig,
    terminal_config: &TerminalConfig,
) -> TerminalPtyConfig {
    config.cols = Some(terminal_config.cols as u16);
    config.rows = Some(terminal_config.rows as u16);
    config.scrollback_lines = Some(terminal_config.scrollback);
    // Preferred shell applies to local spawns only; a hosted runtime resolves its own default.
    if config.shell.is_none() && config.runtime_target.is_local() {
        config.shell = terminal_config.shell.clone();
    }
    // Theme colors for the tool wrapper to seed OSC 10/11: on Windows, ConPTY
    // answers those queries itself from its own (black) palette, so TUIs like
    // codex would detect a dark background under a light app theme.
    let env = config.env.get_or_insert_with(Default::default);
    env.insert(
        "DMUX_TERMINAL_OSC_FG".to_string(),
        terminal_config.colors.foreground_osc_payload(),
    );
    env.insert(
        "DMUX_TERMINAL_OSC_BG".to_string(),
        terminal_config.colors.background_osc_payload(),
    );
    config
}

#[derive(Clone)]
struct TerminalSessionWriter {
    session: TerminalSessionBinding,
}

impl TerminalSessionWriter {
    fn new(session: TerminalSessionBinding) -> Self {
        Self { session }
    }
}

impl Write for TerminalSessionWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.session.write(buf).map_err(std::io::Error::other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct TerminalSessionBinding {
    inner: Arc<Mutex<TerminalSessionBindingInner>>,
}

/// A hosted terminal: input/resize go to its runtime over the controller;
/// output arrives via the controller's per-session forwarder (wired at attach).
/// `output_tx` is retained so the forwarder can be re-registered on a fresh
/// controller after a reconnect (rebind to the same runtime session).
#[derive(Clone)]
struct HostedTerminalBackend {
    controller: Arc<dyn RuntimeTerminalController>,
    session_id: String,
    output_tx: flume::Sender<TerminalOutputEvent>,
    session_event_tx: flume::Sender<TerminalUiEvent>,
    session_event_wake_tx: flume::Sender<()>,
    config: TerminalPtyConfig,
    reconnecting: bool,
    viewport: Option<Arc<Mutex<HostedViewportAuthority>>>,
}

struct HostedViewportAuthority {
    identity: String,
    state: codux_terminal_core::TerminalViewportState,
    epoch: u64,
    visible: bool,
    // Desktop UI consumes owner state and requests its baseline in separate
    // steps. This bit deduplicates that intent; it never authorizes I/O.
    needs_owner_baseline: bool,
}

enum TerminalOutputEvent {
    Append(Vec<u8>),
    Replace {
        history: Vec<u8>,
        screen: Vec<u8>,
        cols: usize,
        rows: usize,
    },
}

enum TerminalOutputReceiver {
    Bytes(flume::Receiver<Vec<u8>>),
    Events(flume::Receiver<TerminalOutputEvent>),
}

impl TerminalOutputReceiver {
    async fn recv(&self) -> Option<TerminalOutputEvent> {
        match self {
            Self::Bytes(receiver) => receiver
                .recv_async()
                .await
                .ok()
                .map(TerminalOutputEvent::Append),
            Self::Events(receiver) => receiver.recv_async().await.ok(),
        }
    }
}

#[derive(Clone, Copy)]
enum HostedViewportCommand {
    Auto,
    Force,
    Renew,
    Release,
}

fn awaiting_hosted_viewport(
    identity: String,
    epoch: u64,
    visible: bool,
) -> HostedViewportAuthority {
    HostedViewportAuthority {
        identity,
        state: codux_terminal_core::TerminalViewportState {
            owner: String::new(),
            cols: 0,
            rows: 0,
            generation: 0,
            owner_label: None,
        },
        epoch,
        visible,
        needs_owner_baseline: false,
    }
}

/// Register per-session output and lifecycle event forwarding.
fn register_hosted_terminal(
    controller: &dyn RuntimeTerminalController,
    session_id: &str,
    viewport: Option<&Arc<Mutex<HostedViewportAuthority>>>,
    epoch: u64,
    output_tx: &flume::Sender<TerminalOutputEvent>,
    session_event_tx: &flume::Sender<TerminalUiEvent>,
    session_event_wake_tx: &flume::Sender<()>,
) {
    let output_tx = output_tx.clone();
    let emit = terminal_ui_event_sink(session_event_tx.clone(), session_event_wake_tx.clone());
    let viewport = viewport.cloned();
    controller.register_terminal_stream(
        session_id,
        Arc::new(move |event| {
            let mut authority = viewport.as_ref().map(|state| state.lock());
            if authority.as_ref().is_some_and(|state| state.epoch != epoch) {
                return;
            }
            match event {
                codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Output(bytes) => {
                    let _ = output_tx.send(TerminalOutputEvent::Append(bytes));
                }
                codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Replace {
                    history,
                    screen,
                    cols,
                    rows,
                } => {
                    let _ = output_tx.send(TerminalOutputEvent::Replace {
                        history,
                        screen,
                        cols,
                        rows,
                    });
                }
                codux_runtime::runtime_terminal::RuntimeTerminalStreamEvent::Event(mut event) => {
                    if let Some(authority) = authority.as_mut() {
                        match &event {
                            TerminalEvent::Viewport { generation, .. }
                                if *generation < authority.state.generation => return,
                            TerminalEvent::Viewport {
                                owner,
                                owner_label,
                                cols,
                                rows,
                                generation,
                                ..
                            } => {
                                let was_local = authority.state.owner == authority.identity;
                                let is_local = *owner == authority.identity;
                                authority.state.owner = owner.clone();
                                authority.state.owner_label =
                                    if is_local { None } else { owner_label.clone() };
                                authority.state.cols = *cols;
                                authority.state.rows = *rows;
                                authority.state.generation = *generation;
                                if !is_local {
                                    authority.needs_owner_baseline = false;
                                } else if !was_local {
                                    authority.needs_owner_baseline = true;
                                }
                            }
                            TerminalEvent::Error { .. } => {
                                authority.state.owner.clear();
                                authority.needs_owner_baseline = false;
                            }
                            _ => {}
                        }
                        if let TerminalEvent::Viewport { owner, .. } = &mut event
                            && *owner == authority.identity
                        {
                            *owner = terminal_viewport_local_owner().to_string();
                        }
                    }
                    emit(event);
                }
            }
        }),
    );
}

fn unregister_hosted_terminal(controller: &dyn RuntimeTerminalController, session_id: &str) {
    controller.unregister_terminal_stream(session_id);
}

fn restore_hosted_session(
    controller: &dyn RuntimeTerminalController,
    current_session_id: &str,
    config: &TerminalPtyConfig,
    viewport: Option<&Arc<Mutex<HostedViewportAuthority>>>,
    epoch: u64,
    output_tx: &flume::Sender<TerminalOutputEvent>,
    session_event_tx: &flume::Sender<TerminalUiEvent>,
    session_event_wake_tx: &flume::Sender<()>,
) -> Result<String, String> {
    if hosted_terminal_exists(controller, current_session_id)? {
        return Ok(current_session_id.to_string());
    }
    let pre_registered_terminal_id = config.terminal_id.as_deref();
    if let Some(terminal_id) = pre_registered_terminal_id {
        register_hosted_terminal(
            controller,
            terminal_id,
            viewport,
            epoch,
            output_tx,
            session_event_tx,
            session_event_wake_tx,
        );
    }
    let result = controller.open_terminal(config);
    if result.is_err()
        && let Some(terminal_id) = pre_registered_terminal_id
    {
        unregister_hosted_terminal(controller, terminal_id);
    }
    result
}

fn hosted_terminal_exists(
    controller: &dyn RuntimeTerminalController,
    session_id: &str,
) -> Result<bool, String> {
    let payload = controller.list_terminals()?;
    Ok(payload
        .get("terminals")
        .and_then(|terminals| terminals.as_array())
        .is_some_and(|terminals| {
            terminals.iter().any(|terminal| {
                terminal
                    .get("id")
                    .or_else(|| terminal.get("sessionId"))
                    .and_then(|value| value.as_str())
                    == Some(session_id)
            })
        }))
}

struct TerminalSessionBindingInner {
    session: Option<Arc<TerminalPtySession>>,
    hosted: Option<HostedTerminalBackend>,
    pending_match_config: Option<TerminalPtyConfig>,
    pending_writes: VecDeque<Vec<u8>>,
    pending_write_bytes: usize,
    last_resize: Option<(u16, u16)>,
    initial_layout_tx: Option<mpsc::Sender<(u16, u16)>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalLayoutRecord {
    initialized: bool,
    resized: bool,
}

impl TerminalSessionBinding {
    fn terminal_instance_id(&self) -> Option<String> {
        let inner = self.inner.lock();
        inner
            .session
            .as_ref()
            .and_then(|session| session.ai_runtime_binding().terminal_instance_id)
            .or_else(|| {
                inner
                    .hosted
                    .as_ref()
                    .and_then(|hosted| hosted.config.session_instance_id.clone())
            })
            .or_else(|| {
                inner
                    .pending_match_config
                    .as_ref()
                    .and_then(|config| config.session_instance_id.clone())
            })
    }

    fn pending(config: TerminalPtyConfig) -> (Self, mpsc::Receiver<(u16, u16)>) {
        let (initial_layout_tx, initial_layout_rx) = mpsc::channel();
        (
            Self {
                inner: Arc::new(Mutex::new(TerminalSessionBindingInner {
                    session: None,
                    hosted: None,
                    pending_match_config: Some(config),
                    pending_writes: VecDeque::new(),
                    pending_write_bytes: 0,
                    last_resize: None,
                    initial_layout_tx: Some(initial_layout_tx),
                })),
            },
            initial_layout_rx,
        )
    }

    fn attached(session: Arc<TerminalPtySession>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TerminalSessionBindingInner {
                session: Some(session),
                hosted: None,
                pending_match_config: None,
                pending_writes: VecDeque::new(),
                pending_write_bytes: 0,
                last_resize: None,
                initial_layout_tx: None,
            })),
        }
    }

    fn attach(&self, session: Arc<TerminalPtySession>) -> Result<()> {
        let (pending_writes, last_resize) = {
            let mut inner = self.inner.lock();
            inner.session = Some(session.clone());
            inner.pending_match_config = None;
            inner.pending_write_bytes = 0;
            (std::mem::take(&mut inner.pending_writes), inner.last_resize)
        };
        if let Some((cols, rows)) = last_resize {
            session
                .clone_handle()
                .resize_viewport(terminal_viewport_local_owner(), cols, rows)?;
        }
        for bytes in pending_writes {
            session.write(&bytes)?;
        }
        Ok(())
    }

    /// Wire this pending binding to a hosted runtime session.
    fn attach_hosted(
        &self,
        controller: Arc<dyn RuntimeTerminalController>,
        session_id: String,
        output_tx: flume::Sender<TerminalOutputEvent>,
        session_event_tx: flume::Sender<TerminalUiEvent>,
        session_event_wake_tx: flume::Sender<()>,
        config: TerminalPtyConfig,
        viewport: Option<Arc<Mutex<HostedViewportAuthority>>>,
    ) {
        let epoch = viewport.as_ref().map_or(0, |state| state.lock().epoch);
        let may_send = viewport.as_ref().is_none_or(|state| {
            let state = state.lock();
            state.state.owner == state.identity
        });
        let (pending_writes, last_resize) = {
            let mut inner = self.inner.lock();
            if let Some(hosted) = &inner.hosted {
                if !Arc::ptr_eq(&hosted.controller, &controller)
                    || hosted.session_id != session_id
                {
                    unregister_hosted_terminal(hosted.controller.as_ref(), &hosted.session_id);
                    register_hosted_terminal(
                        controller.as_ref(),
                        &session_id,
                        viewport.as_ref(),
                        epoch,
                        &output_tx,
                        &session_event_tx,
                        &session_event_wake_tx,
                    );
                }
            } else {
                register_hosted_terminal(
                    controller.as_ref(),
                    &session_id,
                    viewport.as_ref(),
                    epoch,
                    &output_tx,
                    &session_event_tx,
                    &session_event_wake_tx,
                );
            }
            inner.hosted = Some(HostedTerminalBackend {
                controller: controller.clone(),
                session_id: session_id.clone(),
                output_tx,
                session_event_tx,
                session_event_wake_tx,
                config,
                reconnecting: false,
                viewport,
            });
            inner.pending_match_config = None;
            inner.pending_write_bytes = 0;
            (std::mem::take(&mut inner.pending_writes), inner.last_resize)
        };
        if may_send {
            if let Some((cols, rows)) = last_resize {
                if !controller.terminal_resize(&session_id, cols, rows) {
                    self.fail_hosted(epoch, "failed to resize hosted terminal");
                }
            }
            for bytes in pending_writes {
                if !controller.terminal_input(&session_id, &bytes) {
                    self.fail_hosted(epoch, "failed to write hosted terminal");
                    break;
                }
            }
        }
    }

    /// Rebind a hosted terminal to its runtime's current controller. A runtime
    /// restart loses the PTY, so recreate its stable id from the saved config.
    fn rebind_hosted(&self, controller: Arc<dyn RuntimeTerminalController>) -> bool {
        let (
            old_controller,
            old_session_id,
            output_tx,
            session_event_tx,
            session_event_wake_tx,
            config,
            viewport,
            epoch,
        ) = {
            let mut inner = self.inner.lock();
            let Some(hosted) = inner.hosted.as_mut() else {
                return false;
            };
            if Arc::ptr_eq(&hosted.controller, &controller) {
                return false;
            }
            if hosted.reconnecting {
                return false;
            }
            hosted.reconnecting = true;
            let visible = hosted
                .viewport
                .as_ref()
                .is_some_and(|state| state.lock().visible);
            if let Some(state) = hosted.viewport.as_ref() {
                let mut state = state.lock();
                state.epoch = state.epoch.wrapping_add(1);
            }
            let viewport = controller.terminal_viewport_identity().map(|identity| {
                let epoch = hosted
                    .viewport
                    .as_ref()
                    .map_or(1, |state| state.lock().epoch);
                Arc::new(Mutex::new(awaiting_hosted_viewport(identity, epoch, visible)))
            });
            let epoch = viewport.as_ref().map_or(0, |state| state.lock().epoch);
            hosted.viewport = viewport.clone();
            (
                hosted.controller.clone(),
                hosted.session_id.clone(),
                hosted.output_tx.clone(),
                hosted.session_event_tx.clone(),
                hosted.session_event_wake_tx.clone(),
                hosted.config.clone(),
                viewport,
                epoch,
            )
        };
        if viewport.is_some() {
            let _ = session_event_tx.send(TerminalUiEvent::Viewport {
                remote_owner: true,
                generation: 0,
                cols: 0,
                rows: 0,
            });
            let _ = session_event_wake_tx.try_send(());
        }

        let binding = self.clone();
        codux_runtime::async_runtime::spawn_blocking(move || {
            let next_session_id =
                restore_hosted_session(
                    controller.as_ref(),
                    &old_session_id,
                    &config,
                    viewport.as_ref(),
                    epoch,
                    &output_tx,
                    &session_event_tx,
                    &session_event_wake_tx,
                );
            let session_id = match next_session_id {
                Ok(session_id) => session_id,
                Err(error) => {
                    binding.fail_hosted(epoch, &format!("failed to restore hosted terminal: {error}"));
                    binding.finish_hosted_rebind(&old_session_id, epoch, None, None, None, None, None);
                    return;
                }
            };
            binding.finish_hosted_rebind(
                &old_session_id,
                epoch,
                Some(controller.clone()),
                Some(session_id.clone()),
                Some(output_tx),
                Some(session_event_tx),
                Some(session_event_wake_tx),
            );
            unregister_hosted_terminal(old_controller.as_ref(), &old_session_id);
            if old_session_id != session_id {
                unregister_hosted_terminal(controller.as_ref(), &old_session_id);
            }
        });
        true
    }

    fn finish_hosted_rebind(
        &self,
        expected_session_id: &str,
        expected_epoch: u64,
        controller: Option<Arc<dyn RuntimeTerminalController>>,
        session_id: Option<String>,
        output_tx: Option<flume::Sender<TerminalOutputEvent>>,
        session_event_tx: Option<flume::Sender<TerminalUiEvent>>,
        session_event_wake_tx: Option<flume::Sender<()>>,
    ) {
        let mut inner = self.inner.lock();
        let Some(hosted) = inner.hosted.as_mut() else {
            return;
        };
        if hosted.session_id != expected_session_id
            || hosted
                .viewport
                .as_ref()
                .map_or(0, |state| state.lock().epoch)
                != expected_epoch
        {
            return;
        }
        let mut reconnected = None;
        if let (
            Some(controller),
            Some(session_id),
            Some(output_tx),
            Some(session_event_tx),
            Some(session_event_wake_tx),
        ) = (
            controller,
            session_id,
            output_tx,
            session_event_tx,
            session_event_wake_tx,
        )
        {
            register_hosted_terminal(
                controller.as_ref(),
                &session_id,
                hosted.viewport.as_ref(),
                expected_epoch,
                &output_tx,
                &session_event_tx,
                &session_event_wake_tx,
            );
            hosted.controller = controller;
            hosted.session_id = session_id;
            hosted.output_tx = output_tx;
            hosted.session_event_tx = session_event_tx;
            hosted.session_event_wake_tx = session_event_wake_tx;
            reconnected = Some((
                hosted.session_event_tx.clone(),
                hosted.session_event_wake_tx.clone(),
            ));
        }
        hosted.reconnecting = false;
        drop(inner);
        if let Some((event_tx, wake_tx)) = reconnected {
            let _ = event_tx.send(TerminalUiEvent::Reconnected);
            let _ = wake_tx.try_send(());
            if self
                .inner
                .lock()
                .hosted
                .as_ref()
                .and_then(|hosted| hosted.viewport.as_ref())
                .is_some_and(|viewport| viewport.lock().visible)
            {
                self.hosted_viewport_command(HostedViewportCommand::Auto);
            }
        }
    }

    fn hosted_runtime_target(&self) -> Option<ProjectRuntimeTarget> {
        self.inner
            .lock()
            .hosted
            .as_ref()
            .map(|hosted| hosted.config.runtime_target.clone())
    }

    /// Fire the runtime PTY close for a hosted binding.
    fn close_hosted(&self) -> bool {
        let Some(hosted) = self.inner.lock().hosted.clone() else {
            return false;
        };
        hosted.controller.close_terminal_fire(&hosted.session_id);
        unregister_hosted_terminal(hosted.controller.as_ref(), &hosted.session_id);
        true
    }

    fn hosted_close_target(&self) -> Option<HostedTerminalCloseTarget> {
        self.inner
            .lock()
            .hosted
            .as_ref()
            .map(|hosted| HostedTerminalCloseTarget {
                controller: hosted.controller.clone(),
                session_id: hosted.session_id.clone(),
            })
    }

    fn write(&self, bytes: &[u8]) -> Result<()> {
        let hosted = {
            let inner = self.inner.lock();
            inner.hosted.clone()
        };
        if let Some(hosted) = hosted {
            let (allowed, epoch) = hosted.viewport.as_ref().map_or((true, 0), |state| {
                let state = state.lock();
                (state.state.owner == state.identity, state.epoch)
            });
            if allowed && !hosted.controller.terminal_input(&hosted.session_id, bytes) {
                self.fail_hosted(epoch, "failed to write hosted terminal");
            }
            return Ok(());
        }
        if let Some(session) = self.inner.lock().session.clone() {
            return session.write(bytes);
        }
        const MAX_PENDING_WRITE_BYTES: usize = 64 * 1024;
        let mut inner = self.inner.lock();
        if inner.pending_write_bytes + bytes.len() > MAX_PENDING_WRITE_BYTES {
            return Ok(());
        }
        inner.pending_write_bytes += bytes.len();
        inner.pending_writes.push_back(bytes.to_vec());
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let (session, hosted, initial_layout_tx) = {
            let mut inner = self.inner.lock();
            inner.last_resize = Some((cols, rows));
            (
                inner.session.clone(),
                inner.hosted.clone(),
                inner.initial_layout_tx.take(),
            )
        };
        if let Some(tx) = initial_layout_tx {
            let _ = tx.send((cols, rows));
        }
        if let Some(hosted) = hosted {
            let (allowed, epoch) = hosted.viewport.as_ref().map_or((true, 0), |state| {
                let state = state.lock();
                (state.state.owner == state.identity, state.epoch)
            });
            if allowed {
                if !hosted.controller.terminal_resize(&hosted.session_id, cols, rows) {
                    self.fail_hosted(epoch, "failed to resize hosted terminal");
                }
            }
            return Ok(());
        }
        if let Some(session) = session {
            session
                .clone_handle()
                .resize_viewport(terminal_viewport_local_owner(), cols, rows)?;
        }
        Ok(())
    }

    fn claim_local_viewport(&self) -> Result<()> {
        let (session, last_resize) = {
            let inner = self.inner.lock();
            if inner
                .hosted
                .as_ref()
                .is_some_and(|hosted| hosted.viewport.is_some())
            {
                return Ok(());
            }
            (inner.session.clone(), inner.last_resize)
        };
        if let Some(session) = session {
            let handle = session.clone_handle();
            let state = handle.claim_viewport_passive(terminal_viewport_local_owner())?;
            if state.owner == terminal_viewport_local_owner()
                && let Some((cols, rows)) = last_resize
                && (state.cols, state.rows) != (cols, rows)
            {
                handle.resize_viewport(terminal_viewport_local_owner(), cols, rows)?;
            }
        }
        Ok(())
    }

    fn restore_local_viewport(&self) -> Result<()> {
        if self
            .inner
            .lock()
            .hosted
            .as_ref()
            .is_some_and(|hosted| hosted.viewport.is_some())
        {
            self.hosted_viewport_command(HostedViewportCommand::Force);
            return Ok(());
        }
        let (session, last_resize) = {
            let inner = self.inner.lock();
            (inner.session.clone(), inner.last_resize)
        };
        if let Some(session) = session {
            let handle = session.clone_handle();
            let state = handle.claim_viewport(terminal_viewport_local_owner())?;
            if let Some((cols, rows)) = last_resize
                && (state.cols, state.rows) != (cols, rows)
            {
                handle.resize_viewport(terminal_viewport_local_owner(), cols, rows)?;
            }
        }
        Ok(())
    }

    fn refresh_local_viewport_if_current_owner(&self) -> Result<()> {
        let (hosted, last_resize) = {
            let inner = self.inner.lock();
            (inner.hosted.clone(), inner.last_resize)
        };
        if let Some(hosted) = hosted
            && let Some(viewport) = hosted.viewport.as_ref()
        {
            let (epoch, needs_baseline) = {
                let mut viewport = viewport.lock();
                if viewport.state.owner != viewport.identity {
                    return Ok(());
                }
                (
                    viewport.epoch,
                    std::mem::take(&mut viewport.needs_owner_baseline),
                )
            };
            if let Some((cols, rows)) = last_resize
                && !hosted
                    .controller
                    .terminal_resize(&hosted.session_id, cols, rows)
            {
                self.fail_hosted(epoch, "failed to resize hosted terminal before baseline");
                return Err(anyhow::anyhow!(
                    "failed to resize hosted terminal before baseline"
                ));
            }
            if !needs_baseline {
                return Ok(());
            }
            {
                let viewport = viewport.lock();
                if viewport.epoch != epoch || viewport.state.owner != viewport.identity {
                    return Ok(());
                }
            }
            if !hosted.controller.terminal_buffer_tail(&hosted.session_id) {
                let mut viewport = viewport.lock();
                if viewport.epoch == epoch && viewport.state.owner == viewport.identity {
                    viewport.needs_owner_baseline = true;
                }
                return Err(anyhow::anyhow!(
                    "failed to request hosted terminal baseline"
                ));
            }
            return Ok(());
        }
        if self.local_viewport_owns() {
            self.claim_local_viewport()?;
        }
        Ok(())
    }

    fn prepare_local_viewport_for_input(&self) -> Result<bool> {
        if self
            .inner
            .lock()
            .hosted
            .as_ref()
            .is_some_and(|hosted| hosted.viewport.is_some())
        {
            return Ok(self.local_viewport_owns());
        }
        self.restore_local_viewport()?;
        Ok(true)
    }

    fn fail_hosted(&self, epoch: u64, message: &str) {
        let Some(hosted) = self.inner.lock().hosted.clone() else {
            return;
        };
        let Some(viewport) = hosted.viewport.as_ref() else {
            return;
        };
        let mut viewport = viewport.lock();
        if viewport.epoch != epoch {
            return;
        }
        viewport.state.owner.clear();
        viewport.needs_owner_baseline = false;
        let _ = hosted.session_event_tx.send(TerminalUiEvent::Viewport {
            remote_owner: true,
            generation: viewport.state.generation,
            cols: 0,
            rows: 0,
        });
        let _ = hosted
            .session_event_tx
            .send(TerminalUiEvent::Error(message.to_string()));
        let _ = hosted.session_event_wake_tx.try_send(());
    }

    fn hosted_viewport_command(&self, command: HostedViewportCommand) -> bool {
        let Some(hosted) = self.inner.lock().hosted.clone() else {
            return false;
        };
        let Some(viewport) = hosted.viewport.as_ref() else {
            return false;
        };
        let epoch = {
            let mut viewport = viewport.lock();
            if !matches!(command, HostedViewportCommand::Renew) {
                viewport.state.owner.clear();
                viewport.needs_owner_baseline = false;
                let _ = hosted.session_event_tx.send(TerminalUiEvent::Viewport {
                    remote_owner: true,
                    generation: viewport.state.generation,
                    cols: 0,
                    rows: 0,
                });
                let _ = hosted.session_event_wake_tx.try_send(());
            }
            viewport.epoch
        };
        let sent = match command {
            HostedViewportCommand::Auto => hosted.controller.terminal_viewport_auto(&hosted.session_id),
            HostedViewportCommand::Force => hosted.controller.terminal_viewport_force(&hosted.session_id),
            HostedViewportCommand::Renew => hosted.controller.terminal_viewport_renew(&hosted.session_id),
            HostedViewportCommand::Release => hosted.controller.terminal_viewport_release(&hosted.session_id),
        };
        if !sent {
            self.fail_hosted(epoch, "hosted terminal viewport command failed");
        }
        true
    }

    fn set_hosted_render_visible(&self, visible: bool) -> bool {
        let Some(hosted) = self.inner.lock().hosted.clone() else {
            return false;
        };
        let Some(viewport) = hosted.viewport.as_ref() else {
            return false;
        };
        {
            let mut viewport = viewport.lock();
            if viewport.visible == visible {
                return false;
            }
            viewport.visible = visible;
        }
        self.hosted_viewport_command(if visible {
            HostedViewportCommand::Auto
        } else {
            HostedViewportCommand::Release
        });
        true
    }

    fn renew_hosted_viewport(&self) -> bool {
        let Some(hosted) = self.inner.lock().hosted.clone() else {
            return false;
        };
        let Some(viewport) = hosted.viewport.as_ref() else {
            return false;
        };
        let viewport = viewport.lock();
        let (visible, owns) = (
            viewport.visible,
            viewport.state.owner == viewport.identity,
        );
        drop(viewport);
        if visible && owns {
            self.hosted_viewport_command(HostedViewportCommand::Renew);
        }
        visible
    }

    fn local_viewport_owns(&self) -> bool {
        let inner = self.inner.lock();
        if let Some(viewport) = inner.hosted.as_ref().and_then(|hosted| hosted.viewport.as_ref()) {
            let viewport = viewport.lock();
            return viewport.state.owner == viewport.identity;
        }
        inner.session
            .as_ref()
            .map(|session| session.viewport_state().owner == terminal_viewport_local_owner())
            .unwrap_or(true)
    }

    /// Friendly name of the remote device that currently owns the viewport (for
    /// the "handed off" placeholder); None when locally owned / unknown.
    fn viewport_owner_label(&self) -> Option<String> {
        let inner = self.inner.lock();
        if let Some(viewport) = inner.hosted.as_ref().and_then(|hosted| hosted.viewport.as_ref()) {
            return viewport.lock().state.owner_label.clone();
        }
        inner.session
            .as_ref()
            .and_then(|session| session.viewport_state().owner_label)
    }

    fn record_layout(&self, cols: u16, rows: u16) -> TerminalLayoutRecord {
        let (initial_layout_tx, record) = {
            let mut inner = self.inner.lock();
            let previous = inner.last_resize;
            inner.last_resize = Some((cols, rows));
            (
                inner.initial_layout_tx.take(),
                TerminalLayoutRecord {
                    initialized: previous.is_none(),
                    resized: previous.is_some_and(|size| size != (cols, rows)),
                },
            )
        };
        if let Some(tx) = initial_layout_tx {
            let _ = tx.send((cols, rows));
        }
        record
    }

    fn input_snapshot(&self) -> TerminalInputSnapshot {
        self.inner
            .lock()
            .session
            .as_ref()
            .map(|session| session.input_snapshot())
            .unwrap_or_default()
    }

    fn output_snapshot(&self) -> TerminalOutputSnapshot {
        self.inner
            .lock()
            .session
            .as_ref()
            .map(|session| session.output_snapshot())
            .unwrap_or_default()
    }

    fn matches_pty_config(&self, config: &TerminalPtyConfig) -> bool {
        let inner = self.inner.lock();
        if let Some(session) = inner.session.as_ref() {
            if config.runtime_target.is_hosted() {
                return false;
            }
            return session.matches_config(config, None);
        }
        if let Some(hosted) = inner.hosted.as_ref() {
            // A live hosted pane is the same runtime session iff the stable terminal
            // id matches. Without this it matches nothing (no local session,
            // `pending_match_config` cleared on attach), so every project switch
            // fails the reuse gate, re-creates the pane and re-attaches it — each
            // reattach re-triggers the host's baseline, and overlapping switches
            // race those baselines onto the wrong pane (the duplicate/blank).
            return config
                .terminal_id
                .as_deref()
                .is_some_and(|terminal_id| terminal_id == hosted.session_id)
                && terminal_pty_configs_match(&hosted.config, config);
        }
        inner
            .pending_match_config
            .as_ref()
            .is_some_and(|pending| terminal_pty_configs_match(pending, config))
    }
}

fn terminal_pty_configs_match(left: &TerminalPtyConfig, right: &TerminalPtyConfig) -> bool {
    normalized_config_path(left.cwd.as_deref()) == normalized_config_path(right.cwd.as_deref())
        && normalized_config_value(left.project_id.as_deref())
            == normalized_config_value(right.project_id.as_deref())
        && normalized_config_value(left.session_key.as_deref())
            == normalized_config_value(right.session_key.as_deref())
        && normalized_config_value(left.terminal_id.as_deref())
            == normalized_config_value(right.terminal_id.as_deref())
        && left.runtime_target == right.runtime_target
}

fn normalized_config_path(value: Option<&str>) -> Option<String> {
    normalized_config_value(value).map(|path| {
        PathBuf::from(path)
            .components()
            .as_path()
            .to_string_lossy()
            .to_string()
    })
}

fn normalized_config_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
