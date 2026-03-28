use crate::contracts::PREHOOK_MAX_HINTS;
use serde::Serialize;
use std::collections::BTreeMap;

const PREHOOK_PROFILE_PREFIX: &str = "[thronglets:prehook] ";
const DEFAULT_MIN_PROFILE_SAMPLES: usize = 10;
const DEFAULT_MAX_AVG_STDOUT_BYTES: f64 = 80.0;
const DEFAULT_MAX_P95_STDOUT_BYTES: usize = 160;
const DEFAULT_MAX_AVG_COLLECTIVE_QUERIES: f64 = 0.25;
const DEFAULT_MAX_HINT_SATURATION_PERCENT: f64 = 20.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrehookProfileSample {
    pub tool: String,
    pub emitted: usize,
    pub stdout_bytes: usize,
    pub output_mode: String,
    pub decision_path: String,
    pub evidence_scope: String,
    pub file_guidance_gate: String,
    pub collective_queries_used: usize,
    pub total_us: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrehookProfileSummary {
    pub samples: usize,
    pub avg_total_us: f64,
    pub p95_total_us: u128,
    pub avg_stdout_bytes: f64,
    pub p95_stdout_bytes: usize,
    pub avg_collective_queries_used: f64,
    pub emitted_counts: BTreeMap<usize, usize>,
    pub saturated_samples: usize,
    pub tools: BTreeMap<String, usize>,
    pub output_modes: BTreeMap<String, usize>,
    pub decision_paths: BTreeMap<String, usize>,
    pub evidence_scopes: BTreeMap<String, usize>,
    pub file_guidance_gates: BTreeMap<String, usize>,
    pub collective_query_paths: BTreeMap<String, usize>,
    pub decision_path_costs: BTreeMap<String, DecisionPathCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionPathCost {
    pub samples: usize,
    pub total_stdout_bytes: usize,
    pub total_us: u128,
    pub collective_queries_used: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileCheckThresholds {
    pub min_samples: usize,
    pub max_avg_stdout_bytes: f64,
    pub max_p95_stdout_bytes: usize,
    pub max_avg_collective_queries: f64,
    pub max_hint_saturation_percent: f64,
}

impl Default for ProfileCheckThresholds {
    fn default() -> Self {
        Self {
            min_samples: DEFAULT_MIN_PROFILE_SAMPLES,
            max_avg_stdout_bytes: DEFAULT_MAX_AVG_STDOUT_BYTES,
            max_p95_stdout_bytes: DEFAULT_MAX_P95_STDOUT_BYTES,
            max_avg_collective_queries: DEFAULT_MAX_AVG_COLLECTIVE_QUERIES,
            max_hint_saturation_percent: DEFAULT_MAX_HINT_SATURATION_PERCENT,
        }
    }
}

pub fn parse_prehook_profile_line(line: &str) -> Option<PrehookProfileSample> {
    let body = line.strip_prefix(PREHOOK_PROFILE_PREFIX)?;

    let mut tool = None;
    let mut emitted = None;
    let mut stdout_bytes = None;
    let mut output_mode = None;
    let mut decision_path = None;
    let mut evidence_scope = None;
    let mut file_guidance_gate = None;
    let mut collective_queries_used = None;
    let mut total_us = None;

    for field in body.split_whitespace() {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "tool" => tool = Some(value.to_string()),
            "emitted" => emitted = value.parse().ok(),
            "stdout_bytes" => stdout_bytes = value.parse().ok(),
            "output_mode" => output_mode = Some(value.to_string()),
            "decision_path" => decision_path = Some(value.to_string()),
            "evidence_scope" => evidence_scope = Some(value.to_string()),
            "file_guidance_gate" => file_guidance_gate = Some(value.to_string()),
            "collective_queries_used" => collective_queries_used = value.parse().ok(),
            "total_us" => total_us = value.parse().ok(),
            _ => {}
        }
    }

    Some(PrehookProfileSample {
        tool: tool?,
        emitted: emitted?,
        stdout_bytes: stdout_bytes?,
        output_mode: output_mode?,
        decision_path: decision_path?,
        evidence_scope: evidence_scope?,
        file_guidance_gate: file_guidance_gate?,
        collective_queries_used: collective_queries_used?,
        total_us: total_us?,
    })
}

pub fn summarize_prehook_profiles(input: &str) -> Option<PrehookProfileSummary> {
    let samples: Vec<_> = input
        .lines()
        .filter_map(parse_prehook_profile_line)
        .collect();
    if samples.is_empty() {
        return None;
    }

    let sample_count = samples.len();
    let total_us_sum: u128 = samples.iter().map(|sample| sample.total_us).sum();
    let stdout_bytes_sum: usize = samples.iter().map(|sample| sample.stdout_bytes).sum();
    let collective_sum: usize = samples
        .iter()
        .map(|sample| sample.collective_queries_used)
        .sum();

    let mut total_us_values: Vec<_> = samples.iter().map(|sample| sample.total_us).collect();
    total_us_values.sort_unstable();

    let mut stdout_byte_values: Vec<_> = samples.iter().map(|sample| sample.stdout_bytes).collect();
    stdout_byte_values.sort_unstable();

    let mut tools = BTreeMap::new();
    let mut output_modes = BTreeMap::new();
    let mut emitted_counts = BTreeMap::new();
    let mut decision_paths = BTreeMap::new();
    let mut evidence_scopes = BTreeMap::new();
    let mut file_guidance_gates = BTreeMap::new();
    let mut collective_query_paths = BTreeMap::new();
    let mut decision_path_costs: BTreeMap<String, DecisionPathCost> = BTreeMap::new();

    for sample in samples {
        let decision_path = sample.decision_path.clone();
        *tools.entry(sample.tool).or_insert(0) += 1;
        *emitted_counts.entry(sample.emitted).or_insert(0) += 1;
        *output_modes.entry(sample.output_mode).or_insert(0) += 1;
        *decision_paths.entry(decision_path.clone()).or_insert(0) += 1;
        *evidence_scopes.entry(sample.evidence_scope).or_insert(0) += 1;
        *file_guidance_gates
            .entry(sample.file_guidance_gate)
            .or_insert(0) += 1;
        let cost = decision_path_costs
            .entry(decision_path.clone())
            .or_insert(DecisionPathCost {
                samples: 0,
                total_stdout_bytes: 0,
                total_us: 0,
                collective_queries_used: 0,
            });
        cost.samples += 1;
        cost.total_stdout_bytes += sample.stdout_bytes;
        cost.total_us += sample.total_us;
        cost.collective_queries_used += sample.collective_queries_used;
        if sample.collective_queries_used > 0 {
            *collective_query_paths.entry(decision_path).or_insert(0) +=
                sample.collective_queries_used;
        }
    }

    Some(PrehookProfileSummary {
        samples: sample_count,
        avg_total_us: total_us_sum as f64 / sample_count as f64,
        p95_total_us: percentile_95(&total_us_values),
        avg_stdout_bytes: stdout_bytes_sum as f64 / sample_count as f64,
        p95_stdout_bytes: percentile_95(&stdout_byte_values),
        avg_collective_queries_used: collective_sum as f64 / sample_count as f64,
        saturated_samples: *emitted_counts.get(&PREHOOK_MAX_HINTS).unwrap_or(&0),
        emitted_counts,
        tools,
        output_modes,
        decision_paths,
        evidence_scopes,
        file_guidance_gates,
        collective_query_paths,
        decision_path_costs,
    })
}

impl PrehookProfileSummary {
    pub fn render(&self) -> String {
        [
            format!("samples: {}", self.samples),
            format!("avg total_us: {:.1}", self.avg_total_us),
            format!("p95 total_us: {}", self.p95_total_us),
            format!("avg stdout_bytes: {:.1}", self.avg_stdout_bytes),
            format!("p95 stdout_bytes: {}", self.p95_stdout_bytes),
            format!(
                "avg collective_queries_used: {:.2}",
                self.avg_collective_queries_used
            ),
            format!(
                "emitted lines: {}",
                render_emitted_counts(&self.emitted_counts)
            ),
            format!(
                "max-hint saturation: {:.1}% ({}/{})",
                self.saturated_samples as f64 / self.samples as f64 * 100.0,
                self.saturated_samples,
                self.samples,
            ),
            format!("tools: {}", render_counts(&self.tools)),
            format!("output modes: {}", render_counts(&self.output_modes)),
            format!("decision paths: {}", render_counts(&self.decision_paths)),
            format!("evidence scopes: {}", render_counts(&self.evidence_scopes)),
            format!(
                "file guidance gates: {}",
                render_counts(&self.file_guidance_gates)
            ),
            format!(
                "collective query paths: {}",
                render_counts_or_none(&self.collective_query_paths)
            ),
            format!(
                "top optimization candidate: {}",
                self.top_optimization_candidate()
            ),
            format!(
                "decision path hotspots: {}",
                render_decision_path_costs(&self.decision_path_costs)
            ),
        ]
        .join("\n")
    }

    pub fn top_optimization_candidate(&self) -> String {
        let Some((path, cost)) = sorted_decision_path_costs(&self.decision_path_costs)
            .into_iter()
            .find(|(path, cost)| {
                *path != "none" && (cost.collective_queries_used > 0 || cost.total_stdout_bytes > 0)
            })
        else {
            return "no strong hotspot yet".to_string();
        };

        if cost.collective_queries_used > 0 {
            format!(
                "reduce collective queries in {path} path (collective_queries={}, avg_stdout_bytes={:.1})",
                cost.collective_queries_used,
                avg_usize(cost.total_stdout_bytes, cost.samples),
            )
        } else {
            format!(
                "trim stdout-heavy {path} path (avg_stdout_bytes={:.1}, samples={})",
                avg_usize(cost.total_stdout_bytes, cost.samples),
                cost.samples,
            )
        }
    }

    pub fn saturation_percent(&self) -> f64 {
        self.saturated_samples as f64 / self.samples as f64 * 100.0
    }

    pub fn check(&self, thresholds: &ProfileCheckThresholds) -> Vec<String> {
        let mut failures = Vec::new();

        if self.samples < thresholds.min_samples {
            failures.push(format!(
                "need at least {} samples (got {})",
                thresholds.min_samples, self.samples
            ));
        }
        if self.avg_stdout_bytes > thresholds.max_avg_stdout_bytes {
            failures.push(format!(
                "avg_stdout_bytes {:.1} > {:.1}",
                self.avg_stdout_bytes, thresholds.max_avg_stdout_bytes
            ));
        }
        if self.p95_stdout_bytes > thresholds.max_p95_stdout_bytes {
            failures.push(format!(
                "p95_stdout_bytes {} > {}",
                self.p95_stdout_bytes, thresholds.max_p95_stdout_bytes
            ));
        }
        if self.avg_collective_queries_used > thresholds.max_avg_collective_queries {
            failures.push(format!(
                "avg_collective_queries_used {:.2} > {:.2}",
                self.avg_collective_queries_used, thresholds.max_avg_collective_queries
            ));
        }
        if self.saturation_percent() > thresholds.max_hint_saturation_percent {
            failures.push(format!(
                "max_hint_saturation {:.1}% > {:.1}%",
                self.saturation_percent(),
                thresholds.max_hint_saturation_percent
            ));
        }

        failures
    }

    pub fn render_check(&self, thresholds: &ProfileCheckThresholds) -> (bool, String) {
        let failures = self.check(thresholds);
        let mut lines = vec![
            format!("samples: {}", self.samples),
            format!(
                "avg stdout_bytes: {:.1} <= {:.1}",
                self.avg_stdout_bytes, thresholds.max_avg_stdout_bytes
            ),
            format!(
                "p95 stdout_bytes: {} <= {}",
                self.p95_stdout_bytes, thresholds.max_p95_stdout_bytes
            ),
            format!(
                "avg collective_queries_used: {:.2} <= {:.2}",
                self.avg_collective_queries_used, thresholds.max_avg_collective_queries
            ),
            format!(
                "max-hint saturation: {:.1}% <= {:.1}%",
                self.saturation_percent(),
                thresholds.max_hint_saturation_percent
            ),
        ];

        if failures.is_empty() {
            lines.insert(0, "PASS".to_string());
            (true, lines.join("\n"))
        } else {
            lines.insert(0, "FAIL".to_string());
            lines.push(format!("violations: {}", failures.join("; ")));
            lines.push(format!(
                "top optimization candidate: {}",
                self.top_optimization_candidate()
            ));
            (false, lines.join("\n"))
        }
    }
}

fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    let mut entries: Vec<_> = counts.iter().collect();
    entries.sort_by(|(label_a, count_a), (label_b, count_b)| {
        count_b.cmp(count_a).then_with(|| label_a.cmp(label_b))
    });
    entries
        .into_iter()
        .map(|(label, count)| format!("{label}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_emitted_counts(counts: &BTreeMap<usize, usize>) -> String {
    counts
        .iter()
        .map(|(emitted, count)| format!("{emitted}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_counts_or_none(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        "none".to_string()
    } else {
        render_counts(counts)
    }
}

fn render_decision_path_costs(costs: &BTreeMap<String, DecisionPathCost>) -> String {
    sorted_decision_path_costs(costs)
        .into_iter()
        .map(|(label, cost)| {
            format!(
                "{label}(samples={}, avg_stdout_bytes={:.1}, avg_total_us={:.1}, collective_queries={})",
                cost.samples,
                avg_usize(cost.total_stdout_bytes, cost.samples),
                avg_u128(cost.total_us, cost.samples),
                cost.collective_queries_used,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sorted_decision_path_costs(
    costs: &BTreeMap<String, DecisionPathCost>,
) -> Vec<(&str, &DecisionPathCost)> {
    let mut entries: Vec<_> = costs
        .iter()
        .map(|(label, cost)| (label.as_str(), cost))
        .collect();
    entries.sort_by(|(label_a, cost_a), (label_b, cost_b)| {
        cost_b
            .collective_queries_used
            .cmp(&cost_a.collective_queries_used)
            .then_with(|| {
                avg_usize(cost_b.total_stdout_bytes, cost_b.samples)
                    .total_cmp(&avg_usize(cost_a.total_stdout_bytes, cost_a.samples))
            })
            .then_with(|| {
                avg_u128(cost_b.total_us, cost_b.samples)
                    .total_cmp(&avg_u128(cost_a.total_us, cost_a.samples))
            })
            .then_with(|| cost_b.samples.cmp(&cost_a.samples))
            .then_with(|| label_a.cmp(label_b))
    });
    entries
}

fn avg_usize(total: usize, count: usize) -> f64 {
    total as f64 / count as f64
}

fn avg_u128(total: u128, count: usize) -> f64 {
    total as f64 / count as f64
}

fn percentile_95<T: Copy + Ord>(values: &[T]) -> T {
    let last_index = values.len().saturating_sub(1);
    let percentile_index = (last_index * 95).div_ceil(100);
    values[percentile_index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prehook_profile_line_extracts_core_fields() {
        let sample = parse_prehook_profile_line(
            "[thronglets:prehook] tool=Edit emitted=2 stdout_bytes=88 output_mode=next-step decision_path=repair evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=321 workspace_us=10 git=skipped",
        )
        .unwrap();

        assert_eq!(sample.tool, "Edit");
        assert_eq!(sample.emitted, 2);
        assert_eq!(sample.stdout_bytes, 88);
        assert_eq!(sample.output_mode, "next-step");
        assert_eq!(sample.decision_path, "repair");
        assert_eq!(sample.evidence_scope, "collective");
        assert_eq!(sample.file_guidance_gate, "open");
        assert_eq!(sample.collective_queries_used, 1);
        assert_eq!(sample.total_us, 321);
    }

    #[test]
    fn summarize_prehook_profiles_aggregates_counts_and_percentiles() {
        let summary = summarize_prehook_profiles(
            "[thronglets:prehook] tool=Edit emitted=2 stdout_bytes=88 output_mode=next-step decision_path=repair evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             junk\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Edit emitted=1 stdout_bytes=42 output_mode=context-only decision_path=history evidence_scope=none file_guidance_gate=closed collective_queries_used=0 total_us=200",
        )
        .unwrap();

        assert_eq!(summary.samples, 3);
        assert_eq!(summary.p95_total_us, 300);
        assert_eq!(summary.p95_stdout_bytes, 88);
        assert_eq!(summary.emitted_counts[&2], 1);
        assert_eq!(summary.saturated_samples, 0);
        assert_eq!(summary.tools["Edit"], 2);
        assert_eq!(summary.output_modes["silent"], 1);
        assert_eq!(summary.decision_paths["repair"], 1);
        assert_eq!(summary.evidence_scopes["none"], 2);
        assert_eq!(summary.file_guidance_gates["open"], 1);
        assert_eq!(summary.file_guidance_gates["closed"], 1);
        assert_eq!(summary.collective_query_paths["repair"], 1);
        assert_eq!(
            summary.decision_path_costs["repair"].collective_queries_used,
            1
        );
        assert_eq!(summary.decision_path_costs["repair"].total_stdout_bytes, 88);
        assert_eq!(
            summary.top_optimization_candidate(),
            "reduce collective queries in repair path (collective_queries=1, avg_stdout_bytes=88.0)"
        );
    }

    #[test]
    fn profile_check_passes_when_metrics_fit_thresholds() {
        let summary = summarize_prehook_profiles(
            "[thronglets:prehook] tool=Edit emitted=2 stdout_bytes=40 output_mode=next-step decision_path=repair evidence_scope=collective file_guidance_gate=open collective_queries_used=0 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=1 stdout_bytes=30 output_mode=context-only decision_path=history evidence_scope=none file_guidance_gate=closed collective_queries_used=0 total_us=200\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100\n\
             [thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100",
        )
        .unwrap();

        let (passed, rendered) = summary.render_check(&ProfileCheckThresholds::default());
        assert!(passed);
        assert!(rendered.starts_with("PASS"));
    }

    #[test]
    fn profile_check_fails_when_metrics_exceed_thresholds() {
        let summary = summarize_prehook_profiles(
            "[thronglets:prehook] tool=Edit emitted=3 stdout_bytes=200 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=180 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300\n\
             [thronglets:prehook] tool=Edit emitted=3 stdout_bytes=160 output_mode=next-step decision_path=adjacency evidence_scope=collective file_guidance_gate=open collective_queries_used=1 total_us=300",
        )
        .unwrap();

        let (passed, rendered) = summary.render_check(&ProfileCheckThresholds::default());
        assert!(!passed);
        assert!(rendered.starts_with("FAIL"));
        assert!(rendered.contains("violations:"));
        assert!(
            rendered.contains(
                "top optimization candidate: reduce collective queries in adjacency path"
            )
        );
    }
}
