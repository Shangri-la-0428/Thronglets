//! Workspace state — persistent context across AI sessions.
//!
//! Maintains a lightweight JSON file that tracks what the AI was doing:
//! recent files, recent errors, current project context. This lets the
//! next session pick up where the last one left off without the AI
//! needing to re-discover everything.

mod hints;

use crate::posts::DERIVED_GUIDANCE_EPOCH;
use crate::signals::{Recommendation, RecommendationKind, SignalKind, StepAction, StepCandidate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

fn default_succeeded() -> String {
    "succeeded".to_string()
}

/// Maximum number of recent file entries to keep.
const MAX_RECENT_FILES: usize = 20;
/// Maximum number of recent error entries to keep.
const MAX_RECENT_ERRORS: usize = 10;
/// Maximum number of session entries to keep.
const MAX_SESSIONS: usize = 5;
/// Maximum number of recent tool calls to keep (for decision context).
const MAX_RECENT_ACTIONS: usize = 50;
/// Maximum number of learned repair patterns.
const MAX_REPAIR_PATTERNS: usize = 20;
/// Maximum number of pending feedback items.
const MAX_PENDING_FEEDBACK: usize = 30;
/// Maximum number of recent prehook interventions to keep.
const MAX_RECENT_INTERVENTIONS: usize = 20;
/// Maximum number of pending recommendation feedback entries.
const MAX_PENDING_RECOMMENDATION_FEEDBACK: usize = 20;
/// Maximum number of resolved recommendation feedback events to keep.
const MAX_RECENT_RECOMMENDATION_FEEDBACK: usize = 40;
/// Maximum number of recent recommendation emissions to keep for same-turn dedupe.
const MAX_RECENT_RECOMMENDATION_EMISSIONS: usize = 40;
/// Dedupe repeated recommendations within the same session over a short window.
const RECOMMENDATION_DEDUPE_WINDOW_MS: i64 = 30_000;
/// Pending feedback remains meaningful only within this window.
/// 30 min covers a typical coding task unit (was 10 min, too narrow for co-edit patterns).
const RECOMMENDATION_FEEDBACK_WINDOW_MS: i64 = 1_800_000;

/// A file that was recently touched by the AI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: String,
    pub action: String,  // "read", "write", "edit", "grep"
    pub context: String, // what was done (from build_hook_context)
    pub timestamp_ms: i64,
    pub outcome: String, // "succeeded" | "failed"
}

/// An error the AI encountered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentError {
    pub tool: String,
    pub context: String,
    pub error_snippet: String, // first 300 chars of error
    pub timestamp_ms: i64,
    #[serde(default)]
    pub context_hash: Option<[u8; 16]>, // cached SimHash, avoids recomputation in prehook
    #[serde(default)]
    pub space: Option<String>,
}

/// A pending feedback item — an edit/write waiting to see if it was committed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingFeedback {
    pub file_path: String,
    pub action: String, // "Edit" | "Write"
    pub timestamp_ms: i64,
    pub resolved: bool,
    pub outcome: Option<String>, // "committed" | "reverted" | "modified"
}

/// A tool call in the action sequence (for decision context).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentAction {
    pub tool: String,
    pub file_path: Option<String>, // if the tool targets a file
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_succeeded")]
    pub outcome: String, // "succeeded" | "failed"
    pub timestamp_ms: i64,
}

/// A lightweight local repair pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairPattern {
    pub error_tool: String,
    pub repair_tool: String,
    #[serde(default)]
    pub repair_target: Option<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
    pub count: u32,
    pub last_seen_ms: i64,
}

/// A recent prehook intervention emitted by Thronglets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentIntervention {
    pub tool: String,
    pub kinds: Vec<String>,
    pub timestamp_ms: i64,
}

/// A recent recommendation emitted to a specific session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentRecommendationEmission {
    pub session_id: String,
    #[serde(default)]
    pub space: Option<String>,
    pub fingerprint: String,
    pub timestamp_ms: i64,
}

/// A pending recommendation waiting to see what the AI did next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRecommendationFeedback {
    pub session_id: String,
    #[serde(default)]
    pub space: Option<String>,
    pub trigger_tool: String,
    pub recommendation_kind: String,
    pub source_kind: String,
    pub expected_tool: Option<String>,
    pub expected_target: Option<String>,
    pub fingerprint: String,
    pub timestamp_ms: i64,
}

/// A resolved feedback event derived from what the AI actually did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationFeedbackEvent {
    pub recommendation_kind: String,
    pub source_kind: String,
    #[serde(default)]
    pub space: Option<String>,
    pub positive: bool,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DerivedGuidanceResetReport {
    pub recent_recommendation_emissions_cleared: usize,
    pub pending_recommendation_feedback_cleared: usize,
    pub recent_recommendation_feedback_cleared: usize,
    pub recent_interventions_cleared: usize,
}

impl DerivedGuidanceResetReport {
    pub fn total_cleared(&self) -> usize {
        self.recent_recommendation_emissions_cleared
            + self.pending_recommendation_feedback_cleared
            + self.recent_recommendation_feedback_cleared
            + self.recent_interventions_cleared
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SubstrateActivity {
    pub activity: String,
    pub recent_interventions_15m: u32,
    pub last_intervention_tool: Option<String>,
    pub last_intervention_kinds: Vec<String>,
    pub last_intervention_age_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpaceFeedbackSummary {
    pub positive_24h: u32,
    pub negative_24h: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpaceEmergenceSummary {
    pub active_spaces_24h: u32,
    pub global_positive_24h: u32,
    pub global_negative_24h: u32,
    pub false_signal_pressure: f64,
    pub false_consensus_spaces_24h: u32,
    pub recoverable_spaces_24h: u32,
    pub cross_space_contamination_rate: f64,
    pub space_feedback: BTreeMap<String, SpaceFeedbackSummary>,
    /// Per-source-kind breakdown (e.g. "repair", "danger", "preparation").
    /// Reveals which signal type drives the global positive/negative counts,
    /// preventing misdiagnosis when one dominant type skews the aggregate.
    pub feedback_by_source_kind: BTreeMap<String, SpaceFeedbackSummary>,
}

/// AI-facing repair hint with an explicit ranking score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairHint {
    pub body: String,
    pub score: i32,
    pub candidate: StepCandidate,
}

/// AI-facing danger hint with an explicit ranking score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DangerHint {
    pub body: String,
    pub score: i32,
}

/// A session summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub started_ms: i64,
    pub last_seen_ms: i64,
    pub tool_count: u32,
    pub error_count: u32,
    /// Top 3 capabilities used in this session
    pub top_capabilities: Vec<String>,
}

/// The workspace state file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceState {
    /// Recently touched files (most recent first).
    pub recent_files: VecDeque<RecentFile>,
    /// Recent errors (most recent first).
    pub recent_errors: VecDeque<RecentError>,
    /// Recent sessions.
    pub sessions: VecDeque<SessionSummary>,
    /// Recent tool call sequence (for decision context / co-edit patterns).
    #[serde(default)]
    pub recent_actions: VecDeque<RecentAction>,
    /// Learned local repair paths: what usually comes after a failed tool.
    #[serde(default)]
    pub repair_patterns: VecDeque<RepairPattern>,
    /// Pending feedback: edits waiting to see if they were committed.
    #[serde(default)]
    pub pending_feedback: VecDeque<PendingFeedback>,
    /// Recent visible prehook interventions.
    #[serde(default)]
    pub recent_interventions: VecDeque<RecentIntervention>,
    /// Recent emitted recommendation fingerprints for same-session dedupe.
    #[serde(default)]
    pub recent_recommendation_emissions: VecDeque<RecentRecommendationEmission>,
    /// Pending recommendation feedback waiting for the next meaningful action.
    #[serde(default)]
    pub pending_recommendation_feedback: VecDeque<PendingRecommendationFeedback>,
    /// Resolved recommendation feedback events used to bias future scores.
    #[serde(default)]
    pub recent_recommendation_feedback: VecDeque<RecommendationFeedbackEvent>,
    /// Epoch of the current derived-guidance control law.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_guidance_epoch: Option<String>,
    /// Last update timestamp.
    pub updated_ms: i64,
}

impl WorkspaceState {
    pub(crate) fn repair_recency_weight(age_ms: i64) -> f64 {
        if age_ms < 3_600_000 {
            1.0
        } else if age_ms < 21_600_000 {
            0.7
        } else if age_ms < 86_400_000 {
            0.4
        } else {
            0.0
        }
    }

    pub(crate) fn repair_confidence(
        weighted_support: f64,
        count: u32,
    ) -> Option<(&'static str, i32)> {
        if weighted_support >= 2.2 || count >= 3 {
            Some(("high", 290))
        } else if weighted_support >= 1.2 || count >= 2 {
            Some(("medium", 270))
        } else {
            None
        }
    }

    /// Load workspace state from disk. Returns default if file doesn't exist or is corrupt.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(workspace) => workspace,
                Err(error) => {
                    let backup = Self::corrupt_backup_path(&path);
                    let _ = std::fs::rename(&path, &backup);
                    tracing::warn!(
                        path = %path.display(),
                        backup = %backup.display(),
                        %error,
                        "Workspace state was corrupt; moved aside and reset"
                    );
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Failed to read workspace state");
                Self::default()
            }
        }
    }

    /// Save workspace state to disk atomically. Silently ignores errors.
    pub fn save(&self, data_dir: &Path) {
        let path = Self::path(data_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let tmp_path = Self::tmp_path(&path);
            if std::fs::write(&tmp_path, json).is_ok() {
                let _ = std::fs::rename(&tmp_path, &path);
            }
        }
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("workspace.json")
    }

    fn tmp_path(path: &Path) -> PathBuf {
        path.with_extension(format!("json.tmp.{}", std::process::id()))
    }

    fn corrupt_backup_path(path: &Path) -> PathBuf {
        path.with_extension(format!(
            "json.corrupt.{}",
            chrono::Utc::now().timestamp_millis()
        ))
    }

    pub fn ensure_derived_guidance_epoch(
        &mut self,
        epoch: &str,
    ) -> Option<DerivedGuidanceResetReport> {
        if self.derived_guidance_epoch.as_deref() == Some(epoch) {
            return None;
        }
        let report = self.reset_derived_guidance(epoch);
        Some(report)
    }

    pub fn reset_derived_guidance(&mut self, epoch: &str) -> DerivedGuidanceResetReport {
        let report = DerivedGuidanceResetReport {
            recent_recommendation_emissions_cleared: self.recent_recommendation_emissions.len(),
            pending_recommendation_feedback_cleared: self.pending_recommendation_feedback.len(),
            recent_recommendation_feedback_cleared: self.recent_recommendation_feedback.len(),
            recent_interventions_cleared: self.recent_interventions.len(),
        };
        self.recent_recommendation_emissions.clear();
        self.pending_recommendation_feedback.clear();
        self.recent_recommendation_feedback.clear();
        self.recent_interventions.clear();
        self.derived_guidance_epoch = Some(epoch.to_string());
        self.updated_ms = chrono::Utc::now().timestamp_millis();
        report
    }

    pub fn ensure_current_derived_guidance_epoch(&mut self) -> Option<DerivedGuidanceResetReport> {
        self.ensure_derived_guidance_epoch(DERIVED_GUIDANCE_EPOCH)
    }

    pub(crate) fn step_action(tool: &str, file_path: Option<&str>) -> StepAction {
        let target = match file_path {
            Some(path) if matches!(tool, "Read" | "Edit" | "Write") => {
                Some(Self::short_target(path))
            }
            _ => None,
        };
        StepAction::new(tool, target)
    }

    fn short_target(path: &str) -> String {
        std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string()
    }

    pub(crate) fn push_unique_source(target: &mut Vec<String>, session_id: Option<&str>) {
        let Some(session_id) = session_id else {
            return;
        };
        if !target.iter().any(|id| id == session_id) {
            target.push(session_id.to_string());
        }
    }

    pub(crate) fn source_count(source_ids: &[String]) -> u32 {
        source_ids.len().max(1) as u32
    }

    pub(crate) fn independence_bonus(source_count: u32) -> i32 {
        source_count.saturating_sub(1).min(2) as i32 * 10
    }

    fn file_name_matches(path: Option<&str>, target: Option<&str>) -> bool {
        match (path, target) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(path), Some(target)) => Self::file_name(path) == target,
        }
    }

    fn recommendation_fingerprint(recommendation: &Recommendation) -> String {
        let detail = recommendation
            .candidate
            .as_ref()
            .map(StepCandidate::render)
            .unwrap_or_else(|| recommendation.body.trim().to_string());
        format!(
            "{}|{}|{}",
            recommendation.kind.as_str(),
            recommendation.source_kind.as_str(),
            detail
        )
    }

    fn make_feedback_event(
        recommendation: &PendingRecommendationFeedback,
        positive: bool,
    ) -> RecommendationFeedbackEvent {
        RecommendationFeedbackEvent {
            recommendation_kind: recommendation.recommendation_kind.clone(),
            source_kind: recommendation.source_kind.clone(),
            space: recommendation.space.clone(),
            positive,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    fn push_feedback_event(&mut self, event: RecommendationFeedbackEvent) {
        self.recent_recommendation_feedback.push_front(event);
        self.recent_recommendation_feedback
            .truncate(MAX_RECENT_RECOMMENDATION_FEEDBACK);
        self.updated_ms = chrono::Utc::now().timestamp_millis();
    }

    fn recommendation_score_step(signal_kind: SignalKind) -> i32 {
        match signal_kind {
            SignalKind::Danger => 12,
            SignalKind::Repair => 14,
            SignalKind::Preparation => 12,
            SignalKind::Adjacency => 8,
            SignalKind::History => 0,
        }
    }

    pub fn recommendation_score_adjustment(
        &self,
        signal_kind: SignalKind,
        space: Option<&str>,
    ) -> i32 {
        let now = chrono::Utc::now().timestamp_millis();
        let mut positive = 0;
        let mut negative = 0;
        for event in self.recent_recommendation_feedback.iter() {
            if event.source_kind != signal_kind.as_str() {
                continue;
            }
            if event.space.as_deref() != space {
                continue;
            }
            if (now - event.timestamp_ms) > 86_400_000 {
                continue;
            }
            if event.positive {
                positive += 1;
            } else {
                negative += 1;
            }
        }

        let delta = (positive - negative).clamp(-2, 2);
        delta * Self::recommendation_score_step(signal_kind)
    }

    pub fn suppress_duplicate_recommendations(
        &self,
        session_id: Option<&str>,
        space: Option<&str>,
        recommendations: Vec<Recommendation>,
    ) -> Vec<Recommendation> {
        let Some(session_id) = session_id else {
            return recommendations;
        };
        let now = chrono::Utc::now().timestamp_millis();
        recommendations
            .into_iter()
            .filter(|recommendation| {
                let fingerprint = Self::recommendation_fingerprint(recommendation);
                !self.recent_recommendation_emissions.iter().any(|emission| {
                    emission.session_id == session_id
                        && emission.space.as_deref() == space
                        && emission.fingerprint == fingerprint
                        && (now - emission.timestamp_ms) < RECOMMENDATION_DEDUPE_WINDOW_MS
                })
            })
            .collect()
    }

    pub fn record_recommendation_emissions(
        &mut self,
        trigger_tool: &str,
        session_id: Option<&str>,
        space: Option<&str>,
        recommendations: &[Recommendation],
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        let now = chrono::Utc::now().timestamp_millis();

        for recommendation in recommendations {
            let fingerprint = Self::recommendation_fingerprint(recommendation);
            self.recent_recommendation_emissions
                .push_front(RecentRecommendationEmission {
                    session_id: session_id.to_string(),
                    space: space.map(ToOwned::to_owned),
                    fingerprint: fingerprint.clone(),
                    timestamp_ms: now,
                });
            self.recent_recommendation_emissions
                .truncate(MAX_RECENT_RECOMMENDATION_EMISSIONS);

            let (expected_tool, expected_target) = match recommendation.kind {
                RecommendationKind::DoNext | RecommendationKind::MaybeAlso => recommendation
                    .candidate
                    .as_ref()
                    .and_then(|candidate| candidate.steps.first())
                    .map(|step| (Some(step.tool.clone()), step.target.clone()))
                    .unwrap_or((None, None)),
                RecommendationKind::Avoid => (Some(trigger_tool.to_string()), None),
                RecommendationKind::Context => continue,
            };

            self.pending_recommendation_feedback
                .push_front(PendingRecommendationFeedback {
                    session_id: session_id.to_string(),
                    space: space.map(ToOwned::to_owned),
                    trigger_tool: trigger_tool.to_string(),
                    recommendation_kind: recommendation.kind.as_str().to_string(),
                    source_kind: recommendation.source_kind.as_str().to_string(),
                    expected_tool,
                    expected_target,
                    fingerprint,
                    timestamp_ms: now,
                });
        }
        self.pending_recommendation_feedback
            .truncate(MAX_PENDING_RECOMMENDATION_FEEDBACK);
        self.updated_ms = now;
    }

    pub fn resolve_recommendation_feedback(
        &mut self,
        session_id: Option<&str>,
        space: Option<&str>,
        tool: &str,
        file_path: Option<&str>,
        outcome: &str,
    ) -> Vec<RecommendationFeedbackEvent> {
        let Some(session_id) = session_id else {
            return Vec::new();
        };
        let now = chrono::Utc::now().timestamp_millis();
        let mut retained = VecDeque::new();
        let mut resolved = Vec::new();

        while let Some(recommendation) = self.pending_recommendation_feedback.pop_front() {
            if recommendation.session_id != session_id {
                retained.push_back(recommendation);
                continue;
            }
            if recommendation.space.as_deref() != space {
                retained.push_back(recommendation);
                continue;
            }
            if (now - recommendation.timestamp_ms) > RECOMMENDATION_FEEDBACK_WINDOW_MS {
                continue;
            }

            let same_trigger_tool = recommendation.trigger_tool == tool;
            match recommendation.recommendation_kind.as_str() {
                "avoid" => {
                    let positive = if same_trigger_tool {
                        outcome == "failed"
                    } else {
                        true
                    };
                    let event = Self::make_feedback_event(&recommendation, positive);
                    self.push_feedback_event(event.clone());
                    resolved.push(event);
                }
                "do_next" => {
                    if same_trigger_tool {
                        retained.push_back(recommendation);
                        continue;
                    }

                    let followed = recommendation.expected_tool.as_deref() == Some(tool)
                        && Self::file_name_matches(
                            file_path,
                            recommendation.expected_target.as_deref(),
                        );
                    let positive = if followed {
                        outcome != "failed"
                    } else {
                        outcome == "failed"
                    };
                    let event = Self::make_feedback_event(&recommendation, positive);
                    self.push_feedback_event(event.clone());
                    resolved.push(event);
                }
                "maybe_also" => {
                    if same_trigger_tool {
                        retained.push_back(recommendation);
                        continue;
                    }

                    let followed = recommendation.expected_tool.as_deref() == Some(tool)
                        && Self::file_name_matches(
                            file_path,
                            recommendation.expected_target.as_deref(),
                        );
                    if followed && outcome != "failed" {
                        let event = Self::make_feedback_event(&recommendation, true);
                        self.push_feedback_event(event.clone());
                        resolved.push(event);
                    }
                    // Not followed → discard silently (information, not advice)
                }
                _ => {}
            }
        }

        self.pending_recommendation_feedback = retained;
        resolved
    }

    fn record_repair_pattern(
        &mut self,
        error_tool: &str,
        repair_tool: &str,
        repair_target: Option<&str>,
        session_id: Option<&str>,
        now: i64,
    ) {
        let repair_target = repair_target.map(Self::short_target);
        if let Some(existing) = self.repair_patterns.iter_mut().find(|p| {
            p.error_tool == error_tool
                && p.repair_tool == repair_tool
                && p.repair_target == repair_target
        }) {
            existing.count += 1;
            existing.last_seen_ms = now;
            Self::push_unique_source(&mut existing.source_ids, session_id);
            return;
        }

        let mut source_ids = Vec::new();
        Self::push_unique_source(&mut source_ids, session_id);
        self.repair_patterns.push_front(RepairPattern {
            error_tool: error_tool.to_string(),
            repair_tool: repair_tool.to_string(),
            repair_target,
            source_ids,
            count: 1,
            last_seen_ms: now,
        });
        self.repair_patterns.truncate(MAX_REPAIR_PATTERNS);
    }

    pub(crate) fn file_name(path: &str) -> &str {
        std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
    }

    /// Record a file interaction from a PostToolUse hook.
    pub fn record_file(&mut self, path: String, action: &str, context: String, outcome: &str) {
        let now = chrono::Utc::now().timestamp_millis();

        // Deduplicate: if same file+action within last 2 seconds, update instead of adding
        if let Some(existing) = self.recent_files.front_mut()
            && existing.path == path
            && existing.action == action
            && (now - existing.timestamp_ms) < 2000
        {
            existing.timestamp_ms = now;
            existing.context = context;
            existing.outcome = outcome.to_string();
            return;
        }

        self.recent_files.push_front(RecentFile {
            path,
            action: action.to_string(),
            context,
            timestamp_ms: now,
            outcome: outcome.to_string(),
        });
        self.recent_files.truncate(MAX_RECENT_FILES);
        self.updated_ms = now;
    }

    /// Record an error from a PostToolUse hook.
    pub fn record_error(
        &mut self,
        tool: &str,
        context: String,
        error_snippet: String,
        space: Option<String>,
    ) {
        let now = chrono::Utc::now().timestamp_millis();
        let context_hash = Some(crate::context::simhash(&context));
        self.recent_errors.push_front(RecentError {
            tool: tool.to_string(),
            context,
            error_snippet,
            timestamp_ms: now,
            context_hash,
            space,
        });
        self.recent_errors.truncate(MAX_RECENT_ERRORS);
        self.updated_ms = now;
    }

    /// Update session tracking.
    pub fn track_session(&mut self, session_id: &str, capability: &str, is_error: bool) {
        let now = chrono::Utc::now().timestamp_millis();

        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|s| s.session_id == session_id)
        {
            session.last_seen_ms = now;
            session.tool_count += 1;
            if is_error {
                session.error_count += 1;
            }
            // Update top capabilities (simple frequency tracking)
            if !session.top_capabilities.contains(&capability.to_string())
                && session.top_capabilities.len() < 5
            {
                session.top_capabilities.push(capability.to_string());
            }
        } else {
            self.sessions.push_front(SessionSummary {
                session_id: session_id.to_string(),
                started_ms: now,
                last_seen_ms: now,
                tool_count: 1,
                error_count: if is_error { 1 } else { 0 },
                top_capabilities: vec![capability.to_string()],
            });
            self.sessions.truncate(MAX_SESSIONS);
        }
        self.updated_ms = now;
    }

    /// Record that prehook surfaced one or more visible signals.
    pub fn record_intervention(&mut self, tool: &str, kinds: Vec<String>) {
        if kinds.is_empty() {
            return;
        }

        let now = chrono::Utc::now().timestamp_millis();
        if let Some(existing) = self.recent_interventions.front_mut()
            && existing.tool == tool
            && existing.kinds == kinds
            && (now - existing.timestamp_ms) < 5_000
        {
            existing.timestamp_ms = now;
            self.updated_ms = now;
            return;
        }

        self.recent_interventions.push_front(RecentIntervention {
            tool: tool.to_string(),
            kinds,
            timestamp_ms: now,
        });
        self.recent_interventions.truncate(MAX_RECENT_INTERVENTIONS);
        self.updated_ms = now;
    }

    pub fn substrate_activity(&self) -> SubstrateActivity {
        let now = chrono::Utc::now().timestamp_millis();
        let recent_interventions_15m = self
            .recent_interventions
            .iter()
            .filter(|entry| (now - entry.timestamp_ms) < 900_000)
            .count() as u32;
        let has_recent_learning = self
            .recent_actions
            .iter()
            .any(|action| (now - action.timestamp_ms) < 900_000)
            || self
                .recent_errors
                .iter()
                .any(|error| (now - error.timestamp_ms) < 900_000);
        let activity = if recent_interventions_15m > 0 {
            "active"
        } else if has_recent_learning {
            "learning"
        } else {
            "quiet"
        };
        let (last_intervention_tool, last_intervention_kinds, last_intervention_age_ms) = self
            .recent_interventions
            .front()
            .map(|entry| {
                (
                    Some(entry.tool.clone()),
                    entry.kinds.clone(),
                    Some(now - entry.timestamp_ms),
                )
            })
            .unwrap_or((None, Vec::new(), None));

        SubstrateActivity {
            activity: activity.to_string(),
            recent_interventions_15m,
            last_intervention_tool,
            last_intervention_kinds,
            last_intervention_age_ms,
        }
    }

    pub fn space_feedback_summary(&self, space: Option<&str>) -> SpaceFeedbackSummary {
        let now = chrono::Utc::now().timestamp_millis();
        let mut positive_24h = 0;
        let mut negative_24h = 0;
        for event in self.recent_recommendation_feedback.iter() {
            if event.space.as_deref() != space {
                continue;
            }
            if (now - event.timestamp_ms) > 86_400_000 {
                continue;
            }
            if event.positive {
                positive_24h += 1;
            } else {
                negative_24h += 1;
            }
        }

        SpaceFeedbackSummary {
            positive_24h,
            negative_24h,
        }
    }

    pub fn emergence_summary(&self) -> SpaceEmergenceSummary {
        let now = chrono::Utc::now().timestamp_millis();
        let mut grouped_feedback: BTreeMap<String, SpaceFeedbackSummary> = BTreeMap::new();
        let mut by_source_kind: BTreeMap<String, SpaceFeedbackSummary> = BTreeMap::new();
        let mut global_positive_24h = 0;
        let mut global_negative_24h = 0;

        for event in self.recent_recommendation_feedback.iter() {
            if (now - event.timestamp_ms) > 86_400_000 {
                continue;
            }
            let space = event.space.clone().unwrap_or_else(|| "global".to_string());
            let entry = grouped_feedback
                .entry(space)
                .or_insert(SpaceFeedbackSummary {
                    positive_24h: 0,
                    negative_24h: 0,
                });
            let source_entry =
                by_source_kind
                    .entry(event.source_kind.clone())
                    .or_insert(SpaceFeedbackSummary {
                        positive_24h: 0,
                        negative_24h: 0,
                    });
            if event.positive {
                entry.positive_24h += 1;
                source_entry.positive_24h += 1;
                global_positive_24h += 1;
            } else {
                entry.negative_24h += 1;
                source_entry.negative_24h += 1;
                global_negative_24h += 1;
            }
        }

        let false_consensus_spaces_24h = grouped_feedback
            .values()
            .filter(|summary| summary.negative_24h > summary.positive_24h)
            .count() as u32;
        let recoverable_spaces_24h = grouped_feedback
            .values()
            .filter(|summary| {
                summary.positive_24h > 0 && summary.positive_24h >= summary.negative_24h
            })
            .count() as u32;

        let total_feedback = global_positive_24h + global_negative_24h;
        let false_signal_pressure = if total_feedback == 0 {
            0.0
        } else {
            f64::from(global_negative_24h) / f64::from(total_feedback)
        };

        let mut fingerprints: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for emission in self.recent_recommendation_emissions.iter() {
            if (now - emission.timestamp_ms) > 86_400_000 {
                continue;
            }
            let space = emission
                .space
                .clone()
                .unwrap_or_else(|| "global".to_string());
            fingerprints
                .entry(emission.fingerprint.clone())
                .or_default()
                .insert(space);
        }

        let total_unique_fingerprints = fingerprints.len();
        let overlapping_fingerprints = fingerprints
            .values()
            .filter(|spaces| spaces.len() > 1)
            .count();
        let cross_space_contamination_rate = if total_unique_fingerprints == 0 {
            0.0
        } else {
            overlapping_fingerprints as f64 / total_unique_fingerprints as f64
        };

        SpaceEmergenceSummary {
            active_spaces_24h: grouped_feedback.len() as u32,
            global_positive_24h,
            global_negative_24h,
            false_signal_pressure,
            false_consensus_spaces_24h,
            recoverable_spaces_24h,
            cross_space_contamination_rate,
            space_feedback: grouped_feedback,
            feedback_by_source_kind: by_source_kind,
        }
    }

    /// Add a file edit/write to the pending feedback queue.
    pub fn add_pending_feedback(&mut self, file_path: String, action: &str) {
        let now = chrono::Utc::now().timestamp_millis();

        // Don't duplicate: if same file is already pending, update timestamp
        if let Some(existing) = self
            .pending_feedback
            .iter_mut()
            .find(|p| p.file_path == file_path && !p.resolved)
        {
            existing.timestamp_ms = now;
            existing.action = action.to_string();
            return;
        }

        self.pending_feedback.push_front(PendingFeedback {
            file_path,
            action: action.to_string(),
            timestamp_ms: now,
            resolved: false,
            outcome: None,
        });
        self.pending_feedback.truncate(MAX_PENDING_FEEDBACK);
    }

    /// Resolve pending feedback by checking git status.
    /// Call this periodically (e.g., every Nth hook invocation).
    pub fn resolve_feedback(&mut self) {
        use std::collections::HashSet;
        use std::process::Command;

        let unresolved: Vec<usize> = self
            .pending_feedback
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.resolved)
            .map(|(i, _)| i)
            .collect();

        if unresolved.is_empty() {
            return;
        }

        // Find a git working directory from the first unresolved item
        let git_dir = unresolved.iter().find_map(|&i| {
            let path = std::path::Path::new(&self.pending_feedback[i].file_path);
            path.parent().filter(|d| d.exists())
        });
        let git_dir = match git_dir {
            Some(d) => d.to_path_buf(),
            None => return,
        };

        // Single `git diff --name-only` call for all uncommitted files
        let diff_output = Command::new("git")
            .args(["diff", "--name-only"])
            .current_dir(&git_dir)
            .output();
        let staged_output = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&git_dir)
            .output();

        let dirty_files: HashSet<String> = [&diff_output, &staged_output]
            .iter()
            .filter_map(|o| o.as_ref().ok())
            .flat_map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();

        let now = chrono::Utc::now().timestamp_millis();

        for &idx in &unresolved {
            let item = &self.pending_feedback[idx];
            let fp = &item.file_path;

            // Check if file path (or its basename) appears in dirty set
            let is_dirty = dirty_files.contains(fp)
                || std::path::Path::new(fp)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| dirty_files.iter().any(|d| d.ends_with(n)));

            if is_dirty {
                if (now - item.timestamp_ms) > 3_600_000 {
                    self.pending_feedback[idx].resolved = true;
                    self.pending_feedback[idx].outcome = Some("stale".to_string());
                }
                continue;
            }

            // Not dirty — check git log with a single call
            let after_ts = item.timestamp_ms / 1000;
            let log_output = Command::new("git")
                .args([
                    "log",
                    "--oneline",
                    "-1",
                    &format!("--after={after_ts}"),
                    "--",
                    fp,
                ])
                .current_dir(&git_dir)
                .output();

            let has_commit = log_output
                .as_ref()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);

            self.pending_feedback[idx].resolved = true;
            self.pending_feedback[idx].outcome = Some(if has_commit {
                "committed".to_string()
            } else {
                "reverted".to_string()
            });
        }

        // Clean up: keep at most 10 resolved
        let resolved_count = self.pending_feedback.iter().filter(|p| p.resolved).count();
        if resolved_count > 10 {
            let mut to_remove = resolved_count - 10;
            self.pending_feedback.retain(|p| {
                if p.resolved && to_remove > 0 {
                    to_remove -= 1;
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Record a tool call in the action sequence.
    pub fn record_action(
        &mut self,
        tool: &str,
        file_path: Option<String>,
        outcome: &str,
        session_id: Option<&str>,
    ) {
        let now = chrono::Utc::now().timestamp_millis();

        if outcome != "failed"
            && let Some((prev_tool, prev_outcome, prev_timestamp_ms)) = self
                .recent_actions
                .front()
                .map(|prev| (prev.tool.clone(), prev.outcome.clone(), prev.timestamp_ms))
            && prev_outcome == "failed"
            && prev_tool != tool
            && (now - prev_timestamp_ms) < 600_000
        {
            self.record_repair_pattern(&prev_tool, tool, file_path.as_deref(), session_id, now);
        }

        self.recent_actions.push_front(RecentAction {
            tool: tool.to_string(),
            file_path,
            session_id: session_id.map(str::to_string),
            outcome: outcome.to_string(),
            timestamp_ms: now,
        });
        self.recent_actions.truncate(MAX_RECENT_ACTIONS);
        self.updated_ms = now;
    }
}

/// Extract file path from tool_input if the tool operates on a file.
pub fn extract_file_path(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "Read" | "Write" | "Edit" => tool_input["file_path"].as_str().map(String::from),
        "Grep" | "Glob" => tool_input["path"].as_str().map(String::from),
        _ => None,
    }
}

/// Extract error snippet from tool_response if the tool failed.
pub fn extract_error(tool_response: &serde_json::Value) -> Option<String> {
    if let Some(err) = tool_response.get("error").and_then(|e| e.as_str()) {
        let truncated = if err.len() > 300 { &err[..300] } else { err };
        return Some(truncated.to_string());
    }
    if let Some(s) = tool_response.as_str()
        && (s.contains("error") || s.contains("Error") || s.contains("failed"))
    {
        let truncated = if s.len() > 300 { &s[..300] } else { s };
        return Some(truncated.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ws() -> WorkspaceState {
        WorkspaceState::default()
    }

    // ── record_file ──

    #[test]
    fn record_file_adds_entry() {
        let mut ws = make_ws();
        ws.record_file("/a.rs".into(), "Read", "read file".into(), "succeeded");
        assert_eq!(ws.recent_files.len(), 1);
        assert_eq!(ws.recent_files[0].path, "/a.rs");
    }

    #[test]
    fn record_file_deduplicates_within_2s() {
        let mut ws = make_ws();
        ws.record_file("/a.rs".into(), "Read", "ctx1".into(), "succeeded");
        // Same file+action immediately → should update, not add
        ws.record_file("/a.rs".into(), "Read", "ctx2".into(), "succeeded");
        assert_eq!(ws.recent_files.len(), 1);
        assert_eq!(ws.recent_files[0].context, "ctx2");
    }

    #[test]
    fn record_file_different_action_not_deduped() {
        let mut ws = make_ws();
        ws.record_file("/a.rs".into(), "Read", "ctx1".into(), "succeeded");
        ws.record_file("/a.rs".into(), "Edit", "ctx2".into(), "succeeded");
        assert_eq!(ws.recent_files.len(), 2);
    }

    #[test]
    fn record_file_truncates_at_max() {
        let mut ws = make_ws();
        for i in 0..25 {
            ws.record_file(format!("/f{i}.rs"), "Write", "ctx".into(), "succeeded");
            // Force different timestamps so dedup doesn't trigger
            if let Some(f) = ws.recent_files.front_mut() {
                f.timestamp_ms -= 5000;
            }
        }
        assert_eq!(ws.recent_files.len(), MAX_RECENT_FILES);
    }

    // ── record_error ──

    #[test]
    fn record_error_adds_and_truncates() {
        let mut ws = make_ws();
        for i in 0..15 {
            ws.record_error(
                "Bash",
                format!("ctx{i}"),
                format!("err{i}"),
                Some("test/project".into()),
            );
        }
        assert_eq!(ws.recent_errors.len(), MAX_RECENT_ERRORS);
        assert_eq!(ws.recent_errors[0].error_snippet, "err14");
    }

    // ── track_session ──

    #[test]
    fn track_session_creates_new() {
        let mut ws = make_ws();
        ws.track_session("s1", "claude-code/Bash", false);
        assert_eq!(ws.sessions.len(), 1);
        assert_eq!(ws.sessions[0].tool_count, 1);
        assert_eq!(ws.sessions[0].error_count, 0);
    }

    #[test]
    fn track_session_increments_existing() {
        let mut ws = make_ws();
        ws.track_session("s1", "claude-code/Bash", false);
        ws.track_session("s1", "claude-code/Read", true);
        assert_eq!(ws.sessions.len(), 1);
        assert_eq!(ws.sessions[0].tool_count, 2);
        assert_eq!(ws.sessions[0].error_count, 1);
        assert_eq!(ws.sessions[0].top_capabilities.len(), 2);
    }

    #[test]
    fn track_session_truncates_at_max() {
        let mut ws = make_ws();
        for i in 0..8 {
            ws.track_session(&format!("s{i}"), "cap", false);
        }
        assert_eq!(ws.sessions.len(), MAX_SESSIONS);
    }

    #[test]
    fn substrate_activity_is_active_after_intervention() {
        let mut ws = make_ws();
        ws.record_intervention("Edit", vec!["danger".into(), "repair".into()]);

        let activity = ws.substrate_activity();
        assert_eq!(activity.activity, "active");
        assert_eq!(activity.recent_interventions_15m, 1);
        assert_eq!(activity.last_intervention_tool.as_deref(), Some("Edit"));
        assert_eq!(activity.last_intervention_kinds, vec!["danger", "repair"]);
        assert!(activity.last_intervention_age_ms.is_some());
    }

    #[test]
    fn substrate_activity_is_learning_without_intervention() {
        let mut ws = make_ws();
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);

        let activity = ws.substrate_activity();
        assert_eq!(activity.activity, "learning");
        assert_eq!(activity.recent_interventions_15m, 0);
        assert!(activity.last_intervention_tool.is_none());
    }

    #[test]
    fn duplicate_recommendations_are_suppressed_within_same_session_window() {
        let mut ws = make_ws();
        let recommendations = vec![Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Preparation,
            body: String::new(),
            candidate: Some(StepCandidate::single(
                "Read",
                Some("helper.rs".into()),
                "medium",
                2,
                1,
            )),
        }];

        ws.record_recommendation_emissions("Edit", Some("s1"), Some("psyche"), &recommendations);
        let suppressed = ws.suppress_duplicate_recommendations(
            Some("s1"),
            Some("psyche"),
            recommendations.clone(),
        );
        assert!(suppressed.is_empty());

        let other_session =
            ws.suppress_duplicate_recommendations(Some("s2"), Some("psyche"), recommendations);
        assert_eq!(other_session.len(), 1);
    }

    #[test]
    fn followed_do_next_increases_preparation_adjustment() {
        let mut ws = make_ws();
        let recommendations = vec![Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Preparation,
            body: String::new(),
            candidate: Some(StepCandidate::single(
                "Read",
                Some("helper.rs".into()),
                "medium",
                2,
                1,
            )),
        }];

        ws.record_recommendation_emissions("Edit", Some("s1"), Some("psyche"), &recommendations);
        ws.resolve_recommendation_feedback(
            Some("s1"),
            Some("psyche"),
            "Edit",
            Some("/main.rs"),
            "succeeded",
        );
        assert_eq!(
            ws.recommendation_score_adjustment(SignalKind::Preparation, Some("psyche")),
            0
        );

        ws.resolve_recommendation_feedback(
            Some("s1"),
            Some("psyche"),
            "Read",
            Some("/tmp/helper.rs"),
            "succeeded",
        );
        assert!(ws.recommendation_score_adjustment(SignalKind::Preparation, Some("psyche")) > 0);
        assert_eq!(
            ws.recommendation_score_adjustment(SignalKind::Preparation, Some("other-space")),
            0
        );
    }

    #[test]
    fn ignored_avoid_that_still_fails_increases_danger_adjustment() {
        let mut ws = make_ws();
        let recommendations = vec![Recommendation {
            kind: RecommendationKind::Avoid,
            source_kind: SignalKind::Danger,
            body: "  ⚠ recent error: linker failed".into(),
            candidate: None,
        }];

        ws.record_recommendation_emissions("Bash", Some("s1"), Some("psyche"), &recommendations);
        ws.resolve_recommendation_feedback(Some("s1"), Some("psyche"), "Bash", None, "failed");

        assert!(ws.recommendation_score_adjustment(SignalKind::Danger, Some("psyche")) > 0);
        assert_eq!(
            ws.recommendation_score_adjustment(SignalKind::Danger, Some("other-space")),
            0
        );
    }

    #[test]
    fn duplicate_recommendations_are_not_suppressed_across_spaces() {
        let mut ws = make_ws();
        let recommendations = vec![Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Preparation,
            body: String::new(),
            candidate: Some(StepCandidate::single(
                "Read",
                Some("helper.rs".into()),
                "medium",
                2,
                1,
            )),
        }];

        ws.record_recommendation_emissions("Edit", Some("s1"), Some("psyche"), &recommendations);
        let still_visible =
            ws.suppress_duplicate_recommendations(Some("s1"), Some("other-space"), recommendations);
        assert_eq!(still_visible.len(), 1);
    }

    #[test]
    fn emergence_summary_detects_cross_space_contamination_and_false_signal_pressure() {
        let mut ws = make_ws();
        let recommendation = Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Preparation,
            body: String::new(),
            candidate: Some(StepCandidate::single(
                "Read",
                Some("helper.rs".into()),
                "medium",
                2,
                1,
            )),
        };

        ws.record_recommendation_emissions(
            "Edit",
            Some("s1"),
            Some("space-alpha"),
            std::slice::from_ref(&recommendation),
        );
        ws.resolve_recommendation_feedback(
            Some("s1"),
            Some("space-alpha"),
            "Read",
            Some("/tmp/helper.rs"),
            "succeeded",
        );

        ws.record_recommendation_emissions(
            "Edit",
            Some("s2"),
            Some("space-beta"),
            std::slice::from_ref(&recommendation),
        );
        ws.resolve_recommendation_feedback(
            Some("s2"),
            Some("space-beta"),
            "Bash",
            None,
            "succeeded",
        );

        let summary = ws.emergence_summary();
        assert_eq!(summary.active_spaces_24h, 2);
        assert_eq!(summary.global_positive_24h, 1);
        assert_eq!(summary.global_negative_24h, 1);
        assert!(summary.false_signal_pressure > 0.0);
        assert_eq!(summary.false_consensus_spaces_24h, 1);
        assert_eq!(summary.recoverable_spaces_24h, 1);
        assert!(summary.cross_space_contamination_rate > 0.0);
        assert_eq!(summary.space_feedback["space-alpha"].positive_24h, 1);
        assert_eq!(summary.space_feedback["space-beta"].negative_24h, 1);

        // Per-source-kind breakdown: both events come from "preparation"
        assert_eq!(summary.feedback_by_source_kind.len(), 1);
        let prep = &summary.feedback_by_source_kind["preparation"];
        assert_eq!(prep.positive_24h, 1);
        assert_eq!(prep.negative_24h, 1);
    }

    // ── record_action ──

    #[test]
    fn record_action_tracks_sequence() {
        let mut ws = make_ws();
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Edit", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Bash", None, "succeeded", None);
        assert_eq!(ws.recent_actions.len(), 3);
        assert_eq!(ws.recent_actions[0].tool, "Bash"); // most recent first
    }

    #[test]
    fn record_action_learns_repair_path_after_failure() {
        let mut ws = make_ws();
        ws.record_action("Bash", None, "failed", None);
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Bash", None, "failed", None);
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);

        let hints = ws.repair_hints("Bash").unwrap();
        assert!(hints.body.contains("repair path after Bash failure"));
        assert!(hints.body.contains("Read a.rs (medium, 2x)"));
    }

    #[test]
    fn repair_trajectory_learns_two_steps_after_failure() {
        let mut ws = make_ws();
        ws.record_action("Bash", None, "failed", None);
        ws.record_action("Read", Some("/Cargo.toml".into()), "succeeded", None);
        ws.record_action("Bash", None, "succeeded", None);
        ws.record_action("Bash", None, "failed", None);
        ws.record_action("Read", Some("/Cargo.toml".into()), "succeeded", None);
        ws.record_action("Bash", None, "succeeded", None);

        let hints = ws.repair_trajectory_hint("Bash").unwrap();
        assert!(hints.body.contains("repair trajectory after Bash failure"));
        assert!(hints.body.contains("Read Cargo.toml -> Bash (medium, 2x)"));
    }

    #[test]
    fn single_repair_example_is_suppressed() {
        let mut ws = make_ws();
        ws.record_action("Bash", None, "failed", None);
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);

        assert!(ws.repair_hints("Bash").is_none());
        assert!(ws.repair_trajectory_hint("Bash").is_none());
    }

    #[test]
    fn repair_hint_does_not_overcount_same_source() {
        let mut ws = make_ws();
        ws.record_action("Bash", None, "failed", Some("s1"));
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", Some("s1"));
        ws.record_action("Bash", None, "failed", Some("s1"));
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", Some("s1"));

        let hint = ws.repair_hints("Bash").unwrap();
        assert_eq!(hint.candidate.source_count, 1);
        assert_eq!(hint.score, 270);
    }

    // ── infer_strategy ──

    #[test]
    fn infer_strategy_too_few_actions() {
        let mut ws = make_ws();
        ws.record_action("Read", None, "succeeded", None);
        ws.record_action("Read", None, "succeeded", None);
        assert!(ws.infer_strategy().is_none());
    }

    #[test]
    fn infer_strategy_build_fix_cycle() {
        let mut ws = make_ws();
        ws.record_action("Bash", None, "succeeded", None);
        ws.record_action("Edit", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Bash", None, "succeeded", None);
        assert_eq!(ws.infer_strategy().unwrap(), "build-fix-cycle");
    }

    #[test]
    fn infer_strategy_codebase_exploration() {
        let mut ws = make_ws();
        ws.record_action("Grep", None, "succeeded", None);
        ws.record_action("Glob", None, "succeeded", None);
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);
        assert_eq!(ws.infer_strategy().unwrap(), "codebase-exploration");
    }

    #[test]
    fn infer_strategy_analyze_modify() {
        let mut ws = make_ws();
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Read", Some("/b.rs".into()), "succeeded", None);
        ws.record_action("Edit", Some("/a.rs".into()), "succeeded", None);
        assert_eq!(ws.infer_strategy().unwrap(), "analyze-modify");
    }

    #[test]
    fn infer_strategy_code_review() {
        let mut ws = make_ws();
        ws.record_action("Read", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Read", Some("/b.rs".into()), "succeeded", None);
        ws.record_action("Read", Some("/c.rs".into()), "succeeded", None);
        assert_eq!(ws.infer_strategy().unwrap(), "code-review");
    }

    #[test]
    fn infer_strategy_delegated_research() {
        let mut ws = make_ws();
        ws.record_action("Read", None, "succeeded", None);
        ws.record_action("Read", None, "succeeded", None);
        ws.record_action("Agent", None, "succeeded", None);
        assert_eq!(ws.infer_strategy().unwrap(), "delegated-research");
    }

    #[test]
    fn infer_strategy_multi_file_refactor() {
        let mut ws = make_ws();
        ws.record_action("Edit", Some("/a.rs".into()), "succeeded", None);
        ws.record_action("Edit", Some("/b.rs".into()), "succeeded", None);
        ws.record_action("Edit", Some("/c.rs".into()), "succeeded", None);
        // bashes >= 2 check fires first if we add Bash, so keep it pure edits
        assert_eq!(ws.infer_strategy().unwrap(), "multi-file-refactor");
    }

    // ── context_hints ──

    #[test]
    fn context_hints_none_when_stale() {
        let ws = make_ws(); // updated_ms = 0 → stale
        assert!(ws.context_hints("Bash", None).is_none());
    }

    #[test]
    fn context_hints_shows_file_history() {
        let mut ws = make_ws();
        ws.record_file("/a.rs".into(), "Edit", "edit main".into(), "succeeded");
        let hints = ws.context_hints("Edit", Some("/a.rs"));
        assert!(hints.is_some());
        assert!(hints.unwrap().contains("file history for /a.rs"));
    }

    #[test]
    fn context_hints_shows_errors() {
        let mut ws = make_ws();
        ws.record_error(
            "Bash",
            "compile".into(),
            "error[E0308]: mismatched types".into(),
            None,
        );
        ws.updated_ms = chrono::Utc::now().timestamp_millis();
        let hints = ws.context_hints("Bash", None);
        assert!(hints.is_some());
        assert!(hints.unwrap().contains("recent Bash errors"));
    }

    // ── decision_hints ──

    #[test]
    fn decision_hints_none_for_bash() {
        let ws = make_ws();
        assert!(ws.decision_hints("Bash", Some("/a.rs")).is_none());
    }

    #[test]
    fn decision_hints_co_edit_pattern() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
        // Simulate editing /a.rs and /b.rs together
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/a.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/b.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 1000,
        });
        let hints = ws.decision_hints("Edit", Some("/a.rs"));
        assert!(hints.is_some());
        assert!(hints.unwrap().contains("co-edited with"));
    }

    #[test]
    fn decision_hints_prep_reads() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
        // Simulate: Read /b.rs → Edit /a.rs (actions are newest-first)
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some("/b.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/a.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 2000,
        });
        let hints = ws.decision_hints("Edit", Some("/a.rs"));
        assert!(hints.is_some());
        assert!(hints.unwrap().contains("prep reads"));
    }

    // ── feedback_hints ──

    #[test]
    fn feedback_hints_none_when_empty() {
        let ws = make_ws();
        assert!(ws.feedback_hints(None).is_none());
    }

    #[test]
    fn feedback_hints_shows_retention() {
        let mut ws = make_ws();
        for i in 0..4 {
            ws.pending_feedback.push_front(PendingFeedback {
                file_path: format!("/f{i}.rs"),
                action: "Edit".into(),
                timestamp_ms: 0,
                resolved: true,
                outcome: Some(if i < 3 { "committed" } else { "reverted" }.into()),
            });
        }
        let hints = ws.feedback_hints(None).unwrap();
        assert!(hints.contains("edit retention: 75%"));
        assert!(hints.contains("3/4 committed"));
    }

    #[test]
    fn feedback_hints_shows_per_file() {
        let mut ws = make_ws();
        ws.pending_feedback.push_front(PendingFeedback {
            file_path: "/a.rs".into(),
            action: "Edit".into(),
            timestamp_ms: 0,
            resolved: true,
            outcome: Some("committed".into()),
        });
        let hints = ws.feedback_hints(Some("/a.rs")).unwrap();
        assert!(hints.contains("a.rs: 1/1 edits committed"));
    }

    #[test]
    fn retention_warning_requires_repeated_local_failures() {
        let mut ws = make_ws();
        ws.pending_feedback.push_front(PendingFeedback {
            file_path: "/a.rs".into(),
            action: "Edit".into(),
            timestamp_ms: 0,
            resolved: true,
            outcome: Some("reverted".into()),
        });

        assert!(ws.retention_warning(Some("/a.rs")).is_none());
    }

    #[test]
    fn retention_warning_is_scoped_to_current_file() {
        let mut ws = make_ws();
        for i in 0..3 {
            ws.pending_feedback.push_front(PendingFeedback {
                file_path: format!("/other{i}.rs"),
                action: "Edit".into(),
                timestamp_ms: 0,
                resolved: true,
                outcome: Some("reverted".into()),
            });
        }

        assert!(ws.retention_warning(Some("/a.rs")).is_none());
    }

    #[test]
    fn retention_warning_emits_localized_danger() {
        let mut ws = make_ws();
        for outcome in ["reverted", "reverted", "committed"] {
            ws.pending_feedback.push_front(PendingFeedback {
                file_path: "/a.rs".into(),
                action: "Edit".into(),
                timestamp_ms: 0,
                resolved: true,
                outcome: Some(outcome.into()),
            });
        }

        let hint = ws.retention_warning(Some("/a.rs")).unwrap();
        assert_eq!(hint.score, 340);
        assert!(
            hint.body
                .contains("low retention for a.rs: 1/3 edits committed")
        );
    }

    // ── add_pending_feedback ──

    #[test]
    fn add_pending_feedback_deduplicates() {
        let mut ws = make_ws();
        ws.add_pending_feedback("/a.rs".into(), "Edit");
        ws.add_pending_feedback("/a.rs".into(), "Write");
        // Same file, unresolved → should update, not add
        assert_eq!(ws.pending_feedback.len(), 1);
        assert_eq!(ws.pending_feedback[0].action, "Write");
    }

    // ── extract_file_path ──

    #[test]
    fn extract_file_path_read() {
        let input = serde_json::json!({"file_path": "/foo.rs"});
        assert_eq!(extract_file_path("Read", &input).unwrap(), "/foo.rs");
    }

    #[test]
    fn extract_file_path_grep() {
        let input = serde_json::json!({"path": "/src", "pattern": "fn main"});
        assert_eq!(extract_file_path("Grep", &input).unwrap(), "/src");
    }

    #[test]
    fn extract_file_path_unknown_tool() {
        let input = serde_json::json!({"file_path": "/foo.rs"});
        assert!(extract_file_path("Agent", &input).is_none());
    }

    // ── extract_error ──

    #[test]
    fn extract_error_from_object() {
        let resp = serde_json::json!({"error": "file not found"});
        assert_eq!(extract_error(&resp).unwrap(), "file not found");
    }

    #[test]
    fn extract_error_from_string() {
        let resp = serde_json::json!("command failed with error code 1");
        assert!(extract_error(&resp).is_some());
    }

    #[test]
    fn extract_error_no_error() {
        let resp = serde_json::json!({"result": "ok"});
        assert!(extract_error(&resp).is_none());
    }

    #[test]
    fn extract_error_truncates_long() {
        let long_err = "x".repeat(500);
        let resp = serde_json::json!({"error": long_err});
        assert_eq!(extract_error(&resp).unwrap().len(), 300);
    }

    // ── age_str ──

    #[test]
    fn age_str_formatting() {
        assert_eq!(WorkspaceState::age_str(10_000, 5_000), "5s ago");
        assert_eq!(WorkspaceState::age_str(300_000, 0), "5m ago");
        assert_eq!(WorkspaceState::age_str(7_200_000, 0), "2h ago");
        assert_eq!(WorkspaceState::age_str(172_800_000, 0), "2d ago");
    }

    // ── save / load round-trip ──

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join("thronglets_test_ws");
        let _ = std::fs::create_dir_all(&dir);

        let mut ws = make_ws();
        ws.record_file("/a.rs".into(), "Edit", "test".into(), "succeeded");
        ws.record_error("Bash", "ctx".into(), "err".into(), Some("test/proj".into()));
        ws.track_session("s1", "cap", false);
        ws.record_action("Read", Some("/b.rs".into()), "succeeded", None);
        ws.add_pending_feedback("/a.rs".into(), "Edit");
        ws.save(&dir);

        let loaded = WorkspaceState::load(&dir);
        assert_eq!(loaded.recent_files.len(), 1);
        assert_eq!(loaded.recent_errors.len(), 1);
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.recent_actions.len(), 1);
        assert_eq!(loaded.pending_feedback.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_current_derived_guidance_epoch_clears_guidance_caches_once() {
        let mut ws = make_ws();
        ws.recent_recommendation_emissions
            .push_front(RecentRecommendationEmission {
                session_id: "s1".into(),
                space: None,
                fingerprint: "fp".into(),
                timestamp_ms: 1,
            });
        ws.pending_recommendation_feedback
            .push_front(PendingRecommendationFeedback {
                session_id: "s1".into(),
                space: None,
                trigger_tool: "Edit".into(),
                recommendation_kind: "do_next".into(),
                source_kind: "history".into(),
                expected_tool: Some("Edit".into()),
                expected_target: None,
                fingerprint: "fp".into(),
                timestamp_ms: 1,
            });
        ws.recent_recommendation_feedback
            .push_front(RecommendationFeedbackEvent {
                recommendation_kind: "do_next".into(),
                source_kind: "history".into(),
                space: None,
                positive: true,
                timestamp_ms: 1,
            });
        ws.recent_interventions.push_front(RecentIntervention {
            tool: "Edit".into(),
            kinds: vec!["history".into()],
            timestamp_ms: 1,
        });

        let report = ws.ensure_current_derived_guidance_epoch().unwrap();
        assert_eq!(report.recent_recommendation_emissions_cleared, 1);
        assert_eq!(report.pending_recommendation_feedback_cleared, 1);
        assert_eq!(report.recent_recommendation_feedback_cleared, 1);
        assert_eq!(report.recent_interventions_cleared, 1);
        assert_eq!(
            ws.derived_guidance_epoch.as_deref(),
            Some(DERIVED_GUIDANCE_EPOCH)
        );
        assert!(ws.recent_recommendation_emissions.is_empty());
        assert!(ws.pending_recommendation_feedback.is_empty());
        assert!(ws.recent_recommendation_feedback.is_empty());
        assert!(ws.recent_interventions.is_empty());
        assert!(ws.ensure_current_derived_guidance_epoch().is_none());
    }

    #[test]
    fn load_missing_file_returns_default() {
        let ws = WorkspaceState::load(Path::new("/nonexistent/path"));
        assert_eq!(ws.recent_files.len(), 0);
    }

    #[test]
    fn load_corrupt_file_moves_backup_and_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = WorkspaceState::path(dir.path());
        std::fs::write(&path, "{not valid json").unwrap();

        let ws = WorkspaceState::load(dir.path());

        assert!(ws.recent_files.is_empty());
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("workspace.json.corrupt."))
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(
            !path.exists(),
            "corrupt workspace.json should be moved aside"
        );
    }

    #[test]
    fn save_cleans_up_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = make_ws();

        ws.save(dir.path());

        assert!(WorkspaceState::path(dir.path()).exists());
        assert!(
            !WorkspaceState::tmp_path(&WorkspaceState::path(dir.path())).exists(),
            "temporary workspace file should not remain after atomic save"
        );
    }
}
