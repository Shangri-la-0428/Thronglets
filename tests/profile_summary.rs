use std::io::Write;
use std::process::{Command, Stdio};

fn run_profile_summary(input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .arg("profile-summary")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn thronglets profile-summary");

    {
        let mut stdin = child.stdin.take().expect("stdin available");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }

    child.wait_with_output().expect("wait for thronglets")
}

#[test]
fn profile_summary_aggregates_profile_lines() {
    let output = run_profile_summary(
        "[thronglets:prehook] tool=Edit emitted=2 stdout_bytes=88 output_mode=next-step decision_path=repair evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
         [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
         [thronglets:prehook] tool=Edit emitted=1 stdout_bytes=42 output_mode=context-only decision_path=history evidence_scope=none file_guidance_gate=closed collective_queries_used=0 total_us=200\n",
    );

    assert!(output.status.success(), "profile-summary failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("samples: 3"));
    assert!(stdout.contains("avg total_us: 200.0"));
    assert!(stdout.contains("p95 total_us: 300"));
    assert!(stdout.contains("avg stdout_bytes: 43.3"));
    assert!(stdout.contains("emitted lines: 0=1, 1=1, 2=1"));
    assert!(stdout.contains("max-hint saturation: 0.0% (0/3)"));
    assert!(stdout.contains("tools: Edit=2, Bash=1"));
    assert!(stdout.contains("output modes: context-only=1, next-step=1, silent=1"));
    assert!(stdout.contains("decision paths: history=1, none=1, repair=1"));
    assert!(stdout.contains("evidence scopes: none=2, collective=1"));
    assert!(stdout.contains("file guidance gates: closed=1, na=1, open=1"));
    assert!(stdout.contains("collective query paths: repair=1"));
    assert!(stdout.contains("top optimization candidate: reduce collective queries in repair path (collective_queries=1, avg_stdout_bytes=88.0)"));
    assert!(stdout.contains("decision path hotspots: repair(samples=1, avg_stdout_bytes=88.0, avg_total_us=300.0, collective_queries=1), history(samples=1, avg_stdout_bytes=42.0, avg_total_us=200.0, collective_queries=0), none(samples=1, avg_stdout_bytes=0.0, avg_total_us=100.0, collective_queries=0)"));
}

#[test]
fn profile_summary_reports_when_no_samples_exist() {
    let output = run_profile_summary("noise only\nstill noise\n");

    assert!(output.status.success(), "profile-summary failed: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "no prehook profile samples found");
}
