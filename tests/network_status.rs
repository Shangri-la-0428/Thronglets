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
    snapshot.mark_peer_connected("12D3KooWStatus", 3);
    snapshot.observe_peer_address("12D3KooWStatus", "/ip4/10.0.0.3/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.merge_trusted_peer_seeds(["/ip4/10.0.0.8/tcp/4001".to_string()]);
    snapshot.mark_trace_received();
    snapshot.save(&data_dir);

    let status = run_bin(&["status", "--json"], &data_dir);
    assert_eq!(status["data"]["network"]["activity"], "connected");
    assert_eq!(status["data"]["network"]["transport_mode"], "direct");
    assert_eq!(status["data"]["network"]["vps_dependency_level"], "low");
    assert_eq!(
        status["data"]["network"]["bootstrap_fallback_mode"],
        "delayed"
    );
    assert_eq!(status["data"]["network"]["peer_count"], 3);
    assert_eq!(status["data"]["network"]["bootstrap_targets"], 2);
    assert_eq!(status["data"]["network"]["known_peer_count"], 1);
    assert_eq!(status["data"]["network"]["trusted_peer_seed_count"], 1);
    assert_eq!(status["data"]["network"]["peer_seed_count"], 2);
}

#[test]
fn status_json_requires_actual_bootstrap_contact_for_bootstrapping() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let snapshot = NetworkSnapshot::begin(1);
    snapshot.save(&data_dir);

    let status = run_bin(&["status", "--json"], &data_dir);
    assert_eq!(status["data"]["network"]["activity"], "offline");
    assert_eq!(status["data"]["network"]["bootstrap_targets"], 1);
    assert_eq!(status["data"]["network"]["bootstrap_contacted_recently"], false);
}

#[test]
fn peers_json_surfaces_known_peer_book() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.mark_peer_connected("12D3KooWAlice", 1);
    snapshot.observe_peer_address("12D3KooWAlice", "/ip4/127.0.0.1/tcp/4001");
    snapshot.mark_peer_connected("12D3KooWBob", 2);
    snapshot.observe_peer_address("12D3KooWBob", "/ip4/127.0.0.1/tcp/4002");
    snapshot.mark_peer_disconnected("12D3KooWAlice", 1);
    snapshot.merge_peer_seeds(["/ip4/127.0.0.1/tcp/4999".to_string()]);
    snapshot.merge_trusted_peer_seeds(["/ip4/127.0.0.1/tcp/4998".to_string()]);
    snapshot.save(&data_dir);

    let peers = run_bin(&["peers", "--json", "--limit", "10"], &data_dir);
    assert_eq!(peers["schema_version"], "thronglets.network.v1");
    assert_eq!(peers["command"], "peers");
    assert_eq!(peers["data"]["summary"]["connected_peers"], 1);
    assert_eq!(peers["data"]["summary"]["known_peers"], 2);
    assert_eq!(peers["data"]["summary"]["trusted_peer_seed_count"], 1);
    assert_eq!(peers["data"]["summary"]["peer_seed_count"], 2);
    let list = peers["data"]["peers"].as_array().unwrap();
    let alice = list
        .iter()
        .find(|peer| peer["peer_id"] == "12D3KooWAlice")
        .unwrap();
    let bob = list
        .iter()
        .find(|peer| peer["peer_id"] == "12D3KooWBob")
        .unwrap();
    assert_eq!(alice["connected"], false);
    assert_eq!(bob["connected"], true);
    assert_eq!(alice["addresses"][0], "/ip4/127.0.0.1/tcp/4001");
    assert_eq!(bob["addresses"][0], "/ip4/127.0.0.1/tcp/4002");
}

#[test]
fn net_check_json_flags_bootstrap_only_nodes() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let snapshot = NetworkSnapshot::begin(1);
    snapshot.save(&data_dir);

    let check = run_bin(&["net-check", "--json"], &data_dir);
    assert_eq!(check["schema_version"], "thronglets.network.v1");
    assert_eq!(check["command"], "net-check");
    assert_eq!(check["data"]["summary"]["status"], "bootstrap-only");
    assert_eq!(check["data"]["summary"]["peer_first_ready"], false);
    assert_eq!(check["data"]["summary"]["same_owner_direct_ready"], false);
    assert_eq!(check["data"]["summary"]["bootstrap_offline_ready"], false);
    assert_eq!(
        check["data"]["summary"]["vps_dependency_level"],
        "bootstrap-only"
    );
    assert_eq!(
        check["data"]["summary"]["bootstrap_fallback_mode"],
        "immediate"
    );
    let next_steps = check["data"]["next_steps"].as_array().unwrap();
    assert!(!next_steps.is_empty());
}

#[test]
fn net_check_json_accepts_peer_first_state() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.mark_peer_connected("12D3KooWPeerFirst", 3);
    snapshot.observe_peer_address("12D3KooWPeerFirst", "/ip4/10.0.0.3/tcp/4001");
    snapshot.merge_trusted_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&data_dir);

    let check = run_bin(&["net-check", "--json"], &data_dir);
    assert_eq!(check["data"]["summary"]["status"], "peer-first");
    assert_eq!(check["data"]["summary"]["peer_first_ready"], true);
    assert_eq!(check["data"]["summary"]["same_owner_direct_ready"], true);
    assert_eq!(check["data"]["summary"]["bootstrap_offline_ready"], true);
    assert_eq!(check["data"]["summary"]["transport_mode"], "direct");
    assert_eq!(
        check["data"]["summary"]["bootstrap_fallback_mode"],
        "delayed"
    );
    assert_eq!(check["data"]["summary"]["trusted_peer_seed_count"], 1);
    assert_eq!(check["data"]["summary"]["peer_seed_count"], 1);
    assert!(check["data"]["next_steps"].as_array().unwrap().is_empty());
}

#[test]
fn net_check_json_can_simulate_bootstrap_offline() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let mut snapshot = NetworkSnapshot::begin(2);
    snapshot.merge_trusted_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&data_dir);

    let check = run_bin(&["net-check", "--json", "--bootstrap-offline"], &data_dir);
    assert_eq!(check["data"]["summary"]["scenario"], "bootstrap-offline");
    assert_eq!(check["data"]["summary"]["bootstrap_targets"], 0);
    assert_eq!(
        check["data"]["summary"]["bootstrap_fallback_mode"],
        "disabled"
    );
    assert_eq!(check["data"]["summary"]["same_owner_direct_ready"], false);
    assert_eq!(check["data"]["summary"]["bootstrap_offline_ready"], true);
    assert_eq!(check["data"]["summary"]["vps_dependency_level"], "offline");
}
