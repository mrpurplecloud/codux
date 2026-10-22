use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentActivityState {
    Absent,
    Working,
    NeedsInput,
    Idle,
    #[default]
    Unknown,
}

impl AgentActivityState {
    pub fn aggregate(states: impl IntoIterator<Item = Self>) -> Self {
        states
            .into_iter()
            .max_by_key(|state| match state {
                Self::Absent => 0,
                Self::Idle => 1,
                Self::Working => 2,
                Self::Unknown => 3,
                Self::NeedsInput => 4,
            })
            .unwrap_or(Self::Absent)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivityRecord {
    pub terminal_id: String,
    pub terminal_instance_id: Option<String>,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub state: AgentActivityState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Recent rendered terminal output, not a semantic assistant answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_output: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivitySnapshot {
    pub version: u64,
    pub terminals: Vec<AgentActivityRecord>,
}

impl AgentActivitySnapshot {
    /// Version the display projection, not usage counters or heartbeats.
    pub fn replace(&mut self, mut terminals: Vec<AgentActivityRecord>) {
        terminals.sort_by(|a, b| a.terminal_id.cmp(&b.terminal_id));
        if self.terminals != terminals {
            self.version += 1;
            self.terminals = terminals;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_activity_old_host_record_and_text_only_updates_are_compatible() {
        let record: AgentActivityRecord = serde_json::from_value(serde_json::json!({
            "terminalId": "terminal", "terminalInstanceId": null,
            "projectId": "project", "worktreeId": null, "state": "idle"
        }))
        .unwrap();
        assert_eq!(record.recent_output, None);
        assert_eq!(record.tool, None);
        let mut snapshot = AgentActivitySnapshot::default();
        snapshot.replace(vec![record.clone()]);
        assert_eq!(snapshot.version, 1);
        let mut updated = record;
        updated.recent_output = Some("new output".into());
        snapshot.replace(vec![updated.clone()]);
        assert_eq!(snapshot.version, 2);
        snapshot.replace(vec![updated]);
        assert_eq!(snapshot.version, 2);
    }

    #[test]
    fn agent_activity_aggregation_prioritizes_action_without_hiding_uncertainty() {
        use AgentActivityState::*;
        assert_eq!(
            AgentActivityState::aggregate([Working, NeedsInput]),
            NeedsInput
        );
        assert_eq!(AgentActivityState::aggregate([Idle, Working]), Working);
        assert_eq!(AgentActivityState::aggregate([Working, Unknown]), Unknown);
        assert_eq!(AgentActivityState::aggregate([]), Absent);
    }
}
