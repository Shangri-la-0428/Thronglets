use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

const SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";

fn run_bin(args: &[&str], home: &Path, data_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run thronglets")
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
    assert_eq!(summary["summary"]["status"], Value::String("planned".into()));
    assert_eq!(summary["summary"]["restart_required"], Value::Bool(false));
    assert!(summary["summary"]["restart_commands"].as_array().unwrap().is_empty());
    let plans = summary["plans"].as_array().unwrap();
    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(plan["agent"], Value::String("generic".into()));
    assert_eq!(plan["contract"]["prehook_stdin"]["tool_name"], "Edit");
    assert!(plan["contract"]["hook_stdin"]["tool_response"].is_object());
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
    assert_eq!(summary["summary"]["status"], Value::String("applied".into()));
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
            .any(|step| step == "thronglets clear-restart --agent codex")
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
    assert_eq!(summary["summary"]["status"], Value::String("needs-fix".into()));
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
    assert_eq!(envelope["data"]["summary"]["restart_required"], Value::Bool(true));
    assert_eq!(
        envelope["data"]["summary"]["restart_commands"].as_array().unwrap()[0],
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
    assert_eq!(summary["summary"]["status"], Value::String("cleared".into()));
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
    assert_eq!(summary["summary"]["status"], Value::String("healthy".into()));
    assert_eq!(summary["summary"]["healthy"], Value::Bool(true));
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
    assert!(!stdout.contains("Detected adapters:"));
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
    assert!(stdout.contains("Restart pending: yes"));
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
