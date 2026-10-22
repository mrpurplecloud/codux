use super::*;
use codux_protocol::REMOTE_RESOURCE_AGENT_ACTIVITY;

impl RemoteHostRuntime {
    pub(super) fn reply_agent_activity(&self, envelope: &RemoteEnvelope) {
        match self
            .ai_current_sessions
            .as_ref()
            .ok_or_else(|| "Agent activity is unavailable.".to_string())
            .and_then(|provider| provider.agent_activity())
        {
            Ok(snapshot) => self.reply(envelope, REMOTE_RESOURCE_AGENT_ACTIVITY, json!(snapshot)),
            Err(error) => self.send_error(envelope, &error),
        }
    }

    /// Reuse the runtime tick to reconcile subscribers, including failed sends.
    /// Consumers ignore equal versions; no animation or extra polling task.
    pub fn broadcast_agent_activity(&self) {
        let devices = self.resource_subscriptions.devices_for_exact(
            REMOTE_RESOURCE_AGENT_ACTIVITY,
            None,
            None,
        );
        if devices.is_empty() {
            return;
        }
        let Some(provider) = &self.ai_current_sessions else {
            return;
        };
        match provider.agent_activity() {
            Ok(snapshot) => {
                let payload = json!(snapshot);
                for device in devices {
                    self.send(
                        REMOTE_RESOURCE_AGENT_ACTIVITY,
                        Some(&device),
                        None,
                        payload.clone(),
                    );
                }
            }
            Err(_) => {
                for device in devices {
                    self.send(
                        REMOTE_RESOURCE_AGENT_ACTIVITY,
                        Some(&device),
                        None,
                        Value::Null,
                    );
                }
            }
        }
    }
}
