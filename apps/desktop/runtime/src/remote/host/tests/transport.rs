use super::*;

fn live_candidate_transport() -> Arc<CapturingTransport> {
    Arc::new(CapturingTransport::with_dial_candidate(
        codux_remote_transport::RemoteTransportCandidate {
            kind: codux_protocol::REMOTE_TRANSPORT_IROH.to_string(),
            url: "https://live.example".to_string(),
            node_id: "live-node".to_string(),
            relay_url: "https://live-relay.example".to_string(),
            ticket: "live-ticket".to_string(),
            relay_authentication: String::new(),
        },
    ))
}

struct BlockingFetchTransport {
    fetch_started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    fetch_release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    dropped: std::sync::mpsc::Sender<()>,
}

impl Drop for BlockingFetchTransport {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

#[async_trait::async_trait]
impl RemoteTransport for BlockingFetchTransport {
    fn kind(&self) -> RemoteTransportKind {
        RemoteTransportKind::Iroh
    }

    fn send(&self, _data: Vec<u8>, _device_id: Option<&str>) -> bool {
        true
    }

    async fn fetch_blob(&self, _ticket: &str) -> Result<Vec<u8>, String> {
        if let Some(started) = self.fetch_started.lock().ok().and_then(|mut tx| tx.take()) {
            let _ = started.send(());
        }
        let release = self
            .fetch_release
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.take())
            .ok_or_else(|| "missing fetch release".to_string())?;
        release
            .await
            .map_err(|_| "fetch release dropped".to_string())?;
        Ok(b"stale transport bytes".to_vec())
    }

    async fn shutdown(&self) {}
}

#[test]
fn host_transport_disconnect_clears_stale_transport_and_enters_reconnect() {
    let support_dir = temp_support_dir("codux-remote-host-reconnect");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let current =
        runtime.set_transport_lifecycle_for_test(7, Arc::new(CapturingTransport::default()));

    let restart = runtime.prepare_transport_reconnect_after_disconnect(&current);

    assert!(restart.is_some());
    let (_, restart_generation) = restart.expect("restart generation");
    assert_eq!(restart_generation.generation, 8);
    assert_eq!(runtime.transport_generation_for_test(), 8);
    assert!(!runtime.has_transport());
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.status, "connecting");
    assert_eq!(
        snapshot.message,
        "Remote transport disconnected. Reconnecting..."
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn stale_host_transport_disconnect_does_not_clear_current_transport() {
    let support_dir = temp_support_dir("codux-remote-host-reconnect-stale");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let stale =
        runtime.set_transport_lifecycle_for_test(7, Arc::new(CapturingTransport::default()));
    let (retired, current) = runtime
        .prepare_transport_reconnect_after_disconnect(&stale)
        .expect("current transport start");
    drop(retired);
    assert!(
        runtime
            .publish_started_transport(
                current,
                Arc::new(CapturingTransport::default()),
                String::new(),
            )
            .is_none()
    );

    let restart = runtime.prepare_transport_reconnect_after_disconnect(&stale);

    assert!(restart.is_none());
    assert_eq!(runtime.transport_generation_for_test(), 8);
    assert!(runtime.has_transport());

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn pairing_preparation_reuses_live_host_transport() {
    let support_dir = temp_support_dir("codux-remote-host-pairing-reuse");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    runtime.set_transport_lifecycle_for_test(11, Arc::new(CapturingTransport::default()));

    let preparation = runtime
        .prepare_transport_for_pairing()
        .expect("prepare pairing transport");

    assert!(preparation.is_none());
    assert_eq!(runtime.transport_generation_for_test(), 11);
    assert!(runtime.has_transport());

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn pairing_preparation_joins_current_transport_start() {
    let support_dir = temp_support_dir("codux-remote-host-pairing-join-start");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());

    let (_, generation) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare first pairing")
        .expect("transport start needed");
    let (retired, joined_generation) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare concurrent pairing")
        .expect("join current start");

    assert!(retired.is_none());
    assert!(Arc::ptr_eq(&joined_generation.owner, &generation.owner));
    assert_eq!(
        runtime.transport_generation_for_test(),
        generation.generation
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn pairing_start_joiner_accepts_transport_published_by_leader() {
    let support_dir = temp_support_dir("codux-remote-host-pairing-join-published");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let (_, start) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare pairing transport")
        .expect("transport start needed");
    let transport: Arc<dyn RemoteTransport> = Arc::new(CapturingTransport::default());

    assert!(
        runtime
            .publish_started_transport(start.clone(), Arc::clone(&transport), String::new(),)
            .is_none()
    );

    crate::async_runtime::block_on(runtime.ensure_transport_ready(start))
        .expect("joiner must accept leader's published transport");
    assert!(Arc::ptr_eq(
        &runtime.current_transport().expect("active transport"),
        &transport,
    ));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn pairing_start_joiner_rejects_same_generation_from_another_owner() {
    let support_dir = temp_support_dir("codux-remote-host-pairing-join-wrong-owner");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let transport: Arc<dyn RemoteTransport> = Arc::new(CapturingTransport::default());
    let active = runtime.set_transport_for_test(Arc::clone(&transport));
    let forged = RemoteTransportStart {
        generation: active.generation,
        owner: Arc::new(RemoteTransportCallbackOwner::default()),
    };

    let error = crate::async_runtime::block_on(runtime.ensure_transport_ready(forged))
        .expect_err("same generation from another owner must be stale");

    assert_eq!(error, "Remote transport start was superseded.");
    assert!(Arc::ptr_eq(
        &runtime.current_transport().expect("active transport"),
        &transport,
    ));
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn callbacks_are_inert_until_their_transport_is_published() {
    let support_dir = temp_support_dir("codux-remote-host-callback-publication");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let (_, start) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare transport")
        .expect("transport start");
    let callbacks = runtime.callback_adapter_for_test(start.clone());
    let connected = serde_json::to_vec(&RemoteOutgoingEnvelope {
        kind: REMOTE_DEVICE_CONNECTED.to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: None,
        seq: None,
        payload: json!({}),
    })
    .unwrap();

    callbacks.handle_message("device-1".to_string(), connected.clone());
    assert_eq!(runtime.snapshot().online_devices, 0);

    assert!(
        runtime
            .publish_started_transport(
                start,
                Arc::new(CapturingTransport::default()),
                String::new(),
            )
            .is_none()
    );
    callbacks.handle_message("device-1".to_string(), connected);
    assert_eq!(runtime.snapshot().online_devices, 1);

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn retired_message_task_cannot_publish_through_replacement() {
    let support_dir = temp_support_dir("codux-remote-host-retired-callback");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let (_, first_start) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare first transport")
        .expect("first transport start");
    let first_callbacks = runtime.callback_adapter_for_test(first_start.clone());
    assert!(
        runtime
            .publish_started_transport(
                first_start.clone(),
                Arc::new(CapturingTransport::default()),
                "first-auth".to_string(),
            )
            .is_none()
    );
    let (_, replacement_start) = runtime
        .prepare_transport_reconnect_after_disconnect(&first_start)
        .expect("replacement start");
    let replacement = Arc::new(CapturingTransport::default());
    assert!(
        runtime
            .publish_started_transport(
                replacement_start,
                replacement.clone(),
                "replacement-auth".to_string(),
            )
            .is_none()
    );
    let request = serde_json::to_vec(&RemoteOutgoingEnvelope {
        kind: REMOTE_HOST_INFO.to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: Some("stale-host-info".to_string()),
        seq: None,
        payload: json!({}),
    })
    .unwrap();

    first_callbacks.handle_message("device-1".to_string(), request);

    assert!(replacement.take_messages().is_empty());
    let settings = remote_settings_from_raw(&runtime.service().raw_settings());
    assert_ne!(settings.cached_devices[0].online, Some(true));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn retired_blob_write_cannot_commit_or_reply_through_replacement() {
    let support_dir = temp_support_dir("codux-remote-host-retired-blob-write");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let (fetch_started_tx, fetch_started_rx) = std::sync::mpsc::channel();
    let (fetch_release_tx, fetch_release_rx) = tokio::sync::oneshot::channel();
    let mut fetch_release_tx = Some(fetch_release_tx);
    let (source_dropped_tx, source_dropped_rx) = std::sync::mpsc::channel();
    let source = Arc::new(BlockingFetchTransport {
        fetch_started: Mutex::new(Some(fetch_started_tx)),
        fetch_release: Mutex::new(Some(fetch_release_rx)),
        dropped: source_dropped_tx,
    });
    let start = runtime.set_transport_for_test(source.clone());
    let callbacks = runtime.callback_adapter_for_test(start.clone());
    let destination = support_dir.join("received");
    let request = serde_json::to_vec(&RemoteOutgoingEnvelope {
        kind: REMOTE_FILE_WRITE_BLOB.to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: Some("blob-write-1".to_string()),
        seq: None,
        payload: json!({
            "directory": destination.to_string_lossy(),
            "name": "stale.txt",
            "ticket": "source-ticket",
        }),
    })
    .unwrap();

    callbacks.handle_message("device-1".to_string(), request);
    fetch_started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("source fetch started");

    let replacing_runtime = Arc::clone(&runtime);
    let replacing_start = start.clone();
    let (replacement_tx, replacement_rx) = std::sync::mpsc::channel();
    let replacement_worker = std::thread::spawn(move || {
        let result =
            replacing_runtime.prepare_transport_reconnect_after_disconnect(&replacing_start);
        let _ = replacement_tx.send(result);
    });
    let replacement = match replacement_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Some(replacement)) => replacement,
        _ => {
            fetch_release_tx
                .take()
                .expect("fetch release")
                .send(())
                .ok();
            replacement_worker.join().ok();
            panic!("replacement must not wait on read-only fetch");
        }
    };
    replacement_worker.join().unwrap();
    let (retired, replacement_start) = replacement;
    let replacement = Arc::new(CapturingTransport::default());
    assert!(
        runtime
            .publish_started_transport(replacement_start, replacement.clone(), String::new(),)
            .is_none()
    );
    drop(retired);
    drop(source);
    fetch_release_tx
        .take()
        .expect("fetch release")
        .send(())
        .expect("release source fetch");
    source_dropped_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("source task completed");

    assert!(!destination.join("stale.txt").exists());
    assert!(replacement.take_messages().is_empty());
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn stopped_transport_cannot_publish_prepared_pairing() {
    let support_dir = temp_support_dir("codux-remote-host-stopped-pairing");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    runtime.set_transport_for_test(live_candidate_transport());
    let prepared = runtime.prepare_pairing().expect("prepare pairing");
    let pairing = prepared.info.clone();

    runtime.stop_with_message("Stopped before pairing publication.");
    let error = runtime
        .commit_prepared_pairing(prepared)
        .expect_err("retired pairing must not publish");

    assert_eq!(
        error,
        "Remote transport changed before pairing was published."
    );
    assert_eq!(runtime.snapshot().status, "stopped");
    assert!(runtime.snapshot().pairing.is_none());
    assert!(runtime.poll_pairing_status(&pairing).unwrap().finished);

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn replaced_transport_cannot_publish_prepared_pairing() {
    let support_dir = temp_support_dir("codux-remote-host-replaced-pairing");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let start = runtime.set_transport_for_test(live_candidate_transport());
    let prepared = runtime.prepare_pairing().expect("prepare pairing");
    let pairing = prepared.info.clone();

    runtime
        .prepare_transport_reconnect_after_disconnect(&start)
        .expect("replacement start");
    let error = runtime
        .commit_prepared_pairing(prepared)
        .expect_err("replaced pairing must not publish");

    assert_eq!(
        error,
        "Remote transport changed before pairing was published."
    );
    assert_eq!(runtime.snapshot().status, "connecting");
    assert!(runtime.snapshot().pairing.is_none());
    assert!(runtime.poll_pairing_status(&pairing).unwrap().finished);

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn stop_cannot_split_transport_ownership_check_from_publication() {
    let support_dir = temp_support_dir("codux-remote-host-publish-stop-race");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let (_, generation) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare transport start")
        .expect("transport start needed");
    let transport: Arc<dyn RemoteTransport> = Arc::new(CapturingTransport::default());
    let snapshot_guard = runtime.snapshot.lock().expect("snapshot lock");

    let publishing_runtime = Arc::clone(&runtime);
    let publisher = std::thread::spawn(move || {
        publishing_runtime.publish_started_transport(generation, transport, String::new())
    });
    for _ in 0..1000 {
        if runtime.transport_lifecycle.try_lock().is_err() {
            break;
        }
        std::thread::yield_now();
    }
    assert!(
        runtime.transport_lifecycle.try_lock().is_err(),
        "publisher must hold lifecycle owner before test starts stop"
    );

    let (stop_entered_tx, stop_entered_rx) = std::sync::mpsc::channel();
    let (stop_done_tx, stop_done_rx) = std::sync::mpsc::channel();
    let stopping_runtime = Arc::clone(&runtime);
    let stopper = std::thread::spawn(move || {
        stop_entered_tx.send(()).unwrap();
        stopping_runtime.stop_with_message("Stopped during publication.");
        stop_done_tx.send(()).unwrap();
    });
    stop_entered_rx.recv().unwrap();
    assert!(
        stop_done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "stop must wait for ownership check and publication to finish together"
    );

    drop(snapshot_guard);
    assert!(publisher.join().unwrap().is_none());
    stopper.join().unwrap();
    assert!(!runtime.has_transport());
    assert_eq!(runtime.snapshot().status, "stopped");
    assert_eq!(runtime.snapshot().message, "Stopped during publication.");

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn superseded_a_b_a_start_cannot_publish_old_transport() {
    let support_dir = temp_support_dir("codux-remote-host-start-a-b-a");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let first_a = runtime.begin_transport_start_for_test();
    let b = runtime.begin_transport_start_for_test();
    let latest_a = runtime.begin_transport_start_for_test();
    assert!(first_a.generation < b.generation && b.generation < latest_a.generation);

    let stale: Arc<dyn RemoteTransport> = Arc::new(CapturingTransport::default());
    let current: Arc<dyn RemoteTransport> = Arc::new(CapturingTransport::default());
    assert!(
        runtime
            .publish_started_transport(first_a, Arc::clone(&stale), String::new())
            .is_some()
    );
    assert!(!runtime.has_transport());
    assert!(
        runtime
            .publish_started_transport(latest_a, Arc::clone(&current), String::new())
            .is_none()
    );
    assert!(Arc::ptr_eq(
        &runtime.current_transport().expect("current transport"),
        &current
    ));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn superseded_start_does_not_persist_registration_side_effects() {
    let support_dir = temp_support_dir("codux-remote-host-stale-registration");
    fs::write(
        support_dir.join("settings.json"),
        serde_json::to_string_pretty(&json!({
            "remote": {
                "isEnabled": true,
                "relayUrl": "http://relay.example",
                "hostID": "",
                "hostToken": "",
                "cachedDevices": [{
                    "id": "device-1",
                    "token": "device-token-1",
                    "name": "Phone",
                    "online": true
                }]
            }
        }))
        .unwrap(),
    )
    .expect("write settings");
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let (_, stale_generation) = runtime
        .prepare_transport_for_pairing()
        .expect("prepare stale start")
        .expect("transport start needed");
    let current_generation = runtime.begin_transport_start_for_test();

    let error = crate::async_runtime::block_on(runtime.ensure_transport_ready(stale_generation))
        .expect_err("stale start is rejected");

    assert_eq!(error, "Remote transport start was superseded.");
    let settings = remote_settings_from_raw(&runtime.service().raw_settings());
    assert!(settings.host_id.is_empty());
    assert!(settings.host_token.is_empty());
    assert_eq!(settings.cached_devices[0].online, Some(true));
    assert_eq!(
        runtime.transport_generation_for_test(),
        current_generation.generation
    );

    fs::remove_dir_all(support_dir).ok();
}

#[cfg(unix)]
#[test]
fn terminal_upload_accepts_legacy_request_without_id() {
    let support_dir = temp_support_dir("codux-remote-legacy-upload");
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let session_id = format!("desktop-legacy-upload-{}", uuid::Uuid::new_v4());
    runtime
        .terminals
        .create(
            TerminalPtyConfig {
                terminal_id: Some(session_id.clone()),
                shell: Some("/bin/sh".to_string()),
                command: Some("cat".to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal");

    let outcome = runtime
        .handle_transport_upload(RemoteTransportUpload {
            device_id: "device-1".to_string(),
            session_id: session_id.clone(),
            request_id: None,
            name: "proof.txt".to_string(),
            mime: "text/plain".to_string(),
            kind: "file".to_string(),
            bytes: b"proof".to_vec(),
            ticket: String::new(),
        })
        .expect("legacy upload");

    assert_eq!(outcome.name, "proof.txt");
    assert_eq!(fs::read(&outcome.path).expect("stored upload"), b"proof");
    let _ = runtime
        .terminals
        .kill_and_wait(&session_id, std::time::Duration::from_secs(2));
    fs::remove_dir_all(remote_terminal_upload_directory(&session_id)).ok();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn terminal_upload_pty_failure_removes_the_owned_file() {
    let support_dir = temp_support_dir("codux-remote-upload-pty-failure");
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let session_id = format!("missing-session-{}", uuid::Uuid::new_v4());
    let upload_directory = remote_terminal_upload_directory(&session_id);

    let error = runtime
        .handle_transport_upload(RemoteTransportUpload {
            device_id: "device-1".to_string(),
            session_id: session_id.clone(),
            request_id: Some("upload-1".to_string()),
            name: "proof.txt".to_string(),
            mime: "text/plain".to_string(),
            kind: "file".to_string(),
            bytes: b"proof".to_vec(),
            ticket: String::new(),
        })
        .expect_err("missing PTY must fail");

    assert!(!error.trim().is_empty());
    assert!(
        fs::read_dir(&upload_directory)
            .expect("upload directory")
            .next()
            .is_none(),
        "failed PTY write must remove only its newly-created upload"
    );

    fs::remove_dir_all(upload_directory).ok();
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn authorization_rejects_unknown_device_credentials() {
    let support_dir = temp_support_dir("codux-remote-unauthorized-repair");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    assert!(!runtime.is_authorized_device_token(Some("unknown-device"), Some("device-token-1"),));
    assert!(!runtime.is_authorized_device_token(Some("device-1"), Some("unknown-token"),));
    assert!(runtime.is_authorized_device_token(Some("device-1"), Some("device-token-1")));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn authorization_rejects_missing_device_credentials() {
    let support_dir = temp_support_dir("codux-remote-unauthorized-transport-device-repair");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    assert!(!runtime.is_authorized_device_token(None, Some("device-token-1")));
    assert!(!runtime.is_authorized_device_token(Some("device-1"), None));
    assert!(!runtime.is_authorized_device_token(Some(""), Some("device-token-1")));
    assert!(!runtime.is_authorized_device_token(Some("device-1"), Some("")));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn pairing_credentials_are_consumed_once_under_concurrency() {
    let support_dir = temp_support_dir("codux-remote-pairing-single-use");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let pairing = RemotePairingInfo {
        pairing_id: "pair-1".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    let owner = runtime.set_active_pairing_for_test(pairing);
    let handshake = RemoteTransportPairingRequest {
        device_id: "phone-1".to_string(),
        device_name: "Phone".to_string(),
        platform: Some("ios".to_string()),
        pairing_id: Some("pair-1".to_string()),
        pairing_code: Some("123456".to_string()),
        pairing_secret: Some("secret".to_string()),
    };
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let runtime = Arc::clone(&runtime);
        let owner = Arc::clone(&owner);
        let handshake = handshake.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            runtime.handle_transport_pairing_request(&owner, handshake)
        }));
    }
    barrier.wait();

    let responses = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(responses.len(), 1);
    let token = responses[0]["token"].as_str().unwrap();
    assert!(runtime.is_authorized_device_token(Some("phone-1"), Some(token)));
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn relay_reconfigure_waits_for_pairing_callback_before_mutating_settings() {
    let support_dir = temp_support_dir("codux-remote-relay-pairing-race");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    runtime.set_transport_for_test(Arc::new(CapturingTransport::default()));
    let pairing = RemotePairingInfo {
        pairing_id: "pair-relay-change".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    let owner = runtime.set_active_pairing_for_test(pairing);
    let handshake = RemoteTransportPairingRequest {
        device_id: "phone-during-relay-change".to_string(),
        device_name: "Phone".to_string(),
        platform: Some("ios".to_string()),
        pairing_id: Some("pair-relay-change".to_string()),
        pairing_code: Some("123456".to_string()),
        pairing_secret: Some("secret".to_string()),
    };

    let settings_store = crate::config::ConfigStore::for_file(crate::config::settings_file_path(
        support_dir.clone(),
    ));
    let (settings_locked_tx, settings_locked_rx) = std::sync::mpsc::channel();
    let (release_settings_tx, release_settings_rx) = std::sync::mpsc::channel();
    let settings_blocker = std::thread::spawn(move || {
        settings_store.update_sync(|_| {
            settings_locked_tx.send(()).unwrap();
            release_settings_rx.recv().unwrap();
            Ok(())
        })
    });
    settings_locked_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("settings write lock acquired");

    let pairing_runtime = Arc::clone(&runtime);
    let pairing_owner = Arc::clone(&owner);
    let pairing_callback = std::thread::spawn(move || {
        pairing_owner
            .run(|| pairing_runtime.handle_transport_pairing_request(&pairing_owner, handshake))
            .flatten()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if owner.state.lock().expect("callback owner state").in_flight == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pairing callback did not acquire ownership"
        );
        std::thread::yield_now();
    }

    let reconfigure_runtime = Arc::clone(&runtime);
    let reconfigure = std::thread::spawn(move || {
        let mutation_runtime = Arc::clone(&reconfigure_runtime);
        reconfigure_runtime.reconfigure_transport(move || {
            mutation_runtime.service().update_raw_settings_sync(|raw| {
                let remote = raw
                    .get_mut("remote")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| "Remote settings are invalid.".to_string())?;
                remote.insert(
                    "relayUrl".to_string(),
                    Value::String("https://new-relay.example".to_string()),
                );
                remote.insert("cachedDevices".to_string(), json!([]));
                remote.insert("isEnabled".to_string(), Value::Bool(false));
                Ok(())
            })
        })
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let state = owner.state.lock().expect("callback owner state");
        if !state.published {
            assert_eq!(state.in_flight, 1);
            break;
        }
        drop(state);
        assert!(
            std::time::Instant::now() < deadline,
            "relay reconfigure did not revoke callback owner"
        );
        std::thread::yield_now();
    }

    release_settings_tx.send(()).unwrap();
    settings_blocker.join().unwrap().unwrap();
    assert!(pairing_callback.join().unwrap().is_some());
    let (result, _) = reconfigure.join().unwrap();
    result.expect("reconfigure relay");

    let settings = remote_settings_from_raw(&runtime.service().raw_settings());
    assert_eq!(settings.relay_url, "https://new-relay.example");
    assert!(settings.cached_devices.is_empty());
    assert_eq!(runtime.snapshot().status, "stopped");
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn failed_reconfigure_retires_old_owner_and_follows_persisted_settings() {
    let support_dir = temp_support_dir("codux-remote-relay-partial-failure");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let start = runtime.set_transport_for_test(Arc::new(CapturingTransport::default()));
    let mutation_runtime = Arc::clone(&runtime);

    let (result, _) = runtime.reconfigure_transport(move || {
        mutation_runtime.service().update_raw_settings_sync(|raw| {
            let remote = raw
                .get_mut("remote")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| "Remote settings are invalid.".to_string())?;
            remote.insert("isEnabled".to_string(), Value::Bool(false));
            Ok(())
        })?;
        Err::<(), _>("settings side effect failed".to_string())
    });
    let error = result.expect_err("reconfigure must return mutation failure");

    assert_eq!(error, "settings side effect failed");
    assert!(!start.owner.state.lock().unwrap().published);
    assert!(!runtime.has_transport());
    assert_eq!(runtime.snapshot().status, "stopped");
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn expired_pairing_poll_consumes_the_ticket() {
    let support_dir = temp_support_dir("codux-remote-pairing-expired");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let pairing = RemotePairingInfo {
        pairing_id: "pair-expired".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    runtime.set_active_pairing_for_test(pairing.clone());

    let result = runtime.poll_pairing_status(&pairing).unwrap();

    assert!(result.finished);
    assert_eq!(result.summary.message, "Pairing expired.");
    assert!(result.summary.pairing.is_none());
    assert!(runtime.active_pairing_is_none_for_test());
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn cancelled_pairing_poll_does_not_report_confirmation() {
    let support_dir = temp_support_dir("codux-remote-pairing-cancelled");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let pairing = RemotePairingInfo {
        pairing_id: "pair-cancelled".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    runtime.set_active_pairing_for_test(pairing.clone());

    runtime.cancel_pairing(&pairing.pairing_id).unwrap();
    let result = runtime.poll_pairing_status(&pairing).unwrap();

    assert!(result.finished);
    assert_eq!(result.summary.message, "Pairing cancelled.");
    assert!(result.summary.pairing.is_none());
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn stopped_transport_cannot_be_republished_connected_by_pairing_cancel() {
    let support_dir = temp_support_dir("codux-remote-pairing-cancel-after-stop");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    runtime.set_transport_for_test(Arc::new(CapturingTransport::default()));
    let pairing = RemotePairingInfo {
        pairing_id: "pair-stopped".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    runtime.set_active_pairing_for_test(pairing.clone());

    runtime.stop_with_message("Stopped before pairing cancellation.");
    let summary = runtime.cancel_pairing(&pairing.pairing_id).unwrap();

    assert_eq!(summary.status, "stopped");
    assert_eq!(runtime.snapshot().status, "stopped");
    assert_eq!(
        runtime.snapshot().message,
        "Stopped before pairing cancellation."
    );
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn stop_waits_for_pairing_cancel_publication_then_wins() {
    let support_dir = temp_support_dir("codux-remote-pairing-cancel-stop-race");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    runtime.set_transport_for_test(Arc::new(CapturingTransport::default()));
    let pairing = RemotePairingInfo {
        pairing_id: "pair-cancelling".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    let owner = runtime.set_active_pairing_for_test(pairing.clone());
    let pending_guard = runtime
        .pending_pairings
        .lock()
        .expect("pending pairing lock");
    let cancelling_runtime = Arc::clone(&runtime);
    let cancel = std::thread::spawn(move || cancelling_runtime.cancel_pairing(&pairing.pairing_id));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while runtime.active_pairing.try_lock().is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "pairing cancel did not acquire active pairing ownership"
        );
        std::thread::yield_now();
    }

    let stopping_runtime = Arc::clone(&runtime);
    let stop = std::thread::spawn(move || {
        stopping_runtime.stop_with_message("Stopped during pairing cancellation.");
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let state = owner.state.lock().expect("callback owner state");
        if !state.published {
            assert_eq!(state.in_flight, 1);
            break;
        }
        drop(state);
        assert!(
            std::time::Instant::now() < deadline,
            "stop did not revoke the pairing owner"
        );
        std::thread::yield_now();
    }

    drop(pending_guard);
    assert_eq!(cancel.join().unwrap().unwrap().status, "connected");
    stop.join().unwrap();
    assert_eq!(runtime.snapshot().status, "stopped");
    assert_eq!(
        runtime.snapshot().message,
        "Stopped during pairing cancellation."
    );
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn pairing_poll_waits_for_consumed_ticket_snapshot() {
    let support_dir = temp_support_dir("codux-remote-pairing-snapshot-race");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());
    let pairing = RemotePairingInfo {
        pairing_id: "pair-confirming".to_string(),
        code: "123456".to_string(),
        secret: "secret".to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(),
        qr_payload: String::new(),
    };
    let mut summary = runtime.snapshot();
    summary.pairing = Some(pairing.clone());
    runtime.update_snapshot(summary);

    let confirming = runtime.poll_pairing_status(&pairing).unwrap();

    assert!(!confirming.finished);
    assert_eq!(
        confirming
            .summary
            .pairing
            .as_ref()
            .map(|current| current.pairing_id.as_str()),
        Some("pair-confirming")
    );

    let mut confirmed = runtime.snapshot();
    confirmed.pairing = None;
    confirmed.message = "Pairing confirmed.".to_string();
    runtime.update_snapshot(confirmed);
    let result = runtime.poll_pairing_status(&pairing).unwrap();

    assert!(result.finished);
    assert_eq!(result.summary.message, "Pairing confirmed.");
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn host_metrics_request_replies_with_metrics_payload() {
    let support_dir = temp_support_dir("codux-remote-host-metrics");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let transport = Arc::new(CapturingTransport::default());
    runtime.set_transport_for_test(transport.clone());

    let raw = RemoteOutgoingEnvelope {
        kind: REMOTE_HOST_METRICS.to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: Some("metrics-1".to_string()),
        seq: None,
        payload: json!({}),
    };
    runtime.handle_transport_message("device-1".to_string(), serde_json::to_vec(&raw).unwrap());

    let (_, bytes) = transport
        .wait_for_message(|message| {
            serde_json::from_slice::<RemoteEnvelope>(&message.1)
                .map(|envelope| envelope.kind == REMOTE_HOST_METRICS)
                .unwrap_or(false)
        })
        .expect("host metrics reply");
    let envelope: RemoteEnvelope = serde_json::from_slice(&bytes).expect("metrics envelope");
    assert_eq!(envelope.kind, REMOTE_HOST_METRICS);
    assert_eq!(envelope.device_id.as_deref(), Some("device-1"));
    assert_eq!(envelope.request_id.as_deref(), Some("metrics-1"));
    let metrics: codux_protocol::RemoteHostMetrics =
        serde_json::from_value(envelope.payload).expect("metrics payload");
    assert!(metrics.sampled_at_millis > 0);
    assert!(metrics.processes.len() <= 30);

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn host_info_preserves_live_candidate_and_injects_current_authentication() {
    let support_dir = temp_support_dir("codux-remote-host-live-candidate");
    fs::write(
        support_dir.join("settings.json"),
        serde_json::to_string_pretty(&json!({
            "remote": {
                "isEnabled": true,
                "relayUrl": "https://configured.example",
                "relayAuthentication": "current-auth",
                "hostID": "host-1",
                "cachedDevices": [{
                    "id": "device-1",
                    "token": "device-token-1",
                    "hostId": "host-1",
                    "name": "Phone"
                }]
            }
        }))
        .unwrap(),
    )
    .expect("write settings");
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let transport = Arc::new(CapturingTransport::with_dial_candidate(
        codux_remote_transport::RemoteTransportCandidate {
            kind: codux_protocol::REMOTE_TRANSPORT_IROH.to_string(),
            url: "https://live.example".to_string(),
            node_id: "live-node".to_string(),
            relay_url: "https://live-relay.example".to_string(),
            ticket: "live-ticket".to_string(),
            relay_authentication: "stale-auth".to_string(),
        },
    ));
    runtime.set_transport_for_test(transport.clone());
    runtime
        .service()
        .update_raw_settings_sync(|raw| {
            raw.get_mut("remote")
                .and_then(Value::as_object_mut)
                .expect("remote settings")
                .insert(
                    "relayAuthentication".to_string(),
                    Value::String("later-auth".to_string()),
                );
            Ok(())
        })
        .expect("update later relay authentication");
    let request = RemoteOutgoingEnvelope {
        kind: REMOTE_HOST_INFO.to_string(),
        device_id: Some("device-1".to_string()),
        session_id: None,
        request_id: Some("host-info-1".to_string()),
        seq: None,
        payload: json!({}),
    };

    Arc::clone(&runtime).handle_transport_message(
        "device-1".to_string(),
        serde_json::to_vec(&request).unwrap(),
    );

    let (_, bytes) = transport
        .wait_for_message(|message| {
            serde_json::from_slice::<RemoteEnvelope>(&message.1)
                .is_ok_and(|envelope| envelope.kind == REMOTE_HOST_INFO)
        })
        .expect("host info reply");
    let envelope: RemoteEnvelope = serde_json::from_slice(&bytes).expect("host info envelope");
    assert_eq!(
        envelope.payload["transports"][0],
        json!({
            "kind": codux_protocol::REMOTE_TRANSPORT_IROH,
            "role": codux_protocol::REMOTE_TRANSPORT_ROLE_HOST,
            "url": "https://live.example",
            "nodeId": "live-node",
            "relayUrl": "https://live-relay.example",
            "ticket": "live-ticket",
            "relayAuthentication": "current-auth"
        })
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn web_tunnel_requires_paired_device_token() {
    let support_dir = temp_support_dir("codux-remote-web-tunnel-token");
    write_paired_remote_settings(&support_dir);
    let runtime = RemoteHostRuntime::new(support_dir.clone());

    assert!(
        runtime
            .authorize_web_tunnel_tcp_connect(WebTunnelTcpConnectRequest {
                device_id: "device-1".to_string(),
                device_token: "device-token-1".to_string(),
                host: "localhost".to_string(),
                port: 5173,
            })
            .is_ok()
    );
    assert!(
        runtime
            .authorize_web_tunnel_tcp_connect(WebTunnelTcpConnectRequest {
                device_id: "device-1".to_string(),
                device_token: "wrong-token".to_string(),
                host: "localhost".to_string(),
                port: 5173,
            })
            .is_err()
    );
    assert!(
        runtime
            .authorize_web_tunnel_tcp_connect(WebTunnelTcpConnectRequest {
                device_id: "unknown-device".to_string(),
                device_token: "device-token-1".to_string(),
                host: "localhost".to_string(),
                port: 5173,
            })
            .is_err()
    );

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn viewport_resize_reply_keeps_request_id() {
    let support_dir = temp_support_dir("codux-remote-viewport-request-id");
    write_paired_remote_settings(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let transport = Arc::new(CapturingTransport::default());
    runtime.set_transport_for_test(transport.clone());
    let session_id = terminals
        .create(
            TerminalPtyConfig {
                shell: Some("sh".to_string()),
                command: Some("sleep 1".to_string()),
                cwd: Some(support_dir.to_string_lossy().to_string()),
                ..Default::default()
            },
            |_| {},
        )
        .expect("create terminal");

    runtime.handle_terminal_viewport_resize(&RemoteEnvelope {
        kind: codux_protocol::REMOTE_TERMINAL_VIEWPORT_RESIZE.to_string(),
        device_id: Some("device-1".to_string()),
        session_id: Some(session_id),
        request_id: Some("resize-1".to_string()),
        seq: None,
        payload: json!({ "cols": 80, "rows": 24 }),
    });

    let (_, bytes) = transport
        .wait_for_message(|message| {
            serde_json::from_slice::<RemoteEnvelope>(&message.1).is_ok_and(|envelope| {
                envelope.kind == REMOTE_TERMINAL_VIEWPORT_STATE
                    && envelope.request_id.as_deref() == Some("resize-1")
            })
        })
        .expect("viewport reply");
    let envelope: RemoteEnvelope = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope.request_id.as_deref(), Some("resize-1"));

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn transport_ping_runtime_fallback_replies_plain_pong() {
    let support_dir = temp_support_dir("codux-remote-transport-ping-pong");
    write_paired_remote_settings(&support_dir);
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let transport = Arc::new(CapturingTransport::default());
    runtime.set_transport_for_test(transport.clone());

    Arc::clone(&runtime).handle_transport_message(
        "device-1".to_string(),
        json!({
            "type": REMOTE_TRANSPORT_PING,
            "deviceId": "device-1",
            "payload": { "id": "ping-1" },
        })
        .to_string()
        .into_bytes(),
    );

    let messages = transport.take_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0.as_deref(), Some("device-1"));
    let reply: Value = serde_json::from_slice(&messages[0].1).expect("plain pong json");
    assert_eq!(
        reply.get("type").and_then(Value::as_str),
        Some(REMOTE_TRANSPORT_PONG)
    );
    assert_eq!(
        reply.get("deviceId").and_then(Value::as_str),
        Some("device-1")
    );
    assert_eq!(reply["payload"]["id"], "ping-1");

    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn viewport_events_do_not_broadcast_terminal_list() {
    let support_dir = temp_support_dir("codux-remote-viewport-no-terminal-list");
    write_paired_remote_settings(&support_dir);
    let terminals = Arc::new(TerminalManager::new());
    let runtime = Arc::new(RemoteHostRuntime::new_with_ai_history_and_terminals(
        support_dir.clone(),
        Default::default(),
        Arc::clone(&terminals),
    ));
    let transport = Arc::new(CapturingTransport::default());
    runtime.set_transport_for_test(transport.clone());
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
    runtime.register_terminal_viewer(&session_id, Some("device-1"));

    transport.take_messages();
    runtime.handle_terminal_event(TerminalEvent::Viewport {
        session_id: session_id.clone(),
        owner: "desktop".to_string(),
        cols: 100,
        rows: 32,
        generation: 1,
    });

    let mut kinds = Vec::new();
    for (_, data) in transport.take_messages() {
        let text = String::from_utf8(data).expect("utf8 transport");
        let envelope = runtime
            .service()
            .parse_incoming_envelope(&text)
            .expect("parse outgoing envelope");
        kinds.push(envelope.kind);
    }

    assert_eq!(kinds, vec!["terminal.viewport.state"]);

    fs::remove_dir_all(support_dir).ok();
}
