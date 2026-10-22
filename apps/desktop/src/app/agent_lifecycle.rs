use super::ai_runtime_status::AgentLifecycleState;
use super::{CoduxApp, ProjectInfo, WorktreeInfo};
use codux_runtime::ai_runtime::{AgentActivityRecord, AgentActivitySnapshot};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct PaneAgentLifecycle {
    pub state: AgentLifecycleState,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub terminal_instance_id: Option<String>,
    pub tool: Option<String>,
    pub recent_output: Option<String>,
}

pub(in crate::app) type AgentLifecycleMap = HashMap<(Option<String>, String), PaneAgentLifecycle>;

pub(in crate::app) fn aggregate_agent_lifecycle(
    states: impl Iterator<Item = AgentLifecycleState>,
) -> Option<AgentLifecycleState> {
    match AgentLifecycleState::aggregate(states) {
        AgentLifecycleState::Absent => None,
        state => Some(state),
    }
}

fn mirror_snapshot(
    mirror: &mut AgentLifecycleMap,
    device_id: Option<String>,
    snapshot: AgentActivitySnapshot,
) {
    for AgentActivityRecord {
        terminal_id,
        project_id,
        worktree_id,
        state,
        terminal_instance_id,
        tool,
        recent_output,
    } in snapshot.terminals
    {
        if state != AgentLifecycleState::Absent {
            mirror.insert(
                (device_id.clone(), terminal_id),
                PaneAgentLifecycle {
                    state,
                    project_id,
                    worktree_id,
                    terminal_instance_id,
                    tool,
                    recent_output,
                },
            );
        }
    }
}

impl CoduxApp {
    /// Replace UI mirrors from current authoritative snapshots. Detaching a
    /// view or reading a result never changes the runtime's pending user action.
    pub(in crate::app) fn sync_agent_activity(&mut self) -> (bool, bool) {
        // Drain legacy transient pushes without using them as Agent truth.
        let _ = self.runtime_service.drain_remote_terminal_status();
        let _ = self.runtime_service.drain_disconnected_remote_devices();
        let mut next = AgentLifecycleMap::new();
        let mut known = HashSet::new();
        if let Ok(snapshot) = self.runtime_service.agent_activity_snapshot() {
            known.insert(None);
            mirror_snapshot(&mut next, None, snapshot);
        }
        for (device_id, snapshot) in self.runtime_service.remote_agent_activity_snapshots() {
            if let Some(snapshot) = snapshot {
                known.insert(Some(device_id.clone()));
                mirror_snapshot(&mut next, Some(device_id), snapshot);
            }
        }
        let finished = self.pane_agent_lifecycle.iter().any(|(key, previous)| {
            previous.state == AgentLifecycleState::Working
                && next
                    .get(key)
                    .is_none_or(|current| current.state != AgentLifecycleState::Working)
        });
        // Text updates belong to the overview, not project badge invalidation.
        let changed = known != self.agent_activity_known_sources
            || next.len() != self.pane_agent_lifecycle.len()
            || next.iter().any(|(key, entry)| {
                self.pane_agent_lifecycle.get(key).is_none_or(|old| {
                    old.state != entry.state
                        || old.project_id != entry.project_id
                        || old.worktree_id != entry.worktree_id
                })
            });
        self.pane_agent_lifecycle = next;
        self.agent_activity_known_sources = known;
        (changed, finished)
    }

    pub(in crate::app) fn pane_agent_state(
        &self,
        terminal_id: &str,
    ) -> Option<AgentLifecycleState> {
        let device = self
            .state
            .selected_project
            .as_ref()
            .and_then(|project| project.remote_device_id())
            .map(str::to_string);
        if !self.agent_activity_known_sources.contains(&device) {
            return Some(AgentLifecycleState::Unknown);
        }
        self.pane_agent_lifecycle
            .get(&(device, terminal_id.to_string()))
            .map(|entry| entry.state)
    }

    pub(in crate::app) fn worktree_agent_lifecycle(
        &self,
        worktree: &WorktreeInfo,
    ) -> Option<AgentLifecycleState> {
        let device = self
            .state
            .selected_project
            .as_ref()
            .and_then(|project| project.remote_device_id())
            .map(str::to_string);
        if !self.agent_activity_known_sources.contains(&device) {
            return Some(AgentLifecycleState::Unknown);
        }
        aggregate_agent_lifecycle(
            self.pane_agent_lifecycle
                .iter()
                .filter(|((source, _), entry)| {
                    source == &device && entry.worktree_id.as_deref() == Some(worktree.id.as_str())
                })
                .map(|(_, entry)| entry.state),
        )
    }

    pub(in crate::app) fn project_agent_lifecycle(
        &self,
        project: &ProjectInfo,
    ) -> Option<AgentLifecycleState> {
        let device = project.remote_device_id().map(str::to_string);
        if !self.agent_activity_known_sources.contains(&device) {
            return Some(AgentLifecycleState::Unknown);
        }
        aggregate_agent_lifecycle(
            self.pane_agent_lifecycle
                .iter()
                .filter(|((source, _), entry)| source == &device && entry.project_id == project.id)
                .map(|(_, entry)| entry.state),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_activity_project_prioritizes_input_over_work_and_idle() {
        use AgentLifecycleState::*;
        assert_eq!(
            aggregate_agent_lifecycle([Working, NeedsInput, Idle].into_iter()),
            Some(NeedsInput)
        );
        assert_eq!(
            aggregate_agent_lifecycle([Idle, Working].into_iter()),
            Some(Working)
        );
        assert_eq!(aggregate_agent_lifecycle([Idle].into_iter()), Some(Idle));
        assert_eq!(
            aggregate_agent_lifecycle([Working, Unknown].into_iter()),
            Some(Unknown)
        );
        assert_eq!(aggregate_agent_lifecycle([Absent].into_iter()), None);
    }

    #[test]
    fn agent_activity_same_terminal_id_is_isolated_by_host() {
        let record = |state| AgentActivitySnapshot {
            version: 1,
            terminals: vec![AgentActivityRecord {
                terminal_id: "terminal-1".into(),
                terminal_instance_id: None,
                project_id: "project".into(),
                worktree_id: Some("worktree".into()),
                state,
                tool: None,
                recent_output: None,
            }],
        };
        let mut mirror = AgentLifecycleMap::new();
        mirror_snapshot(
            &mut mirror,
            Some("host-a".into()),
            record(AgentLifecycleState::Working),
        );
        mirror_snapshot(
            &mut mirror,
            Some("host-b".into()),
            record(AgentLifecycleState::NeedsInput),
        );
        assert_eq!(mirror.len(), 2);
        assert_eq!(
            mirror[&(Some("host-a".into()), "terminal-1".into())].state,
            AgentLifecycleState::Working
        );
        assert_eq!(
            mirror[&(Some("host-b".into()), "terminal-1".into())].state,
            AgentLifecycleState::NeedsInput
        );
    }
}
