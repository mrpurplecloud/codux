use super::crypto::remote_random_token;
use super::summary::remote_summary_from_settings;
use super::types::{RemoteSettings, RemoteSummary};
use super::{RemoteService, remote_settings_from_raw, remote_settings_mut};
use crate::runtime_trace::{runtime_trace, runtime_trace_elapsed};
use serde_json::{Map, Value};
use std::time::Instant;

impl RemoteService {
    pub fn set_enabled(&self, enabled: bool) -> Result<RemoteSummary, String> {
        let settings = self.update_raw_settings(|raw| {
            let remote = remote_settings_mut(raw)?;
            remote.insert("isEnabled".to_string(), Value::Bool(enabled));
            Ok(remote_settings_from_raw(raw))
        })?;
        Ok(remote_summary_from_settings(settings))
    }

    pub fn register_host(&self) -> Result<RemoteSummary, String> {
        crate::async_runtime::block_on(self.register_host_async())
    }

    pub async fn register_host_async(&self) -> Result<RemoteSummary, String> {
        let settings = self.update_raw_settings(|raw| self.register_host_in_raw(raw))?;
        Ok(remote_summary_from_settings(settings))
    }

    pub(super) fn register_host_in_raw(
        &self,
        raw: &mut Map<String, Value>,
    ) -> Result<RemoteSettings, String> {
        let started_at = Instant::now();
        let mut settings = remote_settings_from_raw(raw);
        if !settings.is_enabled {
            runtime_trace("remote", "register_host skipped enabled=false");
            return Ok(settings);
        }
        if settings.host_id.trim().is_empty() {
            settings.host_id = uuid::Uuid::new_v4().to_string();
        }
        if settings.host_token.trim().is_empty() {
            settings.host_token = remote_random_token();
        }
        runtime_trace(
            "remote",
            &format!(
                "register_host has_host={} has_token={}",
                !settings.host_id.trim().is_empty(),
                !settings.host_token.trim().is_empty()
            ),
        );
        raw.insert(
            "remote".to_string(),
            serde_json::to_value(&settings).map_err(|error| error.to_string())?,
        );
        runtime_trace_elapsed(
            "remote",
            "register_host ok",
            started_at,
            &format!("host_id={}", settings.host_id),
        );
        Ok(settings)
    }
}
