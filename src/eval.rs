use crate::signals::StepAction;
use crate::storage::TraceStore;
use crate::trace::{Outcome, Trace};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

const SESSION_TRACE_LIMIT: usize = 10_000;
const FILE_WINDOW_MS: i64 = 300_000;
const REPAIR_WINDOW_MS: i64 = 600_000;
const LOCAL_HISTORY_GATE_MIN: u32 = 2;
const PATTERN_SUPPORT_MIN: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalFocus {
    All,
    Repair,
    Preparation,
    Adjacency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignalEvalSummary {
    pub project_scope: Option<String>,
    pub sessions_considered: usize,
    pub sessions_scored: usize,
    pub edit_points: usize,
    pub edit_points_with_signal: usize,
    pub repair_opportunities: usize,
    pub repair_predictions: usize,
    pub repair_first_step_hits: usize,
    pub repair_exact_hits: usize,
    pub preparation_gated_edit_points: usize,
    pub preparation_predictions: usize,
    pub preparation_hits: usize,
    pub adjacency_gated_edit_points: usize,
    pub adjacency_predictions: usize,
    pub adjacency_hits: usize,
    pub repair_breakdown: BTreeMap<String, RepairEvalBreakdown>,
    pub preparation_breakdown: BTreeMap<String, FileGuidanceEvalBreakdown>,
    pub adjacency_breakdown: BTreeMap<String, FileGuidanceEvalBreakdown>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct RepairEvalBreakdown {
    pub opportunities: usize,
    pub predictions: usize,
    pub first_step_hits: usize,
    pub exact_hits: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct FileGuidanceEvalBreakdown {
    pub edit_points: usize,
    pub gated_points: usize,
    pub predictions: usize,
    pub hits: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionEvent {
    tool: String,
    target: Option<String>,
    outcome: Outcome,
    timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RepairKey {
    failed_tool: String,
    steps: Vec<StepAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatternChoice<T> {
    value: T,
    support: u32,
    source_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatternStats {
    count: u32,
    sources: HashSet<String>,
}

#[derive(Debug, Default)]
struct SignalTrainingSet {
    file_touch_counts: HashMap<String, u32>,
    repair_patterns: HashMap<RepairKey, PatternStats>,
    preparation_patterns: HashMap<(String, String), PatternStats>,
    adjacency_patterns: HashMap<(String, String), PatternStats>,
}

impl SignalEvalSummary {
    pub fn focused(mut self, focus: EvalFocus, top_breakdowns: usize) -> Self {
        self.repair_breakdown = trim_repair_breakdown(&self.repair_breakdown, top_breakdowns);
        self.preparation_breakdown =
            trim_file_guidance_breakdown(&self.preparation_breakdown, top_breakdowns);
        self.adjacency_breakdown =
            trim_file_guidance_breakdown(&self.adjacency_breakdown, top_breakdowns);

        match focus {
            EvalFocus::All => {}
            EvalFocus::Repair => {
                self.preparation_breakdown.clear();
                self.adjacency_breakdown.clear();
            }
            EvalFocus::Preparation => {
                self.repair_breakdown.clear();
                self.adjacency_breakdown.clear();
            }
            EvalFocus::Adjacency => {
                self.repair_breakdown.clear();
                self.preparation_breakdown.clear();
            }
        }

        self
    }

    pub fn render(&self) -> String {
        let mut lines = vec![
            format!(
                "project scope: {}",
                self.project_scope.as_deref().unwrap_or("global")
            ),
            format!("sessions considered: {}", self.sessions_considered),
            format!("sessions scored: {}", self.sessions_scored),
            format!(
                "edit silence rate: {:.1}% ({}/{})",
                percent(
                    self.edit_points
                        .saturating_sub(self.edit_points_with_signal),
                    self.edit_points
                ),
                self.edit_points
                    .saturating_sub(self.edit_points_with_signal),
                self.edit_points,
            ),
            format!(
                "repair coverage: {:.1}% ({}/{})",
                percent(self.repair_predictions, self.repair_opportunities),
                self.repair_predictions,
                self.repair_opportunities,
            ),
            format!(
                "repair first-step precision: {:.1}% ({}/{})",
                percent(self.repair_first_step_hits, self.repair_predictions),
                self.repair_first_step_hits,
                self.repair_predictions,
            ),
            format!(
                "repair exact precision: {:.1}% ({}/{})",
                percent(self.repair_exact_hits, self.repair_predictions),
                self.repair_exact_hits,
                self.repair_predictions,
            ),
            format!(
                "preparation local-gate block rate: {:.1}% ({}/{})",
                percent(self.preparation_gated_edit_points, self.edit_points),
                self.preparation_gated_edit_points,
                self.edit_points,
            ),
            format!(
                "preparation precision: {:.1}% ({}/{})",
                percent(self.preparation_hits, self.preparation_predictions),
                self.preparation_hits,
                self.preparation_predictions,
            ),
            format!(
                "adjacency local-gate block rate: {:.1}% ({}/{})",
                percent(self.adjacency_gated_edit_points, self.edit_points),
                self.adjacency_gated_edit_points,
                self.edit_points,
            ),
            format!(
                "adjacency precision: {:.1}% ({}/{})",
                percent(self.adjacency_hits, self.adjacency_predictions),
                self.adjacency_hits,
                self.adjacency_predictions,
            ),
            format!("diagnosis: {}", self.diagnosis()),
        ];

        if !self.repair_breakdown.is_empty() {
            lines.push(format!(
                "repair breakdown: {}",
                render_repair_breakdown(&self.repair_breakdown)
            ));
        }
        if !self.preparation_breakdown.is_empty() {
            lines.push(format!(
                "preparation breakdown: {}",
                render_file_guidance_breakdown(&self.preparation_breakdown)
            ));
        }
        if !self.adjacency_breakdown.is_empty() {
            lines.push(format!(
                "adjacency breakdown: {}",
                render_file_guidance_breakdown(&self.adjacency_breakdown)
            ));
        }

        lines.join("\n")
    }

    pub fn diagnosis(&self) -> &'static str {
        if self.repair_opportunities > 0 && self.repair_predictions == 0 {
            return "repair has too little repeated support; collect more failed->fixed sequences before widening hints";
        }
        if self.preparation_gated_edit_points > self.preparation_predictions {
            return "file guidance is mostly blocked by the local repetition gate; this repo needs more repeated edit history";
        }
        if self.adjacency_predictions > 0 && self.adjacency_hits * 5 < self.adjacency_predictions {
            return "adjacency patterns are noisy; tighten thresholds before emitting more maybe-also hints";
        }
        if self.edit_points > 0 && self.edit_points_with_signal * 4 < self.edit_points {
            return "sparse-signal policy is staying mostly silent; keep it that way unless precision improves";
        }
        "signal mix looks reasonable; keep tuning by measured precision rather than adding new hint types"
    }
}

impl PatternStats {
    fn record(&mut self, source_id: &str) {
        self.count += 1;
        self.sources.insert(source_id.to_string());
    }

    fn is_strong(&self) -> bool {
        self.count >= PATTERN_SUPPORT_MIN
    }
}

impl SignalTrainingSet {
    fn observe_session(&mut self, session_id: &str, events: &[SessionEvent]) {
        for event in events {
            if matches!(event.tool.as_str(), "Edit" | "Write") {
                if let Some(target) = event.target.as_ref() {
                    *self.file_touch_counts.entry(target.clone()).or_insert(0) += 1;
                }
            }
        }

        for (idx, event) in events.iter().enumerate() {
            if event.outcome == Outcome::Failed {
                let steps = actual_repair_steps(events, idx);
                if !steps.is_empty() {
                    self.repair_patterns
                        .entry(RepairKey {
                            failed_tool: event.tool.clone(),
                            steps,
                        })
                        .or_insert_with(|| PatternStats {
                            count: 0,
                            sources: HashSet::new(),
                        })
                        .record(session_id);
                }
            }

            if !matches!(event.tool.as_str(), "Edit" | "Write") {
                continue;
            }

            let Some(current_target) = event.target.as_ref() else {
                continue;
            };

            for read_target in actual_preparation_targets(events, idx) {
                self.preparation_patterns
                    .entry((current_target.clone(), read_target))
                    .or_insert_with(|| PatternStats {
                        count: 0,
                        sources: HashSet::new(),
                    })
                    .record(session_id);
            }

            for companion_target in actual_companion_targets(events, idx) {
                self.adjacency_patterns
                    .entry((current_target.clone(), companion_target))
                    .or_insert_with(|| PatternStats {
                        count: 0,
                        sources: HashSet::new(),
                    })
                    .record(session_id);
            }
        }
    }

    fn best_repair(&self, failed_tool: &str) -> Option<PatternChoice<Vec<StepAction>>> {
        self.repair_patterns
            .iter()
            .filter(|(key, stats)| key.failed_tool == failed_tool && stats.is_strong())
            .map(|(key, stats)| PatternChoice {
                value: key.steps.clone(),
                support: stats.count,
                source_count: stats.sources.len(),
            })
            .max_by(|a, b| rank_tuple(a).cmp(&rank_tuple(b)))
    }

    fn best_preparation(&self, edit_target: &str) -> Option<PatternChoice<String>> {
        if !self.file_guidance_gate_open(edit_target) {
            return None;
        }

        self.preparation_patterns
            .iter()
            .filter(|((target, _), stats)| target == edit_target && stats.is_strong())
            .map(|((_, read_target), stats)| PatternChoice {
                value: read_target.clone(),
                support: stats.count,
                source_count: stats.sources.len(),
            })
            .max_by(|a, b| rank_tuple(a).cmp(&rank_tuple(b)))
    }

    fn best_adjacency(&self, edit_target: &str) -> Option<PatternChoice<String>> {
        if !self.file_guidance_gate_open(edit_target) {
            return None;
        }

        self.adjacency_patterns
            .iter()
            .filter(|((target, _), stats)| target == edit_target && stats.is_strong())
            .map(|((_, companion_target), stats)| PatternChoice {
                value: companion_target.clone(),
                support: stats.count,
                source_count: stats.sources.len(),
            })
            .max_by(|a, b| rank_tuple(a).cmp(&rank_tuple(b)))
    }

    fn file_guidance_gate_open(&self, edit_target: &str) -> bool {
        self.file_touch_counts
            .get(edit_target)
            .copied()
            .unwrap_or(0)
            >= LOCAL_HISTORY_GATE_MIN
    }
}

pub fn evaluate_signal_quality(
    store: &TraceStore,
    hours: u64,
    max_sessions: usize,
    project_root: Option<&Path>,
) -> rusqlite::Result<Option<SignalEvalSummary>> {
    let session_ids = store.recent_session_ids(hours, max_sessions)?;
    if session_ids.len() < 2 {
        return Ok(None);
    }

    let mut sessions = Vec::new();
    for session_id in session_ids {
        let traces = store.query_session(&session_id, SESSION_TRACE_LIMIT)?;
        if let Some(project_root) = project_root {
            if !session_matches_project_root(&traces, project_root) {
                continue;
            }
        }
        let events = traces.iter().filter_map(trace_to_event).collect::<Vec<_>>();
        if !events.is_empty() {
            sessions.push((session_id, events));
        }
    }

    if sessions.len() < 2 {
        return Ok(None);
    }

    let mut summary = SignalEvalSummary {
        project_scope: project_root.map(|path| path.display().to_string()),
        sessions_considered: sessions.len(),
        sessions_scored: 0,
        edit_points: 0,
        edit_points_with_signal: 0,
        repair_opportunities: 0,
        repair_predictions: 0,
        repair_first_step_hits: 0,
        repair_exact_hits: 0,
        preparation_gated_edit_points: 0,
        preparation_predictions: 0,
        preparation_hits: 0,
        adjacency_gated_edit_points: 0,
        adjacency_predictions: 0,
        adjacency_hits: 0,
        repair_breakdown: BTreeMap::new(),
        preparation_breakdown: BTreeMap::new(),
        adjacency_breakdown: BTreeMap::new(),
    };
    let mut training = SignalTrainingSet::default();

    for (index, (session_id, events)) in sessions.iter().enumerate() {
        if index > 0 {
            summary.sessions_scored += 1;
            score_session(&training, events, &mut summary);
        }
        training.observe_session(session_id, events);
    }

    Ok(Some(summary))
}

fn score_session(
    training: &SignalTrainingSet,
    events: &[SessionEvent],
    summary: &mut SignalEvalSummary,
) {
    for (idx, event) in events.iter().enumerate() {
        if matches!(event.tool.as_str(), "Edit" | "Write") {
            summary.edit_points += 1;

            let mut emitted_signal = false;
            if let Some(current_target) = event.target.as_deref() {
                let prep_breakdown = summary
                    .preparation_breakdown
                    .entry(current_target.to_string())
                    .or_default();
                prep_breakdown.edit_points += 1;
                let prep_gate_open = training.file_guidance_gate_open(current_target);
                if !prep_gate_open {
                    summary.preparation_gated_edit_points += 1;
                    prep_breakdown.gated_points += 1;
                }

                if let Some(predicted) = training.best_preparation(current_target) {
                    summary.preparation_predictions += 1;
                    prep_breakdown.predictions += 1;
                    emitted_signal = true;
                    if actual_preparation_targets(events, idx).contains(&predicted.value) {
                        summary.preparation_hits += 1;
                        prep_breakdown.hits += 1;
                    }
                }

                let adjacency_breakdown = summary
                    .adjacency_breakdown
                    .entry(current_target.to_string())
                    .or_default();
                adjacency_breakdown.edit_points += 1;
                let adjacency_gate_open = training.file_guidance_gate_open(current_target);
                if !adjacency_gate_open {
                    summary.adjacency_gated_edit_points += 1;
                    adjacency_breakdown.gated_points += 1;
                }

                if let Some(predicted) = training.best_adjacency(current_target) {
                    summary.adjacency_predictions += 1;
                    adjacency_breakdown.predictions += 1;
                    emitted_signal = true;
                    if actual_companion_targets(events, idx).contains(&predicted.value) {
                        summary.adjacency_hits += 1;
                        adjacency_breakdown.hits += 1;
                    }
                }
            }

            if emitted_signal {
                summary.edit_points_with_signal += 1;
            }
        }

        if event.outcome == Outcome::Failed {
            summary.repair_opportunities += 1;
            let repair_breakdown = summary
                .repair_breakdown
                .entry(event.tool.clone())
                .or_default();
            repair_breakdown.opportunities += 1;
            if let Some(predicted) = training.best_repair(&event.tool) {
                summary.repair_predictions += 1;
                repair_breakdown.predictions += 1;
                let actual = actual_repair_steps(events, idx);
                if let (Some(predicted_first), Some(actual_first)) =
                    (predicted.value.first(), actual.first())
                {
                    if predicted_first == actual_first {
                        summary.repair_first_step_hits += 1;
                        repair_breakdown.first_step_hits += 1;
                    }
                }
                if predicted.value == actual {
                    summary.repair_exact_hits += 1;
                    repair_breakdown.exact_hits += 1;
                }
            }
        }
    }
}

fn trace_to_event(trace: &Trace) -> Option<SessionEvent> {
    let tool = trace.capability.strip_prefix("claude-code/")?.to_string();
    Some(SessionEvent {
        target: parse_target(&tool, trace.context_text.as_deref()),
        tool,
        outcome: trace.outcome,
        timestamp_ms: trace.timestamp as i64,
    })
}

fn session_matches_project_root(traces: &[Trace], project_root: &Path) -> bool {
    let roots = project_root_variants(project_root);
    traces.iter().any(|trace| {
        trace.context_text
            .as_deref()
            .and_then(extract_context_path)
            .is_some_and(|path| roots.iter().any(|root| path.starts_with(root)))
    })
}

fn extract_context_path(context_text: &str) -> Option<&str> {
    ["read file: ", "edit file: ", "write file: "]
        .iter()
        .find_map(|prefix| context_text.strip_prefix(prefix))
}

fn project_root_variants(project_root: &Path) -> Vec<String> {
    let mut variants = Vec::new();
    let root = project_root.to_string_lossy().trim_end_matches('/').to_string();
    variants.push(root.clone());

    if let Some(stripped) = root.strip_prefix("/private") {
        variants.push(stripped.to_string());
    } else if root.starts_with("/var/") {
        variants.push(format!("/private{root}"));
    }

    variants.sort();
    variants.dedup();
    variants
}

fn parse_target(tool: &str, context_text: Option<&str>) -> Option<String> {
    let prefix = match tool {
        "Read" => "read file: ",
        "Edit" => "edit file: ",
        "Write" => "write file: ",
        _ => return None,
    };

    let raw = context_text?.strip_prefix(prefix)?;
    Some(short_target(raw))
}

fn short_target(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

fn actual_repair_steps(events: &[SessionEvent], failed_index: usize) -> Vec<StepAction> {
    let failed = &events[failed_index];
    let mut steps = Vec::new();
    let mut last_timestamp = failed.timestamp_ms;

    for next in events.iter().skip(failed_index + 1) {
        if next.timestamp_ms - last_timestamp > REPAIR_WINDOW_MS {
            break;
        }
        if next.outcome == Outcome::Failed {
            break;
        }
        steps.push(StepAction::new(next.tool.clone(), next.target.clone()));
        last_timestamp = next.timestamp_ms;
        if steps.len() == 2 {
            break;
        }
    }

    steps
}

fn actual_preparation_targets(events: &[SessionEvent], edit_index: usize) -> HashSet<String> {
    let edit = &events[edit_index];
    let Some(current_target) = edit.target.as_ref() else {
        return HashSet::new();
    };

    let start = edit_index.saturating_sub(5);
    let mut targets = HashSet::new();
    for prev in events.iter().take(edit_index).skip(start) {
        if prev.tool != "Read" {
            continue;
        }
        if edit.timestamp_ms - prev.timestamp_ms > FILE_WINDOW_MS {
            continue;
        }
        if let Some(target) = prev.target.as_ref() {
            if target != current_target {
                targets.insert(target.clone());
            }
        }
    }
    targets
}

fn actual_companion_targets(events: &[SessionEvent], edit_index: usize) -> HashSet<String> {
    let edit = &events[edit_index];
    let Some(current_target) = edit.target.as_ref() else {
        return HashSet::new();
    };

    let start = edit_index.saturating_sub(10);
    let end = (edit_index + 11).min(events.len());
    let mut targets = HashSet::new();
    for (idx, other) in events.iter().enumerate().take(end).skip(start) {
        if idx == edit_index {
            continue;
        }
        if !matches!(other.tool.as_str(), "Edit" | "Write") {
            continue;
        }
        if (other.timestamp_ms - edit.timestamp_ms).abs() > FILE_WINDOW_MS {
            continue;
        }
        if let Some(target) = other.target.as_ref() {
            if target != current_target {
                targets.insert(target.clone());
            }
        }
    }
    targets
}

fn rank_tuple<T>(choice: &PatternChoice<T>) -> (usize, u32) {
    (choice.source_count, choice.support)
}

fn percent(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn render_repair_breakdown(breakdown: &BTreeMap<String, RepairEvalBreakdown>) -> String {
    if breakdown.is_empty() {
        return "none".to_string();
    }

    let mut rows: Vec<_> = breakdown.iter().collect();
    rows.sort_by(|(tool_a, stats_a), (tool_b, stats_b)| {
        stats_b
            .opportunities
            .cmp(&stats_a.opportunities)
            .then_with(|| stats_b.predictions.cmp(&stats_a.predictions))
            .then_with(|| tool_a.cmp(tool_b))
    });

    rows.into_iter()
        .map(|(tool, stats)| {
            format!(
                "{}: cov {}/{}, step {}/{}, exact {}/{}",
                tool,
                stats.predictions,
                stats.opportunities,
                stats.first_step_hits,
                stats.predictions,
                stats.exact_hits,
                stats.predictions,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_file_guidance_breakdown(
    breakdown: &BTreeMap<String, FileGuidanceEvalBreakdown>,
) -> String {
    if breakdown.is_empty() {
        return "none".to_string();
    }

    let mut rows: Vec<_> = breakdown.iter().collect();
    rows.sort_by(|(target_a, stats_a), (target_b, stats_b)| {
        stats_b
            .edit_points
            .cmp(&stats_a.edit_points)
            .then_with(|| stats_b.predictions.cmp(&stats_a.predictions))
            .then_with(|| target_a.cmp(target_b))
    });

    rows.into_iter()
        .map(|(target, stats)| {
            format!(
                "{}: hit {}/{}, gated {}/{}",
                target, stats.hits, stats.predictions, stats.gated_points, stats.edit_points,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn trim_repair_breakdown(
    breakdown: &BTreeMap<String, RepairEvalBreakdown>,
    top_breakdowns: usize,
) -> BTreeMap<String, RepairEvalBreakdown> {
    let mut rows: Vec<_> = breakdown.iter().collect();
    rows.sort_by(|(tool_a, stats_a), (tool_b, stats_b)| {
        stats_b
            .opportunities
            .cmp(&stats_a.opportunities)
            .then_with(|| stats_b.predictions.cmp(&stats_a.predictions))
            .then_with(|| tool_a.cmp(tool_b))
    });

    rows.into_iter()
        .take(top_breakdowns)
        .map(|(tool, stats)| (tool.clone(), stats.clone()))
        .collect()
}

fn trim_file_guidance_breakdown(
    breakdown: &BTreeMap<String, FileGuidanceEvalBreakdown>,
    top_breakdowns: usize,
) -> BTreeMap<String, FileGuidanceEvalBreakdown> {
    let mut rows: Vec<_> = breakdown.iter().collect();
    rows.sort_by(|(target_a, stats_a), (target_b, stats_b)| {
        stats_b
            .edit_points
            .cmp(&stats_a.edit_points)
            .then_with(|| stats_b.predictions.cmp(&stats_a.predictions))
            .then_with(|| target_a.cmp(target_b))
    });

    rows.into_iter()
        .take(top_breakdowns)
        .map(|(target, stats)| (target.clone(), stats.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::identity::NodeIdentity;

    fn make_trace(
        identity: &NodeIdentity,
        capability: &str,
        outcome: Outcome,
        context: &str,
        session_id: &str,
        timestamp: u64,
    ) -> Trace {
        let mut trace = Trace::new(
            capability.to_string(),
            outcome,
            10,
            1,
            simhash(context),
            Some(context.to_string()),
            Some(session_id.to_string()),
            "test-agent".to_string(),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        trace.timestamp = timestamp;
        trace
    }

    #[test]
    fn holdout_eval_scores_repair_and_file_guidance() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();

        let sessions = ["s1", "s2", "s3"];
        let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
        for session in sessions {
            for (capability, outcome, context) in [
                (
                    "claude-code/Read",
                    Outcome::Succeeded,
                    "read file: helper.rs",
                ),
                ("claude-code/Edit", Outcome::Succeeded, "edit file: main.rs"),
                (
                    "claude-code/Edit",
                    Outcome::Succeeded,
                    "edit file: helper.rs",
                ),
                ("claude-code/Bash", Outcome::Failed, "bash: cargo test"),
                (
                    "claude-code/Read",
                    Outcome::Succeeded,
                    "read file: Cargo.toml",
                ),
                ("claude-code/Bash", Outcome::Succeeded, "bash: cargo test"),
            ] {
                let trace = make_trace(&identity, capability, outcome, context, session, timestamp);
                store.insert(&trace).unwrap();
                timestamp += 1_000;
            }
            timestamp += 60_000;
        }

        let summary = evaluate_signal_quality(&store, 168, 10, None)
            .unwrap()
            .expect("expected evaluation summary");

        assert_eq!(summary.sessions_considered, 3);
        assert_eq!(summary.sessions_scored, 2);
        assert!(summary.repair_predictions >= 1);
        assert!(summary.repair_first_step_hits >= 1);
        assert!(summary.preparation_predictions >= 1);
        assert!(summary.preparation_hits >= 1);
        assert!(summary.adjacency_predictions >= 1);
        assert!(summary.adjacency_hits >= 1);
        assert!(summary.repair_breakdown.contains_key("Bash"));
        assert!(summary.preparation_breakdown.contains_key("main.rs"));
        assert!(summary.render().contains("repair first-step precision"));
        assert!(summary.render().contains("repair breakdown:"));
    }

    #[test]
    fn returns_none_without_enough_sessions() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let trace = make_trace(
            &identity,
            "claude-code/Edit",
            Outcome::Succeeded,
            "edit file: main.rs",
            "solo",
            chrono::Utc::now().timestamp_millis() as u64,
        );
        store.insert(&trace).unwrap();

        assert!(evaluate_signal_quality(&store, 168, 10, None).unwrap().is_none());
    }

    #[test]
    fn tracks_file_guidance_gate_blocks() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;

        for session in ["s1", "s2"] {
            let trace = make_trace(
                &identity,
                "claude-code/Edit",
                Outcome::Succeeded,
                "edit file: main.rs",
                session,
                timestamp,
            );
            store.insert(&trace).unwrap();
            timestamp += 60_000;
        }

        let summary = evaluate_signal_quality(&store, 168, 10, None)
            .unwrap()
            .expect("expected gate summary");

        assert_eq!(summary.sessions_scored, 1);
        assert_eq!(summary.preparation_gated_edit_points, 1);
        assert_eq!(summary.adjacency_gated_edit_points, 1);
        assert!(summary.diagnosis().contains("local repetition gate"));
    }

    #[test]
    fn focused_summary_trims_and_filters_breakdowns() {
        let summary = SignalEvalSummary {
            project_scope: None,
            sessions_considered: 3,
            sessions_scored: 2,
            edit_points: 10,
            edit_points_with_signal: 2,
            repair_opportunities: 2,
            repair_predictions: 1,
            repair_first_step_hits: 1,
            repair_exact_hits: 0,
            preparation_gated_edit_points: 4,
            preparation_predictions: 1,
            preparation_hits: 0,
            adjacency_gated_edit_points: 4,
            adjacency_predictions: 1,
            adjacency_hits: 0,
            repair_breakdown: BTreeMap::from([
                (
                    "Bash".to_string(),
                    RepairEvalBreakdown {
                        opportunities: 3,
                        predictions: 1,
                        first_step_hits: 1,
                        exact_hits: 0,
                    },
                ),
                (
                    "TaskUpdate".to_string(),
                    RepairEvalBreakdown {
                        opportunities: 1,
                        predictions: 0,
                        first_step_hits: 0,
                        exact_hits: 0,
                    },
                ),
            ]),
            preparation_breakdown: BTreeMap::from([
                (
                    "main.rs".to_string(),
                    FileGuidanceEvalBreakdown {
                        edit_points: 8,
                        gated_points: 6,
                        predictions: 1,
                        hits: 0,
                    },
                ),
                (
                    "lib.rs".to_string(),
                    FileGuidanceEvalBreakdown {
                        edit_points: 2,
                        gated_points: 2,
                        predictions: 0,
                        hits: 0,
                    },
                ),
            ]),
            adjacency_breakdown: BTreeMap::from([
                (
                    "helper.rs".to_string(),
                    FileGuidanceEvalBreakdown {
                        edit_points: 5,
                        gated_points: 2,
                        predictions: 1,
                        hits: 0,
                    },
                ),
                (
                    "mod.rs".to_string(),
                    FileGuidanceEvalBreakdown {
                        edit_points: 1,
                        gated_points: 1,
                        predictions: 0,
                        hits: 0,
                    },
                ),
            ]),
        };

        let repair_only = summary.clone().focused(EvalFocus::Repair, 1);
        assert_eq!(repair_only.repair_breakdown.len(), 1);
        assert!(repair_only.repair_breakdown.contains_key("Bash"));
        assert!(repair_only.preparation_breakdown.is_empty());
        assert!(repair_only.adjacency_breakdown.is_empty());

        let all_top_one = summary.focused(EvalFocus::All, 1);
        assert_eq!(all_top_one.repair_breakdown.len(), 1);
        assert_eq!(all_top_one.preparation_breakdown.len(), 1);
        assert_eq!(all_top_one.adjacency_breakdown.len(), 1);
    }

    #[test]
    fn project_scope_filters_sessions() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let root_a = std::env::temp_dir().join("eval-root-a");
        let root_b = std::env::temp_dir().join("eval-root-b");
        let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;

        for (session, root) in [("a1", &root_a), ("a2", &root_a), ("b1", &root_b)] {
            let helper = root.join("helper.rs");
            let main = root.join("main.rs");
            for (capability, outcome, context) in [
                (
                    "claude-code/Read",
                    Outcome::Succeeded,
                    format!("read file: {}", helper.display()),
                ),
                (
                    "claude-code/Edit",
                    Outcome::Succeeded,
                    format!("edit file: {}", main.display()),
                ),
                (
                    "claude-code/Edit",
                    Outcome::Succeeded,
                    format!("edit file: {}", helper.display()),
                ),
            ] {
                let trace = make_trace(&identity, capability, outcome, &context, session, timestamp);
                store.insert(&trace).unwrap();
                timestamp += 1_000;
            }
            timestamp += 60_000;
        }

        let scoped = evaluate_signal_quality(&store, 168, 10, Some(&root_a))
            .unwrap()
            .expect("expected scoped summary");

        assert_eq!(
            scoped.project_scope.as_deref(),
            Some(root_a.to_string_lossy().as_ref())
        );
        assert_eq!(scoped.sessions_considered, 2);
        assert_eq!(scoped.sessions_scored, 1);
    }
}
