use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use thronglets::network_state::NetworkSnapshot;

fn isolated_home(data_dir: &Path) -> std::path::PathBuf {
    let root = data_dir.parent().unwrap_or(data_dir);
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir should be creatable");
    home
}

fn build_command(data_dir: &Path) -> Command {
    let home = isolated_home(data_dir);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thronglets"));
    cmd.args(["--data-dir", data_dir.to_str().unwrap()])
        .env("HOME", &home)
        .env("OASYCE_DIR", home.join(".oasyce"))
        .env("PATH", "");
    cmd
}

fn run_bin(args: &[&str], data_dir: &Path) -> Value {
    let output = build_command(data_dir)
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

fn run_bin_in_home(args: &[&str], home: &Path, data_dir: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .env("OASYCE_DIR", home.join(".oasyce"))
        .env("PATH", "")
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

fn run_bin_raw(args: &[&str], data_dir: &Path) -> std::process::Output {
    build_command(data_dir)
        .args(args)
        .output()
        .expect("failed to run thronglets")
}

fn run_bin_text_in_home(args: &[&str], home: &Path, data_dir: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .env("OASYCE_DIR", home.join(".oasyce"))
        .env("PATH", "")
        .output()
        .expect("failed to run thronglets");
    assert!(
        output.status.success(),
        "command failed: {}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

fn run_bin_text(args: &[&str], data_dir: &Path) -> String {
    let output = build_command(data_dir)
        .args(args)
        .output()
        .expect("failed to run thronglets");
    assert!(
        output.status.success(),
        "command failed: {}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

#[test]
fn id_json_surfaces_identity_summary() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    let data = run_bin(&["id", "--json"], &data_dir);

    assert_eq!(data["schema_version"], "thronglets.identity.v2");
    assert_eq!(data["command"], "id");
    assert_eq!(data["data"]["summary"]["status"], "healthy");
    assert_eq!(data["data"]["summary"]["binding_source"], "local");
    assert_eq!(data["data"]["summary"]["owner_account"], Value::Null);
    assert_eq!(
        data["data"]["summary"]["identity_model"]["principal"]["current_v1_binding"],
        "not-modeled-in-v1"
    );
    assert_eq!(
        data["data"]["summary"]["identity_model"]["account"]["current_v1_binding"],
        "owner_account"
    );
    assert_eq!(
        data["data"]["summary"]["identity_model"]["delegate"]["current_v1_binding"],
        "device_identity"
    );
    assert_eq!(
        data["data"]["summary"]["identity_model"]["session"]["current_v1_binding"],
        "session_id_audit_label"
    );
    assert_eq!(
        data["data"]["summary"]["identity_model"]["account"]["current_id"],
        Value::Null
    );
    assert_eq!(
        data["data"]["summary"]["authorization"]["final_truth_source"],
        "oasyce_chain"
    );
    assert_eq!(
        data["data"]["summary"]["authorization"]["local_binding_status"],
        "unbound"
    );
    assert_eq!(
        data["data"]["summary"]["authorization"]["authoritative_status"],
        "not-checked"
    );
    assert_eq!(data["data"]["summary"]["joined_from_device"], Value::Null);
    assert!(
        data["data"]["summary"]["device_identity"]
            .as_str()
            .unwrap()
            .starts_with("oasyce1")
    );
}

#[test]
fn id_json_imports_owner_account_from_oasyce_sdk_binding_hint() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".oasyce")).unwrap();
    std::fs::write(
        home.join(".oasyce").join("identity.v1.json"),
        serde_json::json!({
            "schema_version": "oasyce.identity.v1",
            "account": "oasyce1owner",
            "delegate": "oasyce1sdkdelegate",
            "signer_address": "oasyce1sdkdelegate",
            "updated_at": "2026-04-04T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let data = run_bin_in_home(&["id", "--json"], &home, &data_dir);

    assert_eq!(data["data"]["summary"]["owner_account"], "oasyce1owner");
    assert_eq!(data["data"]["summary"]["binding_source"], "oasyce_sdk");
    assert_eq!(
        data["data"]["summary"]["identity_model"]["account"]["current_id"],
        "oasyce1owner"
    );
}

#[test]
fn authorization_check_json_surfaces_local_binding_and_final_truth() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    let data = run_bin(&["authorization-check", "--json"], &data_dir);

    assert_eq!(data["schema_version"], "thronglets.identity.v2");
    assert_eq!(data["command"], "authorization-check");
    assert_eq!(
        data["data"]["summary"]["final_truth_source"],
        "oasyce_chain"
    );
    assert_eq!(data["data"]["summary"]["local_binding_status"], "unbound");
    assert_eq!(
        data["data"]["summary"]["authoritative_status"],
        "not-checked"
    );
    assert_eq!(
        data["data"]["summary"]["execution_boundary"],
        "device_identity"
    );
    assert_eq!(
        data["data"]["identity_model"]["delegate"]["current_v1_binding"],
        "device_identity"
    );
}

#[test]
fn status_json_surfaces_quiet_substrate_activity() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");

    let status = run_bin_in_home(&["status", "--json"], &home, &data_dir);

    assert_eq!(status["command"], "status");
    assert_eq!(status["data"]["summary"]["status"], "local-only");
    assert_eq!(status["data"]["summary"]["network_path_ready"], false);
    assert_eq!(status["data"]["runtime"]["status"], "not-detected");
    assert_eq!(
        status["data"]["runtime"]["detected_agents"],
        serde_json::json!([])
    );
    assert_eq!(status["data"]["substrate"]["activity"], "quiet");
    assert_eq!(status["data"]["substrate"]["recent_interventions_15m"], 0);
    assert_eq!(status["data"]["network"]["activity"], "offline");
    assert_eq!(status["data"]["network"]["transport_mode"], "offline");
    assert_eq!(status["data"]["network"]["vps_dependency_level"], "offline");
}

#[test]
fn status_text_prioritizes_runtime_restart_when_runtime_is_pending() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let data_dir = temp.path().join("data");

    run_bin_in_home(&["start", "--json"], &home, &data_dir);

    let status_json = run_bin_in_home(&["status", "--json"], &home, &data_dir);
    assert_eq!(status_json["data"]["runtime"]["status"], "restart-required");

    let output = run_bin_text_in_home(&["status"], &home, &data_dir);
    assert!(output.contains("Status:           restart once"));
    assert!(output.contains("Runtime:          restart once"));
    assert!(output.contains("Restart Codex once"));
}

#[test]
fn start_json_surfaces_local_ready_for_first_device() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");

    let data = run_bin_in_home(&["start", "--json"], &home, &data_dir);

    assert_eq!(data["schema_version"], "thronglets.identity.v2");
    assert_eq!(data["command"], "start");
    assert_eq!(data["data"]["summary"]["status"], "local-ready");
    assert_eq!(data["data"]["readiness"]["status"], "local-only");
    assert_eq!(data["data"]["setup"]["healthy"], true);
}

#[test]
fn top_level_help_stays_normal_user_first() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    let output = run_bin_text(&["--help"], &data_dir);

    assert!(output.contains("thronglets start"));
    assert!(output.contains("thronglets share"));
    assert!(output.contains("thronglets join"));
    assert!(output.contains("thronglets status"));
    assert!(output.contains("Advanced and machine-facing commands remain available"));
    assert!(!output.contains("owner-bind"));
    assert!(!output.contains("connection-export"));
    assert!(!output.contains("runtime-ready"));
    assert!(!output.contains("net-check"));
}

#[test]
fn hidden_advanced_commands_remain_callable_by_name() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    let output = run_bin_text(&["owner-bind", "--help"], &data_dir);

    assert!(output.contains("Bind this device to an owner account"));
    assert!(output.contains("--owner-account"));
}

#[test]
fn share_json_defaults_to_desktop_connection_file_for_primary_device() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    let data_dir = temp.path().join("data");

    let shared = run_bin_in_home(&["share", "--json"], &home, &data_dir);

    assert_eq!(shared["schema_version"], "thronglets.identity.v2");
    assert_eq!(shared["command"], "share");
    assert_eq!(shared["data"]["summary"]["status"], "share-limited");
    assert_eq!(shared["data"]["readiness"]["status"], "identity-only");
    assert_eq!(
        shared["data"]["output"],
        home.join("Desktop")
            .join("thronglets.connection.json")
            .display()
            .to_string()
    );
}

#[test]
fn share_text_hides_seed_scope_details() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    let data_dir = temp.path().join("data");

    let output = run_bin_text_in_home(&["share"], &home, &data_dir);

    assert!(output.contains("Thronglets:"));
    assert!(output.contains("Output:"));
    assert!(!output.contains("Seeds:"));
    assert!(!output.contains("Count:"));
}

#[test]
fn share_json_surfaces_peer_seed_ready_state() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    let data_dir = temp.path().join("data");

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&data_dir);

    let shared = run_bin_in_home(&["share", "--json"], &home, &data_dir);

    assert_eq!(shared["data"]["summary"]["status"], "share-ready");
    assert_eq!(
        shared["data"]["readiness"]["status"],
        "identity-plus-peer-seeds"
    );
    assert_eq!(shared["data"]["peer_seed_scope"], "remembered");
    assert_eq!(shared["data"]["peer_seed_count"], 2);
}

#[test]
fn connection_join_json_preserves_secondary_device_and_owner_binding() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    let owner = run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_dir,
    );
    assert_eq!(owner["data"]["summary"]["status"], "updated");

    let exported = run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    let primary_device = exported["data"]["identity"]["device_identity"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(exported["command"], "connection-export");
    assert_eq!(
        exported["data"]["identity"]["owner_account"],
        "oasyce1owner"
    );
    assert_eq!(exported["data"]["signed_by_device"], primary_device);
    assert_eq!(exported["data"]["peer_seed_scope"], "remembered");
    assert_eq!(exported["data"]["trusted_peer_seed_count"], 0);
    assert_eq!(exported["data"]["peer_seed_count"], 0);
    assert_eq!(exported["data"]["ttl_hours"], 24);
    assert!(exported["data"]["expires_at"].as_u64().unwrap() > 0);

    let joined = run_bin(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_dir,
    );
    let secondary_device = joined["data"]["identity"]["device_identity"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(joined["schema_version"], "thronglets.identity.v2");
    assert_eq!(joined["command"], "connection-join");
    assert_eq!(joined["data"]["summary"]["status"], "identity-only");
    assert_eq!(joined["data"]["summary"]["network_path_ready"], false);
    assert_eq!(joined["data"]["identity"]["owner_account"], "oasyce1owner");
    assert_eq!(
        joined["data"]["identity"]["binding_source"],
        "connection_file"
    );
    assert_eq!(
        joined["data"]["identity"]["joined_from_device"],
        primary_device.as_str()
    );
    assert_eq!(joined["data"]["signature_verified"], true);
    assert_eq!(joined["data"]["peer_seed_scope"], "remembered");
    assert_eq!(joined["data"]["imported_trusted_peer_seed_count"], 0);
    assert_eq!(joined["data"]["imported_peer_seed_count"], 0);
    assert_ne!(secondary_device, primary_device);

    let status = run_bin(&["status", "--json"], &secondary_dir);
    assert_eq!(status["command"], "status");
    assert_eq!(status["data"]["summary"]["status"], "identity-only");
    assert_eq!(status["data"]["identity"]["owner_account"], "oasyce1owner");
    assert_eq!(
        status["data"]["identity"]["binding_source"],
        "connection_file"
    );
    assert_eq!(
        status["data"]["identity"]["joined_from_device"],
        primary_device.as_str()
    );
    assert_eq!(
        status["data"]["identity"]["device_identity"],
        secondary_device.as_str()
    );
}

#[test]
fn connection_export_and_join_carry_oasyce_delegate_policy_bootstrap() {
    let temp = TempDir::new().unwrap();
    let primary_home = temp.path().join("primary-home");
    let secondary_home = temp.path().join("secondary-home");
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    std::fs::create_dir_all(primary_home.join(".oasyce")).unwrap();
    std::fs::create_dir_all(secondary_home.join(".oasyce")).unwrap();
    std::fs::write(
        primary_home.join(".oasyce").join("delegate_policy.v1.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": "oasyce.delegate_policy.v1",
            "principal": "oasyce1owner",
            "allowed_msgs": ["/cosmos.bank.v1beta1.MsgSend"],
            "enrollment_token": "shared-secret",
            "per_tx_limit_uoas": 1000000,
            "window_limit_uoas": 10000000,
            "window_seconds": 86400,
            "expiration_seconds": 0,
            "updated_at": "2026-04-04T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();

    run_bin_in_home(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_home,
        &primary_dir,
    );

    run_bin_in_home(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_home,
        &primary_dir,
    );

    let exported_file: Value =
        serde_json::from_slice(&std::fs::read(&connection_file).unwrap()).unwrap();
    assert_eq!(
        exported_file["oasyce_delegate_policy"]["principal"],
        "oasyce1owner"
    );
    assert_eq!(
        exported_file["oasyce_delegate_policy"]["enrollment_token"],
        "shared-secret"
    );

    run_bin_in_home(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_home,
        &secondary_dir,
    );

    let joined_binding: Value =
        serde_json::from_slice(&std::fs::read(secondary_dir.join("identity.v1.json")).unwrap())
            .unwrap();
    assert_eq!(
        joined_binding["oasyce_delegate_policy"]["principal"],
        "oasyce1owner"
    );
    assert_eq!(
        joined_binding["oasyce_delegate_policy"]["enrollment_token"],
        "shared-secret"
    );
}

#[test]
fn connection_join_imports_peer_seeds_into_local_snapshot() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_dir,
    );

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&primary_dir);

    let exported = run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(exported["data"]["peer_seed_scope"], "remembered");
    assert_eq!(exported["data"]["trusted_peer_seed_count"], 0);
    assert_eq!(exported["data"]["peer_seed_count"], 2);

    let joined = run_bin(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_dir,
    );
    assert_eq!(
        joined["data"]["summary"]["status"],
        "identity-plus-peer-seeds"
    );
    assert_eq!(joined["data"]["summary"]["network_path_ready"], true);
    assert_eq!(joined["data"]["peer_seed_scope"], "remembered");
    assert_eq!(joined["data"]["imported_trusted_peer_seed_count"], 0);
    assert_eq!(joined["data"]["imported_peer_seed_count"], 2);

    let status = run_bin(&["status", "--json"], &secondary_dir);
    assert_eq!(status["data"]["summary"]["status"], "network-paths-ready");
    assert_eq!(status["data"]["network"]["trusted_peer_seed_count"], 0);
    assert_eq!(status["data"]["network"]["peer_seed_count"], 2);
}

#[test]
fn join_json_wraps_setup_and_surfaces_network_path_ready_state() {
    let temp = TempDir::new().unwrap();
    let primary_home = temp.path().join("primary-home");
    let secondary_home = temp.path().join("secondary-home");
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    run_bin_in_home(&["start", "--json"], &primary_home, &primary_dir);

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&primary_dir);

    run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );

    let joined = run_bin_in_home(
        &[
            "join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_home,
        &secondary_dir,
    );

    assert_eq!(joined["command"], "join");
    assert_eq!(joined["data"]["summary"]["status"], "network-paths-ready");
    assert_eq!(
        joined["data"]["inspect"]["status"],
        "identity-plus-peer-seeds"
    );
    assert_eq!(joined["data"]["readiness"]["status"], "network-paths-ready");
    assert_eq!(joined["data"]["setup"]["healthy"], true);
}

#[test]
fn join_json_defaults_to_desktop_connection_file() {
    let temp = TempDir::new().unwrap();
    let primary_home = temp.path().join("primary-home");
    let secondary_home = temp.path().join("secondary-home");
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");

    std::fs::create_dir_all(primary_home.join("Desktop")).unwrap();
    std::fs::create_dir_all(secondary_home.join("Desktop")).unwrap();

    run_bin_in_home(&["start", "--json"], &primary_home, &primary_dir);

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&primary_dir);

    run_bin_in_home(&["share", "--json"], &primary_home, &primary_dir);

    std::fs::copy(
        primary_home
            .join("Desktop")
            .join("thronglets.connection.json"),
        secondary_home
            .join("Desktop")
            .join("thronglets.connection.json"),
    )
    .unwrap();

    let joined = run_bin_in_home(&["join", "--json"], &secondary_home, &secondary_dir);

    assert_eq!(joined["command"], "join");
    assert_eq!(joined["data"]["summary"]["status"], "network-paths-ready");
    assert_eq!(
        joined["data"]["file"],
        secondary_home
            .join("Desktop")
            .join("thronglets.connection.json")
            .display()
            .to_string()
    );
}

#[test]
fn join_text_hides_inspect_stage_details() {
    let temp = TempDir::new().unwrap();
    let primary_home = temp.path().join("primary-home");
    let secondary_home = temp.path().join("secondary-home");
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");

    std::fs::create_dir_all(primary_home.join("Desktop")).unwrap();
    std::fs::create_dir_all(secondary_home.join("Desktop")).unwrap();

    run_bin_in_home(&["start", "--json"], &primary_home, &primary_dir);

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&primary_dir);

    run_bin_in_home(&["share", "--json"], &primary_home, &primary_dir);

    std::fs::copy(
        primary_home
            .join("Desktop")
            .join("thronglets.connection.json"),
        secondary_home
            .join("Desktop")
            .join("thronglets.connection.json"),
    )
    .unwrap();

    let output = run_bin_text_in_home(&["join"], &secondary_home, &secondary_dir);

    assert!(output.contains("Thronglets:"));
    assert!(output.contains("State:"));
    assert!(!output.contains("Inspect:"));
}

#[test]
fn join_prioritizes_network_result_over_runtime_restart_in_summary() {
    let temp = TempDir::new().unwrap();
    let primary_home = temp.path().join("primary-home");
    let secondary_home = temp.path().join("secondary-home");
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");

    std::fs::create_dir_all(primary_home.join("Desktop")).unwrap();
    std::fs::create_dir_all(secondary_home.join("Desktop")).unwrap();
    std::fs::create_dir_all(secondary_home.join(".codex")).unwrap();

    run_bin_in_home(&["start", "--json"], &primary_home, &primary_dir);

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.save(&primary_dir);

    run_bin_in_home(&["share", "--json"], &primary_home, &primary_dir);

    std::fs::copy(
        primary_home
            .join("Desktop")
            .join("thronglets.connection.json"),
        secondary_home
            .join("Desktop")
            .join("thronglets.connection.json"),
    )
    .unwrap();

    let joined = run_bin_in_home(&["join", "--json"], &secondary_home, &secondary_dir);
    assert_eq!(joined["data"]["summary"]["status"], "network-paths-ready");
    assert_eq!(joined["data"]["readiness"]["status"], "network-paths-ready");
    assert_eq!(joined["data"]["setup"]["restart_required"], true);

    let output = run_bin_text_in_home(&["join"], &secondary_home, &secondary_dir);
    assert!(output.contains("Thronglets: waiting for the first live connection"));
    assert!(output.contains("Also:"));
    assert!(output.contains("Restart your AI runtime once:"));
}

#[test]
fn connection_export_prefers_trusted_peer_seeds() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let connection_file = temp.path().join("device.connection.json");

    run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_dir,
    );

    let mut snapshot = NetworkSnapshot::begin(1);
    snapshot.observe_peer_address("12D3KooWAlpha", "/ip4/10.0.0.1/tcp/4001");
    snapshot.merge_peer_seeds(["/ip4/10.0.0.9/tcp/4001".to_string()]);
    snapshot.merge_trusted_peer_seeds(["/ip4/10.0.0.8/tcp/4001".to_string()]);
    snapshot.save(&primary_dir);

    let exported = run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(exported["data"]["peer_seed_scope"], "trusted");
    assert_eq!(exported["data"]["trusted_peer_seed_count"], 1);
    assert_eq!(exported["data"]["peer_seed_count"], 1);

    let inspected = run_bin(
        &[
            "connection-inspect",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(
        inspected["data"]["summary"]["status"],
        "trusted-same-owner-ready"
    );
    assert_eq!(inspected["data"]["peer_seed_scope"], "trusted");
    assert_eq!(inspected["data"]["trusted_peer_seed_count"], 1);
    assert_eq!(inspected["data"]["peer_seed_count"], 1);
}

#[test]
fn expired_connection_file_join_fails() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("expired.connection.json");

    run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_dir,
    );
    let exported = run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--ttl-hours",
            "0",
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(exported["data"]["ttl_hours"], 0);

    let output = run_bin_raw(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_dir,
    );
    assert!(
        !output.status.success(),
        "expired join unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("connection file has expired"));
}

#[test]
fn connection_export_without_owner_binding_still_exports_network_pairing_file() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    let exported = run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(exported["command"], "connection-export");
    assert_eq!(exported["data"]["summary"]["status"], "identity-only");
    assert_eq!(exported["data"]["identity"]["owner_account"], Value::Null);
    assert_eq!(exported["data"]["peer_seed_scope"], "remembered");
    assert_eq!(exported["data"]["peer_seed_count"], 0);

    let inspected = run_bin(
        &[
            "connection-inspect",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(inspected["data"]["summary"]["status"], "identity-only");
    assert_eq!(inspected["data"]["identity"]["owner_account"], Value::Null);
    assert_eq!(inspected["data"]["signature_verified"], true);

    let joined = run_bin(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_dir,
    );
    assert_eq!(joined["data"]["summary"]["status"], "identity-only");
    assert_eq!(joined["data"]["identity"]["owner_account"], Value::Null);
    assert_eq!(
        joined["data"]["identity"]["binding_source"],
        "connection_file"
    );
}

#[test]
fn owner_bind_after_ownerless_connection_join_preserves_join_origin() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    let joined = run_bin(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_dir,
    );
    let primary_device = joined["data"]["identity"]["joined_from_device"]
        .as_str()
        .unwrap()
        .to_string();

    let bound = run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &secondary_dir,
    );
    assert_eq!(bound["data"]["summary"]["owner_account"], "oasyce1owner");
    assert_eq!(
        bound["data"]["summary"]["binding_source"],
        "connection_file"
    );
    assert_eq!(
        bound["data"]["summary"]["joined_from_device"],
        primary_device.as_str()
    );
    assert_eq!(
        bound["data"]["summary"]["identity_model"]["account"]["current_id"],
        "oasyce1owner"
    );
    assert_eq!(
        bound["data"]["summary"]["identity_model"]["delegate"]["current_id"],
        bound["data"]["summary"]["device_identity"]
    );
    assert_eq!(
        bound["data"]["summary"]["authorization"]["local_binding_status"],
        "owner-bound"
    );
    assert_eq!(
        bound["data"]["summary"]["authorization"]["local_binding_source"],
        "connection_file"
    );
    assert_eq!(
        bound["data"]["summary"]["authorization"]["authoritative_status"],
        "not-checked"
    );
}

#[test]
fn connection_inspect_json_surfaces_verified_metadata() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let connection_file = temp.path().join("device.connection.json");

    run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_dir,
    );
    run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--ttl-hours",
            "12",
            "--json",
        ],
        &primary_dir,
    );

    let inspected = run_bin(
        &[
            "connection-inspect",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );
    assert_eq!(inspected["command"], "connection-inspect");
    assert_eq!(inspected["data"]["summary"]["status"], "identity-only");
    assert_eq!(
        inspected["data"]["identity"]["owner_account"],
        "oasyce1owner"
    );
    assert_eq!(inspected["data"]["signature_verified"], true);
    assert_eq!(inspected["data"]["peer_seed_count"], 0);
    assert_eq!(inspected["data"]["trusted_peer_seed_count"], 0);
    assert_eq!(inspected["data"]["ttl_hours"], 12);
    assert!(inspected["data"]["expires_at"].as_u64().unwrap() > 0);
}

#[test]
fn owner_bind_rejects_conflicting_owner() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &data_dir,
    );
    let output = run_bin_raw(
        &["owner-bind", "--owner-account", "oasyce1other", "--json"],
        &data_dir,
    );
    assert!(
        !output.status.success(),
        "conflicting owner-bind unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("refusing to overwrite"));
}

#[test]
fn connection_join_rejects_conflicting_owner_binding() {
    let temp = TempDir::new().unwrap();
    let primary_dir = temp.path().join("primary");
    let secondary_dir = temp.path().join("secondary");
    let connection_file = temp.path().join("device.connection.json");

    run_bin(
        &["owner-bind", "--owner-account", "oasyce1owner", "--json"],
        &primary_dir,
    );
    run_bin(
        &["owner-bind", "--owner-account", "oasyce1other", "--json"],
        &secondary_dir,
    );
    run_bin(
        &[
            "connection-export",
            "--output",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &primary_dir,
    );

    let output = run_bin_raw(
        &[
            "connection-join",
            "--file",
            connection_file.to_str().unwrap(),
            "--json",
        ],
        &secondary_dir,
    );
    assert!(
        !output.status.success(),
        "conflicting connection-join unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("refusing to overwrite"));
}
