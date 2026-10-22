use super::fixtures::*;
use crate::ai_runtime::registry::AIRuntimeTerminalBinding;
use codux_protocol::AgentActivityState;
use std::collections::HashMap;

fn binding() -> AIRuntimeTerminalBinding {
    AIRuntimeTerminalBinding {
        terminal_id: "terminal-1".into(),
        terminal_instance_id: Some("instance-1".into()),
        root_project_id: Some("root-project".into()),
        worktree_id: Some("worktree-1".into()),
        project_id: "project-1".into(),
        ..Default::default()
    }
}

fn detect(store: &AIRuntimeStateStore, present: bool) {
    store.record_agent_processes(
        &[codex_bridge_terminal()],
        &[("terminal-1".into(), 123)],
        Some(&if present {
            HashMap::from([("terminal-1".into(), "codex".into())])
        } else {
            HashMap::new()
        }),
    );
}

fn phase(store: &AIRuntimeStateStore) -> AgentActivityState {
    store.agent_activity(&[binding()]).unwrap()[0].state
}

fn hook(store: &AIRuntimeStateStore, kind: &str, at: f64) {
    apply_hook_unlocked(&mut store.core.lock().unwrap(), test_hook(kind, at));
}

#[test]
fn agent_activity_turn_end_is_idle_and_does_not_imply_input() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    hook(&store, "promptSubmitted", 1000.0);
    assert_eq!(phase(&store), AgentActivityState::Working);
    hook(&store, "turnCompleted", 1001.0);
    assert_eq!(phase(&store), AgentActivityState::Idle);
    let activity = store.agent_activity(&[binding()]).unwrap();
    assert_eq!(activity[0].project_id, "root-project");
    assert_eq!(activity[0].worktree_id.as_deref(), Some("worktree-1"));
}

#[test]
fn agent_activity_output_is_bounded_unicode_tail_without_changing_idle_state() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    hook(&store, "turnCompleted", 1000.0);
    store.record_agent_output(
        "terminal-1",
        Some("instance-1"),
        Some("old\n\n first \n second \n third \n"),
    );
    let record = store.agent_activity(&[binding()]).unwrap().remove(0);
    assert_eq!(record.state, AgentActivityState::Idle);
    assert_eq!(
        record.recent_output.as_deref(),
        Some("first\nsecond\nthird")
    );
    assert_eq!(record.tool.as_deref(), Some("codex"));
    // A completed turn is no longer transcript-polled, but must still be sampled.
    assert!(store.runtime_tracked_sessions(1001.0).is_empty());
    assert_eq!(
        store.agent_screen_targets(),
        vec![("terminal-1".into(), Some("instance-1".into()))]
    );
    store.record_agent_output(
        "terminal-1",
        Some("instance-1"),
        Some(&format!("{}末句", "中".repeat(300))),
    );
    let output = store
        .agent_activity(&[binding()])
        .unwrap()
        .remove(0)
        .recent_output
        .unwrap();
    assert_eq!(output.chars().count(), 240);
    assert!(output.ends_with("末句"));
    store.record_agent_output("terminal-1", Some("instance-1"), Some(" \n  \n"));
    assert_eq!(
        store.agent_activity(&[binding()]).unwrap()[0].recent_output,
        None
    );
}

#[test]
fn agent_activity_output_does_not_survive_instance_replacement_or_process_loss() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    store.record_agent_output("terminal-1", Some("instance-1"), Some("current"));
    detect(&store, true);
    assert_eq!(
        store.agent_activity(&[binding()]).unwrap()[0]
            .recent_output
            .as_deref(),
        Some("current")
    );
    store.record_agent_output("terminal-1", Some("old-instance"), Some("stale"));
    assert_eq!(
        store.agent_activity(&[binding()]).unwrap()[0]
            .recent_output
            .as_deref(),
        Some("current")
    );
    let mut terminal = codex_bridge_terminal();
    terminal.terminal_instance_id = Some("instance-2".into());
    store.record_agent_processes(
        &[terminal],
        &[("terminal-1".into(), 123)],
        Some(&HashMap::from([("terminal-1".into(), "codex".into())])),
    );
    let mut next = binding();
    next.terminal_instance_id = Some("instance-2".into());
    store.record_agent_output("terminal-1", Some("instance-1"), Some("late"));
    assert_eq!(
        store.agent_activity(&[next]).unwrap()[0].recent_output,
        None
    );
    detect(&store, false);
    assert!(store.agent_screen_targets().is_empty());
    store.record_agent_output("terminal-1", Some("instance-1"), Some("shell output"));
    assert_eq!(
        store.agent_activity(&[binding()]).unwrap()[0].recent_output,
        None
    );
}

#[test]
fn agent_activity_explicit_input_survives_turn_end_and_age_until_reply() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    hook(&store, "needsInput", 1.0);
    hook(&store, "turnCompleted", 2.0);
    assert_eq!(phase(&store), AgentActivityState::NeedsInput);
    hook(&store, "promptSubmitted", 1000.0);
    assert_eq!(phase(&store), AgentActivityState::Working);
}

#[test]
fn agent_activity_absent_process_overrides_history_and_failure_is_unknown() {
    let store = AIRuntimeStateStore::default();
    hook(&store, "promptSubmitted", 1.0);
    detect(&store, false);
    assert_eq!(phase(&store), AgentActivityState::Absent);
    store.record_agent_processes(
        &[codex_bridge_terminal()],
        &[("terminal-1".into(), 123)],
        None,
    );
    assert_eq!(phase(&store), AgentActivityState::Unknown);
}

#[test]
fn agent_activity_new_instance_does_not_inherit_input() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    hook(&store, "needsInput", 1.0);
    let mut next = binding();
    next.terminal_instance_id = Some("instance-2".into());
    assert_eq!(
        store.agent_activity(&[next]).unwrap()[0].state,
        AgentActivityState::Unknown
    );
    hook(&store, "sessionStarted", 3.0);
    assert_eq!(phase(&store), AgentActivityState::Idle);
}

#[test]
fn agent_activity_old_question_cannot_restore_answered_action() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    hook(&store, "needsInput", 10.0);
    hook(&store, "promptSubmitted", 20.0);
    hook(&store, "needsInput", 15.0);
    assert_eq!(phase(&store), AgentActivityState::Working);
}

#[test]
fn agent_activity_hook_order_is_scoped_to_session_identity() {
    let store = AIRuntimeStateStore::default();
    detect(&store, true);
    hook(&store, "needsInput", 100.0);
    let mut event = test_hook("promptSubmitted", 20.0);
    event.ai_session_id = Some("session-2".into());
    assert!(apply_hook_unlocked(
        &mut store.core.lock().unwrap(),
        event.clone()
    ));
    assert_eq!(phase(&store), AgentActivityState::Working);
    event.kind = "needsInput".into();
    event.updated_at = 15.0;
    assert!(!apply_hook_unlocked(&mut store.core.lock().unwrap(), event));
    assert_eq!(phase(&store), AgentActivityState::Working);
}
