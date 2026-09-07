use super::terminal_dispatch::sanitized_remote_upload_name;
use super::*;

impl RemoteTransportCallbackOwner {
    pub(super) fn acquire(self: &Arc<Self>) -> Option<RemoteTransportCallbackPermit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.published {
            return None;
        }
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .expect("remote transport callback count overflow");
        Some(RemoteTransportCallbackPermit {
            owner: Arc::clone(self),
        })
    }

    pub(super) fn run<R>(self: &Arc<Self>, effect: impl FnOnce() -> R) -> Option<R> {
        let _permit = self.acquire()?;
        Some(effect())
    }

    pub(super) fn transition<R>(&self, transition: impl FnOnce(&mut bool) -> R) -> R {
        let _transition = self
            .transition_serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut published = state.published;
        state.published = false;
        while state.in_flight > 0 {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let result = transition(&mut published);
        state.published = published;
        result
    }
}

impl Drop for RemoteTransportCallbackPermit {
    fn drop(&mut self) {
        let mut state = self
            .owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight = state
            .in_flight
            .checked_sub(1)
            .expect("remote transport callback count underflow");
        if state.in_flight == 0 {
            self.owner.idle.notify_all();
        }
    }
}

impl RemoteTransportLifecycle {
    pub(super) fn begin_start(&mut self) -> RemoteTransportStart {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("remote transport generation overflow");
        debug_assert!(self.active.is_none());
        let start = RemoteTransportStart {
            generation: self.generation,
            owner: Arc::new(RemoteTransportCallbackOwner::default()),
        };
        self.starting = Some(start.clone());
        start
    }

    pub(super) fn stop(&mut self) -> Option<Arc<dyn RemoteTransport>> {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("remote transport generation overflow");
        self.starting = None;
        self.active.take().map(|binding| binding.transport)
    }

    pub(super) fn retire(&mut self) -> Option<Arc<dyn RemoteTransport>> {
        self.starting = None;
        self.active.take().map(|binding| binding.transport)
    }

    fn owns_start(&self, start: &RemoteTransportStart) -> bool {
        self.generation == start.generation
            && self
                .starting
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.owner, &start.owner))
    }

    fn has_active_start(&self, start: &RemoteTransportStart) -> bool {
        self.generation == start.generation
            && self
                .active
                .as_ref()
                .is_some_and(|binding| Arc::ptr_eq(&binding.start.owner, &start.owner))
    }

    pub(super) fn is_owned_by(&self, owner: &Arc<RemoteTransportCallbackOwner>) -> bool {
        self.starting
            .as_ref()
            .is_some_and(|start| Arc::ptr_eq(&start.owner, owner))
            || self
                .active
                .as_ref()
                .is_some_and(|binding| Arc::ptr_eq(&binding.start.owner, owner))
    }

    fn current_owner(&self) -> Option<Arc<RemoteTransportCallbackOwner>> {
        self.starting
            .as_ref()
            .map(|start| Arc::clone(&start.owner))
            .or_else(|| {
                self.active
                    .as_ref()
                    .map(|binding| Arc::clone(&binding.start.owner))
            })
    }
}

#[derive(Clone)]
pub(super) struct RemoteHostCallbackAdapter {
    runtime: std::sync::Weak<RemoteHostRuntime>,
    start: RemoteTransportStart,
}

impl RemoteHostCallbackAdapter {
    fn new(runtime: &Arc<RemoteHostRuntime>, start: RemoteTransportStart) -> Self {
        Self {
            runtime: Arc::downgrade(runtime),
            start,
        }
    }

    fn run<R>(&self, effect: impl FnOnce(Arc<RemoteHostRuntime>) -> R) -> Option<R> {
        let runtime = self.runtime.upgrade()?;
        self.start.owner.run(|| effect(runtime))
    }

    pub(super) fn handle_message(&self, device_id: String, data: Vec<u8>) {
        let _ = self.run(|runtime| runtime.handle_transport_message(device_id, data));
    }

    fn handle_upload(
        &self,
        upload: RemoteTransportUpload,
    ) -> Result<RemoteTransportUploadOutcome, String> {
        self.run(|runtime| runtime.handle_transport_upload(upload))
            .unwrap_or_else(|| Err("remote transport is no longer active".to_string()))
    }

    fn handle_state(&self, device_id: String, state: String) {
        if device_id.trim().is_empty() {
            if let Some(runtime) = self.runtime.upgrade() {
                runtime.handle_transport_state(self.start.clone(), device_id, state);
            }
        } else {
            let _ = self.run(|runtime| {
                runtime.handle_transport_state(self.start.clone(), device_id, state)
            });
        }
    }

    fn handle_pairing(&self, handshake: RemoteTransportPairingRequest) -> Option<Value> {
        self.run(|runtime| runtime.handle_transport_pairing_request(&self.start.owner, handshake))
            .flatten()
    }

    fn authorize(&self, device_id: &str, device_token: &str) -> bool {
        self.run(|runtime| runtime.is_authorized_device_token(Some(device_id), Some(device_token)))
            .unwrap_or(false)
    }

    fn authorize_web_tunnel(&self, request: WebTunnelTcpConnectRequest) -> Result<(), String> {
        self.run(|runtime| runtime.authorize_web_tunnel_tcp_connect(request))
            .unwrap_or_else(|| Err("remote transport is no longer active".to_string()))
    }
}

impl RemoteHostRuntime {
    pub(super) fn lock_transport_lifecycle(
        &self,
    ) -> std::sync::MutexGuard<'_, RemoteTransportLifecycle> {
        self.transport_lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn commit_transport_lifecycle<R>(
        &self,
        lifecycle: &mut RemoteTransportLifecycle,
        commit: impl FnOnce(&mut RemoteTransportLifecycle, &RemoteSummary) -> (RemoteSummary, R),
    ) -> R {
        let mut current = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (summary, result) = commit(lifecycle, &current);
        *current = summary;
        self.push_event(RemoteHostEvent::Summary(Box::new(current.clone())));
        result
    }

    pub(super) fn retire_transport_lifecycle<R>(
        &self,
        commit: impl FnOnce(&mut RemoteTransportLifecycle, &RemoteSummary) -> (RemoteSummary, R),
    ) -> R {
        let mut commit = Some(commit);
        loop {
            let owner = self.lock_transport_lifecycle().current_owner();
            let Some(owner) = owner else {
                let mut lifecycle = self.lock_transport_lifecycle();
                if lifecycle.current_owner().is_none() {
                    return self.commit_transport_lifecycle(
                        &mut lifecycle,
                        commit.take().expect("transport transition commit"),
                    );
                }
                continue;
            };
            let mut result = None;
            owner.transition(|published| {
                let mut lifecycle = self.lock_transport_lifecycle();
                if !lifecycle.is_owned_by(&owner) {
                    return;
                }
                *published = false;
                let mut active_pairing = self
                    .active_pairing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *active_pairing = None;
                let mut pending_pairings = self
                    .pending_pairings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                pending_pairings.clear();
                result = Some(self.commit_transport_lifecycle(
                    &mut lifecycle,
                    commit.take().expect("transport transition commit"),
                ));
            });
            if let Some(result) = result {
                return result;
            }
        }
    }

    pub(super) fn current_transport(&self) -> Option<Arc<dyn RemoteTransport>> {
        self.lock_transport_lifecycle()
            .active
            .as_ref()
            .map(|binding| Arc::clone(&binding.transport))
    }

    pub(super) fn current_transport_binding(&self) -> Option<RemoteTransportBinding> {
        self.lock_transport_lifecycle().active.clone()
    }

    pub(super) fn current_callback_owner(&self) -> Option<Arc<RemoteTransportCallbackOwner>> {
        self.current_transport_binding()
            .map(|binding| Arc::clone(&binding.start.owner))
    }

    #[cfg(test)]
    pub(super) fn has_transport(&self) -> bool {
        self.lock_transport_lifecycle().active.is_some()
    }

    #[cfg(test)]
    pub(super) fn set_transport_for_test(
        &self,
        transport: Arc<dyn RemoteTransport>,
    ) -> RemoteTransportStart {
        let generation = self
            .lock_transport_lifecycle()
            .generation
            .checked_add(1)
            .expect("remote transport generation overflow");
        self.set_transport_lifecycle_for_test(generation, transport)
    }

    #[cfg(test)]
    pub(super) fn set_transport_lifecycle_for_test(
        &self,
        generation: u64,
        transport: Arc<dyn RemoteTransport>,
    ) -> RemoteTransportStart {
        let settings = super::remote_settings_from_raw(&self.service().raw_settings());
        let start = RemoteTransportStart {
            generation,
            owner: Arc::new(RemoteTransportCallbackOwner::default()),
        };
        start.owner.transition(|published| {
            let mut lifecycle = self.lock_transport_lifecycle();
            lifecycle.generation = generation;
            lifecycle.starting = None;
            lifecycle.active = Some(RemoteTransportBinding {
                start: start.clone(),
                transport,
                relay_authentication: settings.relay_authentication.trim().to_string(),
            });
            *published = true;
        });
        start
    }

    #[cfg(test)]
    pub(super) fn transport_generation_for_test(&self) -> u64 {
        self.lock_transport_lifecycle().generation
    }

    #[cfg(test)]
    pub(super) fn begin_transport_start_for_test(&self) -> RemoteTransportStart {
        self.lock_transport_lifecycle().begin_start()
    }

    #[cfg(test)]
    pub(super) fn callback_adapter_for_test(
        self: &Arc<Self>,
        start: RemoteTransportStart,
    ) -> RemoteHostCallbackAdapter {
        RemoteHostCallbackAdapter::new(self, start)
    }

    pub fn send_transport(
        &self,
        kind: &str,
        device_id: Option<&str>,
        session_id: Option<&str>,
        payload: Value,
    ) -> bool {
        self.send_transport_with_request_id(kind, device_id, session_id, None, payload)
    }

    pub(super) fn send_transport_with_request_id(
        &self,
        kind: &str,
        device_id: Option<&str>,
        session_id: Option<&str>,
        request_id: Option<&str>,
        payload: Value,
    ) -> bool {
        let Some(data) =
            self.outgoing_transport_text(kind, device_id, session_id, request_id, payload)
        else {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!(
                    "send drop kind={kind} device={} reason=encode",
                    device_id.unwrap_or("")
                ),
            );
            return false;
        };
        let transport = self.current_transport();
        let Some(transport) = transport else {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!(
                    "send drop kind={kind} device={} reason=no_transport",
                    device_id.unwrap_or("")
                ),
            );
            return false;
        };
        let bytes = data.into_bytes();
        let ok = if codux_protocol::is_terminal_stream_message(kind) {
            transport.send_terminal(bytes, device_id)
        } else {
            transport.send(bytes, device_id)
        };
        if matches!(
            kind,
            REMOTE_PROJECT_SELECTED | REMOTE_PROJECT_LIST | REMOTE_TERMINAL_LIST | REMOTE_ERROR
        ) {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!(
                    "send kind={kind} device={} session={} ok={ok}",
                    device_id.unwrap_or(""),
                    session_id.unwrap_or("")
                ),
            );
        }
        ok
    }

    pub(super) fn spawn_transport_start(self: &Arc<Self>, start: RemoteTransportStart) {
        let runtime = Arc::clone(self);
        crate::async_runtime::spawn(async move {
            let _ = runtime.ensure_transport_ready(start).await;
        });
    }

    pub(super) fn spawn_transport_restart(
        self: &Arc<Self>,
        transport: Option<Arc<dyn RemoteTransport>>,
        start: RemoteTransportStart,
    ) {
        let runtime = Arc::clone(self);
        crate::async_runtime::spawn(async move {
            if let Some(transport) = transport {
                transport.shutdown().await;
            }
            let _ = runtime.ensure_transport_ready(start).await;
        });
    }

    fn publish_transport_failure(&self, start: &RemoteTransportStart, error: String) {
        start.owner.transition(|published| {
            let mut lifecycle = self.lock_transport_lifecycle();
            if !lifecycle.owns_start(start) {
                return;
            }
            *published = false;
            let mut status = self.service().summary();
            if status.enabled {
                status.status = "failed".to_string();
                status.message = error;
            } else {
                status.status = "stopped".to_string();
                status.message = "Remote Host stopped.".to_string();
            }
            self.commit_transport_lifecycle(&mut lifecycle, |lifecycle, current| {
                lifecycle.starting = None;
                status.pairing = current.pairing.clone();
                (status, ())
            });
        });
    }

    pub(super) fn publish_started_transport(
        &self,
        start: RemoteTransportStart,
        transport: Arc<dyn RemoteTransport>,
        relay_authentication: String,
    ) -> Option<Arc<dyn RemoteTransport>> {
        let transport_kind = transport.kind();
        let mut result = Some(Arc::clone(&transport));
        start.owner.transition(|published| {
            let mut lifecycle = self.lock_transport_lifecycle();
            if !lifecycle.owns_start(&start) {
                return;
            }

            let mut connected = self.service().summary();
            if !connected.enabled {
                connected.status = "stopped".to_string();
                connected.message = "Remote Host stopped.".to_string();
                self.commit_transport_lifecycle(&mut lifecycle, |lifecycle, _| {
                    lifecycle.stop();
                    connected.pairing = None;
                    (connected, ())
                });
                return;
            }

            connected.status = "connected".to_string();
            connected.message = "Remote transport connected.".to_string();
            self.commit_transport_lifecycle(&mut lifecycle, |lifecycle, current| {
                lifecycle.starting = None;
                lifecycle.active = Some(RemoteTransportBinding {
                    start: start.clone(),
                    transport,
                    relay_authentication,
                });
                connected.pairing = current.pairing.clone();
                crate::runtime_trace::runtime_trace(
                    "remote",
                    &format!("transport_connected kind={}", transport_kind.as_str()),
                );
                (connected, ())
            });
            *published = true;
            result = None;
        });
        result
    }

    pub(super) fn prepare_transport_reconnect_after_disconnect(
        &self,
        current_start: &RemoteTransportStart,
    ) -> Option<(Option<Arc<dyn RemoteTransport>>, RemoteTransportStart)> {
        let mut result = None;
        current_start.owner.transition(|published| {
            let mut lifecycle = self.lock_transport_lifecycle();
            if !lifecycle.is_owned_by(&current_start.owner) {
                return;
            }
            *published = false;
            let mut active_pairing = self
                .active_pairing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *active_pairing = None;
            let mut pending_pairings = self
                .pending_pairings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending_pairings.clear();
            let mut status = self.service().summary();
            if !status.enabled {
                status.status = "stopped".to_string();
                status.message = "Remote Host stopped.".to_string();
                let transport = self.commit_transport_lifecycle(&mut lifecycle, |lifecycle, _| {
                    let transport = lifecycle.stop();
                    status.pairing = None;
                    (status, transport)
                });
                result = Some((transport, None));
                return;
            }
            status.status = "connecting".to_string();
            status.message = "Remote transport disconnected. Reconnecting...".to_string();
            let restart = self.commit_transport_lifecycle(&mut lifecycle, |lifecycle, _| {
                let transport = lifecycle.retire();
                let start = lifecycle.begin_start();
                status.pairing = None;
                (status, (transport, start))
            });
            result = Some((restart.0, Some(restart.1)));
        });
        match result {
            Some((transport, Some(start))) => Some((transport, start)),
            Some((Some(transport), None)) => {
                crate::async_runtime::spawn(async move {
                    transport.shutdown().await;
                });
                None
            }
            _ => None,
        }
    }

    pub(super) fn prepare_transport_for_pairing(
        &self,
    ) -> Result<Option<(Option<Arc<dyn RemoteTransport>>, RemoteTransportStart)>, String> {
        let mut lifecycle = self.lock_transport_lifecycle();
        let mut status = self.service().summary();
        if !status.enabled {
            return Err("Remote Host is disabled.".to_string());
        }
        if lifecycle.active.is_some() {
            return Ok(None);
        }
        if let Some(start) = lifecycle.starting.clone() {
            return Ok(Some((None, start)));
        }
        status.status = "connecting".to_string();
        status.message = "Connecting remote transport...".to_string();
        status.pairing = None;
        status.pending_pairing_list.clear();
        status.pending_pairings = 0;
        let start = self.commit_transport_lifecycle(&mut lifecycle, |lifecycle, _| {
            (status, lifecycle.begin_start())
        });
        Ok(Some((None, start)))
    }

    pub(super) fn handle_transport_state(
        self: &Arc<Self>,
        start: RemoteTransportStart,
        device_id: String,
        state: String,
    ) {
        if !device_id.trim().is_empty() {
            if state == "connected" {
                self.update_device_online(Some(&device_id), true);
            } else if matches!(state.as_str(), "closed" | "failed" | "disconnected") {
                self.update_device_online(Some(&device_id), false);
                self.clear_remote_project_scope(Some(&device_id));
                self.remove_terminal_viewer(Some(&device_id));
            }
            return;
        }
        if matches!(state.as_str(), "closed" | "failed" | "disconnected") {
            if let Some((transport, restart_generation)) =
                self.prepare_transport_reconnect_after_disconnect(&start)
            {
                crate::runtime_trace::runtime_trace(
                    "remote",
                    &format!(
                        "host_transport_disconnected state={state} generation={} ",
                        start.generation
                    ),
                );
                self.release_all_remote_viewports();
                self.spawn_transport_restart(transport, restart_generation);
            }
        }
    }

    pub(super) async fn ensure_transport_ready(
        self: &Arc<Self>,
        start: RemoteTransportStart,
    ) -> Result<(), String> {
        let _serial = self.transport_start_serial.lock().await;
        {
            let lifecycle = self.lock_transport_lifecycle();
            if lifecycle.has_active_start(&start) {
                return Ok(());
            }
            if !lifecycle.owns_start(&start) {
                return Err("Remote transport start was superseded.".to_string());
            }
            debug_assert!(lifecycle.active.is_none());
        }

        let result = self.start_remote_transport(start.clone()).await;
        if let Err(error) = &result {
            self.publish_transport_failure(&start, error.clone());
        }
        result
    }

    pub(super) fn transport_candidates_snapshot(&self) -> Vec<RemoteTransportCandidate> {
        let binding = self.lock_transport_lifecycle().active.clone();
        binding
            .as_ref()
            .map(Self::transport_candidates_for_binding)
            .unwrap_or_default()
    }

    pub(super) fn transport_candidates_for_binding(
        binding: &RemoteTransportBinding,
    ) -> Vec<RemoteTransportCandidate> {
        binding
            .transport
            .current_dial_candidate()
            .map(|candidate| {
                vec![
                    codux_protocol::iroh_transport_candidate_with_ticket_and_authentication(
                        candidate.url,
                        candidate.node_id,
                        candidate.relay_url,
                        candidate.ticket,
                        &binding.relay_authentication,
                    ),
                ]
            })
            .unwrap_or_default()
    }

    pub(super) async fn start_remote_transport(
        self: &Arc<Self>,
        start: RemoteTransportStart,
    ) -> Result<(), String> {
        crate::runtime_trace::runtime_trace(
            "remote",
            &format!("transport_start kind=iroh generation={}", start.generation),
        );
        let settings = {
            let lifecycle = self.lock_transport_lifecycle();
            if !lifecycle.owns_start(&start) {
                return Err("Remote transport start was superseded.".to_string());
            }
            let service = self.service();
            let settings = service.update_raw_settings_sync(|raw| {
                let mut settings = service.register_host_in_raw(raw)?;
                if !settings.is_enabled {
                    return Err("Remote Host is disabled.".to_string());
                }
                for device in &mut settings.cached_devices {
                    device.online = Some(false);
                }
                raw.insert(
                    "remote".to_string(),
                    serde_json::to_value(&settings).map_err(|error| error.to_string())?,
                );
                Ok(settings)
            })?;
            settings
        };
        let callbacks = RemoteHostCallbackAdapter::new(self, start.clone());
        let message_callbacks = callbacks.clone();
        let upload_callbacks = callbacks.clone();
        let state_callbacks = callbacks.clone();
        let pairing_callbacks = callbacks.clone();
        let authorize_callbacks = callbacks.clone();
        let tunnel_callbacks = callbacks;
        let transport = RemoteTransportFactory::connect_host(
            &settings,
            Arc::new(move |device_id, data| {
                let callbacks = message_callbacks.clone();
                crate::async_runtime::spawn(async move {
                    callbacks.handle_message(device_id, data);
                });
            }),
            Arc::new(move |upload| upload_callbacks.handle_upload(upload)),
            Arc::new(move |device_id, state| state_callbacks.handle_state(device_id, state)),
            Arc::new(move |handshake| pairing_callbacks.handle_pairing(handshake)),
            Arc::new(move |device_id, device_token| {
                authorize_callbacks.authorize(device_id, device_token)
            }),
            Some(Arc::new(move |request| {
                tunnel_callbacks.authorize_web_tunnel(request)
            })),
        )
        .await?;
        let stale_transport = self.publish_started_transport(
            start,
            transport,
            settings.relay_authentication.trim().to_string(),
        );
        if let Some(transport) = stale_transport {
            transport.shutdown().await;
            return Err("Remote transport start was superseded.".to_string());
        }
        Ok(())
    }

    pub(super) fn handle_transport_message(self: Arc<Self>, device_id: String, data: Vec<u8>) {
        let Ok(mut raw) = serde_json::from_slice::<RemoteEnvelope>(&data) else {
            crate::runtime_trace::runtime_trace(
                "remote",
                &format!(
                    "drop incoming reason=decode device={device_id} bytes={}",
                    data.len()
                ),
            );
            return;
        };
        if raw
            .device_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
            && !device_id.trim().is_empty()
        {
            raw.device_id = Some(device_id.clone());
        }
        let envelope = {
            if let Some(seq) = raw.seq {
                let Ok(mut received) = self.receive_seq_by_device.lock() else {
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "drop incoming reason=sequence_lock device={device_id} kind={}",
                            raw.kind
                        ),
                    );
                    return;
                };
                let guard = received
                    .entry(device_id.clone())
                    .or_insert_with(|| RemoteSequenceGuard::new(128));
                if !guard.accept(&raw.kind, raw.session_id.as_deref(), Some(seq)) {
                    crate::runtime_trace::runtime_trace(
                        "remote",
                        &format!(
                            "drop incoming reason=duplicate_seq device={device_id} kind={} seq={seq}",
                            raw.kind
                        ),
                    );
                    return;
                }
            }
            raw
        };
        crate::runtime_trace::runtime_trace(
            "remote",
            &format!(
                "recv kind={} transport_device={} envelope_device={} session={}",
                envelope.kind,
                device_id,
                envelope.device_id.as_deref().unwrap_or(""),
                envelope.session_id.as_deref().unwrap_or("")
            ),
        );
        self.update_device_online(envelope.device_id.as_deref(), true);
        self.handle_remote_envelope(envelope);
    }

    pub(super) fn handle_transport_upload(
        &self,
        upload: RemoteTransportUpload,
    ) -> Result<RemoteTransportUploadOutcome, String> {
        let device_id = upload.device_id.trim();
        let request_id = upload.request_id.as_deref().unwrap_or_default();
        crate::runtime_trace::runtime_trace(
            "remote",
            &format!(
                "upload recv device={} session={} request={} name={} bytes={}",
                device_id,
                upload.session_id,
                request_id,
                upload.name,
                upload.bytes.len()
            ),
        );
        if upload.session_id.trim().is_empty() {
            return Err("Terminal session is required.".to_string());
        }
        if upload.bytes.is_empty() || upload.bytes.len() > codux_protocol::REMOTE_BLOB_MAX_BYTES {
            return Err("Upload size is not supported.".to_string());
        }
        let name = sanitized_remote_upload_name(&upload.name);
        let kind = if upload.kind.trim().eq_ignore_ascii_case("image") {
            "image"
        } else {
            "file"
        };
        let outcome =
            self.insert_terminal_upload(&upload.session_id, &name, &upload.bytes, kind)?;
        crate::runtime_trace::runtime_trace(
            "remote",
            &format!(
                "upload stored device={} session={} path={}",
                device_id, upload.session_id, outcome.path
            ),
        );
        Ok(outcome)
    }

    pub(super) fn authorize_web_tunnel_tcp_connect(
        &self,
        request: WebTunnelTcpConnectRequest,
    ) -> Result<(), String> {
        if self.is_authorized_device_token(Some(&request.device_id), Some(&request.device_token)) {
            Ok(())
        } else {
            Err("device is not authorized".to_string())
        }
    }
}
