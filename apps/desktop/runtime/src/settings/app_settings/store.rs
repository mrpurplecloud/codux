use crate::{config::ConfigStore, notification::NotificationChannelConfig};
use serde_json::{Map, Value};
use std::{path::PathBuf, sync::Arc};

use super::{sanitize::sanitize_settings, types::AppSettings};

pub struct AppSettingsStore {
    config: Arc<ConfigStore>,
}

impl AppSettingsStore {
    pub fn from_support_dir(support_dir: PathBuf) -> Self {
        Self::from_settings_file(crate::config::settings_file_path(support_dir))
    }

    pub fn from_settings_file(settings_file: PathBuf) -> Self {
        Self {
            config: ConfigStore::for_file(settings_file),
        }
    }

    pub fn snapshot(&self) -> AppSettings {
        app_settings_from_raw(&self.config.snapshot())
    }

    pub fn configured_notification_channels(&self) -> Vec<NotificationChannelConfig> {
        self.snapshot()
            .notification_channels
            .into_iter()
            .filter_map(|(id, channel)| {
                let endpoint = channel.endpoint.trim().to_string();
                if !channel.enabled || endpoint.is_empty() {
                    return None;
                }
                Some(NotificationChannelConfig {
                    id,
                    endpoint,
                    token: channel.token.trim().to_string(),
                })
            })
            .collect()
    }
}

pub(crate) fn app_settings_from_raw(raw: &Map<String, Value>) -> AppSettings {
    sanitize_settings(settings_from_raw(raw.clone()).unwrap_or_default())
}

fn settings_from_raw(raw: serde_json::Map<String, Value>) -> Option<AppSettings> {
    if raw.is_empty() {
        return None;
    }
    serde_json::from_value(serde_json::Value::Object(raw)).ok()
}
