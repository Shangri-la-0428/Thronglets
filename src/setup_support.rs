use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
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
const RESTART_STATE_FILE: &str = "adapter-restart-state.json";
const MANAGED_LAUNCHER_NAME: &str = "thronglets-managed";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct RestartState {
    agents: BTreeMap<String, RestartStateEntry>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct RestartStateEntry {
    restart_pending: bool,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Claude,
    Codex,
    OpenClaw,
    Generic,
}

impl AdapterKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::OpenClaw => "openclaw",
            Self::Generic => "generic",
        }
    }

    pub fn integration(self) -> &'static str {
        match self {
            Self::Generic => "contract",
            _ => "native",
        }
    }

    pub fn apply_by_default(self) -> bool {
        !matches!(self, Self::Generic)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterDetection {
    pub agent: String,
    pub present: bool,
    pub configurable: bool,
    pub integration: String,
    pub apply_by_default: bool,
    pub paths: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookRuntimeExample {
    pub prehook: String,
    pub hook: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookContractExamples {
    pub prehook_stdin: Value,
    pub hook_stdin: Value,
    pub runtimes: BTreeMap<String, HookRuntimeExample>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterPlan {
    pub agent: String,
    pub present: bool,
    pub configurable: bool,
    pub integration: String,
    pub apply_by_default: bool,
    pub requires_restart: bool,
    pub restart_command: Option<String>,
    pub paths: Vec<String>,
    pub actions: Vec<String>,
    pub apply_command: Option<String>,
    pub doctor_command: String,
    pub contract: Option<HookContractExamples>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterDoctor {
    pub agent: String,
    pub present: bool,
    pub status: String,
    pub healthy: bool,
    pub restart_pending: bool,
    pub fix_command: Option<String>,
    pub restart_command: Option<String>,
    pub checks: Vec<AdapterCheck>,
    pub remediation: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterApplyResult {
    pub agent: String,
    pub applied: bool,
    pub changed: Vec<String>,
    pub requires_restart: bool,
    pub restart_command: Option<String>,
    pub paths: Vec<String>,
    pub note: Option<String>,
}

fn restart_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RESTART_STATE_FILE)
}

fn load_restart_state(data_dir: &Path) -> RestartState {
    let path = restart_state_path(data_dir);
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return RestartState::default(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

fn save_restart_state(data_dir: &Path, state: &RestartState) -> io::Result<()> {
    let path = restart_state_path(data_dir);
    if state.agents.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
    }

    fs::create_dir_all(data_dir)?;
    let formatted =
        serde_json::to_string_pretty(state).map_err(|error| io::Error::other(error.to_string()))?;
    fs::write(path, formatted)
}

pub fn restart_pending(data_dir: &Path, agent: AdapterKind) -> bool {
    if restart_command(agent).is_none() {
        return false;
    }
    load_restart_state(data_dir)
        .agents
        .get(agent.key())
        .is_some_and(|entry| entry.restart_pending)
}

pub fn set_restart_pending(data_dir: &Path, agent: AdapterKind, pending: bool) -> io::Result<()> {
    if restart_command(agent).is_none() {
        return Ok(());
    }

    let mut state = load_restart_state(data_dir);
    if pending {
        state.agents.insert(
            agent.key().into(),
            RestartStateEntry {
                restart_pending: true,
            },
        );
    } else {
        state.agents.remove(agent.key());
    }
    save_restart_state(data_dir, &state)
}

pub fn clear_restart_pending(data_dir: &Path, agent: AdapterKind) -> io::Result<bool> {
    let was_pending = restart_pending(data_dir, agent);
    set_restart_pending(data_dir, agent, false)?;
    Ok(was_pending)
}

pub fn install_claude(
    home_dir: &Path,
    data_dir: &Path,
    bin_path: &Path,
) -> io::Result<ClaudeSetupResult> {
    let settings_path = home_dir.join(".claude").join("settings.json");
    let launcher = ensure_managed_launcher(data_dir, bin_path)?;
    let launcher_cmd = shell_quote_path(&launcher);

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
        "hooks": [{"type": "command", "command": format!("{launcher_cmd} hook")}]
    });
    let added_post_hook = ensure_hook(
        &mut settings["hooks"]["PostToolUse"],
        &post_hook,
        launcher.to_string_lossy().as_ref(),
    );

    let pre_hook = json!({
        "matcher": PREHOOK_MATCHER,
        "hooks": [{"type": "command", "command": format!("{launcher_cmd} prehook")}]
    });
    let added_pre_hook = ensure_hook(
        &mut settings["hooks"]["PreToolUse"],
        &pre_hook,
        launcher.to_string_lossy().as_ref(),
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
    force_install: bool,
) -> io::Result<Option<OpenClawSetupResult>> {
    if !force_install && !should_configure_openclaw(home_dir) {
        return Ok(None);
    }

    let config_path = openclaw_config_path(home_dir);
    let created_config = !config_path.exists();
    let plugin_dir = data_dir.join(OPENCLAW_PLUGIN_ID);
    let launcher = ensure_managed_launcher(data_dir, bin_path)?;

    write_openclaw_plugin_assets(&plugin_dir)?;

    let mut config: Value = if config_path.exists() {
        let content = fs::read_to_string(&config_path).unwrap_or_else(|_| "{}".into());
        serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    configure_openclaw_config(&mut config, &plugin_dir, &launcher, data_dir);

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
    force_install: bool,
) -> io::Result<Option<CodexSetupResult>> {
    if !force_install && !should_configure_codex(home_dir) {
        return Ok(None);
    }

    let codex_dir = home_dir.join(".codex");
    let config_path = codex_dir.join("config.toml");
    let agents_path = codex_dir.join("AGENTS.md");
    let created_config = !config_path.exists();
    let launcher = ensure_managed_launcher(data_dir, bin_path)?;

    fs::create_dir_all(&codex_dir)?;

    let mut config: toml::Table = if config_path.exists() {
        let content = fs::read_to_string(&config_path).unwrap_or_default();
        toml::from_str(&content).unwrap_or_default()
    } else {
        toml::Table::new()
    };
    let updated_server = configure_codex_config(&mut config, &launcher, data_dir);
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

pub fn detect_adapter(home_dir: &Path, data_dir: &Path, agent: AdapterKind) -> AdapterDetection {
    match agent {
        AdapterKind::Claude => {
            let settings_path = claude_settings_path(home_dir);
            let present = should_configure_claude(home_dir);
            AdapterDetection {
                agent: agent.key().into(),
                present,
                configurable: true,
                integration: agent.integration().into(),
                apply_by_default: agent.apply_by_default(),
                paths: vec![settings_path.display().to_string()],
                note: (!present).then_some(
                    "Claude was not detected, but Thronglets can still preseed ~/.claude/settings.json."
                        .into(),
                ),
            }
        }
        AdapterKind::Codex => {
            let config_path = codex_config_path(home_dir);
            let agents_path = codex_agents_path(home_dir);
            let present = should_configure_codex(home_dir);
            AdapterDetection {
                agent: agent.key().into(),
                present,
                configurable: true,
                integration: agent.integration().into(),
                apply_by_default: agent.apply_by_default(),
                paths: vec![
                    config_path.display().to_string(),
                    agents_path.display().to_string(),
                ],
                note: (!present).then_some(
                    "Codex was not detected; apply-plan can still bootstrap ~/.codex if explicitly requested."
                        .into(),
                ),
            }
        }
        AdapterKind::OpenClaw => {
            let config_path = openclaw_config_path(home_dir);
            let plugin_dir = data_dir.join(OPENCLAW_PLUGIN_ID);
            let present = should_configure_openclaw(home_dir);
            AdapterDetection {
                agent: agent.key().into(),
                present,
                configurable: present,
                integration: agent.integration().into(),
                apply_by_default: agent.apply_by_default(),
                paths: vec![
                    config_path.display().to_string(),
                    plugin_dir.display().to_string(),
                ],
                note: (!present).then_some(
                    "OpenClaw was not detected; apply-plan will skip it unless explicitly targeted."
                        .into(),
                ),
            }
        }
        AdapterKind::Generic => AdapterDetection {
            agent: agent.key().into(),
            present: true,
            configurable: true,
            integration: agent.integration().into(),
            apply_by_default: agent.apply_by_default(),
            paths: vec![],
            note: Some(
                "Unknown agents should self-bootstrap by calling `thronglets prehook` and `thronglets hook` with the universal JSON contract."
                    .into(),
            ),
        },
    }
}

pub fn install_plan(home_dir: &Path, data_dir: &Path, agent: AdapterKind) -> AdapterPlan {
    let detection = detect_adapter(home_dir, data_dir, agent);
    let launcher_path = managed_launcher_path(data_dir);
    match agent {
        AdapterKind::Claude => AdapterPlan {
            agent: detection.agent,
            present: detection.present,
            configurable: detection.configurable,
            integration: detection.integration,
            apply_by_default: detection.apply_by_default,
            requires_restart: false,
            restart_command: restart_command(agent),
            paths: detection.paths,
            actions: vec![
                format!(
                    "Write PostToolUse hook in {} that runs managed launcher `{}`",
                    claude_settings_path(home_dir).display(),
                    launcher_path.display()
                ) + " hook",
                format!(
                    "Write PreToolUse hook with matcher `{PREHOOK_MATCHER}` that runs managed launcher `{}`",
                    launcher_path.display()
                ) + " prehook",
            ],
            apply_command: Some("thronglets apply-plan --agent claude".into()),
            doctor_command: "thronglets doctor --agent claude".into(),
            contract: None,
        },
        AdapterKind::Codex => AdapterPlan {
            agent: detection.agent,
            present: detection.present,
            configurable: detection.configurable,
            integration: detection.integration,
            apply_by_default: detection.apply_by_default,
            requires_restart: true,
            restart_command: restart_command(agent),
            paths: detection.paths,
            actions: vec![
                format!(
                    "Write [mcp_servers.{CODEX_MCP_SERVER_ID}] in {} pointing to managed launcher `{}` with `--data-dir {}` and `mcp`",
                    codex_config_path(home_dir).display(),
                    launcher_path.display(),
                    data_dir.display()
                ),
                format!(
                    "Write or refresh the managed Thronglets block in {}",
                    codex_agents_path(home_dir).display()
                ),
            ],
            apply_command: Some("thronglets apply-plan --agent codex".into()),
            doctor_command: "thronglets doctor --agent codex".into(),
            contract: None,
        },
        AdapterKind::OpenClaw => AdapterPlan {
            agent: detection.agent,
            present: detection.present,
            configurable: detection.configurable,
            integration: detection.integration,
            apply_by_default: detection.apply_by_default,
            requires_restart: true,
            restart_command: restart_command(agent),
            paths: detection.paths,
            actions: vec![
                format!(
                    "Write plugin assets into {}",
                    data_dir.join(OPENCLAW_PLUGIN_ID).display()
                ),
                format!(
                    "Enable `{OPENCLAW_PLUGIN_ID}` in {} and point it at managed launcher `{}`",
                    openclaw_config_path(home_dir).display(),
                    launcher_path.display()
                ),
                "Request `openclaw gateway restart` in the background.".into(),
            ],
            apply_command: Some("thronglets apply-plan --agent openclaw".into()),
            doctor_command: "thronglets doctor --agent openclaw".into(),
            contract: None,
        },
        AdapterKind::Generic => AdapterPlan {
            agent: detection.agent,
            present: detection.present,
            configurable: detection.configurable,
            integration: detection.integration,
            apply_by_default: detection.apply_by_default,
            requires_restart: false,
            restart_command: restart_command(agent),
            paths: detection.paths,
            actions: vec![
                "Before high-impact tools, send a JSON payload to `thronglets prehook` and treat stdout as internal decision guidance.".into(),
                "After tool execution, send the same payload plus `tool_response` to `thronglets hook`.".into(),
            ],
            apply_command: None,
            doctor_command: "thronglets doctor --agent generic".into(),
            contract: Some(hook_contract_examples()),
        },
    }
}

pub fn doctor_adapter(home_dir: &Path, data_dir: &Path, agent: AdapterKind) -> AdapterDoctor {
    match agent {
        AdapterKind::Claude => doctor_claude(home_dir, data_dir),
        AdapterKind::Codex => doctor_codex(home_dir, data_dir),
        AdapterKind::OpenClaw => doctor_openclaw(home_dir, data_dir),
        AdapterKind::Generic => AdapterDoctor {
            agent: agent.key().into(),
            present: true,
            status: "healthy".into(),
            healthy: true,
            restart_pending: false,
            fix_command: None,
            restart_command: restart_command(agent),
            checks: vec![AdapterCheck {
                name: "contract".into(),
                ok: true,
                detail:
                    "Generic adapters do not require local config. Use the hook/prehook contract."
                        .into(),
            }],
            remediation: vec![],
            note: Some(
                "Run `thronglets install-plan --agent generic --json` to fetch the exact contract examples."
                    .into(),
            ),
        },
    }
}

fn should_configure_claude(home_dir: &Path) -> bool {
    home_dir.join(".claude").exists() || executable_on_path("claude")
}

fn claude_settings_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".claude").join("settings.json")
}

fn codex_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".codex").join("config.toml")
}

fn codex_agents_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".codex").join("AGENTS.md")
}

fn restart_command(agent: AdapterKind) -> Option<String> {
    match agent {
        AdapterKind::Codex => Some("Restart Codex".into()),
        AdapterKind::OpenClaw => Some("openclaw gateway restart".into()),
        AdapterKind::Claude | AdapterKind::Generic => None,
    }
}

fn runtime_ready_command(agent: AdapterKind) -> Option<String> {
    match agent {
        AdapterKind::Codex => Some("thronglets runtime-ready --agent codex".into()),
        AdapterKind::OpenClaw => Some("thronglets runtime-ready --agent openclaw".into()),
        AdapterKind::Claude | AdapterKind::Generic => None,
    }
}

fn openclaw_root_dir(home_dir: &Path) -> PathBuf {
    let legacy = home_dir.join(".openclaw");
    if legacy.exists() {
        legacy
    } else {
        let xdg = home_dir.join(".config").join("openclaw");
        if xdg.exists() { xdg } else { legacy }
    }
}

fn openclaw_config_path(home_dir: &Path) -> PathBuf {
    openclaw_root_dir(home_dir).join("openclaw.json")
}

fn hook_contract_examples() -> HookContractExamples {
    let prehook_stdin = json!({
        "agent_source": "my-agent",
        "model": "my-model",
        "session_id": "session-123",
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "src/main.rs"
        }
    });
    let mut hook_stdin = prehook_stdin.clone();
    if let Some(obj) = hook_stdin.as_object_mut() {
        obj.insert("tool_response".into(), json!({"success": true}));
    }
    let mut runtimes = BTreeMap::new();
    runtimes.insert(
        "node".into(),
        HookRuntimeExample {
            prehook: "const payload = {...};\nconst stdout = execFileSync(\"thronglets\", [\"prehook\"], { input: JSON.stringify(payload) });".into(),
            hook: "const payload = {..., tool_response: {...}};\nexecFileSync(\"thronglets\", [\"hook\"], { input: JSON.stringify(payload) });".into(),
        },
    );
    runtimes.insert(
        "python".into(),
        HookRuntimeExample {
            prehook: "payload = {...}\nstdout = subprocess.run([\"thronglets\", \"prehook\"], input=json.dumps(payload), text=True, capture_output=True, check=True).stdout".into(),
            hook: "payload = {**payload, \"tool_response\": {...}}\nsubprocess.run([\"thronglets\", \"hook\"], input=json.dumps(payload), text=True, check=True)".into(),
        },
    );
    runtimes.insert(
        "shell".into(),
        HookRuntimeExample {
            prehook: "printf '%s\\n' '{\"tool_name\":\"Edit\",...}' | thronglets prehook".into(),
            hook: "printf '%s\\n' '{\"tool_name\":\"Edit\",...,\"tool_response\":{\"success\":true}}' | thronglets hook".into(),
        },
    );
    HookContractExamples {
        prehook_stdin,
        hook_stdin,
        runtimes,
    }
}

fn managed_launcher_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join(MANAGED_LAUNCHER_NAME)
}

fn ensure_managed_launcher(data_dir: &Path, bin_path: &Path) -> io::Result<PathBuf> {
    let launcher_path = managed_launcher_path(data_dir);
    let repo_root = detect_repo_root();

    if let Some(parent) = launcher_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &launcher_path,
        render_managed_launcher(bin_path, repo_root.as_deref()),
    )?;
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&launcher_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&launcher_path, perms)?;
    }
    Ok(launcher_path)
}

fn detect_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    looks_like_repo_root(&cwd).then_some(cwd)
}

fn looks_like_repo_root(path: &Path) -> bool {
    let cargo_toml = path.join("Cargo.toml");
    if !cargo_toml.exists() || !path.join("src").join("main.rs").exists() {
        return false;
    }

    fs::read_to_string(cargo_toml)
        .ok()
        .is_some_and(|content| content.contains("name = \"thronglets\""))
}

fn render_managed_launcher(bin_path: &Path, repo_root: Option<&Path>) -> String {
    let managed_bin = shell_quote_path(bin_path);
    let (repo_root, repo_bin) = repo_root
        .map(|root| {
            (
                shell_quote_path(root),
                shell_quote_path(&root.join("target").join("debug").join("thronglets")),
            )
        })
        .unwrap_or_else(|| ("''".into(), "''".into()));

    format!(
        r#"#!/usr/bin/env sh
set -eu

MANAGED_BIN={managed_bin}
MANAGED_REPO={repo_root}
MANAGED_REPO_BIN={repo_bin}

if [ -n "${{THRONGLETS_REPO_ROOT:-}}" ]; then
  REPO_ROOT="$THRONGLETS_REPO_ROOT"
  REPO_BIN="$REPO_ROOT/target/debug/thronglets"
else
  REPO_ROOT="$MANAGED_REPO"
  REPO_BIN="$MANAGED_REPO_BIN"
fi

if [ -n "$REPO_ROOT" ] && [ -f "$REPO_ROOT/Cargo.toml" ]; then
  if [ ! -x "$REPO_BIN" ] && command -v cargo >/dev/null 2>&1; then
    cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml" >/dev/null 2>&1 || true
  fi
  if [ -x "$REPO_BIN" ]; then
    exec "$REPO_BIN" "$@"
  fi
fi

exec "$MANAGED_BIN" "$@"
"#
    )
}

fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r#"'\''"#))
}

fn read_json(path: &Path) -> Option<Value> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn read_toml(path: &Path) -> Option<toml::Table> {
    let content = fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

fn claude_hook_present(settings: &Value, phase: &str, command_fragment: &str) -> bool {
    settings["hooks"][phase].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry["hooks"].as_array().is_some_and(|hooks| {
                hooks.iter().any(|candidate| {
                    candidate["command"]
                        .as_str()
                        .is_some_and(|command| command.contains(command_fragment))
                })
            })
        })
    })
}

fn doctor_claude(home_dir: &Path, data_dir: &Path) -> AdapterDoctor {
    let settings_path = claude_settings_path(home_dir);
    let launcher_path = managed_launcher_path(data_dir);
    let settings = read_json(&settings_path);
    let launcher_ok = launcher_path.is_file();
    let post_ok = settings.as_ref().is_some_and(|value| {
        claude_hook_present(
            value,
            "PostToolUse",
            launcher_path.to_string_lossy().as_ref(),
        ) && claude_hook_present(value, "PostToolUse", " hook")
    });
    let pre_ok = settings.as_ref().is_some_and(|value| {
        claude_hook_present(
            value,
            "PreToolUse",
            launcher_path.to_string_lossy().as_ref(),
        ) && claude_hook_present(value, "PreToolUse", " prehook")
    });
    let checks = vec![
        AdapterCheck {
            name: "managed-launcher".into(),
            ok: launcher_ok,
            detail: format!("managed launcher at {}", launcher_path.display()),
        },
        AdapterCheck {
            name: "post-hook".into(),
            ok: post_ok,
            detail: format!("PostToolUse hook in {}", settings_path.display()),
        },
        AdapterCheck {
            name: "pre-hook".into(),
            ok: pre_ok,
            detail: format!("PreToolUse hook in {}", settings_path.display()),
        },
    ];
    let healthy = checks.iter().all(|check| check.ok);
    let fix_command = (!healthy).then_some("thronglets apply-plan --agent claude".into());
    AdapterDoctor {
        agent: AdapterKind::Claude.key().into(),
        present: should_configure_claude(home_dir),
        status: if healthy { "healthy" } else { "needs-fix" }.into(),
        healthy,
        restart_pending: false,
        fix_command: fix_command.clone(),
        restart_command: restart_command(AdapterKind::Claude),
        remediation: fix_command.into_iter().collect(),
        checks,
        note: None,
    }
}

fn codex_server_present(config: &toml::Table, launcher_path: &Path) -> bool {
    config
        .get("mcp_servers")
        .and_then(TomlValue::as_table)
        .and_then(|servers| servers.get(CODEX_MCP_SERVER_ID))
        .and_then(TomlValue::as_table)
        .is_some_and(|server| {
            server
                .get("command")
                .and_then(TomlValue::as_str)
                .is_some_and(|command| command == launcher_path.to_string_lossy())
                && server.get("args").and_then(TomlValue::as_array).is_some()
        })
}

fn codex_agents_block_present(path: &Path) -> bool {
    fs::read_to_string(path).ok().is_some_and(|content| {
        content.contains(CODEX_AGENTS_START) && content.contains(CODEX_AGENTS_END)
    })
}

fn doctor_codex(home_dir: &Path, data_dir: &Path) -> AdapterDoctor {
    let config_path = codex_config_path(home_dir);
    let agents_path = codex_agents_path(home_dir);
    let launcher_path = managed_launcher_path(data_dir);
    let config = read_toml(&config_path);
    let launcher_ok = launcher_path.is_file();
    let server_ok = config
        .as_ref()
        .is_some_and(|value| codex_server_present(value, &launcher_path));
    let agents_ok = codex_agents_block_present(&agents_path);
    let checks = vec![
        AdapterCheck {
            name: "managed-launcher".into(),
            ok: launcher_ok,
            detail: format!("managed launcher at {}", launcher_path.display()),
        },
        AdapterCheck {
            name: "mcp-server".into(),
            ok: server_ok,
            detail: format!(
                "[mcp_servers.{CODEX_MCP_SERVER_ID}] in {}",
                config_path.display()
            ),
        },
        AdapterCheck {
            name: "agents-memory".into(),
            ok: agents_ok,
            detail: format!("managed Thronglets block in {}", agents_path.display()),
        },
    ];
    let healthy = checks.iter().all(|check| check.ok);
    let restart_pending = healthy && restart_pending(data_dir, AdapterKind::Codex);
    let fix_command = (!healthy).then_some("thronglets apply-plan --agent codex".into());
    let mut remediation: Vec<_> = fix_command.clone().into_iter().collect();
    if restart_pending {
        if let Some(command) = restart_command(AdapterKind::Codex) {
            remediation.push(command);
        }
        if let Some(command) = runtime_ready_command(AdapterKind::Codex) {
            remediation.push(command);
        }
    }
    AdapterDoctor {
        agent: AdapterKind::Codex.key().into(),
        present: should_configure_codex(home_dir),
        status: if !healthy {
            "needs-fix"
        } else if restart_pending {
            "restart-pending"
        } else {
            "healthy"
        }
        .into(),
        healthy,
        restart_pending,
        fix_command: fix_command.clone(),
        restart_command: restart_command(AdapterKind::Codex),
        remediation,
        checks,
        note: if restart_pending {
            Some(
                "Codex config is correct, but the MCP server is still waiting for a client restart."
                    .into(),
            )
        } else if healthy {
            Some("Restart Codex after future config changes so the MCP server is loaded.".into())
        } else {
            None
        },
    }
}

fn openclaw_plugin_config_present(config: &Value, plugin_dir: &Path, launcher_path: &Path) -> bool {
    let allow_ok = config["plugins"]["allow"].as_array().is_some_and(|values| {
        values
            .iter()
            .any(|value| value.as_str() == Some(OPENCLAW_PLUGIN_ID))
    });
    let load_ok = config["plugins"]["load"]["paths"]
        .as_array()
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str() == Some(plugin_dir.to_string_lossy().as_ref()))
        });
    let entry_ok = config["plugins"]["entries"][OPENCLAW_PLUGIN_ID]["enabled"] == Value::Bool(true);
    let binary_ok = config["plugins"]["entries"][OPENCLAW_PLUGIN_ID]["config"]["binaryPath"]
        .as_str()
        .is_some_and(|path| path == launcher_path.to_string_lossy());
    let install_ok = !config["plugins"]["installs"][OPENCLAW_PLUGIN_ID].is_null();
    allow_ok && load_ok && entry_ok && binary_ok && install_ok
}

fn doctor_openclaw(home_dir: &Path, data_dir: &Path) -> AdapterDoctor {
    let config_path = openclaw_config_path(home_dir);
    let plugin_dir = data_dir.join(OPENCLAW_PLUGIN_ID);
    let launcher_path = managed_launcher_path(data_dir);
    let config = read_json(&config_path);
    let launcher_ok = launcher_path.is_file();
    let assets_ok =
        plugin_dir.join("openclaw.plugin.json").exists() && plugin_dir.join("index.mjs").exists();
    let config_ok = config
        .as_ref()
        .is_some_and(|value| openclaw_plugin_config_present(value, &plugin_dir, &launcher_path));
    let checks = vec![
        AdapterCheck {
            name: "managed-launcher".into(),
            ok: launcher_ok,
            detail: format!("managed launcher at {}", launcher_path.display()),
        },
        AdapterCheck {
            name: "plugin-assets".into(),
            ok: assets_ok,
            detail: format!("plugin assets in {}", plugin_dir.display()),
        },
        AdapterCheck {
            name: "plugin-config".into(),
            ok: config_ok,
            detail: format!("plugin entry in {}", config_path.display()),
        },
    ];
    let healthy = checks.iter().all(|check| check.ok);
    let restart_pending = healthy && restart_pending(data_dir, AdapterKind::OpenClaw);
    let fix_command = (!healthy).then_some("thronglets apply-plan --agent openclaw".into());
    let mut remediation: Vec<_> = fix_command.clone().into_iter().collect();
    if restart_pending {
        if let Some(command) = restart_command(AdapterKind::OpenClaw) {
            remediation.push(command);
        }
        if let Some(command) = runtime_ready_command(AdapterKind::OpenClaw) {
            remediation.push(command);
        }
    }
    AdapterDoctor {
        agent: AdapterKind::OpenClaw.key().into(),
        present: should_configure_openclaw(home_dir),
        status: if !healthy {
            "needs-fix"
        } else if restart_pending {
            "restart-pending"
        } else {
            "healthy"
        }
        .into(),
        healthy,
        restart_pending,
        fix_command: fix_command.clone(),
        restart_command: restart_command(AdapterKind::OpenClaw),
        remediation,
        checks,
        note: if restart_pending {
            Some(
                "OpenClaw plugin config is correct, but the gateway restart is still pending."
                    .into(),
            )
        } else if healthy {
            Some("OpenClaw gateway restart may be required after future plugin changes.".into())
        } else {
            None
        },
    }
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
    launcher_path: &Path,
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
            "binaryPath": launcher_path.to_string_lossy(),
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

fn configure_codex_config(config: &mut toml::Table, launcher_path: &Path, data_dir: &Path) -> bool {
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
        TomlValue::String(launcher_path.to_string_lossy().into_owned()),
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
        let mut end = end + CODEX_AGENTS_END.len();
        if original[end..].starts_with("\r\n") {
            end += 2;
        } else if original[end..].starts_with('\n') {
            end += 1;
        }
        let mut content = original.clone();
        content.replace_range(start..end, CODEX_AGENTS_BLOCK);
        content
    } else if original.trim().is_empty() {
        CODEX_AGENTS_BLOCK.into()
    } else {
        format!("{}\n\n{}", original.trim_end(), CODEX_AGENTS_BLOCK)
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

        let result = install_openclaw(&home, &data_dir, Path::new("/tmp/thronglets"), false, false)
            .unwrap()
            .unwrap();
        let launcher = managed_launcher_path(&data_dir);

        assert!(result.plugin_dir.join("openclaw.plugin.json").exists());
        assert!(result.plugin_dir.join("index.mjs").exists());
        assert!(launcher.exists());
        assert!(
            fs::read_to_string(&launcher)
                .unwrap()
                .contains("/tmp/thronglets")
        );

        let config: Value =
            serde_json::from_str(&fs::read_to_string(&result.config_path).unwrap()).unwrap();
        assert_eq!(
            config["plugins"]["entries"][OPENCLAW_PLUGIN_ID]["enabled"],
            Value::Bool(true)
        );
        assert_eq!(
            config["plugins"]["entries"][OPENCLAW_PLUGIN_ID]["config"]["binaryPath"],
            Value::String(launcher.to_string_lossy().into_owned())
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

        install_openclaw(&home, &data_dir, Path::new("/tmp/thronglets"), false, false)
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

        let result = install_codex(&home, &data_dir, Path::new("/tmp/thronglets"), false)
            .unwrap()
            .unwrap();
        let launcher = managed_launcher_path(&data_dir);

        let config: toml::Table =
            toml::from_str(&fs::read_to_string(&result.config_path).unwrap()).unwrap();
        let server = config["mcp_servers"][CODEX_MCP_SERVER_ID]
            .as_table()
            .unwrap();
        assert_eq!(
            server["command"].as_str(),
            Some(launcher.to_string_lossy().as_ref())
        );
        assert_eq!(
            server["args"].as_array().unwrap(),
            &vec![
                TomlValue::String("--data-dir".into()),
                TomlValue::String(data_dir.to_string_lossy().into_owned()),
                TomlValue::String("mcp".into()),
            ]
        );
        assert!(launcher.exists());
        assert!(
            fs::read_to_string(&launcher)
                .unwrap()
                .contains("/tmp/thronglets")
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

        let result = install_codex(&home, &data_dir, Path::new("/tmp/thronglets"), false)
            .unwrap()
            .unwrap();

        assert!(result.updated_agents_memory);
        let agents = fs::read_to_string(codex_dir.join("AGENTS.md")).unwrap();
        assert!(!agents.contains("old block"));
        assert_eq!(agents.matches(CODEX_AGENTS_START).count(), 1);
    }

    #[test]
    fn install_codex_keeps_managed_block_idempotent() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        let codex_dir = home.join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();

        let first = install_codex(&home, &data_dir, Path::new("/tmp/thronglets"), false)
            .unwrap()
            .unwrap();
        assert!(first.updated_agents_memory);

        let second = install_codex(&home, &data_dir, Path::new("/tmp/thronglets"), false)
            .unwrap()
            .unwrap();
        assert!(!second.updated_agents_memory);
    }

    #[test]
    fn install_claude_writes_managed_launcher_hooks() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let data_dir = temp.path().join("data");
        fs::create_dir_all(home.join(".claude")).unwrap();

        let result = install_claude(&home, &data_dir, Path::new("/tmp/thronglets")).unwrap();
        let launcher = managed_launcher_path(&data_dir);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(&result.settings_path).unwrap()).unwrap();

        assert!(launcher.exists());
        assert!(
            fs::read_to_string(&launcher)
                .unwrap()
                .contains("/tmp/thronglets")
        );
        assert!(claude_hook_present(
            &settings,
            "PostToolUse",
            launcher.to_string_lossy().as_ref()
        ));
        assert!(claude_hook_present(
            &settings,
            "PreToolUse",
            launcher.to_string_lossy().as_ref()
        ));
    }
}
