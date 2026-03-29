use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";

fn run_bin(args: &[&str], home: &Path, data_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run thronglets")
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("bootstrap_v2")
        .join(name)
}

fn normalize_string(value: &str, home: &Path, data_dir: &Path, bin: &Path) -> String {
    value
        .replace(&bin.display().to_string(), "<BIN>")
        .replace(&home.display().to_string(), "<HOME>")
        .replace(&data_dir.display().to_string(), "<DATA>")
}

fn normalize_json(value: &mut Value, home: &Path, data_dir: &Path, bin: &Path) {
    match value {
        Value::String(string) => {
            *string = normalize_string(string, home, data_dir, bin);
        }
        Value::Array(values) => {
            for entry in values {
                normalize_json(entry, home, data_dir, bin);
            }
        }
        Value::Object(map) => {
            for entry in map.values_mut() {
                normalize_json(entry, home, data_dir, bin);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn assert_fixture(fixture: &str, output: &std::process::Output, home: &Path, data_dir: &Path) {
    let mut actual: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual["schema_version"],
        Value::String(SCHEMA_VERSION.into())
    );
    normalize_json(
        &mut actual,
        home,
        data_dir,
        Path::new(env!("CARGO_BIN_EXE_thronglets")),
    );
    let expected: Value =
        serde_json::from_str(&fs::read_to_string(fixture_path(fixture)).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn detect_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::create_dir_all(home.join(".openclaw")).unwrap();

    let output = run_bin(&["detect", "--json"], &home, &data_dir);
    assert!(
        output.status.success(),
        "detect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture("detect_all_present.json", &output, &home, &data_dir);
}

#[test]
fn install_plan_codex_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

    let output = run_bin(
        &["install-plan", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "install-plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture("install_plan_codex.json", &output, &home, &data_dir);
}

#[test]
fn install_plan_generic_matches_golden_fixture() {
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
    assert_fixture("install_plan_generic.json", &output, &home, &data_dir);
}

#[test]
fn apply_plan_codex_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

    let output = run_bin(
        &["apply-plan", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture("apply_plan_codex.json", &output, &home, &data_dir);
}

#[test]
fn doctor_codex_needs_fix_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

    let output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        !output.status.success(),
        "doctor unexpectedly passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_fixture("doctor_codex_needs_fix.json", &output, &home, &data_dir);
}

#[test]
fn doctor_codex_restart_pending_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

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

    let output = run_bin(&["doctor", "--agent", "codex", "--json"], &home, &data_dir);
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture(
        "doctor_codex_restart_pending.json",
        &output,
        &home,
        &data_dir,
    );
}

#[test]
fn bootstrap_codex_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

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
    assert_fixture("bootstrap_codex.json", &output, &home, &data_dir);
}

#[test]
fn clear_restart_codex_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

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

    let output = run_bin(
        &["clear-restart", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "clear-restart failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture("clear_restart_codex.json", &output, &home, &data_dir);
}

#[test]
fn runtime_ready_codex_matches_golden_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    fs::create_dir_all(home.join(".codex")).unwrap();

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

    let output = run_bin(
        &["runtime-ready", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "runtime-ready failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture("runtime_ready_codex.json", &output, &home, &data_dir);
}
