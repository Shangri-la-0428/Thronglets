use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_bin(args: &[&str], data_dir: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .output()
        .expect("failed to run thronglets");
    assert!(
        output.status.success(),
        "command failed: {}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout should be valid json")
}

#[test]
fn presence_ping_and_feed_roundtrip() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    let ping = run_bin(
        &[
            "presence-ping",
            "--space",
            "psyche",
            "--mode",
            "focus",
            "--session-id",
            "codex-1",
            "--json",
        ],
        &data_dir,
    );
    assert_eq!(ping["schema_version"], "thronglets.presence.v1");
    assert_eq!(ping["command"], "presence-ping");
    assert_eq!(ping["data"]["summary"]["status"], "active");
    assert_eq!(ping["data"]["summary"]["space"], "psyche");
    assert_eq!(ping["data"]["mode"], "focus");

    let feed = run_bin(
        &["presence-feed", "--space", "psyche", "--hours", "1", "--json"],
        &data_dir,
    );
    assert_eq!(feed["schema_version"], "thronglets.presence.v1");
    assert_eq!(feed["command"], "presence-feed");
    assert_eq!(feed["data"]["summary"]["status"], "active");
    assert_eq!(feed["data"]["summary"]["active_sessions"], 1);
    assert_eq!(feed["data"]["sessions"][0]["space"], "psyche");
    assert_eq!(feed["data"]["sessions"][0]["mode"], "focus");
    assert_eq!(feed["data"]["sessions"][0]["session_id"], "codex-1");
}
