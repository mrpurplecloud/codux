use super::{
    codewhale::uninstall_codewhale_hooks_in,
    codex::uninstall_codex_config,
    command::is_managed_hook_action,
    json::load_json_object,
    json::write_json_object,
    kimi::uninstall_kimi_hooks_in,
    legacy::{
        AGY_LEGACY_HOOKS, KIRO_LEGACY_HOOKS, LEGACY_JSON_HOOK_CONFIGS, LegacyHookDefinition,
        legacy_json_hook_path,
    },
};
use crate::ai_runtime::tool_driver::{AIRuntimeToolHookDriver, ai_runtime_tool_drivers};
use serde_json::{Map, Value};
use std::path::Path;

/// Remove every codux-managed hook entry this build (or a prior one) wrote into
/// the CLIs' own config files, leaving them genuinely hookless -- the point of
/// the non-intrusive runtime. Each format preserves the user's own hooks, skips
/// its write when nothing changed, and never creates a config file that was
/// absent (so a tool the user never set up stays untouched). Stripping is
/// idempotent, so it is safe to run on every start; once a config is clean the
/// subsequent runs are no-op no-write passes.
pub fn uninstall_managed_hook_configs_in(home_dir: &Path) -> Result<(), String> {
    for config in LEGACY_JSON_HOOK_CONFIGS {
        let path = legacy_json_hook_path(home_dir, config);
        // Codex's trust blocks are cleared while hooks.json still names the
        // managed hooks, and the check that decides it is read-only: a failed
        // or interrupted pass leaves the same evidence behind, so the next run
        // clears the trust again instead of stranding it forever. (The trust is
        // only cleared alongside a managed hook — a config whose hooks are all
        // the user's own keeps the trust they reviewed.)
        if config.tool == "codex" && has_managed_tool_hooks(&path, config.tool, config.definitions)?
        {
            uninstall_codex_config(&path)?;
        }
        uninstall_tool_hooks(&path, config.tool, config.definitions)?;
    }
    for driver in ai_runtime_tool_drivers() {
        match driver.hook {
            AIRuntimeToolHookDriver::CodeWhaleToml => {
                uninstall_codewhale_hooks_in(home_dir)?;
            }
            AIRuntimeToolHookDriver::OpenCodePlugin | AIRuntimeToolHookDriver::None => {}
        }
    }
    uninstall_kimi_hooks_in(home_dir)?;
    let agy_settings_path = home_dir
        .join(".gemini")
        .join("antigravity-cli")
        .join("settings.json");
    uninstall_tool_hooks(&agy_settings_path, "agy", AGY_LEGACY_HOOKS)?;
    uninstall_kiro_tool_hooks(
        &home_dir
            .join(".kiro")
            .join("agents")
            .join("codux-managed.json"),
        KIRO_LEGACY_HOOKS,
    )?;
    Ok(())
}

/// Strip every codux-managed hook entry (any codux owner) from a Standard JSON
/// config, preserving the user's own hooks and the rest of the file. Never
/// creates the file if it is absent, and `write_json_object` skips the write when
/// nothing changed.
fn uninstall_tool_hooks(
    path: &Path,
    tool: &str,
    definitions: &[LegacyHookDefinition],
) -> Result<bool, String> {
    if tool == "kiro" {
        return uninstall_kiro_tool_hooks(path, definitions);
    }
    let Some(stripped) = stripped_tool_hooks(path, tool, definitions)? else {
        return Ok(false);
    };
    if !stripped.removed_managed_hooks {
        return Ok(false);
    }
    write_json_object(path, stripped.root)?;
    Ok(true)
}

/// A config with every codux-managed hook removed, and whether one was there.
struct StrippedToolHooks {
    root: Map<String, Value>,
    removed_managed_hooks: bool,
}

/// Whether this config still carries a codux-managed hook entry. Reads only, so
/// asking costs nothing and can be repeated until the write that answers it
/// actually lands.
fn has_managed_tool_hooks(
    path: &Path,
    tool: &str,
    definitions: &[LegacyHookDefinition],
) -> Result<bool, String> {
    Ok(stripped_tool_hooks(path, tool, definitions)?
        .is_some_and(|stripped| stripped.removed_managed_hooks))
}

/// The config as it would be without any codux-managed hook. `None` when the
/// file is absent (nothing to strip, nothing to create).
fn stripped_tool_hooks(
    path: &Path,
    tool: &str,
    definitions: &[LegacyHookDefinition],
) -> Result<Option<StrippedToolHooks>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let mut root = load_json_object(path)?;
    let mut hooks = root
        .remove("hooks")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    let mut removed_managed_hooks = false;
    for (event_key, action) in removed_hook_definitions(tool) {
        removed_managed_hooks |=
            strip_managed_action_from_hooks(&mut hooks, event_key, action, None, Some(tool));
    }
    if tool == "claude" {
        removed_managed_hooks |= strip_managed_action_from_hooks(
            &mut hooks,
            "Notification",
            "notification",
            None,
            Some("claude"),
        );
    }
    for definition in definitions {
        removed_managed_hooks |= strip_managed_action_from_hooks(
            &mut hooks,
            definition.event_key,
            definition.action,
            None,
            Some(tool),
        );
    }

    if !hooks.is_empty() {
        root.insert("hooks".to_string(), Value::Object(hooks));
    }
    Ok(Some(StrippedToolHooks {
        root,
        removed_managed_hooks,
    }))
}

/// Kiro's flat-array uninstall: entries sit directly under each event key, so
/// filter the codux-managed ones out per entry. Leaves the inert
/// name/description/prompt agent fields (removing them risks clobbering a
/// user-authored agent).
fn uninstall_kiro_tool_hooks(
    path: &Path,
    definitions: &[LegacyHookDefinition],
) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let mut root = load_json_object(path)?;
    let mut hooks = root
        .remove("hooks")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    let mut removed_managed_hooks = false;
    for definition in definitions {
        let entries = hooks
            .remove(definition.event_key)
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        let cleaned = entries
            .into_iter()
            .filter(|entry| {
                let managed = is_managed_hook_action(entry, definition.action, None, Some("kiro"));
                removed_managed_hooks |= managed;
                !managed
            })
            .collect::<Vec<_>>();
        if !cleaned.is_empty() {
            hooks.insert(definition.event_key.to_string(), Value::Array(cleaned));
        }
    }

    if !removed_managed_hooks {
        return Ok(false);
    }

    if !hooks.is_empty() {
        root.insert("hooks".to_string(), Value::Object(hooks));
    }
    write_json_object(path, root)?;
    Ok(true)
}

fn removed_hook_definitions(tool: &str) -> &'static [(&'static str, &'static str)] {
    match tool {
        "codex" => &[
            ("PreToolUse", "codex-pre-tool-use"),
            ("PostToolUse", "codex-post-tool-use"),
            ("SessionEnd", "codex-session-end"),
        ],
        "claude" => &[
            ("PreToolUse", "pre-tool-use"),
            ("PostToolUse", "post-tool-use"),
            ("PostToolUseFailure", "post-tool-use-failure"),
        ],
        _ => &[],
    }
}

fn strip_managed_action_from_hooks(
    hooks: &mut Map<String, Value>,
    event_key: &str,
    action: &str,
    owner: Option<&str>,
    tool: Option<&str>,
) -> bool {
    let groups = hooks
        .remove(event_key)
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    if groups.is_empty() {
        return false;
    }

    let mut cleaned_groups = Vec::new();
    let mut removed_managed_hooks = false;
    for group in groups {
        let Some(group_object) = group.as_object() else {
            continue;
        };
        let next_hooks = group_object
            .get("hooks")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|item| {
                let managed = is_managed_hook_action(item, action, owner, tool);
                removed_managed_hooks |= managed;
                !managed
            })
            .collect::<Vec<_>>();
        if next_hooks.is_empty() {
            continue;
        }
        let mut next_group = group_object.clone();
        next_group.insert("hooks".to_string(), Value::Array(next_hooks));
        cleaned_groups.push(Value::Object(next_group));
    }

    if !cleaned_groups.is_empty() {
        hooks.insert(event_key.to_string(), Value::Array(cleaned_groups));
    }
    removed_managed_hooks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_managed_action_removes_all_codux_owners_when_owner_is_unspecified() {
        let mut hooks = Map::new();
        hooks.insert(
            "Stop".to_string(),
            json!([
                {
                    "matcher": "",
                    "hooks": [
                        {
                            "type": "command",
                            "command": "'/tmp/codux/dmux-ai-state.sh' 'codex-stop' 'codux' 'codex'"
                        },
                        {
                            "type": "command",
                            "command": "'/tmp/codux-dev/dmux-ai-state.sh' 'codex-stop' 'codux-dev' 'codex'"
                        },
                        {
                            "type": "command",
                            "command": "'/tmp/custom.sh' 'codex-stop' 'custom' 'codex'"
                        }
                    ]
                }
            ])
            .as_array()
            .cloned()
            .map(Value::Array)
            .unwrap(),
        );

        strip_managed_action_from_hooks(&mut hooks, "Stop", "codex-stop", None, Some("codex"));

        let remaining = hooks
            .get("Stop")
            .and_then(|value| value.as_array())
            .and_then(|groups| groups.first())
            .and_then(|group| group.get("hooks"))
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0]["command"].as_str().unwrap().contains("custom"));
    }

    #[test]
    fn repeated_uninstall_preserves_user_codex_hook_trust() {
        let home = std::env::temp_dir().join(format!(
            "codux-codex-trust-uninstall-{}",
            uuid::Uuid::new_v4()
        ));
        let codex_dir = home.join(".codex");
        let hooks_path = codex_dir.join("hooks.json");
        let config_path = codex_dir.join("config.toml");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            &hooks_path,
            serde_json::json!({
                "hooks": {
                    "SessionStart": [
                        {
                            "hooks": [{
                                "type": "command",
                                "command": "'/tmp/codux/dmux-ai-state.sh' 'codex-session-start' 'codux' 'codex'"
                            }]
                        },
                        {
                            "hooks": [{
                                "type": "command",
                                "command": "'/tmp/custom-session-start.sh'"
                            }]
                        }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &config_path,
            format!(
                "[features]\nhooks = true\n\n[hooks.state.\"{}:session_start:1:0\"]\ntrusted_hash = \"sha256:stale\"\n",
                hooks_path.display()
            ),
        )
        .unwrap();

        uninstall_managed_hook_configs_in(&home).unwrap();
        assert!(
            !std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("trusted_hash")
        );

        let trusted_config = format!(
            "[features]\nhooks = true\n\n[hooks.state.\"{}:session_start:0:0\"]\ntrusted_hash = \"sha256:user-reviewed\"\n",
            hooks_path.display()
        );
        std::fs::write(&config_path, &trusted_config).unwrap();

        uninstall_managed_hook_configs_in(&home).unwrap();

        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            trusted_config
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_uninstall_retries_trust_cleanup_after_a_failed_pass() {
        use std::os::unix::fs::PermissionsExt;

        let home =
            std::env::temp_dir().join(format!("codux-codex-trust-retry-{}", uuid::Uuid::new_v4()));
        let codex_dir = home.join(".codex");
        let hooks_path = codex_dir.join("hooks.json");
        let config_path = codex_dir.join("config.toml");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let managed_hooks = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "'/tmp/codux/dmux-ai-state.sh' 'codex-session-start' 'codux' 'codex'"
                    }]
                }]
            }
        })
        .to_string();
        std::fs::write(&hooks_path, &managed_hooks).unwrap();
        let stale_trust = format!(
            "[features]\nhooks = true\n\n[hooks.state.\"{}:session_start:0:0\"]\ntrusted_hash = \"sha256:stale\"\n",
            hooks_path.display()
        );
        std::fs::write(&config_path, &stale_trust).unwrap();

        // The trust cleanup cannot land this pass — neither when the config
        // cannot be read (a failed read must not be mistaken for "no config")
        // nor when it cannot be written.
        for mode in [0o000, 0o444] {
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(mode)).unwrap();
            if std::fs::read_to_string(&config_path).is_ok()
                && std::fs::write(&config_path, &stale_trust).is_ok()
            {
                // Running as root: permissions cannot make this fail.
                std::fs::remove_dir_all(home).unwrap();
                return;
            }
            assert!(
                uninstall_managed_hook_configs_in(&home).is_err(),
                "mode {mode:o} must report the failure"
            );
            // The managed hook is still on disk, so the next pass still knows
            // the trust block is dead.
            assert!(
                std::fs::read_to_string(&hooks_path)
                    .unwrap()
                    .contains("codex-session-start"),
                "mode {mode:o} must keep the retry evidence"
            );
        }

        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        uninstall_managed_hook_configs_in(&home).unwrap();

        assert!(
            !std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("trusted_hash")
        );
        assert!(
            !std::fs::read_to_string(&hooks_path)
                .unwrap()
                .contains("codex-session-start")
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn uninstall_strips_legacy_kiro_agent_hooks() {
        let home = std::env::temp_dir().join(format!(
            "codux-legacy-kiro-uninstall-{}",
            uuid::Uuid::new_v4()
        ));
        let path = home.join(".kiro").join("agents").join("codux-managed.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::json!({
                "name": "Codux Managed",
                "hooks": {
                    "agentSpawn": [{
                        "command": "'/tmp/dmux-ai-state.sh' 'session-start' 'codux' 'kiro'",
                        "timeout_ms": 5000,
                        "matcher": ""
                    }],
                    "userPromptSubmit": [{
                        "command": "'/tmp/dmux-ai-state.sh' 'prompt-submit' 'codux' 'kiro'",
                        "timeout_ms": 5000,
                        "matcher": ""
                    }],
                    "stop": [{
                        "command": "'/tmp/custom.sh' 'stop' 'custom' 'kiro'",
                        "timeout_ms": 5000,
                        "matcher": ""
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();

        uninstall_managed_hook_configs_in(&home).unwrap();

        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let hooks = value["hooks"].as_object().unwrap();
        assert!(!hooks.contains_key("agentSpawn"));
        assert!(!hooks.contains_key("userPromptSubmit"));
        assert_eq!(hooks["stop"].as_array().unwrap().len(), 1);
        assert!(
            hooks["stop"][0]["command"]
                .as_str()
                .unwrap()
                .contains("custom")
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
