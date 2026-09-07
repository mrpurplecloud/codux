impl SettingsService {
    pub(crate) fn set_desktop_pet_widget(&self, enabled: bool) -> Result<AppSettings, String> {
        self.update_raw_settings(|raw| {
            pet_mut(raw)?.insert("desktopWidget".to_string(), Value::Bool(enabled));
            Ok(app_settings_from_raw(raw))
        })
    }

    pub(crate) fn set_pet_speech_temporary_mute_until(
        &self,
        until: i64,
    ) -> Result<AppSettings, String> {
        self.update_raw_settings(|raw| {
            ai_pet_mut(raw)?.insert(
                "speechTemporaryMuteUntil".to_string(),
                Value::Number(until.into()),
            );
            Ok(app_settings_from_raw(raw))
        })
    }

    pub(crate) fn update_pet_speech_frequency(
        &self,
        update: impl FnOnce(&str) -> String,
    ) -> Result<AppSettings, String> {
        self.update_raw_settings(|raw| {
            let current = app_settings_from_raw(raw).ai.pet.speech_frequency;
            ai_pet_mut(raw)?.insert(
                "speechFrequency".to_string(),
                Value::String(sanitize_pet_speech_frequency(&update(&current))),
            );
            Ok(app_settings_from_raw(raw))
        })
    }

    pub fn toggle_pet_enabled(&self) -> Result<SettingsSummary, String> {
        self.toggle_pet_bool("enabled", true)
    }

    pub fn toggle_pet_desktop_widget(&self) -> Result<SettingsSummary, String> {
        self.toggle_pet_bool("desktopWidget", false)
    }

    pub fn toggle_pet_static_mode(&self) -> Result<SettingsSummary, String> {
        self.toggle_pet_bool("staticMode", false)
    }

    pub fn toggle_pet_reminders(&self) -> Result<SettingsSummary, String> {
        self.toggle_pet_bool("reminders", false)
    }

    pub fn toggle_pet_sedentary_reminders(&self) -> Result<SettingsSummary, String> {
        self.toggle_pet_bool("sedentaryReminders", false)
    }

    pub fn toggle_pet_late_night_reminders(&self) -> Result<SettingsSummary, String> {
        self.toggle_pet_bool("lateNightReminders", false)
    }

    pub fn set_pet_hydration_reminder_minutes(
        &self,
        minutes: &str,
    ) -> Result<SettingsSummary, String> {
        self.update_pet_string(
            "hydrationReminderMinutes",
            sanitize_pet_reminder_minutes(minutes),
        )
    }

    pub fn set_pet_sedentary_reminder_minutes(
        &self,
        minutes: &str,
    ) -> Result<SettingsSummary, String> {
        self.update_pet_string(
            "sedentaryReminderMinutes",
            sanitize_pet_reminder_minutes(minutes),
        )
    }

    pub fn set_pet_late_night_reminder_minutes(
        &self,
        minutes: &str,
    ) -> Result<SettingsSummary, String> {
        self.update_pet_string(
            "lateNightReminderMinutes",
            sanitize_pet_reminder_minutes(minutes),
        )
    }

    pub fn cycle_pet_speech_mode(&self) -> Result<SettingsSummary, String> {
        let current = self.summary().pet_speech_mode;
        let next = match current.as_str() {
            "off" => "mixed",
            "mixed" => "encourage",
            "encourage" => "roast",
            "roast" => "flirty",
            "flirty" => "chuunibyou",
            "chuunibyou" => "off",
            _ => "off",
        };
        self.update_ai_pet_string("speechMode", next.to_string())
    }

    pub fn cycle_pet_speech_frequency(&self) -> Result<SettingsSummary, String> {
        let current = self.summary().pet_speech_frequency;
        let next = match current.as_str() {
            "quiet" => "normal",
            "normal" => "lively",
            "lively" => "chatterbox",
            "chatterbox" => "quiet",
            _ => "normal",
        };
        self.update_ai_pet_string("speechFrequency", next.to_string())
    }

    pub fn toggle_pet_speech_llm_enabled(&self) -> Result<SettingsSummary, String> {
        self.toggle_ai_pet_bool("speechLlmEnabled", false)
    }

    pub fn toggle_pet_speech_quiet_during_work(&self) -> Result<SettingsSummary, String> {
        self.toggle_ai_pet_bool("speechQuietDuringWork", true)
    }

    pub fn toggle_pet_speech_louder_at_night(&self) -> Result<SettingsSummary, String> {
        self.toggle_ai_pet_bool("speechLouderAtNight", false)
    }

    pub fn toggle_pet_speech_mute_on_fullscreen(&self) -> Result<SettingsSummary, String> {
        self.toggle_ai_pet_bool("speechMuteOnFullscreen", true)
    }

    pub fn toggle_pet_speech_quiet_hours(&self) -> Result<SettingsSummary, String> {
        self.update_raw_settings(|raw| {
            let pet = ai_pet_mut(raw)?;
            let enabled = pet
                .get("speechQuietHoursStart")
                .filter(|value| !value.is_null())
                .is_some()
                && pet
                    .get("speechQuietHoursEnd")
                    .filter(|value| !value.is_null())
                    .is_some();
            if enabled {
                pet.insert("speechQuietHoursStart".to_string(), Value::Null);
                pet.insert("speechQuietHoursEnd".to_string(), Value::Null);
            } else {
                pet.insert(
                    "speechQuietHoursStart".to_string(),
                    Value::Number(22.into()),
                );
                pet.insert("speechQuietHoursEnd".to_string(), Value::Number(8.into()));
            }
            Ok(summary_from_raw(raw))
        })
    }

    pub fn toggle_pet_speech_temporary_mute(&self) -> Result<SettingsSummary, String> {
        self.set_pet_speech_temporary_mute(!self.summary().pet_speech_temporary_muted)
    }

    pub fn set_pet_speech_temporary_mute(&self, muted: bool) -> Result<SettingsSummary, String> {
        self.update_raw_settings(|raw| {
            let pet = ai_pet_mut(raw)?;
            if muted {
                pet.insert(
                    "speechTemporaryMuteUntil".to_string(),
                    Value::Number((current_unix_seconds() + 1800).into()),
                );
            } else {
                pet.insert("speechTemporaryMuteUntil".to_string(), Value::Null);
            }
            Ok(summary_from_raw(raw))
        })
    }

    pub fn set_pet_speech_mode(&self, mode: &str) -> Result<SettingsSummary, String> {
        self.update_ai_pet_string("speechMode", sanitize_pet_speech_mode(mode))
    }

    pub fn set_pet_speech_frequency(&self, frequency: &str) -> Result<SettingsSummary, String> {
        self.update_ai_pet_string("speechFrequency", sanitize_pet_speech_frequency(frequency))
    }

    pub fn set_pet_speech_provider(&self, provider_id: &str) -> Result<SettingsSummary, String> {
        self.update_ai_pet_string("speechProviderId", sanitize_provider_reference(provider_id))
    }
}
