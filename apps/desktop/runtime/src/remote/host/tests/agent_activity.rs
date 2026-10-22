use super::*;
use codux_protocol::{
    AgentActivityRecord, AgentActivitySnapshot, AgentActivityState, REMOTE_RESOURCE_AGENT_ACTIVITY,
};

struct ActivityProvider(Mutex<AgentActivitySnapshot>);

impl runtime_ai_stats::RemoteAICurrentSessionProvider for ActivityProvider {
    fn current_sessions(&self, _: &str) -> Vec<codux_protocol::RemoteAICurrentSession> {
        vec![]
    }
    fn agent_activity(&self) -> Result<AgentActivitySnapshot, String> {
        Ok(self.0.lock().unwrap().clone())
    }
}

#[test]
fn agent_activity_subscription_has_full_baseline_and_removes_closed_sessions() {
    let support_dir = temp_support_dir("codux-agent-activity");
    let provider = Arc::new(ActivityProvider(Mutex::new(AgentActivitySnapshot {
        version: 2,
        terminals: [AgentActivityState::Working, AgentActivityState::NeedsInput]
            .into_iter()
            .enumerate()
            .map(|(i, state)| AgentActivityRecord {
                terminal_id: format!("terminal-{i}"),
                terminal_instance_id: None,
                project_id: format!("project-{i}"),
                worktree_id: None,
                state,
                tool: Some("codex".into()),
                recent_output: Some(format!("output-{i}")),
            })
            .collect(),
    })));
    let runtime = Arc::new(
        RemoteHostRuntime::new_with_ai_history_current_sessions_and_terminals(
            support_dir.clone(),
            Default::default(),
            provider.clone(),
            Arc::new(TerminalManager::new()),
        ),
    );
    let transport = Arc::new(CapturingTransport::default());
    runtime.set_transport_for_test(transport.clone());
    let envelope = RemoteEnvelope {
        kind: REMOTE_RESOURCE_SUBSCRIBE.into(),
        device_id: Some("viewer".into()),
        session_id: None,
        request_id: Some("baseline".into()),
        seq: None,
        payload: json!({"resource": REMOTE_RESOURCE_AGENT_ACTIVITY, "baseline": true}),
    };
    runtime.handle_resource_subscribe(&envelope);
    let messages = transport.take_messages();
    assert_eq!(messages.len(), 1);
    let baseline: Value = serde_json::from_slice(&messages[0].1).unwrap();
    assert_eq!(baseline["requestId"], "baseline");
    assert_eq!(
        baseline["payload"]["terminals"].as_array().unwrap().len(),
        2
    );
    assert_eq!(baseline["payload"]["terminals"][1]["state"], "needsInput");
    assert_eq!(
        baseline["payload"]["terminals"][1]["recentOutput"],
        "output-1"
    );
    let mut next = provider.0.lock().unwrap().terminals.clone();
    next[0].recent_output = Some("new output without a state change".into());
    provider.0.lock().unwrap().replace(next);
    runtime.broadcast_agent_activity();
    let messages = transport.take_messages();
    let update: Value = serde_json::from_slice(&messages[0].1).unwrap();
    assert_eq!(update["payload"]["version"], 3);
    assert_eq!(
        update["payload"]["terminals"][0]["recentOutput"],
        "new output without a state change"
    );
    provider.0.lock().unwrap().replace(vec![]);
    runtime.broadcast_agent_activity();
    let messages = transport.take_messages();
    let update: Value = serde_json::from_slice(&messages[0].1).unwrap();
    assert_eq!(update["payload"]["version"], 4);
    assert_eq!(update["payload"]["terminals"], json!([]));
    runtime.resource_subscriptions.remove_device("viewer");
    runtime.broadcast_agent_activity();
    assert!(transport.take_messages().is_empty());
    fs::remove_dir_all(support_dir).ok();
}

#[test]
fn agent_activity_resource_rejects_project_scope() {
    let support_dir = temp_support_dir("codux-agent-activity-scope");
    let runtime = Arc::new(RemoteHostRuntime::new(support_dir.clone()));
    let transport = Arc::new(CapturingTransport::default());
    runtime.set_transport_for_test(transport.clone());
    runtime.handle_resource_subscribe(&RemoteEnvelope {
        kind: REMOTE_RESOURCE_SUBSCRIBE.into(),
        device_id: Some("viewer".into()),
        session_id: None,
        request_id: None,
        seq: None,
        payload: json!({"resource": REMOTE_RESOURCE_AGENT_ACTIVITY, "projectId": "other"}),
    });
    let messages = transport.take_messages();
    let reply: Value = serde_json::from_slice(&messages[0].1).unwrap();
    assert_eq!(reply["type"], REMOTE_ERROR);
    assert!(
        runtime
            .resource_subscriptions
            .devices_for_resource(REMOTE_RESOURCE_AGENT_ACTIVITY)
            .is_empty()
    );
    fs::remove_dir_all(support_dir).ok();
}
