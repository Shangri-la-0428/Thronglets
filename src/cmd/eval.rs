use super::*;

use crate::cli::{EvalSignalFocusArg, ReleaseEvalScopeArg};
use thronglets::eval::{
    EvalCheckThresholds, EvalConfig, LocalFeedbackSummary, SignalEvalSummary,
    evaluate_signal_quality,
};
use thronglets::pheromone::PheromoneField;
use thronglets::profile::{ProfileCheckThresholds, summarize_prehook_profiles};

#[derive(Debug, serde::Serialize)]
pub(crate) struct FieldConvergenceCapability {
    capability: String,
    intensity: f64,
    valence: f64,
    source_count: u32,
    excitations: u64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct FieldCouplingEdge {
    predecessor: String,
    successor: String,
    weight: f64,
    level: u8,
    bucket: i64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct FieldConvergence {
    traces_replayed: u64,
    active_capabilities: usize,
    multi_source_capabilities: usize,
    total_coupling_edges: usize,
    capabilities: Vec<FieldConvergenceCapability>,
    top_couplings: Vec<FieldCouplingEdge>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct EvalEmergenceOutput {
    project_scope: Option<String>,
    signal_eval: Option<SignalEvalSummary>,
    field_convergence: FieldConvergence,
    workspace_emergence: workspace::SpaceEmergenceSummary,
    substrate_activity: workspace::SubstrateActivity,
}

impl EvalEmergenceOutput {
    fn render(&self) -> String {
        let mut lines = vec![format!(
            "project scope: {}",
            self.project_scope.as_deref().unwrap_or("global")
        )];

        if let Some(summary) = self.signal_eval.as_ref() {
            let repair_precision = if summary.repair_predictions == 0 {
                0.0
            } else {
                (summary.repair_first_step_hits as f64 / summary.repair_predictions as f64) * 100.0
            };
            lines.push(format!(
                "offline eval: sessions_scored={}, repair_first_step_precision={repair_precision:.1}%",
                summary.sessions_scored,
            ));
        } else {
            lines.push("offline eval: pending (not enough recent session history)".to_string());
        }

        lines.push(format!(
            "field: {} capabilities ({} multi-source), {} coupling edges, {} traces replayed",
            self.field_convergence.active_capabilities,
            self.field_convergence.multi_source_capabilities,
            self.field_convergence.total_coupling_edges,
            self.field_convergence.traces_replayed,
        ));
        if !self.field_convergence.capabilities.is_empty() {
            let top: Vec<String> = self
                .field_convergence
                .capabilities
                .iter()
                .filter(|c| c.source_count > 1)
                .take(5)
                .map(|c| {
                    format!(
                        "  {}  src={} exc={}",
                        c.capability, c.source_count, c.excitations
                    )
                })
                .collect();
            if !top.is_empty() {
                lines.push("multi-source convergence:".to_string());
                lines.extend(top);
            }
        }

        lines.push(format!(
            "active spaces (24h): {}",
            self.workspace_emergence.active_spaces_24h
        ));
        lines.push(format!(
            "false signal pressure (24h): {:.3}",
            self.workspace_emergence.false_signal_pressure
        ));
        lines.push(format!(
            "cross-space contamination rate (24h): {:.3}",
            self.workspace_emergence.cross_space_contamination_rate
        ));
        lines.push(format!(
            "false-consensus spaces (24h): {}",
            self.workspace_emergence.false_consensus_spaces_24h
        ));
        lines.push(format!(
            "recoverable spaces (24h): {}",
            self.workspace_emergence.recoverable_spaces_24h
        ));
        lines.push(format!(
            "substrate activity: {}",
            self.substrate_activity.activity
        ));

        if !self.workspace_emergence.feedback_by_source_kind.is_empty() {
            let by_kind = self
                .workspace_emergence
                .feedback_by_source_kind
                .iter()
                .map(|(kind, summary)| {
                    format!(
                        "{kind}(+{} / -{})",
                        summary.positive_24h, summary.negative_24h
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("feedback by signal kind: {by_kind}"));
        }

        if !self.workspace_emergence.space_feedback.is_empty() {
            let compact = self
                .workspace_emergence
                .space_feedback
                .iter()
                .map(|(space, summary)| {
                    format!(
                        "{space}(+{} / -{})",
                        summary.positive_24h, summary.negative_24h
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("space feedback: {compact}"));
        }

        lines.join("\n")
    }
}

pub(crate) fn profile_summary() {
    let mut input = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
        std::process::exit(0);
    }

    if let Some(summary) = summarize_prehook_profiles(&input) {
        println!("{}", summary.render());
    } else {
        println!("no prehook profile samples found");
    }
}

pub(crate) fn profile_check() {
    let mut input = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
        std::process::exit(0);
    }

    if let Some(summary) = summarize_prehook_profiles(&input) {
        let (passed, rendered) = summary.render_check(&ProfileCheckThresholds::default());
        println!("{rendered}");
        if !passed {
            std::process::exit(1);
        }
    } else {
        println!("FAIL");
        println!("violations: no prehook profile samples found");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn release_check(
    base: &BaseCtx,
    hours: u64,
    max_sessions: usize,
    project_root: Option<PathBuf>,
    eval_scope: ReleaseEvalScopeArg,
    global: bool,
    require_profile_samples: bool,
    compare_baseline: Option<PathBuf>,
    json: bool,
) {
    let dir = &base.dir;
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    let profile_thresholds = ProfileCheckThresholds::default();
    let effective_eval_scope = if global {
        ReleaseEvalScopeArg::Global
    } else {
        eval_scope
    };
    let baseline = compare_baseline
        .as_deref()
        .map(load_eval_baseline)
        .transpose()
        .expect("failed to load eval baseline");

    let profile_section = match summarize_prehook_profiles(&input) {
        Some(summary) => {
            let violations = summary.check(&profile_thresholds);
            let passed = violations.is_empty();
            let (_, rendered) = summary.render_check(&profile_thresholds);
            (
                if passed { "PASS" } else { "FAIL" },
                !passed,
                strip_check_header(&rendered),
                serde_json::json!({
                    "status": if passed { "PASS" } else { "FAIL" },
                    "thresholds": profile_thresholds,
                    "summary": summary,
                    "violations": violations,
                    "notes": Vec::<String>::new(),
                }),
            )
        }
        None => {
            let status = if require_profile_samples {
                "FAIL"
            } else {
                "SKIP"
            };
            let violations = if require_profile_samples {
                vec!["no prehook profile samples found".to_string()]
            } else {
                Vec::new()
            };
            let notes = if require_profile_samples {
                Vec::new()
            } else {
                vec!["no prehook profile samples found".to_string()]
            };
            (
                status,
                require_profile_samples,
                if require_profile_samples {
                    "violations: no prehook profile samples found".to_string()
                } else {
                    "notes: no prehook profile samples found".to_string()
                },
                serde_json::json!({
                    "status": status,
                    "thresholds": profile_thresholds,
                    "summary": serde_json::Value::Null,
                    "violations": violations,
                    "notes": notes,
                }),
            )
        }
    };

    let hd = home_dir();
    let doctor_section = run_release_doctor_section(&hd, dir);

    let eval_thresholds = EvalCheckThresholds::default();
    let store = open_store(dir);
    let local_feedback = LocalFeedbackSummary::from_workspace(&load_workspace_state(dir));
    let default_project_root = project_root.unwrap_or_else(|| {
        std::env::current_dir().expect("failed to determine current working directory")
    });
    let eval_sections: Vec<_> = match effective_eval_scope {
        ReleaseEvalScopeArg::Project => vec![(
            "project",
            run_release_eval_section(
                &store,
                hours,
                max_sessions,
                Some(default_project_root.as_path()),
                local_feedback.clone(),
                &eval_thresholds,
                baseline.as_ref(),
            ),
        )],
        ReleaseEvalScopeArg::Global => vec![(
            "global",
            run_release_eval_section(
                &store,
                hours,
                max_sessions,
                None,
                None,
                &eval_thresholds,
                baseline.as_ref(),
            ),
        )],
        ReleaseEvalScopeArg::Both => vec![
            (
                "project",
                run_release_eval_section(
                    &store,
                    hours,
                    max_sessions,
                    Some(default_project_root.as_path()),
                    local_feedback,
                    &eval_thresholds,
                    baseline.as_ref(),
                ),
            ),
            (
                "global",
                run_release_eval_section(
                    &store,
                    hours,
                    max_sessions,
                    None,
                    None,
                    &eval_thresholds,
                    baseline.as_ref(),
                ),
            ),
        ],
    };

    let overall_failed =
        profile_section.1 || doctor_section.1 || eval_sections.iter().any(|(_, section)| section.1);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": if overall_failed { "FAIL" } else { "PASS" },
                "eval_scope": match effective_eval_scope {
                    ReleaseEvalScopeArg::Project => "project",
                    ReleaseEvalScopeArg::Global => "global",
                    ReleaseEvalScopeArg::Both => "both",
                },
                "profile": profile_section.3,
                "doctor": doctor_section.3,
                "eval": if eval_sections.len() == 1 {
                    eval_sections[0].1.3.clone()
                } else {
                    serde_json::json!({
                        "project": eval_sections[0].1.3.clone(),
                        "global": eval_sections[1].1.3.clone(),
                    })
                },
            }))
            .expect("failed to serialize release check")
        );
    } else {
        println!("{}", if overall_failed { "FAIL" } else { "PASS" });
        print_release_section("profile", profile_section.0, &profile_section.2);
        print_release_section("doctor", doctor_section.0, &doctor_section.2);
        if eval_sections.len() == 1 {
            let (label, section) = &eval_sections[0];
            print_release_section(&format!("eval ({label})"), section.0, &section.2);
        } else {
            for (label, section) in &eval_sections {
                print_release_section(&format!("eval ({label})"), section.0, &section.2);
            }
        }
    }
    if overall_failed {
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_signals(
    base: &BaseCtx,
    hours: u64,
    max_sessions: usize,
    project_root: Option<PathBuf>,
    global: bool,
    local_history_gate_min: u32,
    pattern_support_min: u32,
    compare_baseline: Option<PathBuf>,
    top_breakdowns: usize,
    focus: EvalSignalFocusArg,
    json: bool,
) {
    let dir = &base.dir;
    let store = open_store(dir);
    let project_scope = if global {
        None
    } else {
        Some(project_root.unwrap_or_else(|| {
            std::env::current_dir().expect("failed to determine current working directory")
        }))
    };
    let eval_config = EvalConfig {
        local_history_gate_min,
        pattern_support_min,
    };
    let default_config = EvalConfig::default();
    match evaluate_signal_quality(
        &store,
        hours,
        max_sessions,
        project_scope.as_deref(),
        eval_config,
    )
    .expect("failed to evaluate signal quality")
    {
        Some(summary) => {
            let summary = if eval_config != default_config {
                match evaluate_signal_quality(
                    &store,
                    hours,
                    max_sessions,
                    project_scope.as_deref(),
                    default_config,
                )
                .expect("failed to evaluate default signal quality")
                {
                    Some(baseline) => summary.with_comparison_to_default(&baseline),
                    None => summary,
                }
            } else {
                summary
            }
            .with_local_feedback(if project_scope.is_some() {
                LocalFeedbackSummary::from_workspace(&load_workspace_state(dir))
            } else {
                None
            });
            let summary = if let Some(baseline_path) = compare_baseline.as_ref() {
                summary.with_comparison_to_baseline(
                    &load_eval_baseline(baseline_path).expect("failed to load eval baseline"),
                )
            } else {
                summary
            };
            let summary = summary.focused(focus.into(), top_breakdowns);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary)
                        .expect("failed to serialize eval summary")
                );
            } else {
                println!("{}", summary.render());
            }
        }
        None => {
            if json {
                println!("null");
            } else {
                println!("not enough recent session history to evaluate signals yet");
            }
        }
    }
}

pub(crate) fn eval_emergence(
    base: &BaseCtx,
    hours: u64,
    max_sessions: usize,
    project_root: Option<PathBuf>,
    global: bool,
    json: bool,
) {
    let dir = &base.dir;
    let store = open_store(dir);
    let ws = load_workspace_state(dir);
    let project_scope = if global {
        None
    } else {
        Some(project_root.unwrap_or_else(|| {
            std::env::current_dir().expect("failed to determine current working directory")
        }))
    };
    let signal_eval = evaluate_signal_quality(
        &store,
        hours,
        max_sessions,
        project_scope.as_deref(),
        EvalConfig::default(),
    )
    .expect("failed to evaluate emergence signal quality")
    .map(|summary| {
        summary.with_local_feedback(if project_scope.is_some() {
            LocalFeedbackSummary::from_workspace(&ws)
        } else {
            None
        })
    });

    // Build field convergence report: replay recent traces through
    // pheromone field with full Hebbian coupling, then inspect state.
    let field_convergence = {
        let field = PheromoneField::new();
        let traces = store
            .recent_traces_with_space(hours, 50_000)
            .unwrap_or_default();
        let trace_count = traces.len() as u64;
        for (trace, space) in &traces {
            field.excite_with_space(trace, space.as_deref());
        }
        let caps = field.capabilities(100);
        let multi_source = caps.iter().filter(|c| c.source_count > 1).count();
        let top_couplings: Vec<FieldCouplingEdge> = field
            .active_edges(20)
            .into_iter()
            .map(|(pred, succ, w, level, bucket)| FieldCouplingEdge {
                predecessor: pred,
                successor: succ,
                weight: (w * 1000.0).round() / 1000.0,
                level: level as u8,
                bucket,
            })
            .collect();
        FieldConvergence {
            traces_replayed: trace_count,
            active_capabilities: caps.len(),
            multi_source_capabilities: multi_source,
            total_coupling_edges: field.coupling_count(),
            capabilities: caps
                .iter()
                .map(|c| FieldConvergenceCapability {
                    capability: c.capability.clone(),
                    intensity: (c.intensity * 100.0).round() / 100.0,
                    valence: (c.valence * 1000.0).round() / 1000.0,
                    source_count: c.source_count,
                    excitations: c.total_excitations,
                })
                .collect(),
            top_couplings,
        }
    };

    let output = EvalEmergenceOutput {
        project_scope: project_scope
            .as_ref()
            .map(|path| path.display().to_string()),
        signal_eval,
        field_convergence,
        workspace_emergence: ws.emergence_summary(),
        substrate_activity: ws.substrate_activity(),
    };

    if json {
        print_json(&output);
    } else {
        println!("{}", output.render());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use thronglets::eval::{EvalCheckStatus, EvalConfig, LocalFeedbackSummary, SignalEvalSummary};

    fn eval_summary(
        retention_percent: u32,
        failed_calls: usize,
        command_calls: usize,
        avg_latency_ms: Option<u64>,
        p50_latency_ms: Option<u64>,
    ) -> SignalEvalSummary {
        SignalEvalSummary {
            project_scope: Some("/tmp/project".into()),
            eval_config: EvalConfig::default(),
            local_feedback: Some(LocalFeedbackSummary {
                resolved_edits: 10,
                committed_edits: (retention_percent / 10) as usize,
                reverted_edits: 0,
                retention_percent,
            }),
            comparison_to_default: None,
            comparison_to_baseline: None,
            sessions_considered: 8,
            sessions_scored: 8,
            holdout_command_calls: command_calls,
            holdout_failed_command_calls: failed_calls,
            sessions_with_successful_change: 8,
            first_successful_change_latency_avg_ms: avg_latency_ms,
            first_successful_change_latency_p50_ms: p50_latency_ms,
            edit_points: 20,
            edit_points_with_signal: 1,
            repair_opportunities: 4,
            repair_predictions: 3,
            repair_first_step_hits: 2,
            repair_exact_hits: 1,
            preparation_gated_edit_points: 2,
            preparation_predictions: 5,
            preparation_hits: 2,
            adjacency_gated_edit_points: 2,
            adjacency_predictions: 5,
            adjacency_hits: 2,
            repair_breakdown: BTreeMap::new(),
            preparation_breakdown: BTreeMap::new(),
            adjacency_breakdown: BTreeMap::new(),
        }
    }

    #[test]
    fn release_baseline_check_fails_on_outcome_regressions() {
        let baseline = eval_summary(90, 0, 10, Some(1_000), Some(1_000));
        let candidate = eval_summary(80, 2, 10, Some(7_000), Some(7_000))
            .with_comparison_to_baseline(&baseline);

        let check = release_baseline_check(
            candidate.comparison_to_baseline.as_ref(),
            EvalCheckStatus::Pass,
        );

        assert_eq!(check.status, "FAIL");
        assert_eq!(check.violations.len(), 4);
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("local edit retention"))
        );
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("failed command rate"))
        );
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("first successful change latency avg"))
        );
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("first successful change latency p50"))
        );
    }

    #[test]
    fn release_baseline_check_skips_while_eval_is_skip() {
        let baseline = eval_summary(90, 0, 10, Some(1_000), Some(1_000));
        let candidate = eval_summary(80, 2, 10, Some(7_000), Some(7_000))
            .with_comparison_to_baseline(&baseline);

        let check = release_baseline_check(
            candidate.comparison_to_baseline.as_ref(),
            EvalCheckStatus::Skip,
        );

        assert_eq!(check.status, "SKIP");
        assert!(check.violations.is_empty());
        assert_eq!(check.notes.len(), 1);
    }
}
