use super::types::RemoteSummary;
use super::{RemoteService, remote_settings_from_raw, remote_settings_mut};
use serde_json::{Map, Value};

/// The stored device list, for editing in place.
///
/// Both paths below change one thing about the devices, so they change it on
/// the stored objects rather than re-serializing the typed view over them.
/// `RemoteDeviceSettings` does not model every key a device may carry, and a
/// round trip through it silently drops the rest — which is how the app
/// settings store came to delete every paired phone's token.
fn cached_devices_mut(raw: &mut Map<String, Value>) -> Result<&mut Vec<Value>, String> {
    remote_settings_mut(raw)?
        .entry("cachedDevices".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| "Remote device list is invalid.".to_string())
}

impl RemoteService {
    pub fn revoke_device(&self, device_id: &str) -> Result<RemoteSummary, String> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Err("Missing device id.".to_string());
        }
        let settings = self.update_raw_settings(|raw| {
            let devices = cached_devices_mut(raw)?;
            let before_len = devices.len();
            devices.retain(|device| device.get("id").and_then(Value::as_str) != Some(device_id));
            if devices.len() == before_len {
                return Err("Remote device not found.".to_string());
            }
            Ok(remote_settings_from_raw(raw))
        })?;
        let mut summary = super::summary::remote_summary_from_settings(settings);
        summary.message = "Device removed.".to_string();
        Ok(summary)
    }

    pub fn refresh_devices(&self) -> Result<RemoteSummary, String> {
        self.refresh_devices_local()
    }

    pub async fn refresh_devices_async(&self) -> Result<RemoteSummary, String> {
        self.refresh_devices_local()
    }

    fn refresh_devices_local(&self) -> Result<RemoteSummary, String> {
        let settings = self.update_raw_settings(|raw| {
            if !remote_settings_from_raw(raw).is_enabled {
                return Ok(remote_settings_from_raw(raw));
            }
            for device in cached_devices_mut(raw)?.iter_mut() {
                if let Some(device) = device.as_object_mut() {
                    device.insert("online".to_string(), Value::Bool(false));
                }
            }
            Ok(remote_settings_from_raw(raw))
        })?;
        Ok(super::summary::remote_summary_from_settings(settings))
    }
}
