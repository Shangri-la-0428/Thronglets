use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";

fn run_bin(args: &[&str], home: &Path, data_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run thronglets")
}

fn run_bin_with_input(args: &[&str], input: &str, home: &Path, data_dir: &Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run thronglets with input");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for thronglets")
}

fn parse_command_data(output: &Output, command: &str) -> Value {
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["schema_version"],
        Value::String(SCHEMA_VERSION.into())
    );
    assert_eq!(envelope["command"], Value::String(command.into()));
    envelope["data"].clone()
}

fn parse_doctor_envelope(output: &Output) -> Value {
    parse_command_data(output, "doctor")
}

fn parse_clear_restart_envelope(output: &Output) -> Value {
    parse_command_data(output, "clear-restart")
}

fn parse_runtime_ready_envelope(output: &Output) -> Value {
    parse_command_data(output, "runtime-ready")
}

#[test]
fn detect_json_reports_present_adapters_and_generic_contract() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".openclaw")).unwrap();

    let output = run_bin(&["detect", "--json"], &home, &data_dir);
    assert!(
        output.status.success(),
        "detect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary = parse_command_data(&output, "detect");
    assert_eq!(summary["summary"]["status"], Value::String("ready".into()));
    let detections = summary["detections"].as_array().unwrap();
    assert!(
        summary["summary"]["recommended_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent == "codex")
    );
    let codex = detections
        .iter()
        .find(|entry| entry["agent"] == "codex")
        .unwrap();
    assert_eq!(codex["present"], Value::Bool(true));

    let openclaw = detections
        .iter()
        .find(|entry| entry["agent"] == "openclaw")
        .unwrap();
    assert_eq!(openclaw["present"], Value::Bool(true));

    let generic = detections
        .iter()
        .find(|entry| entry["agent"] == "generic")
        .unwrap();
    assert_eq!(generic["integration"], Value::String("contract".into()));
}

#[test]
fn adapter_management_commands_do_not_generate_identity_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let commands: &[&[&str]] = &[
        &["detect", "--json"],
        &["install-plan", "--agent", "generic", "--json"],
        &["apply-plan", "--agent", "generic", "--json"],
        &["doctor", "--agent", "generic", "--json"],
        &["bootstrap", "--agent", "generic", "--json"],
        &["clear-restart", "--agent", "generic", "--json"],
        &["runtime-ready", "--agent", "generic", "--json"],
    ];

    for args in commands {
        let output = run_bin(args, &home, &data_dir);
        assert!(
            output.status.success(),
            "command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(
        !data_dir.join("node.key").exists(),
        "adapter management commands should not generate node.key",
    );
    assert!(
        !data_dir.join("identity.v1.json").exists(),
        "adapter management commands should not generate identity binding",
    );
}

#[test]
fn install_plan_generic_json_includes_contract_examples() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");

    let output = run_bin(
        &["install-plan", "--agent", "generic", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "install-plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary = parse_command_data(&output, "install-plan");
    assert_eq!(
        summary["summary"]["status"],
        Value::String("planned".into())
    );
    assert_eq!(summary["summary"]["restart_required"], Value::Bool(false));
    assert!(
        summary["summary"]["restart_commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let plans = summary["plans"].as_array().unwrap();
    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(plan["agent"], Value::String("generic".into()));
    assert_eq!(plan["contract"]["prehook_stdin"]["tool_name"], "Edit");
    assert_eq!(plan["contract"]["prehook_stdin"]["space"], "shared-space");
    assert_eq!(plan["contract"]["prehook_stdin"]["mode"], "focus");
    assert!(plan["contract"]["hook_stdin"]["tool_response"].is_object());
    assert!(
        plan["contract"]["runtimes"]["node"]["prehook"]
            .as_str()
            .unwrap()
            .contains("execFileSync")
    );
    assert!(
        plan["contract"]["runtimes"]["python"]["prehook"]
            .as_str()
            .unwrap()
            .contains("subprocess.run")
    );
    assert!(
        plan["contract"]["runtimes"]["shell"]["hook"]
            .as_str()
            .unwrap()
            .contains("thronglets hook")
    );
}

#[test]
fn install_plan_generic_can_filter_runtime_snippets() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");

    let output = run_bin(
        &[
            "install-plan",
            "--agent",
            "generic",
            "--runtime",
            "python",
            "--json",
        ],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "install-plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary = parse_command_data(&output, "install-plan");
    let runtimes = summary["plans"][0]["contract"]["runtimes"]
        .as_object()
        .unwrap();
    assert_eq!(runtimes.len(), 1);
    assert!(runtimes.contains_key("python"));
}

#[test]
fn apply_plan_codex_then_doctor_reports_restart_pending() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let apply_output = run_bin(
        &["apply-plan", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        apply_output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );
    let summary = parse_command_data(&apply_output, "apply-plan");
    assert_eq!(
        summary["summary"]["status"],
        Value::String("applied".into())
    );
    assert_eq!(summary["summary"]["restart_required"], Value::Bool(true));
    assert_eq!(
        summary["summary"]["restart_commands"].as_array().unwrap()[0],
        Value::String("Restart Codex".into())
    );
    assert!(
        summary["summary"]["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "Restart the targeted agent so the new integration is loaded.")
    );
    let apply_results = summary["results"].as_array().unwrap();
    assert_eq!(apply_results.len(), 1);
    assert_eq!(apply_results[0]["applied"], Value::Bool(true));
    assert_eq!(
        apply_results[0]["restart_command"],
        Value::String("Restart Codex".into())
    );

    let doctor_output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        doctor_output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor_output.stderr)
    );
    let summary = parse_doctor_envelope(&doctor_output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("restart-pending".into())
    );
    assert_eq!(summary["summary"]["healthy"], Value::Bool(true));
    assert_eq!(summary["summary"]["restart_pending"], Value::Bool(true));
    assert_eq!(
        summary["summary"]["restart_commands"].as_array().unwrap()[0],
        Value::String("Restart Codex".into())
    );
    assert!(
        summary["summary"]["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "thronglets runtime-ready --agent codex")
    );
    let reports = summary["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["healthy"], Value::Bool(true));
    assert_eq!(reports[0]["restart_pending"], Value::Bool(true));
    assert_eq!(
        reports[0]["status"],
        Value::String("restart-pending".into())
    );
    assert!(reports[0]["fix_command"].is_null());
    assert_eq!(
        reports[0]["restart_command"],
        Value::String("Restart Codex".into())
    );
}

#[test]
fn doctor_fails_for_unconfigured_specific_adapter() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        !output.status.success(),
        "doctor unexpectedly passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let summary = parse_doctor_envelope(&output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("needs-fix".into())
    );
    assert_eq!(summary["summary"]["healthy"], Value::Bool(false));
    assert_eq!(summary["summary"]["restart_pending"], Value::Bool(false));
    assert_eq!(
        summary["summary"]["restart_commands"].as_array().unwrap()[0],
        Value::String("Restart Codex".into())
    );
    assert_eq!(
        summary["summary"]["next_steps"].as_array().unwrap()[0],
        Value::String("thronglets apply-plan --agent codex".into())
    );
    let reports = summary["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["healthy"], Value::Bool(false));
    assert_eq!(reports[0]["restart_pending"], Value::Bool(false));
    assert_eq!(reports[0]["status"], Value::String("needs-fix".into()));
    assert_eq!(
        reports[0]["fix_command"],
        Value::String("thronglets apply-plan --agent codex".into())
    );
    assert_eq!(
        reports[0]["restart_command"],
        Value::String("Restart Codex".into())
    );
}

#[test]
fn bootstrap_codex_json_applies_and_reports_restart_pending() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let output = run_bin(
        &["bootstrap", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["schema_version"],
        Value::String(SCHEMA_VERSION.into())
    );
    assert_eq!(envelope["command"], Value::String("bootstrap".into()));
    assert_eq!(
        envelope["data"]["summary"]["status"],
        Value::String("restart-pending".into())
    );
    assert_eq!(envelope["data"]["summary"]["healthy"], Value::Bool(true));
    assert_eq!(
        envelope["data"]["summary"]["restart_pending"],
        Value::Bool(true)
    );
    assert_eq!(
        envelope["data"]["summary"]["restart_required"],
        Value::Bool(true)
    );
    assert_eq!(
        envelope["data"]["summary"]["restart_commands"]
            .as_array()
            .unwrap()[0],
        Value::String("Restart Codex".into())
    );
    assert!(
        envelope["data"]["summary"]["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "Restart the targeted agent so the new integration is loaded.")
    );
    assert_eq!(envelope["data"]["results"].as_array().unwrap().len(), 1);
    assert_eq!(
        envelope["data"]["reports"].as_array().unwrap()[0]["healthy"],
        Value::Bool(true)
    );
    assert_eq!(
        envelope["data"]["reports"].as_array().unwrap()[0]["restart_pending"],
        Value::Bool(true)
    );
}

#[test]
fn clear_restart_codex_restores_healthy() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let apply_output = run_bin(
        &["apply-plan", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        apply_output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );

    let clear_output = run_bin(
        &["clear-restart", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        clear_output.status.success(),
        "clear-restart failed: {}",
        String::from_utf8_lossy(&clear_output.stderr)
    );
    let summary = parse_clear_restart_envelope(&clear_output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("cleared".into())
    );
    assert_eq!(
        summary["summary"]["cleared_agents"].as_array().unwrap()[0],
        Value::String("codex".into())
    );

    let doctor_output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        doctor_output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor_output.stderr)
    );
    let summary = parse_doctor_envelope(&doctor_output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("healthy".into())
    );
    assert_eq!(summary["summary"]["healthy"], Value::Bool(true));
    assert_eq!(summary["summary"]["restart_pending"], Value::Bool(false));
}

#[test]
fn runtime_ready_codex_restores_healthy() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let apply_output = run_bin(
        &["apply-plan", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        apply_output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );

    let ready_output = run_bin(
        &["runtime-ready", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        ready_output.status.success(),
        "runtime-ready failed: {}",
        String::from_utf8_lossy(&ready_output.stderr)
    );
    let summary = parse_runtime_ready_envelope(&ready_output);
    assert_eq!(summary["summary"]["status"], Value::String("ready".into()));
    assert_eq!(
        summary["summary"]["ready_agents"].as_array().unwrap()[0],
        Value::String("codex".into())
    );

    let doctor_output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        doctor_output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor_output.stderr)
    );
    let summary = parse_doctor_envelope(&doctor_output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("healthy".into())
    );
    assert_eq!(summary["summary"]["healthy"], Value::Bool(true));
    assert_eq!(summary["summary"]["restart_pending"], Value::Bool(false));
}

#[test]
fn codex_mcp_contact_auto_clears_restart_pending() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let apply_output = run_bin(
        &["apply-plan", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        apply_output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args([
            "--data-dir",
            data_dir.to_str().unwrap(),
            "mcp",
            "--agent",
            "codex",
        ])
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");
    std::thread::sleep(Duration::from_millis(300));
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }

    let doctor_output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        doctor_output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor_output.stderr)
    );
    let summary = parse_doctor_envelope(&doctor_output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("healthy".into())
    );
    assert_eq!(summary["summary"]["restart_pending"], Value::Bool(false));
}

#[test]
fn openclaw_prehook_contact_auto_clears_restart_pending() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".openclaw")).unwrap();

    let apply_output = run_bin(
        &["apply-plan", "--agent", "openclaw", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        apply_output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );

    let prehook_output = run_bin_with_input(
        &["prehook"],
        r#"{"agent_source":"openclaw","tool_name":"Read","tool_input":{"file_path":"README.md"}}"#,
        &home,
        &data_dir,
    );
    assert!(
        prehook_output.status.success(),
        "prehook failed: {}",
        String::from_utf8_lossy(&prehook_output.stderr)
    );

    let doctor_output = run_bin(
        &["doctor", "--agent", "openclaw", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        doctor_output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&doctor_output.stderr)
    );
    let summary = parse_doctor_envelope(&doctor_output);
    assert_eq!(
        summary["summary"]["status"],
        Value::String("healthy".into())
    );
    assert_eq!(summary["summary"]["restart_pending"], Value::Bool(false));
}

#[test]
fn detect_text_stays_summary_first_when_adapters_are_present() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".openclaw")).unwrap();

    let output = run_bin(&["detect"], &home, &data_dir);
    assert!(
        output.status.success(),
        "detect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Detect status: ready"));
    assert!(stdout.contains("Detected: claude-code, codex, openclaw, generic"));
    assert!(stdout.contains("Recommended: claude-code, codex, openclaw"));
    // Summary always comes first — verbose details (if any) appear after
    let summary_pos = stdout.find("Detect status:").unwrap();
    if let Some(detail_pos) = stdout.find("Detected adapters:") {
        assert!(
            summary_pos < detail_pos,
            "summary must precede adapter details"
        );
    }
}

#[test]
fn doctor_text_stays_summary_first_when_healthy() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let apply_output = run_bin(&["apply-plan", "--agent", "codex"], &home, &data_dir);
    assert!(
        apply_output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );

    let output = run_bin(&["doctor", "--agent", "codex"], &home, &data_dir);
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Doctor status: restart-pending"));
    assert!(stdout.contains("Pending restart: codex"));
    assert!(stdout.contains("Restart still pending: yes"));
    assert!(!stdout.contains("Adapter health:"));
}

#[test]
fn setup_text_stays_summary_first() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".openclaw")).unwrap();

    let output = run_bin(&["setup"], &home, &data_dir);
    assert!(
        output.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Thronglets setup: restart-pending"));
    assert!(stdout.contains("Installed: claude-code, codex, openclaw"));
    assert!(!stdout.contains("Applied adapter plan:"));
}
