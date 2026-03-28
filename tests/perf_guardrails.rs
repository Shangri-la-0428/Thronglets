use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use thronglets::context::simhash;
use thronglets::contracts::{PREHOOK_HEADER, PREHOOK_MATCHER};
use thronglets::identity::NodeIdentity;
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};
use thronglets::workspace::{PendingFeedback, RecentAction, RecentError, RepairPattern, WorkspaceState};

fn run_bin(args: &[&str], input: Option<&str>, home: Option<&Path>) -> Output {
    run_bin_env(args, input, home, &[])
}

fn run_bin_env(
    args: &[&str],
    input: Option<&str>,
    home: Option<&Path>,
    envs: &[(&str, &str)],
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thronglets"));
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().expect("spawn thronglets");

    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("stdin available");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }

    child.wait_with_output().expect("wait for thronglets")
}

fn init_git_repo(repo: &Path) {
    let status = Command::new("git")
        .args(["init"])
        .current_dir(repo)
        .status()
        .expect("git init");
    assert!(status.success());

    let status = Command::new("git")
        .args(["config", "user.email", "thronglets@example.com"])
        .current_dir(repo)
        .status()
        .expect("git config email");
    assert!(status.success());

    let status = Command::new("git")
        .args(["config", "user.name", "Thronglets Test"])
        .current_dir(repo)
        .status()
        .expect("git config name");
    assert!(status.success());
}

fn git_commit_all(repo: &Path, message: &str) {
    let status = Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .status()
        .expect("git add");
    assert!(status.success());

    let status = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(repo)
        .status()
        .expect("git commit");
    assert!(status.success());
}

fn insert_trace(
    store: &TraceStore,
    identity: &NodeIdentity,
    capability: &str,
    outcome: Outcome,
    context: &str,
    session_id: &str,
) {
    let trace = Trace::new(
        capability.into(),
        outcome,
        10,
        10,
        simhash(context),
        Some(context.into()),
        Some(session_id.into()),
        "test-model".into(),
        identity.public_key_bytes(),
        |msg| identity.sign(msg),
    );
    store.insert(&trace).expect("insert trace");
}

#[test]
fn setup_keeps_prehook_narrow() {
    let home = tempfile::tempdir().unwrap();

    let output = run_bin(&["setup"], None, Some(home.path()));
    assert!(output.status.success(), "setup failed: {}", String::from_utf8_lossy(&output.stderr));

    let settings_path = home.path().join(".claude/settings.json");
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(settings_path).expect("settings.json"),
    )
    .expect("valid settings json");

    let pre_hooks = settings["hooks"]["PreToolUse"].as_array().expect("PreToolUse hooks");
    let thronglets_hook = pre_hooks.iter().find(|entry| {
        entry["hooks"].as_array().is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook["command"]
                    .as_str()
                    .is_some_and(|cmd| cmd.contains("thronglets prehook"))
            })
        })
    }).expect("thronglets prehook entry");

    assert_eq!(thronglets_hook["matcher"], PREHOOK_MATCHER);
}

#[test]
fn prehook_is_silent_without_signals() {
    let data_dir = tempfile::tempdir().unwrap();
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;

    let output = run_bin(
        &["--data-dir", data_dir.path().to_str().unwrap(), "prehook"],
        Some(payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

#[test]
fn prehook_profile_uses_stderr_only() {
    let data_dir = tempfile::tempdir().unwrap();
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;

    let output = run_bin_env(
        &["--data-dir", data_dir.path().to_str().unwrap(), "prehook"],
        Some(payload),
        None,
        &[("THRONGLETS_PROFILE_PREHOOK", "1")],
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[thronglets:prehook]"));
    assert!(stderr.contains("tool=Bash"));
    assert!(stderr.contains("stdout_bytes=0"));
    assert!(stderr.contains("output_mode=silent"));
    assert!(stderr.contains("decision_path=none"));
    assert!(stderr.contains("evidence_scope=none"));
    assert!(stderr.contains("file_guidance_gate=na"));
    assert!(stderr.contains("collective_queries_used=0"));
    assert!(stderr.contains("git=skipped"));
}

#[test]
fn prehook_emits_git_history_as_context_fallback() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    std::fs::write(&main_rs, "fn main() { println!(\"v2\"); }\n").unwrap();
    git_commit_all(repo.path(), "edit main");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("context: git history for main.rs:"));
}

#[test]
fn prehook_profile_keeps_stdout_shape_when_signals_exist() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_errors.push_front(RecentError {
        tool: "Edit".into(),
        context: "editing main".into(),
        error_snippet: "parser exploded".into(),
        timestamp_ms: now,
    });
    ws.repair_patterns.push_front(RepairPattern {
        error_tool: "Edit".into(),
        repair_tool: "Read".into(),
        repair_target: Some("helper.rs".into()),
        source_ids: vec!["local-a".into(), "local-b".into()],
        count: 2,
        last_seen_ms: now,
    });
    ws.save(&data_dir);

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin_env(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
        &[("THRONGLETS_PROFILE_PREHOOK", "1")],
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("avoid: recent error: parser exploded"));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x, 2 sources)"));
    assert!(stderr.contains("[thronglets:prehook]"));
    assert!(stderr.contains("tool=Edit"));
    assert!(stderr.contains("emitted=2"));
    assert!(stderr.contains(&format!("stdout_bytes={}", stdout.len())));
    assert!(stderr.contains("output_mode=next-step"));
    assert!(stderr.contains("decision_path=repair"));
    assert!(stderr.contains("evidence_scope=collective"));
    assert!(stderr.contains("file_guidance_gate=closed"));
    assert!(stderr.contains("collective_queries_used=0"));
    assert!(stderr.contains("git=skipped"));
}

#[test]
fn prehook_profile_reports_collective_query_usage() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    for offset in [0_i64, 10_000] {
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some(helper_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some(main_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset + 1_000,
        });
    }
    ws.save(&data_dir);

    let store = TraceStore::open(&data_dir.join("traces.db")).unwrap();
    let node_a = NodeIdentity::generate();
    let node_b = NodeIdentity::generate();
    for (identity, session_id, main_context, helper_context) in [
        (&node_a, "agent-a", "edit file: main.rs", "read file: helper.rs"),
        (&node_b, "agent-b", "edit file: /tmp/other/main.rs", "read file: /tmp/other/helper.rs"),
    ] {
        insert_trace(
            &store,
            identity,
            "claude-code/Read",
            Outcome::Succeeded,
            helper_context,
            session_id,
        );
        insert_trace(
            &store,
            identity,
            "claude-code/Edit",
            Outcome::Succeeded,
            main_context,
            session_id,
        );
    }

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin_env(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
        &[("THRONGLETS_PROFILE_PREHOOK", "1")],
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tool=Edit"));
    assert!(stderr.contains(&format!("stdout_bytes={}", stdout.len())));
    assert!(stderr.contains("output_mode=next-step"));
    assert!(stderr.contains("decision_path=preparation"));
    assert!(stderr.contains("evidence_scope=collective"));
    assert!(stderr.contains("file_guidance_gate=open"));
    assert!(stderr.contains("collective_queries_used=1"));
}

#[test]
fn prehook_profile_reports_context_only_mode() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    std::fs::write(&main_rs, "fn main() { println!(\"v2\"); }\n").unwrap();
    git_commit_all(repo.path(), "edit main");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin_env(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
        &[("THRONGLETS_PROFILE_PREHOOK", "1")],
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("context: git history for main.rs:"));
    assert!(stderr.contains("output_mode=context-only"));
    assert!(stderr.contains("decision_path=history"));
    assert!(stderr.contains("evidence_scope=none"));
    assert!(stderr.contains("file_guidance_gate=closed"));
    assert!(stderr.contains(&format!("stdout_bytes={}", stdout.len())));
}

#[test]
fn prehook_ignores_global_retention_without_local_evidence() {
    let data_dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    for i in 0..3 {
        ws.pending_feedback.push_front(PendingFeedback {
            file_path: format!("/other{i}.rs"),
            action: "Edit".into(),
            timestamp_ms: now,
            resolved: true,
            outcome: Some("reverted".into()),
        });
    }
    ws.save(data_dir.path());

    let payload = r#"{"tool_name":"Edit","tool_input":{"file_path":"/current.rs"}}"#;
    let output = run_bin(
        &["--data-dir", data_dir.path().to_str().unwrap(), "prehook"],
        Some(payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

#[test]
fn prehook_does_not_emit_repair_for_retention_only_danger() {
    let data_dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    for offset in [0_i64, 1_000] {
        ws.pending_feedback.push_front(PendingFeedback {
            file_path: "/current.rs".into(),
            action: "Edit".into(),
            timestamp_ms: now + offset,
            resolved: true,
            outcome: Some("reverted".into()),
        });
    }
    ws.repair_patterns.push_front(RepairPattern {
        error_tool: "Edit".into(),
        repair_tool: "Read".into(),
        repair_target: Some("helper.rs".into()),
        source_ids: vec!["local-a".into(), "local-b".into()],
        count: 2,
        last_seen_ms: now,
    });
    ws.save(data_dir.path());

    let payload = r#"{"tool_name":"Edit","tool_input":{"file_path":"/current.rs"}}"#;
    let output = run_bin_env(
        &["--data-dir", data_dir.path().to_str().unwrap(), "prehook"],
        Some(payload),
        None,
        &[("THRONGLETS_PROFILE_PREHOOK", "1")],
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("avoid: low retention for current.rs: 0/2 edits committed"));
    assert!(!stdout.contains("do next:"), "repair should require a recent tool error");
    assert!(!stdout.contains("context:"), "danger-only prehook should stay sparse and skip git fallback");
    assert!(stderr.contains("output_mode=caution"));
    assert!(stderr.contains("decision_path=danger"));
    assert!(stderr.contains("git=skipped"));
    assert!(stderr.contains("collective_queries_used=0"));
}

#[test]
fn prehook_ranks_danger_and_repair_above_history() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    std::fs::write(&main_rs, "fn main() { println!(\"v2\"); }\n").unwrap();
    git_commit_all(repo.path(), "edit main");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_errors.push_front(RecentError {
        tool: "Edit".into(),
        context: "editing main".into(),
        error_snippet: "parser exploded".into(),
        timestamp_ms: now,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 1_000,
    });
    ws.repair_patterns.push_front(RepairPattern {
        error_tool: "Edit".into(),
        repair_tool: "Read".into(),
        repair_target: Some("helper.rs".into()),
        source_ids: Vec::new(),
        count: 2,
        last_seen_ms: now,
    });
    for file in [&main_rs, &helper_rs, &repo.path().join("extra.rs")] {
        ws.pending_feedback.push_front(PendingFeedback {
            file_path: file.to_string_lossy().into_owned(),
            action: "Edit".into(),
            timestamp_ms: now,
            resolved: true,
            outcome: Some("reverted".into()),
        });
    }
    ws.save(&data_dir);

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );

    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("avoid: recent error: parser exploded"));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x)"));
    assert!(!stdout.contains("git history for main.rs"), "lower-priority history should be dropped");
    assert!(!stdout.contains("maybe also:"), "repair guidance should beat adjacency when budget is tight");
    assert!(!stdout.contains("edit retention:"), "global retention should not pollute file-scoped edits");

    let top_level_signals = stdout
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("    "))
        .count();
    assert_eq!(top_level_signals, 2);
}

#[test]
fn prehook_emits_confident_adjacency_when_no_repair_exists() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 1_000,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 10_000,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 11_000,
    });
    ws.save(&data_dir);

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("maybe also: Edit helper.rs (medium, 2x)"));
    assert!(!stdout.contains("git history for main.rs"));
}

#[test]
fn prehook_emits_confident_prep_read_before_editing() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db_path = data_dir.join("traces.db");

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_actions.push_back(RecentAction {
        tool: "Read".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 1_000,
    });
    ws.recent_actions.push_back(RecentAction {
        tool: "Read".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 10_000,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: None,
        outcome: "succeeded".into(),
        timestamp_ms: now + 11_000,
    });
    ws.save(&data_dir);

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x)"));
    assert!(!stdout.contains("git history for main.rs"));
    assert!(!db_path.exists(), "prehook should stay local-only when no collective store exists");
}

#[test]
fn prehook_skips_collective_lookup_when_local_sources_are_already_independent() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    for (offset, session_id) in [(0_i64, "agent-a"), (10_000_i64, "agent-b")] {
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some(helper_rs.to_string_lossy().into_owned()),
            session_id: Some(session_id.into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some(main_rs.to_string_lossy().into_owned()),
            session_id: Some(session_id.into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset + 1_000,
        });
    }
    ws.save(&data_dir);

    let db_path = data_dir.join("traces.db");
    assert!(!db_path.exists(), "test should start without a trace store");

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x, 2 sources)"));
    assert!(!db_path.exists(), "prehook should not open the collective store when local sources already suffice");
}

#[test]
fn prehook_keeps_collective_budget_zero_when_local_file_history_is_too_weak() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    std::fs::write(&main_rs, "fn main() { println!(\"v2\"); }\n").unwrap();
    git_commit_all(repo.path(), "edit main");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_actions.push_back(RecentAction {
        tool: "Read".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: Some("local-only".into()),
        outcome: "succeeded".into(),
        timestamp_ms: now,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: Some("local-only".into()),
        outcome: "succeeded".into(),
        timestamp_ms: now + 1_000,
    });
    ws.save(&data_dir);

    let store = TraceStore::open(&data_dir.join("traces.db")).unwrap();
    let node_a = NodeIdentity::generate();
    let node_b = NodeIdentity::generate();
    for (identity, session_id, main_context, helper_context) in [
        (&node_a, "agent-a", "edit file: main.rs", "read file: helper.rs"),
        (&node_b, "agent-b", "edit file: /tmp/other/main.rs", "read file: /tmp/other/helper.rs"),
    ] {
        insert_trace(
            &store,
            identity,
            "claude-code/Read",
            Outcome::Succeeded,
            helper_context,
            session_id,
        );
        insert_trace(
            &store,
            identity,
            "claude-code/Edit",
            Outcome::Succeeded,
            main_context,
            session_id,
        );
    }

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin_env(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
        &[("THRONGLETS_PROFILE_PREHOOK", "1")],
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("context: git history for main.rs:"));
    assert!(!stdout.contains("do next:"));
    assert!(stderr.contains("output_mode=context-only"));
    assert!(stderr.contains("decision_path=history"));
    assert!(stderr.contains("file_guidance_gate=closed"));
    assert!(stderr.contains("collective_queries_used=0"));
}

#[test]
fn prehook_spends_collective_budget_on_highest_priority_action_signal() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    let adjacent_rs = repo.path().join("adjacent.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    std::fs::write(&adjacent_rs, "pub fn adjacent() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    for offset in [0_i64, 10_000] {
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some(helper_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some(main_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset + 1_000,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Write".into(),
            file_path: Some(adjacent_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset + 2_000,
        });
    }
    ws.save(&data_dir);

    let store = TraceStore::open(&data_dir.join("traces.db")).unwrap();
    let node_a = NodeIdentity::generate();
    let node_b = NodeIdentity::generate();

    insert_trace(
        &store,
        &node_a,
        "claude-code/Edit",
        Outcome::Succeeded,
        "edit file: main.rs",
        "agent-a",
    );
    insert_trace(
        &store,
        &node_a,
        "claude-code/Write",
        Outcome::Succeeded,
        "write file: adjacent.rs",
        "agent-a",
    );
    insert_trace(
        &store,
        &node_b,
        "claude-code/Edit",
        Outcome::Succeeded,
        "edit file: /tmp/other/main.rs",
        "agent-b",
    );
    insert_trace(
        &store,
        &node_b,
        "claude-code/Write",
        Outcome::Succeeded,
        "write file: /tmp/other/adjacent.rs",
        "agent-b",
    );

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x)"));
    assert!(stdout.contains("maybe also: Edit adjacent.rs (medium, 2x)"));
    assert!(!stdout.contains("maybe also: Edit adjacent.rs (medium, 2x, 2 sources)"));
}

#[test]
fn prehook_upgrades_prep_read_with_collective_sources() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    for session_id in ["agent-a", "agent-b"] {
        let read_payload = format!(
            r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"{session_id}"}}"#,
            helper_rs.display()
        );
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&read_payload),
            None,
        );
        assert!(output.status.success(), "hook read failed: {}", String::from_utf8_lossy(&output.stderr));

        let edit_payload = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"{session_id}"}}"#,
            main_rs.display()
        );
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&edit_payload),
            None,
        );
        assert!(output.status.success(), "hook edit failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x, 2 sources)"));
}

#[test]
fn prehook_distinguishes_nodes_with_same_session_id_in_collective_sources() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    for offset in [0_i64, 10_000] {
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some(helper_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some(main_rs.to_string_lossy().into_owned()),
            session_id: Some("local-only".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + offset + 1_000,
        });
    }
    ws.save(&data_dir);

    let store = TraceStore::open(&data_dir.join("traces.db")).unwrap();
    let node_a = NodeIdentity::generate();
    let node_b = NodeIdentity::generate();
    let shared_session = "shared-session";

    insert_trace(
        &store,
        &node_a,
        "claude-code/Read",
        Outcome::Succeeded,
        "read file: helper.rs",
        shared_session,
    );
    insert_trace(
        &store,
        &node_a,
        "claude-code/Edit",
        Outcome::Succeeded,
        "edit file: main.rs",
        shared_session,
    );
    insert_trace(
        &store,
        &node_b,
        "claude-code/Read",
        Outcome::Succeeded,
        "read file: /tmp/other/helper.rs",
        shared_session,
    );
    insert_trace(
        &store,
        &node_b,
        "claude-code/Edit",
        Outcome::Succeeded,
        "edit file: /tmp/other/main.rs",
        shared_session,
    );

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("do next: Read helper.rs (medium, 2x, 2 sources)"));
}

#[test]
fn prehook_upgrades_repair_with_collective_sources() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let cargo_toml = repo.path().join("Cargo.toml");
    std::fs::write(&cargo_toml, "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    for session_id in ["agent-a", "agent-b"] {
        let bash_fail = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_response":{"error":"linker failed"},"session_id":"SESSION"}"#.replace("SESSION", session_id);
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&bash_fail),
            None,
        );
        assert!(output.status.success(), "hook bash fail failed: {}", String::from_utf8_lossy(&output.stderr));

        let read_payload = format!(
            r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"{session_id}"}}"#,
            cargo_toml.display()
        );
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&read_payload),
            None,
        );
        assert!(output.status.success(), "hook read failed: {}", String::from_utf8_lossy(&output.stderr));

        let bash_ok = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_response":{},"session_id":"SESSION"}"#.replace("SESSION", session_id);
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&bash_ok),
            None,
        );
        assert!(output.status.success(), "hook bash success failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_errors.push_front(RecentError {
        tool: "Bash".into(),
        context: "cargo test".into(),
        error_snippet: "linker failed".into(),
        timestamp_ms: now,
    });
    for (offset, (tool, file_path, outcome)) in [
        ("Bash", None, "failed"),
        ("Read", Some(cargo_toml.to_string_lossy().into_owned()), "succeeded"),
        ("Bash", None, "succeeded"),
        ("Bash", None, "failed"),
        ("Read", Some(cargo_toml.to_string_lossy().into_owned()), "succeeded"),
        ("Bash", None, "succeeded"),
    ].into_iter().enumerate() {
        ws.recent_actions.push_front(RecentAction {
            tool: tool.into(),
            file_path,
            session_id: Some("local-only".into()),
            outcome: outcome.into(),
            timestamp_ms: now + offset as i64 * 1_000,
        });
    }
    ws.save(&data_dir);

    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("avoid: recent error: linker failed"));
    assert!(stdout.contains("do next: Read Cargo.toml, then Bash (medium, 2x, 2 sources)"));
}

#[test]
fn prehook_upgrades_adjacency_with_collective_sources() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    for session_id in ["agent-a", "agent-b"] {
        let edit_main = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"{session_id}"}}"#,
            main_rs.display()
        );
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&edit_main),
            None,
        );
        assert!(output.status.success(), "hook edit main failed: {}", String::from_utf8_lossy(&output.stderr));

        let edit_helper = format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"{session_id}"}}"#,
            helper_rs.display()
        );
        let output = run_bin(
            &["--data-dir", data_dir.to_str().unwrap(), "hook"],
            Some(&edit_helper),
            None,
        );
        assert!(output.status.success(), "hook edit helper failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let now = chrono::Utc::now().timestamp_millis();
    let mut ws = WorkspaceState::default();
    ws.updated_ms = now;
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: Some("local-only".into()),
        outcome: "succeeded".into(),
        timestamp_ms: now,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: Some("local-only".into()),
        outcome: "succeeded".into(),
        timestamp_ms: now + 1_000,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(main_rs.to_string_lossy().into_owned()),
        session_id: Some("local-only".into()),
        outcome: "succeeded".into(),
        timestamp_ms: now + 10_000,
    });
    ws.recent_actions.push_front(RecentAction {
        tool: "Edit".into(),
        file_path: Some(helper_rs.to_string_lossy().into_owned()),
        session_id: Some("local-only".into()),
        outcome: "succeeded".into(),
        timestamp_ms: now + 11_000,
    });
    ws.save(&data_dir);

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PREHOOK_HEADER));
    assert!(stdout.contains("maybe also: Edit helper.rs (medium, 2x, 2 sources)"));
}

#[test]
fn prehook_suppresses_action_signal_for_single_collective_example() {
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path());

    let main_rs = repo.path().join("main.rs");
    let helper_rs = repo.path().join("helper.rs");
    std::fs::write(&main_rs, "fn main() {}\n").unwrap();
    std::fs::write(&helper_rs, "pub fn helper() {}\n").unwrap();
    git_commit_all(repo.path(), "init");

    let data_dir = repo.path().join(".thronglets-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let edit_main = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"agent-a"}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "hook"],
        Some(&edit_main),
        None,
    );
    assert!(output.status.success(), "hook edit main failed: {}", String::from_utf8_lossy(&output.stderr));

    let edit_helper = format!(
        r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}},"tool_response":{{}},"session_id":"agent-a"}}"#,
        helper_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "hook"],
        Some(&edit_helper),
        None,
    );
    assert!(output.status.success(), "hook edit helper failed: {}", String::from_utf8_lossy(&output.stderr));

    let payload = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
        main_rs.display()
    );
    let output = run_bin(
        &["--data-dir", data_dir.to_str().unwrap(), "prehook"],
        Some(&payload),
        None,
    );

    assert!(output.status.success(), "prehook failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("do next:"));
    assert!(!stdout.contains("maybe also:"));
}
