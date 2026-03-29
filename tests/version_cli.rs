use serde_json::Value;
use std::process::Command;

#[test]
fn version_json_surfaces_binary_and_schema_versions() {
    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["version", "--json"])
        .output()
        .expect("run thronglets version");

    assert!(
        output.status.success(),
        "version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let data: Value = serde_json::from_slice(&output.stdout).expect("parse version json");
    assert_eq!(data["schema_version"], "thronglets.version.v1");
    assert_eq!(data["command"], "version");
    assert_eq!(data["data"]["summary"]["status"], "ready");
    assert_eq!(
        data["data"]["summary"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        data["data"]["summary"]["bootstrap_schema_version"],
        "thronglets.bootstrap.v2"
    );
    assert_eq!(
        data["data"]["summary"]["identity_schema_version"],
        "thronglets.identity.v1"
    );
    assert!(data["data"]["binary_path"].as_str().is_some());
    assert!(
        data["data"]["source_hint"]
            .as_str()
            .expect("source hint")
            .contains("cargo run --quiet --")
    );
}
