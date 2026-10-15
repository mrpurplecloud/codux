use super::super::*;
use super::fixtures::*;

#[cfg(unix)]
#[test]
fn transcript_discovery_releases_state_lock_during_slow_io() {
    use std::{
        ffi::CString,
        fs::OpenOptions,
        io::Write,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        sync::Arc,
        time::{Duration, Instant},
    };

    let directory = std::env::temp_dir().join(format!("codux-discovery-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("session.jsonl");
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // A real driver read, held until this test releases the writer. No test
    // callback, clock threshold, or production API change is needed.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    let store = Arc::new(AIRuntimeStateStore::default());
    store.apply_hook(test_hook("promptSubmitted", 1000.0));
    {
        let mut core = store.core.lock().unwrap();
        let session = core.sessions.get_mut("terminal-1").unwrap();
        session.tool = "omp".to_string();
        session.project_path = Some(directory.to_string_lossy().into_owned());
        session.transcript_path = Some(directory.to_string_lossy().into_owned());
        session.ai_session_id = None;
        session.started_at = None;
    }
    let reader = std::thread::spawn({
        let store = Arc::clone(&store);
        move || store.transcript_monitored_sessions()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut writer = loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(writer) => break writer,
            Err(error)
                if error.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("discovery did not reach FIFO: {error}"),
        }
    };
    // Opening the writer proves the driver has reached the FIFO. Withhold EOF
    // while checking the state lock; do not infer lock contention from timing.
    let state_is_accessible = store.core.try_lock().is_ok();
    let (snapshot_count, removed) = if state_is_accessible {
        (
            store.snapshot().sessions.len(),
            store.remove_session("terminal-1"),
        )
    } else {
        (0, false)
    };
    let row = serde_json::json!({"type":"session", "id":"slow-session", "cwd":directory});
    writeln!(writer, "{row}").unwrap();
    drop(writer);
    let discovered = reader.join().unwrap();
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&directory).unwrap();

    assert!(
        state_is_accessible,
        "resource discovery held the UI state lock during I/O"
    );
    assert_eq!(snapshot_count, 1);
    assert!(removed);
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].terminal_id, "terminal-1");
    assert!(
        store.snapshot().sessions.is_empty(),
        "discovery must not resurrect a removed session"
    );
}

#[test]
fn transcript_discovery_preserves_resource_filtering() {
    let store = AIRuntimeStateStore::default();
    store.apply_hook(test_hook("promptSubmitted", 1000.0));
    let expected = {
        let mut core = store.core.lock().unwrap();
        let session = core.sessions.get_mut("terminal-1").unwrap();
        session.tool = "claude".to_string();
        session.transcript_path = Some("/transcripts/claude.jsonl".to_string());
        let expected = session.clone();
        let mut unsupported = expected.clone();
        unsupported.terminal_id = "unsupported".to_string();
        unsupported.tool = "not-a-runtime-tool".to_string();
        core.sessions
            .insert(unsupported.terminal_id.clone(), unsupported);
        expected
    };
    assert_eq!(store.transcript_monitored_sessions(), vec![expected]);
}

#[test]
fn first_prompt_notifies_when_terminal_already_has_detected_session() {
    // Process detection creates an idle session first; the first prompt event
    // then flips it to responding and notifies.
    let store = AIRuntimeStateStore::default();
    let terminal = codex_bridge_terminal();
    let detected =
        std::collections::HashMap::from([(terminal.terminal_id.clone(), "codex".to_string())]);
    assert!(
        store
            .ensure_detected_sessions(&[terminal], &detected, 1000.0)
            .did_change
    );
    assert_eq!(store.snapshot().sessions[0].state, "idle");

    let prompt = test_hook("promptSubmitted", 1001.0);
    let mutation = store.apply_hook(prompt);

    assert!(mutation.did_change);
    assert!(mutation.completion.is_none());
    let snapshot = store.snapshot();
    assert_eq!(snapshot.running_count, 1);
    assert_eq!(snapshot.sessions[0].state, "responding");
}
#[test]
fn prompt_submitted_uses_wrapper_project_even_when_hook_cwd_differs() {
    let store = AIRuntimeStateStore::default();
    let mut prompt = test_hook("promptSubmitted", 1000.0);
    prompt.project_path = Some("F:\\codux-gpui".to_string());
    prompt.metadata = Some(AIHookEventMetadata {
        cwd: Some("C:\\Users\\dux".to_string()),
        ..empty_metadata()
    });

    let mutation = store.apply_hook(prompt);

    assert!(mutation.did_change);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.running_count, 1);
    assert_eq!(snapshot.sessions[0].state, "responding");
}
#[test]
fn multiple_same_tool_sessions_are_isolated_by_terminal_id() {
    let store = AIRuntimeStateStore::default();
    assert!(
        store
            .apply_hook(test_hook_for(
                "codex",
                "codex-term-1",
                "codex-session-1",
                1000.0
            ))
            .did_change
    );
    assert!(
        store
            .apply_hook(test_hook_for(
                "codex",
                "codex-term-2",
                "codex-session-2",
                1001.0
            ))
            .did_change
    );

    let snapshot = store.snapshot();
    assert_eq!(snapshot.running_count, 1);
    assert_eq!(snapshot.global_totals.running, 2);
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.terminal_id == "codex-term-1"
                && session.ai_session_id.as_deref() == Some("codex-session-1")
                && session.state == "responding")
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.terminal_id == "codex-term-2"
                && session.ai_session_id.as_deref() == Some("codex-session-2")
                && session.state == "responding")
    );

    assert!(
        store
            .apply_hook(AIHookEventPayload {
                kind: "turnCompleted".to_string(),
                updated_at: 1010.0,
                metadata: Some(AIHookEventMetadata {
                    has_completed_turn: Some(true),
                    ..empty_metadata()
                }),
                ..test_hook_for("codex", "codex-term-1", "codex-session-1", 1010.0)
            })
            .did_change
    );

    let snapshot = store.snapshot();
    assert_eq!(snapshot.running_count, 1);
    assert_eq!(snapshot.global_totals.running, 1);
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.terminal_id == "codex-term-1" && session.state == "idle")
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.terminal_id == "codex-term-2"
                && session.ai_session_id.as_deref() == Some("codex-session-2")
                && session.state == "responding")
    );
}
#[test]
fn multiple_claude_sessions_are_isolated_by_terminal_id_and_external_session_id() {
    let store = AIRuntimeStateStore::default();
    assert!(
        store
            .apply_hook(test_hook_for(
                "claude",
                "claude-term-1",
                "claude-external-1",
                1000.0
            ))
            .did_change
    );
    assert!(
        store
            .apply_hook(test_hook_for(
                "claude",
                "claude-term-2",
                "claude-external-2",
                1001.0
            ))
            .did_change
    );

    let snapshot = store.snapshot();
    assert_eq!(snapshot.running_count, 1);
    assert_eq!(snapshot.global_totals.running, 2);
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.terminal_id == "claude-term-1"
                && session.tool == "claude"
                && session.ai_session_id.as_deref() == Some("claude-external-1"))
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.terminal_id == "claude-term-2"
                && session.tool == "claude"
                && session.ai_session_id.as_deref() == Some("claude-external-2"))
    );
}
#[test]
fn stale_runtime_completion_snapshot_after_prompt_stays_running() {
    let store = AIRuntimeStateStore::default();
    assert!(
        store
            .apply_hook(test_hook("sessionStarted", 1000.0))
            .did_change
    );
    assert!(
        store
            .apply_hook(test_hook("promptSubmitted", 1020.0))
            .did_change
    );

    let mutation = store.apply_runtime_snapshot(
        "terminal-1",
        AIRuntimeContextSnapshot {
            tool: "codex".to_string(),
            external_session_id: Some("session-1".to_string()),
            transcript_path: Some("/tmp/codex.jsonl".to_string()),
            model: Some("gpt-5.4".to_string()),
            assistant_preview: None,
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            total_tokens: 150,
            usage_amounts: Vec::new(),
            baseline_usage_amounts: Vec::new(),
            updated_at: 1010.0,
            started_at: Some(1000.0),
            completed_at: Some(1010.0),
            response_state: Some("idle".to_string()),
            was_interrupted: false,
            has_completed_turn: true,
            session_origin: "live".to_string(),
            source: "probe".to_string(),
            plan: None,
        },
    );

    assert!(mutation.did_change);
    assert!(mutation.completion.is_none());
    let snapshot = store.snapshot();
    assert_eq!(snapshot.running_count, 1);
    assert_eq!(snapshot.completion_count, 0);
    assert_eq!(snapshot.sessions[0].state, "responding");
    assert!(!snapshot.sessions[0].has_completed_turn);
    assert!(matches!(
        snapshot.projects[0].completed_phase,
        AIProjectPhase::Idle
    ));
}
