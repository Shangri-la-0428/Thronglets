use std::collections::BTreeMap;

const PREHOOK_PROFILE_PREFIX: &str = "[thronglets:prehook] ";

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

#[derive(Debug, Clone, PartialEq)]
pub struct PrehookProfileSummary {
    pub samples: usize,
    pub avg_total_us: f64,
    pub p95_total_us: u128,
    pub avg_stdout_bytes: f64,
    pub p95_stdout_bytes: usize,
    pub avg_collective_queries_used: f64,
    pub tools: BTreeMap<String, usize>,
    pub output_modes: BTreeMap<String, usize>,
    pub decision_paths: BTreeMap<String, usize>,
    pub evidence_scopes: BTreeMap<String, usize>,
    pub file_guidance_gates: BTreeMap<String, usize>,
    pub collective_query_paths: BTreeMap<String, usize>,
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
    let mut decision_paths = BTreeMap::new();
    let mut evidence_scopes = BTreeMap::new();
    let mut file_guidance_gates = BTreeMap::new();
    let mut collective_query_paths = BTreeMap::new();

    for sample in samples {
        let decision_path = sample.decision_path.clone();
        *tools.entry(sample.tool).or_insert(0) += 1;
        *output_modes.entry(sample.output_mode).or_insert(0) += 1;
        *decision_paths.entry(decision_path.clone()).or_insert(0) += 1;
        *evidence_scopes.entry(sample.evidence_scope).or_insert(0) += 1;
        *file_guidance_gates.entry(sample.file_guidance_gate).or_insert(0) += 1;
        if sample.collective_queries_used > 0 {
            *collective_query_paths.entry(decision_path).or_insert(0) += sample.collective_queries_used;
        }
    }

    Some(PrehookProfileSummary {
        samples: sample_count,
        avg_total_us: total_us_sum as f64 / sample_count as f64,
        p95_total_us: percentile_95(&total_us_values),
        avg_stdout_bytes: stdout_bytes_sum as f64 / sample_count as f64,
        p95_stdout_bytes: percentile_95(&stdout_byte_values),
        avg_collective_queries_used: collective_sum as f64 / sample_count as f64,
        tools,
        output_modes,
        decision_paths,
        evidence_scopes,
        file_guidance_gates,
        collective_query_paths,
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
            format!("tools: {}", render_counts(&self.tools)),
            format!("output modes: {}", render_counts(&self.output_modes)),
            format!("decision paths: {}", render_counts(&self.decision_paths)),
            format!("evidence scopes: {}", render_counts(&self.evidence_scopes)),
            format!("file guidance gates: {}", render_counts(&self.file_guidance_gates)),
            format!(
                "collective query paths: {}",
                render_counts_or_none(&self.collective_query_paths)
            ),
        ]
        .join("\n")
    }
}

fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(label, count)| format!("{label}={count}"))
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
        assert_eq!(summary.tools["Edit"], 2);
        assert_eq!(summary.output_modes["silent"], 1);
        assert_eq!(summary.decision_paths["repair"], 1);
        assert_eq!(summary.evidence_scopes["none"], 2);
        assert_eq!(summary.file_guidance_gates["open"], 1);
        assert_eq!(summary.file_guidance_gates["closed"], 1);
        assert_eq!(summary.collective_query_paths["repair"], 1);
    }
}
