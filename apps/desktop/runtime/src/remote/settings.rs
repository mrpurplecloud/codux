use super::RemoteService;
use super::types::RemoteSettings;
use crate::config::ConfigStore;
use serde_json::{Map, Value};

impl RemoteService {
    pub(super) fn raw_settings(&self) -> Map<String, Value> {
        ConfigStore::for_file(self.settings_path.clone()).snapshot()
    }

    pub(super) fn update_raw_settings<R>(
        &self,
        update: impl FnOnce(&mut Map<String, Value>) -> Result<R, String>,
    ) -> Result<R, String> {
        ConfigStore::for_file(self.settings_path.clone()).update(update)
    }

    pub(super) fn update_raw_settings_sync<R>(
        &self,
        update: impl FnOnce(&mut Map<String, Value>) -> Result<R, String>,
    ) -> Result<R, String> {
        ConfigStore::for_file(self.settings_path.clone()).update_sync(update)
    }

    pub(crate) fn set_relay_url(&self, relay_url: &str, reset_devices: bool) -> Result<(), String> {
        let relay_url = relay_url.trim().to_string();
        let relay_preset = if relay_url.is_empty() {
            super::relay::remote_relay_preset_for_url(&relay_url)
        } else {
            "custom".to_string()
        };
        self.update_relay_settings(reset_devices, move |_, remote| {
            remote.insert("relayPreset".to_string(), Value::String(relay_preset));
            remote.insert("relayUrl".to_string(), Value::String(relay_url));
        })
    }

    pub(crate) fn set_relay_preset(
        &self,
        relay_preset: &str,
        reset_devices: bool,
    ) -> Result<(), String> {
        let relay_preset = relay_preset.to_string();
        self.update_relay_settings(reset_devices, move |current, remote| {
            let relay_preset =
                super::relay::normalize_remote_relay_preset(&relay_preset, &current.relay_url);
            let relay_url =
                super::relay::remote_relay_url_for_preset(&relay_preset, &current.relay_url);
            remote.insert("relayPreset".to_string(), Value::String(relay_preset));
            remote.insert("relayUrl".to_string(), Value::String(relay_url));
        })
    }

    pub(crate) fn set_relay_authentication(
        &self,
        relay_authentication: &str,
        reset_devices: bool,
    ) -> Result<(), String> {
        let relay_authentication = relay_authentication.trim().to_string();
        self.update_relay_settings(reset_devices, move |_, remote| {
            remote.insert(
                "relayAuthentication".to_string(),
                Value::String(relay_authentication),
            );
        })
    }

    fn update_relay_settings(
        &self,
        reset_devices: bool,
        update: impl FnOnce(&RemoteSettings, &mut Map<String, Value>),
    ) -> Result<(), String> {
        self.update_raw_settings(|raw| {
            let current = remote_settings_from_raw(raw);
            let remote = remote_settings_mut(raw)?;
            update(&current, remote);
            if reset_devices {
                remote.insert("cachedDevices".to_string(), Value::Array(Vec::new()));
            }
            Ok(())
        })
    }
}

pub(crate) fn remote_settings_mut(
    raw: &mut Map<String, Value>,
) -> Result<&mut Map<String, Value>, String> {
    raw.entry("remote".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Remote settings are invalid.".to_string())
}

pub(crate) fn remote_settings_from_raw(raw: &Map<String, Value>) -> RemoteSettings {
    let mut settings = raw
        .get("remote")
        .cloned()
        .and_then(|remote| serde_json::from_value::<RemoteSettings>(remote).ok())
        .unwrap_or_default();
    settings.relay_preset =
        super::relay::normalize_remote_relay_preset(&settings.relay_preset, &settings.relay_url);
    settings.relay_url =
        super::relay::remote_relay_url_for_preset(&settings.relay_preset, &settings.relay_url);
    settings
}
