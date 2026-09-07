impl SettingsService {
    pub fn set_ai_memory_bool(&self, key: &str, value: bool) -> Result<SettingsSummary, String> {
        let key = match key {
            "enabled" => "enabled",
            "automaticInjectionEnabled" => "automaticInjectionEnabled",
            "automaticExtractionEnabled" => "automaticExtractionEnabled",
            "allowCrossProjectUserRecall" => "allowCrossProjectUserRecall",
            "extractionHeuristicGateEnabled" => "extractionHeuristicGateEnabled",
            "recallUseFts" => "recallUseFts",
            "privacyScrubEnabled" => "privacyScrubEnabled",
            _ => return Err("Unsupported memory setting.".to_string()),
        };
        self.update_raw_settings(|raw| {
            let memory = ai_memory_mut(raw)?;
            memory.insert(key.to_string(), Value::Bool(value));
            Ok(summary_from_raw(raw))
        })
    }

    pub fn set_ai_memory_number(&self, key: &str, value: &str) -> Result<SettingsSummary, String> {
        let (key, allowed, default, min, max) = match key {
            "extractionIdleDelaySeconds" => (
                "extractionIdleDelaySeconds",
                Some(&[60, 120, 300, 600, 900][..]),
                300,
                1,
                86_400,
            ),
            "sessionExtractionCooldownSeconds" => (
                "sessionExtractionCooldownSeconds",
                None,
                900,
                1,
                86_400,
            ),
            "extractionGrowthThresholdLines" => (
                "extractionGrowthThresholdLines",
                None,
                8,
                0,
                5_000,
            ),
            "maxIndexSessions" => ("maxIndexSessions", Some(&[5, 10, 20, 50, 100][..]), 20, 1, 500),
            "maxInjectedUserWorkingMemories" => (
                "maxInjectedUserWorkingMemories",
                None,
                4,
                0,
                100,
            ),
            "maxInjectedProjectWorkingMemories" => (
                "maxInjectedProjectWorkingMemories",
                None,
                6,
                0,
                100,
            ),
            "maxActiveWorkingEntries" => ("maxActiveWorkingEntries", None, 50, 1, 500),
            "maxSummaryVersions" => ("maxSummaryVersions", None, 10, 1, 100),
            "summaryTargetTokenBudget" => ("summaryTargetTokenBudget", None, 900, 100, 20_000),
            "maxInjectedSummaryTokens" => ("maxInjectedSummaryTokens", None, 900, 100, 20_000),
            "maxExtractionTranscriptLines" => (
                "maxExtractionTranscriptLines",
                None,
                80,
                10,
                5_000,
            ),
            "maxExtractionTranscriptTokens" => (
                "maxExtractionTranscriptTokens",
                None,
                8_000,
                1_000,
                200_000,
            ),
            _ => return Err("Unsupported memory setting.".to_string()),
        };
        let parsed = numeric_string(value, default, min, max);
        let value = allowed
            .and_then(|options| options.iter().find(|option| **option == parsed).copied())
            .unwrap_or(parsed);
        self.update_raw_settings(|raw| {
            let memory = ai_memory_mut(raw)?;
            memory.insert(key.to_string(), Value::Number(value.into()));
            Ok(summary_from_raw(raw))
        })
    }

    pub fn set_ai_memory_provider(&self, provider_id: &str) -> Result<SettingsSummary, String> {
        self.update_raw_settings(|raw| {
            let memory = ai_memory_mut(raw)?;
            memory.insert(
                "defaultExtractorProviderId".to_string(),
                Value::String(sanitize_provider_reference(provider_id)),
            );
            Ok(summary_from_raw(raw))
        })
    }
}
