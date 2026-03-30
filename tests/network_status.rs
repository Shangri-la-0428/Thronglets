use serde_json::Value;
use tempfile::TempDir;

use thronglets::network_state::NetworkSnapshot;

fn run_bin(args: &[&str], data_dir: &std::path::Path) -> Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_thronglets"))
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
fn status_json_surfaces_network_snapshot() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut snapshot = NetworkSnapshot::begin(2);
    snapshot.mark_peer_connected(3);
    snapshot.mark_trace_received();
    snapshot.save(&data_dir);

    let status = run_bin(&["status", "--json"], &data_dir);
    assert_eq!(status["data"]["network"]["activity"], "connected");
    assert_eq!(status["data"]["network"]["transport_mode"], "direct");
    assert_eq!(status["data"]["network"]["vps_dependency_level"], "low");
    assert_eq!(status["data"]["network"]["peer_count"], 3);
    assert_eq!(status["data"]["network"]["bootstrap_targets"], 2);
}
