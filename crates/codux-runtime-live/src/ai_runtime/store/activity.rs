use super::*;
use crate::ai_runtime::registry::AIRuntimeTerminalBinding;
use codux_protocol::{AgentActivityRecord, AgentActivityState};

type SessionIdentity = (Option<String>, Option<String>);

pub(super) struct HookOrder {
    pub tool: String,
    pub identity: SessionIdentity,
    pub updated_at: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentProcess {
    instance: Option<String>,
    tool: Option<String>,
    present: Option<bool>,
    recent_output: Option<String>,
}

fn recent_terminal_output(text: &str) -> Option<String> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>();
    lines.reverse();
    let text = lines.join("\n");
    let mut tail = text.chars().rev().take(240).collect::<Vec<_>>();
    tail.reverse();
    (!tail.is_empty()).then(|| tail.into_iter().collect())
}

pub(super) fn note_input(core: &mut AIRuntimeStateCore, terminal_id: &str, clear: bool) {
    // The current turn may end while its explicit question remains unanswered.
    // This identity is a user-action fact, not another copy of the turn phase.
    let Some(session) = core.sessions.get(terminal_id) else {
        return;
    };
    let identity = (
        session.terminal_instance_id.clone(),
        session.ai_session_id.clone(),
    );
    if clear
        || session.state == "responding"
        || core
            .agent_input
            .get(terminal_id)
            .is_some_and(|old| old != &identity)
    {
        core.agent_input.remove(terminal_id);
    }
    if session.state == "needsInput" {
        core.agent_input.insert(terminal_id.to_string(), identity);
    }
}

impl AIRuntimeStateStore {
    pub fn agent_screen_targets(&self) -> Vec<(String, Option<String>)> {
        self.core
            .lock()
            .map(|core| {
                core.agent_processes
                    .iter()
                    .filter(|(_, process)| process.present == Some(true))
                    .map(|(id, process)| (id.clone(), process.instance.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Process enumeration happens outside the state lock. Unknown is distinct
    /// from a successful scan that found no Agent process.
    pub fn record_agent_processes(
        &self,
        terminals: &[AIRuntimeTerminalState],
        shell_pids: &[(String, u32)],
        detected: Option<&HashMap<String, String>>,
    ) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let registered = terminals
            .iter()
            .map(|t| t.terminal_id.as_str())
            .collect::<HashSet<_>>();
        core.agent_processes
            .retain(|id, _| registered.contains(id.as_str()));
        core.agent_input
            .retain(|id, _| registered.contains(id.as_str()));
        core.hook_order
            .retain(|id, _| registered.contains(id.as_str()));
        for terminal in terminals {
            let observable = shell_pids.iter().any(|(id, _)| id == &terminal.terminal_id);
            let mut process = AgentProcess {
                instance: terminal.terminal_instance_id.clone(),
                tool: detected
                    .and_then(|tools| tools.get(&terminal.terminal_id))
                    .cloned(),
                present: detected
                    .filter(|_| observable)
                    .map(|tools| tools.contains_key(&terminal.terminal_id)),
                recent_output: None,
            };
            if process.present == Some(true)
                && let Some(old) = core.agent_processes.get(&terminal.terminal_id)
                && old.instance == process.instance
                && old.tool == process.tool
            {
                process.recent_output = old.recent_output.clone();
            }
            if process.present == Some(false)
                || core
                    .agent_processes
                    .get(&terminal.terminal_id)
                    .is_some_and(|old| old.instance != process.instance)
            {
                core.agent_input.remove(&terminal.terminal_id);
            }
            core.agent_processes
                .insert(terminal.terminal_id.clone(), process);
        }
    }

    /// Screen I/O belongs to the supervisor, outside the store lock. A delayed
    /// screen read must never supply text for a replacement terminal instance.
    pub fn record_agent_output(
        &self,
        terminal_id: &str,
        instance: Option<&str>,
        text: Option<&str>,
    ) {
        let output = text.and_then(recent_terminal_output);
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        if let Some(process) = core.agent_processes.get_mut(terminal_id)
            && process.present == Some(true)
            && process.instance.as_deref() == instance
        {
            process.recent_output = output;
        }
    }

    pub fn agent_activity(
        &self,
        bindings: &[AIRuntimeTerminalBinding],
    ) -> Result<Vec<AgentActivityRecord>, String> {
        let core = self
            .core
            .lock()
            .map_err(|_| "Agent activity state lock poisoned.".to_string())?;
        Ok(bindings
            .iter()
            .map(|binding| {
                let process = core
                    .agent_processes
                    .get(&binding.terminal_id)
                    .filter(|p| p.instance == binding.terminal_instance_id);
                let session = core
                    .sessions
                    .get(&binding.terminal_id)
                    .filter(|s| s.terminal_instance_id == binding.terminal_instance_id);
                let state = match process.and_then(|p| p.present) {
                    None => AgentActivityState::Unknown,
                    Some(false) => AgentActivityState::Absent,
                    Some(true) => {
                        let session = session.filter(|s| {
                            process.and_then(|p| p.tool.as_deref()) == Some(s.tool.as_str())
                        });
                        if session.is_some_and(|s| {
                            core.agent_input.get(&binding.terminal_id)
                                == Some(&(s.terminal_instance_id.clone(), s.ai_session_id.clone()))
                        }) {
                            AgentActivityState::NeedsInput
                        } else if session.is_some_and(|s| s.state == "responding") {
                            AgentActivityState::Working
                        } else {
                            AgentActivityState::Idle
                        }
                    }
                };
                AgentActivityRecord {
                    terminal_id: binding.terminal_id.clone(),
                    terminal_instance_id: binding.terminal_instance_id.clone(),
                    project_id: binding
                        .root_project_id
                        .clone()
                        .unwrap_or_else(|| binding.project_id.clone()),
                    worktree_id: binding.worktree_id.clone(),
                    state,
                    tool: process
                        .and_then(|p| p.tool.clone())
                        .or_else(|| session.map(|s| s.tool.clone())),
                    recent_output: process
                        .filter(|p| p.present == Some(true))
                        .and_then(|p| p.recent_output.clone()),
                }
            })
            .collect())
    }
}

pub(super) type AgentInput = HashMap<String, SessionIdentity>;
