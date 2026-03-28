//! Signal ranking for the AI-facing hot path.
//!
//! Thronglets is most useful when it tells an agent the single most
//! decision-relevant thing. This module keeps prehook output structured
//! and ranked without reintroducing heavy query work.

use std::cmp::Ordering;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalKind {
    Danger,
    Repair,
    Preparation,
    Adjacency,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub kind: SignalKind,
    pub score: i32,
    pub body: String,
    pub candidate: Option<StepCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecommendationKind {
    Avoid,
    DoNext,
    MaybeAlso,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub kind: RecommendationKind,
    pub source_kind: SignalKind,
    pub body: String,
    pub candidate: Option<StepCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepAction {
    pub tool: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCandidate {
    pub steps: Vec<StepAction>,
    pub confidence: String,
    pub support: u32,
    pub source_count: u32,
    pub evidence_scope: EvidenceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceScope {
    Local,
    Collective,
}

impl StepAction {
    pub fn new(tool: impl Into<String>, target: Option<String>) -> Self {
        Self {
            tool: tool.into(),
            target,
        }
    }

    pub fn render(&self) -> String {
        match &self.target {
            Some(target) => format!("{} {}", self.tool, target),
            None => self.tool.clone(),
        }
    }
}

impl StepCandidate {
    pub fn single(
        tool: impl Into<String>,
        target: Option<String>,
        confidence: impl Into<String>,
        support: u32,
        source_count: u32,
    ) -> Self {
        Self {
            steps: vec![StepAction::new(tool, target)],
            confidence: confidence.into(),
            support,
            source_count,
            evidence_scope: if source_count > 1 {
                EvidenceScope::Collective
            } else {
                EvidenceScope::Local
            },
        }
    }

    pub fn sequence(
        steps: Vec<StepAction>,
        confidence: impl Into<String>,
        support: u32,
        source_count: u32,
    ) -> Self {
        Self {
            steps,
            confidence: confidence.into(),
            support,
            source_count,
            evidence_scope: if source_count > 1 {
                EvidenceScope::Collective
            } else {
                EvidenceScope::Local
            },
        }
    }

    pub fn render(&self) -> String {
        let route = self
            .steps
            .iter()
            .map(StepAction::render)
            .collect::<Vec<_>>()
            .join(", then ");
        let sources = if self.source_count > 1 {
            format!(", {} sources", self.source_count)
        } else {
            String::new()
        };
        format!("{route} ({}, {}x{sources})", self.confidence, self.support)
    }

    pub fn primary_target(&self) -> Option<&str> {
        self.steps.first().and_then(|step| step.target.as_deref())
    }

    pub fn upgrade_collective_sources(&mut self, collective_sources: u32) -> i32 {
        if collective_sources <= self.source_count {
            return 0;
        }

        self.source_count = collective_sources;
        self.evidence_scope = EvidenceScope::Collective;
        ((collective_sources - 1).min(2) as i32) * 10
    }

    pub fn stability_rank(&self) -> (EvidenceScope, u32, u32, usize) {
        (
            self.evidence_scope,
            self.source_count,
            self.support,
            self.steps.len(),
        )
    }
}

impl Signal {
    pub fn danger(body: String, score: i32) -> Self {
        Self {
            kind: SignalKind::Danger,
            score,
            body,
            candidate: None,
        }
    }

    pub fn adjacency(body: String, score: i32) -> Self {
        Self {
            kind: SignalKind::Adjacency,
            score,
            body,
            candidate: None,
        }
    }

    pub fn adjacency_candidate(body: String, score: i32, candidate: StepCandidate) -> Self {
        Self {
            kind: SignalKind::Adjacency,
            score,
            body,
            candidate: Some(candidate),
        }
    }

    pub fn repair(body: String, score: i32) -> Self {
        Self {
            kind: SignalKind::Repair,
            score,
            body,
            candidate: None,
        }
    }

    pub fn repair_candidate(body: String, score: i32, candidate: StepCandidate) -> Self {
        Self {
            kind: SignalKind::Repair,
            score,
            body,
            candidate: Some(candidate),
        }
    }

    pub fn preparation(body: String, score: i32) -> Self {
        Self {
            kind: SignalKind::Preparation,
            score,
            body,
            candidate: None,
        }
    }

    pub fn preparation_candidate(body: String, score: i32, candidate: StepCandidate) -> Self {
        Self {
            kind: SignalKind::Preparation,
            score,
            body,
            candidate: Some(candidate),
        }
    }

    pub fn history(body: String) -> Self {
        Self {
            kind: SignalKind::History,
            score: 100,
            body,
            candidate: None,
        }
    }
}

pub fn rank(mut signals: Vec<Signal>, max: usize) -> Vec<Signal> {
    signals.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| {
                candidate_rank(b.candidate.as_ref()).cmp(&candidate_rank(a.candidate.as_ref()))
            })
            .then_with(|| signal_kind_rank(b.kind).cmp(&signal_kind_rank(a.kind)))
            .then_with(|| lexical_cmp(&a.body, &b.body))
    });
    signals.truncate(max);
    signals
}

pub fn select(signals: Vec<Signal>, max: usize) -> Vec<Recommendation> {
    let ranked = rank(signals, usize::MAX);
    let mut seen: HashSet<RecommendationKind> = HashSet::new();
    let mut recommendations = Vec::new();

    for signal in ranked {
        let kind = recommendation_kind(signal.kind);
        if seen.insert(kind) {
            recommendations.push(Recommendation {
                kind,
                source_kind: signal.kind,
                body: signal.body,
                candidate: signal.candidate,
            });
        }
        if recommendations.len() >= max {
            break;
        }
    }

    recommendations
}

impl Recommendation {
    pub fn render(&self) -> String {
        let label = match self.kind {
            RecommendationKind::Avoid => "avoid",
            RecommendationKind::DoNext => "do next",
            RecommendationKind::MaybeAlso => "maybe also",
            RecommendationKind::Context => "context",
        };

        let normalized = if let Some(candidate) = &self.candidate {
            candidate.render()
        } else {
            let trimmed = self.body.trim_start();
            let trimmed = if matches!(self.kind, RecommendationKind::Avoid) {
                trimmed.strip_prefix("⚠ ").unwrap_or(trimmed)
            } else {
                trimmed
            };
            normalize_recommendation_body(self.kind, trimmed)
        };
        let mut lines = normalized.lines();
        let first = lines.next().unwrap_or("");
        let mut out = format!("  {label}: {first}");
        for line in lines {
            out.push('\n');
            out.push_str(line);
        }
        out
    }
}

fn signal_kind_rank(kind: SignalKind) -> i32 {
    match kind {
        SignalKind::Danger => 3,
        SignalKind::Repair => 2,
        SignalKind::Preparation => 1,
        SignalKind::Adjacency => 0,
        SignalKind::History => -1,
    }
}

fn recommendation_kind(kind: SignalKind) -> RecommendationKind {
    match kind {
        SignalKind::Danger => RecommendationKind::Avoid,
        SignalKind::Repair | SignalKind::Preparation => RecommendationKind::DoNext,
        SignalKind::Adjacency => RecommendationKind::MaybeAlso,
        SignalKind::History => RecommendationKind::Context,
    }
}

fn candidate_rank(candidate: Option<&StepCandidate>) -> (EvidenceScope, u32, u32, usize) {
    candidate
        .map(StepCandidate::stability_rank)
        .unwrap_or((EvidenceScope::Local, 0, 0, 0))
}

fn normalize_recommendation_body(kind: RecommendationKind, body: &str) -> String {
    match kind {
        RecommendationKind::DoNext => {
            if let Some((_, rest)) = body.split_once(": ") {
                return rest.replace(" -> ", ", then ");
            }
            body.to_string()
        }
        RecommendationKind::MaybeAlso => {
            if let Some((_, rest)) = body.split_once(": ") {
                return format!("Edit {rest}");
            }
            body.to_string()
        }
        _ => body.to_string(),
    }
}

fn lexical_cmp(a: &str, b: &str) -> Ordering {
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn danger_beats_context() {
        let ranked = rank(
            vec![
                Signal::history("  git history for lib.rs:".into()),
                Signal::danger("  ⚠ recent error: build failed".into(), 320),
                Signal::adjacency("  co-edited with lib.rs: mod.rs (3x)".into(), 220),
            ],
            3,
        );

        assert_eq!(ranked[0].kind, SignalKind::Danger);
        assert_eq!(ranked[1].kind, SignalKind::Adjacency);
        assert_eq!(ranked[2].kind, SignalKind::History);
    }

    #[test]
    fn repair_beats_history() {
        let ranked = rank(
            vec![
                Signal::history("  git history for lib.rs:".into()),
                Signal::repair("  repair path after Bash failure: Read (2x)".into(), 260),
                Signal::danger("  ⚠ recent error: build failed".into(), 320),
            ],
            3,
        );

        assert_eq!(ranked[0].kind, SignalKind::Danger);
        assert_eq!(ranked[1].kind, SignalKind::Repair);
        assert_eq!(ranked[2].kind, SignalKind::History);
    }

    #[test]
    fn preparation_maps_to_do_next() {
        let selected = select(
            vec![
                Signal::preparation(
                    "  read before editing main.rs: Read helper.rs (medium, 2x)".into(),
                    230,
                ),
                Signal::history("  git history for main.rs:".into()),
            ],
            2,
        );

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].kind, RecommendationKind::DoNext);
        assert_eq!(selected[1].kind, RecommendationKind::Context);
    }

    #[test]
    fn ranking_respects_max() {
        let ranked = rank(
            vec![
                Signal::history("h".into()),
                Signal::adjacency("a".into(), 220),
                Signal::danger("d".into(), 300),
            ],
            2,
        );

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].kind, SignalKind::Danger);
        assert_eq!(ranked[1].kind, SignalKind::Adjacency);
    }

    #[test]
    fn select_keeps_one_recommendation_per_kind() {
        let selected = select(
            vec![
                Signal::danger("  ⚠ recent error".into(), 360),
                Signal::danger("  ⚠ edit retention".into(), 320),
                Signal::repair("  repair path".into(), 270),
                Signal::adjacency("  companion edit".into(), 220),
            ],
            4,
        );

        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].kind, RecommendationKind::Avoid);
        assert_eq!(selected[1].kind, RecommendationKind::DoNext);
        assert_eq!(selected[2].kind, RecommendationKind::MaybeAlso);
    }

    #[test]
    fn render_recommendation_uses_ai_facing_labels() {
        let rendered = Recommendation {
            kind: RecommendationKind::Avoid,
            source_kind: SignalKind::Danger,
            body: "  ⚠ recent error: build failed".into(),
            candidate: None,
        }
        .render();

        assert_eq!(rendered, "  avoid: recent error: build failed");
    }

    #[test]
    fn render_repair_as_direct_next_step() {
        let rendered = Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Repair,
            body: String::new(),
            candidate: Some(StepCandidate::sequence(
                vec![
                    StepAction::new("Read", Some("Cargo.toml".into())),
                    StepAction::new("Bash", None),
                ],
                "medium",
                2,
                1,
            )),
        }
        .render();

        assert_eq!(
            rendered,
            "  do next: Read Cargo.toml, then Bash (medium, 2x)"
        );
    }

    #[test]
    fn render_adjacency_as_direct_followup() {
        let rendered = Recommendation {
            kind: RecommendationKind::MaybeAlso,
            source_kind: SignalKind::Adjacency,
            body: String::new(),
            candidate: Some(StepCandidate::single(
                "Edit",
                Some("helper.rs".into()),
                "medium",
                2,
                1,
            )),
        }
        .render();

        assert_eq!(rendered, "  maybe also: Edit helper.rs (medium, 2x)");
    }

    #[test]
    fn render_candidate_includes_independent_sources_when_present() {
        let rendered = Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Preparation,
            body: String::new(),
            candidate: Some(StepCandidate::single(
                "Read",
                Some("mod.rs".into()),
                "high",
                3,
                2,
            )),
        }
        .render();

        assert_eq!(rendered, "  do next: Read mod.rs (high, 3x, 2 sources)");
    }

    #[test]
    fn upgrade_collective_sources_is_monotonic() {
        let mut candidate = StepCandidate::single("Read", Some("mod.rs".into()), "medium", 2, 1);

        assert_eq!(candidate.upgrade_collective_sources(1), 0);
        assert_eq!(candidate.source_count, 1);

        assert_eq!(candidate.upgrade_collective_sources(2), 10);
        assert_eq!(candidate.source_count, 2);

        assert_eq!(candidate.upgrade_collective_sources(4), 20);
        assert_eq!(candidate.source_count, 4);
    }

    #[test]
    fn rank_prefers_collective_candidate_when_scores_tie() {
        let ranked = rank(
            vec![
                Signal::preparation_candidate(
                    "prep a".into(),
                    230,
                    StepCandidate::single("Read", Some("a.rs".into()), "medium", 2, 1),
                ),
                Signal::preparation_candidate(
                    "prep b".into(),
                    230,
                    StepCandidate::single("Read", Some("b.rs".into()), "medium", 2, 2),
                ),
            ],
            2,
        );

        assert_eq!(ranked[0].body, "prep b");
    }

    #[test]
    fn rank_prefers_stronger_support_when_scores_tie() {
        let ranked = rank(
            vec![
                Signal::preparation_candidate(
                    "prep weak".into(),
                    230,
                    StepCandidate::single("Read", Some("a.rs".into()), "medium", 2, 1),
                ),
                Signal::preparation_candidate(
                    "prep strong".into(),
                    230,
                    StepCandidate::single("Read", Some("b.rs".into()), "medium", 3, 1),
                ),
            ],
            2,
        );

        assert_eq!(ranked[0].body, "prep strong");
    }
}
