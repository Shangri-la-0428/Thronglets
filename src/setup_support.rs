use serde_json::{Map, Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use toml::Value as TomlValue;

use thronglets::contracts::PREHOOK_MATCHER;

const CODEX_MCP_SERVER_ID: &str = "thronglets";
const CODEX_AGENTS_START: &str = "<!-- thronglets:codex:start -->";
const CODEX_AGENTS_END: &str = "<!-- thronglets:codex:end -->";
const CODEX_AGENTS_BLOCK: &str = r#"<!-- thronglets:codex:start -->
## Thronglets

If the `thronglets` MCP server is available in this Codex session:
- Before a high-impact `Edit`, `Write`, `Bash`, or delegated agent step, call `substrate_query` with the current task context.
- After a meaningful tool result, call `trace_record` with the outcome and task context.
- Treat Thronglets output as internal decision guidance. Use it to choose the next step; do not dump raw substrate warnings back to the user.
<!-- thronglets:codex:end -->
"#;
const OPENCLAW_PLUGIN_ID: &str = "thronglets-ai";
const OPENCLAW_PLUGIN_MANIFEST: &str =
    include_str!("../assets/openclaw-plugin/openclaw.plugin.json");
const OPENCLAW_PLUGIN_INDEX: &str = include_str!("../assets/openclaw-plugin/index.mjs");

pub struct ClaudeSetupResult {
    pub settings_path: PathBuf,
    pub added_post_hook: bool,
    pub added_pre_hook: bool,
}

pub struct OpenClawSetupResult {
    pub config_path: PathBuf,
    pub plugin_dir: PathBuf,
    pub created_config: bool,
    pub restarted_gateway: bool,
}

pub struct CodexSetupResult {
    pub config_path: PathBuf,
    pub agents_path: PathBuf,
    pub created_config: bool,
    pub updated_server: bool,
    pub updated_agents_memory: bool,
}

pub fn install_claude(home_dir: &Path, bin_path: &Path) -> io::Result<ClaudeSetupResult> {
    let settings_path = home_dir.join(".claude").join("settings.json");
    let bin_str = bin_path.to_string_lossy().to_string();

    let mut settings: Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".into());
        serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    if settings["hooks"].is_null() {
        settings["hooks"] = json!({});
    }

    let post_hook = json!({
        "matcher": "",
        "hooks": [{"type": "command", "command": format!("{bin_str} hook")}]
    });
    let added_post_hook = ensure_hook(
        &mut settings["hooks"]["PostToolUse"],
        &post_hook,
        "thronglets hook",
    );

    let pre_hook = json!({
        "matcher": PREHOOK_MATCHER,
        "hooks": [{"type": "command", "command": format!("{bin_str} prehook")}]
    });
    let added_pre_hook = ensure_hook(
        &mut settings["hooks"]["PreToolUse"],
        &pre_hook,
        "thronglets prehook",
    );

    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let formatted = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, formatted)?;

    Ok(ClaudeSetupResult {
        settings_path,
        added_post_hook,
        added_pre_hook,
    })
}

pub fn install_openclaw(
    home_dir: &Path,
    data_dir: &Path,
    bin_path: &Path,
    restart_gateway: bool,
) -> io::Result<Option<OpenClawSetupResult>> {
    if !should_configure_openclaw(home_dir) {
        return Ok(None);
    }

    let config_path = home_dir.join(".openclaw").join("openclaw.json");
    let created_config = !config_path.exists();
    let plugin_dir = data_dir.join(OPENCLAW_PLUGIN_ID);

    write_openclaw_plugin_assets(&plugin_dir)?;

    let mut config: Value = if config_path.exists() {
        let content = fs::read_to_string(&config_path).unwrap_or_else(|_| "{}".into());
        serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    configure_openclaw_config(&mut config, &plugin_dir, bin_path, data_dir);

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let formatted = serde_json::to_string_pretty(&config)?;
    fs::write(&config_path, formatted)?;

    let restarted_gateway = if restart_gateway {
        restart_openclaw_gateway()
    } else {
        false
    };

    Ok(Some(OpenClawSetupResult {
        config_path,
        plugin_dir,
        created_config,
        restarted_gateway,
    }))
}

pub fn install_codex(
    home_dir: &Path,
    data_dir: &Path,
    bin_path: &Path,
) -> io::Result<Option<CodexSetupResult>> {
    if !should_configure_codex(home_dir) {
        return Ok(None);
    }

    let codex_dir = home_dir.join(".codex");
    let config_path = codex_dir.join("config.toml");
    let agents_path = codex_dir.join("AGENTS.md");
    let created_config = !config_path.exists();

    fs::create_dir_all(&codex_dir)?;

    let mut config: toml::Table = if config_path.exists() {
        let content = fs::read_to_string(&config_path).unwrap_or_default();
        toml::from_str(&content).unwrap_or_default()
    } else {
        toml::Table::new()
    };
    let updated_server = configure_codex_config(&mut config, bin_path, data_dir);
    let formatted =
        toml::to_string_pretty(&config).map_err(|error| io::Error::other(error.to_string()))?;
    fs::write(&config_path, formatted)?;

    let updated_agents_memory = ensure_codex_agents_block(&agents_path)?;

    Ok(Some(CodexSetupResult {
        config_path,
        agents_path,
        created_config,
        updated_server,
        updated_agents_memory,
    }))
}

fn ensure_hook(target: &mut Value, hook: &Value, command_fragment: &str) -> bool {
    if let Some(arr) = target.as_array_mut() {
        let has_hook = arr.iter().any(|entry| {
            entry["hooks"].as_array().is_some_and(|hooks| {
                hooks.iter().any(|candidate| {
                    candidate["command"]
                        .as_str()
                        .is_some_and(|command| command.contains(command_fragment))
                })
            })
        });
        if has_hook {
            false
        } else {
            arr.push(hook.clone());
            true
        }
    } else {
        *target = json!([hook.clone()]);
        true
    }
}

fn should_configure_openclaw(home_dir: &Path) -> bool {
    home_dir.join(".openclaw").exists()
        || home_dir.join(".config").join("openclaw").exists()
        || executable_on_path("openclaw")
}

fn should_configure_codex(home_dir: &Path) -> bool {
    home_dir.join(".codex").exists() || executable_on_path("codex")
}

fn write_openclaw_plugin_assets(plugin_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(plugin_dir)?;
    fs::write(
        plugin_dir.join("openclaw.plugin.json"),
        OPENCLAW_PLUGIN_MANIFEST,
    )?;
    fs::write(plugin_dir.join("index.mjs"), OPENCLAW_PLUGIN_INDEX)?;
    Ok(())
}

fn configure_openclaw_config(
    config: &mut Value,
    plugin_dir: &Path,
    bin_path: &Path,
    data_dir: &Path,
) {
    let root = object_mut(config);
    let plugins = object_mut(root.entry("plugins").or_insert_with(|| json!({})));
    push_unique_string(
        plugins.entry("allow").or_insert_with(|| json!([])),
        OPENCLAW_PLUGIN_ID,
    );

    let load = object_mut(plugins.entry("load").or_insert_with(|| json!({})));
    push_unique_string(
        load.entry("paths").or_insert_with(|| json!([])),
        plugin_dir.to_string_lossy().as_ref(),
    );

    let entries = object_mut(plugins.entry("entries").or_insert_with(|| json!({})));
    let plugin_entry = object_mut(
        entries
            .entry(OPENCLAW_PLUGIN_ID)
            .or_insert_with(|| json!({})),
    );
    plugin_entry.insert("enabled".into(), Value::Bool(true));
    plugin_entry.insert(
        "config".into(),
        json!({
            "binaryPath": bin_path.to_string_lossy(),
            "dataDir": data_dir.to_string_lossy(),
        }),
    );

    let installs = object_mut(plugins.entry("installs").or_insert_with(|| json!({})));
    installs.insert(
        OPENCLAW_PLUGIN_ID.into(),
        json!({
            "source": "path",
            "spec": OPENCLAW_PLUGIN_ID,
            "sourcePath": plugin_dir.to_string_lossy(),
            "installPath": plugin_dir.to_string_lossy(),
            "version": env!("CARGO_PKG_VERSION"),
            "resolvedName": OPENCLAW_PLUGIN_ID,
            "resolvedVersion": env!("CARGO_PKG_VERSION"),
            "resolvedSpec": format!("{OPENCLAW_PLUGIN_ID}@{}", env!("CARGO_PKG_VERSION")),
        }),
    );
}

fn restart_openclaw_gateway() -> bool {
    if spawn_openclaw_gateway(&["gateway", "restart"]) {
        return true;
    }

    spawn_openclaw_gateway(&["gateway", "start"])
}

fn spawn_openclaw_gateway(args: &[&str]) -> bool {
    Command::new("openclaw")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn executable_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .any(|candidate| candidate.is_file())
}

fn configure_codex_config(config: &mut toml::Table, bin_path: &Path, data_dir: &Path) -> bool {
    let mcp_servers = config
        .entry("mcp_servers")
        .or_insert_with(|| TomlValue::Table(toml::Table::new()));
    let mcp_servers = mcp_servers
        .as_table_mut()
        .expect("mcp_servers should always be a table");

    let created_server = !mcp_servers.contains_key(CODEX_MCP_SERVER_ID);
    let server = mcp_servers
        .entry(CODEX_MCP_SERVER_ID)
        .or_insert_with(|| TomlValue::Table(toml::Table::new()));
    let server = server
        .as_table_mut()
        .expect("mcp_servers.<name> should always be a table");

    server.insert(
        "command".into(),
        TomlValue::String(bin_path.to_string_lossy().into_owned()),
    );
    server.insert(
        "args".into(),
        TomlValue::Array(vec![
            TomlValue::String("--data-dir".into()),
            TomlValue::String(data_dir.to_string_lossy().into_owned()),
            TomlValue::String("mcp".into()),
        ]),
    );

    created_server
}

fn ensure_codex_agents_block(agents_path: &Path) -> io::Result<bool> {
    let original = if agents_path.exists() {
        fs::read_to_string(agents_path)?
    } else {
        String::new()
    };

    let updated = if let (Some(start), Some(end)) = (
        original.find(CODEX_AGENTS_START),
        original.find(CODEX_AGENTS_END),
    ) {
        let end = end + CODEX_AGENTS_END.len();
        let mut content = original.clone();
        content.replace_range(start..end, CODEX_AGENTS_BLOCK);
        content
    } else if original.trim().is_empty() {
        format!("{CODEX_AGENTS_BLOCK}\n")
    } else {
        format!("{}\n\n{}\n", original.trim_end(), CODEX_AGENTS_BLOCK)
    };

    if updated == original {
        return Ok(false);
    }

    fs::write(agents_path, updated)?;
    Ok(true)
}

fn object_mut(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value
        .as_object_mut()
        .expect("value was converted to object")
}

fn push_unique_string(target: &mut Value, item: &str) {
    if !target.is_array() {
        *target = json!([]);
    }

    let arr = target.as_array_mut().expect("value was converted to array");
    let exists = arr.iter().any(|value| value.as_str() == Some(item));
    if !exists {
        arr.push(Value::String(item.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_openclaw_writes_plugin_assets_and_config() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        fs::create_dir_all(home.join(".openclaw")).unwrap();

        let result = install_openclaw(&home, &data_dir, Path::new("/tmp/thronglets"), false)
            .unwrap()
            .unwrap();

        assert!(result.plugin_dir.join("openclaw.plugin.json").exists());
        assert!(result.plugin_dir.join("index.mjs").exists());

        let config: Value =
            serde_json::from_str(&fs::read_to_string(&result.config_path).unwrap()).unwrap();
        assert_eq!(
            config["plugins"]["entries"][OPENCLAW_PLUGIN_ID]["enabled"],
            Value::Bool(true)
        );
        assert_eq!(
            config["plugins"]["entries"][OPENCLAW_PLUGIN_ID]["config"]["binaryPath"],
            Value::String("/tmp/thronglets".into())
        );
        assert!(
            config["plugins"]["load"]["paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str() == Some(result.plugin_dir.to_string_lossy().as_ref()))
        );
    }

    #[test]
    fn install_openclaw_deduplicates_existing_entries() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        let config_dir = home.join(".openclaw");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("openclaw.json");
        let mut entries = Map::new();
        entries.insert(OPENCLAW_PLUGIN_ID.into(), json!({"enabled": true}));
        fs::write(
            &config_path,
            json!({
                "plugins": {
                    "allow": [OPENCLAW_PLUGIN_ID],
                    "load": {"paths": [data_dir.join(OPENCLAW_PLUGIN_ID).to_string_lossy().to_string()]},
                    "entries": entries,
                }
            })
            .to_string(),
        )
        .unwrap();

        install_openclaw(&home, &data_dir, Path::new("/tmp/thronglets"), false)
            .unwrap()
            .unwrap();

        let config: Value =
            serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
        assert_eq!(config["plugins"]["allow"].as_array().unwrap().len(), 1,);
        assert_eq!(
            config["plugins"]["load"]["paths"].as_array().unwrap().len(),
            1,
        );
    }

    #[test]
    fn install_codex_writes_mcp_server_and_agents_memory() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        fs::create_dir_all(home.join(".codex")).unwrap();

        let result = install_codex(&home, &data_dir, Path::new("/tmp/thronglets"))
            .unwrap()
            .unwrap();

        let config: toml::Table =
            toml::from_str(&fs::read_to_string(&result.config_path).unwrap()).unwrap();
        let server = config["mcp_servers"][CODEX_MCP_SERVER_ID]
            .as_table()
            .unwrap();
        assert_eq!(server["command"].as_str(), Some("/tmp/thronglets"));
        assert_eq!(
            server["args"].as_array().unwrap(),
            &vec![
                TomlValue::String("--data-dir".into()),
                TomlValue::String(data_dir.to_string_lossy().into_owned()),
                TomlValue::String("mcp".into()),
            ]
        );

        let agents = fs::read_to_string(&result.agents_path).unwrap();
        assert!(agents.contains(CODEX_AGENTS_START));
        assert!(agents.contains("substrate_query"));
        assert!(agents.contains("trace_record"));
    }

    #[test]
    fn install_codex_replaces_existing_managed_block() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        let codex_dir = home.join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("AGENTS.md"),
            format!("Intro\n\n{CODEX_AGENTS_START}\nold block\n{CODEX_AGENTS_END}\n"),
        )
        .unwrap();

        let result = install_codex(&home, &data_dir, Path::new("/tmp/thronglets"))
            .unwrap()
            .unwrap();

        assert!(result.updated_agents_memory);
        let agents = fs::read_to_string(codex_dir.join("AGENTS.md")).unwrap();
        assert!(!agents.contains("old block"));
        assert_eq!(agents.matches(CODEX_AGENTS_START).count(), 1);
    }
}
