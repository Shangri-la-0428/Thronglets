//! Workspace state — persistent context across AI sessions.
//!
//! Maintains a lightweight JSON file that tracks what the AI was doing:
//! recent files, recent errors, current project context. This lets the
//! next session pick up where the last one left off without the AI
//! needing to re-discover everything.

use crate::signals::{StepAction, StepCandidate};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
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

#[derive(Debug, Clone)]
struct RepairTrajectoryPattern {
    weighted_support: f64,
    count: u32,
    last_seen_ms: i64,
    steps: Vec<StepAction>,
    source_ids: Vec<String>,
}

/// AI-facing repair hint with an explicit ranking score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairHint {
    pub body: String,
    pub score: i32,
    pub candidate: StepCandidate,
}

/// AI-facing adjacency hint with an explicit ranking score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjacencyHint {
    pub body: String,
    pub score: i32,
    pub candidate: StepCandidate,
}

/// AI-facing preparation hint with an explicit ranking score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparationHint {
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
    /// Last update timestamp.
    pub updated_ms: i64,
}

impl WorkspaceState {
    fn repair_recency_weight(age_ms: i64) -> f64 {
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

    fn repair_confidence(weighted_support: f64, count: u32) -> Option<(&'static str, i32)> {
        if weighted_support >= 2.2 || count >= 3 {
            Some(("high", 290))
        } else if weighted_support >= 1.2 || count >= 2 {
            Some(("medium", 270))
        } else {
            None
        }
    }

    fn adjacency_confidence(weighted_support: f64, count: u32) -> Option<(&'static str, i32)> {
        if weighted_support >= 2.2 || count >= 3 {
            Some(("high", 240))
        } else if weighted_support >= 1.2 || count >= 2 {
            Some(("medium", 220))
        } else {
            None
        }
    }

    fn preparation_confidence(weighted_support: f64, count: u32) -> Option<(&'static str, i32)> {
        if weighted_support >= 2.2 || count >= 3 {
            Some(("high", 260))
        } else if weighted_support >= 1.2 || count >= 2 {
            Some(("medium", 230))
        } else {
            None
        }
    }

    /// Load workspace state from disk. Returns default if file doesn't exist or is corrupt.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save workspace state to disk. Silently ignores errors.
    pub fn save(&self, data_dir: &Path) {
        let path = Self::path(data_dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("workspace.json")
    }

    fn step_action(tool: &str, file_path: Option<&str>) -> StepAction {
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

    fn push_unique_source(target: &mut Vec<String>, session_id: Option<&str>) {
        let Some(session_id) = session_id else {
            return;
        };
        if !target.iter().any(|id| id == session_id) {
            target.push(session_id.to_string());
        }
    }

    fn source_count(source_ids: &[String]) -> u32 {
        source_ids.len().max(1) as u32
    }

    fn independence_bonus(source_count: u32) -> i32 {
        source_count.saturating_sub(1).min(2) as i32 * 10
    }

    pub fn has_repeated_recent_file_actions(&self, current_file: Option<&str>) -> bool {
        let Some(file) = current_file else {
            return false;
        };
        let now = chrono::Utc::now().timestamp_millis();

        self.recent_actions
            .iter()
            .filter(|action| {
                matches!(action.tool.as_str(), "Edit" | "Write")
                    && action.file_path.as_deref() == Some(file)
                    && Self::repair_recency_weight(now - action.timestamp_ms) > 0.0
            })
            .take(2)
            .count()
            >= 2
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

    fn file_name(path: &str) -> &str {
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
    pub fn record_error(&mut self, tool: &str, context: String, error_snippet: String) {
        let now = chrono::Utc::now().timestamp_millis();
        self.recent_errors.push_front(RecentError {
            tool: tool.to_string(),
            context,
            error_snippet,
            timestamp_ms: now,
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

    /// Generate feedback hints for prehook injection.
    /// Shows retention rate and specific file feedback.
    pub fn feedback_hints(&self, current_file: Option<&str>) -> Option<String> {
        let resolved: Vec<&PendingFeedback> = self
            .pending_feedback
            .iter()
            .filter(|p| p.resolved)
            .collect();

        if resolved.is_empty() {
            return None;
        }

        let mut lines: Vec<String> = Vec::new();

        // Overall retention rate
        let committed = resolved
            .iter()
            .filter(|p| p.outcome.as_deref() == Some("committed"))
            .count();
        let reverted = resolved
            .iter()
            .filter(|p| p.outcome.as_deref() == Some("reverted"))
            .count();
        let total = committed + reverted;
        if total >= 3 {
            let rate = (committed as f64 / total as f64 * 100.0).round();
            lines.push(format!(
                "  edit retention: {rate}% ({committed}/{total} committed)"
            ));
        }

        // Specific file feedback
        if let Some(file) = current_file {
            let file_fb: Vec<_> = resolved.iter().filter(|p| p.file_path == file).collect();
            if !file_fb.is_empty() {
                let file_committed = file_fb
                    .iter()
                    .filter(|p| p.outcome.as_deref() == Some("committed"))
                    .count();
                let fname = std::path::Path::new(file)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(file);
                lines.push(format!(
                    "  {fname}: {file_committed}/{} edits committed",
                    file_fb.len()
                ));
            }
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Emit a localized retention warning only when the current file has
    /// enough poor outcomes to be decision-relevant.
    pub fn retention_warning(&self, current_file: Option<&str>) -> Option<DangerHint> {
        let file = current_file?;
        let file_feedback: Vec<&PendingFeedback> = self
            .pending_feedback
            .iter()
            .filter(|p| p.resolved && p.file_path == file)
            .filter(|p| matches!(p.outcome.as_deref(), Some("committed" | "reverted")))
            .collect();

        let total = file_feedback.len();
        if total < 2 {
            return None;
        }

        let committed = file_feedback
            .iter()
            .filter(|p| p.outcome.as_deref() == Some("committed"))
            .count();
        let rate = committed as f64 / total as f64 * 100.0;
        if rate >= 50.0 {
            return None;
        }

        let fname = std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file);

        Some(DangerHint {
            body: format!("  ⚠ low retention for {fname}: {committed}/{total} edits committed"),
            score: 340,
        })
    }

    /// Infer current strategy from recent tool call sequence.
    /// Returns a short label like "analyze-modify", "debug-cycle", "explore".
    pub fn infer_strategy(&self) -> Option<String> {
        if self.recent_actions.len() < 3 {
            return None;
        }

        // Look at last 8 actions (most recent first)
        let recent: Vec<&str> = self
            .recent_actions
            .iter()
            .take(8)
            .map(|a| a.tool.as_str())
            .collect();

        // Pattern detection (newest first, so patterns are reversed)
        let reads = recent.iter().filter(|t| **t == "Read").count();
        let edits = recent
            .iter()
            .filter(|t| **t == "Edit" || **t == "Write")
            .count();
        let bashes = recent.iter().filter(|t| **t == "Bash").count();
        let greps = recent
            .iter()
            .filter(|t| **t == "Grep" || **t == "Glob")
            .count();
        let agents = recent.iter().filter(|t| **t == "Agent").count();

        // Debug cycle: Bash(fail) → Read → Edit → Bash
        if bashes >= 2 && edits >= 1 {
            return Some("build-fix-cycle".to_string());
        }

        // Explore: lots of Grep/Glob/Read, no edits
        if greps >= 2 && reads >= 1 && edits == 0 {
            return Some("codebase-exploration".to_string());
        }

        // Analyze-modify: Read(s) → Edit
        if reads >= 2 && edits >= 1 && bashes == 0 {
            return Some("analyze-modify".to_string());
        }

        // Multi-file refactor: many Edits across different files
        if edits >= 3 {
            let unique_files: std::collections::HashSet<_> = self
                .recent_actions
                .iter()
                .take(8)
                .filter(|a| a.tool == "Edit" || a.tool == "Write")
                .filter_map(|a| a.file_path.as_deref())
                .collect();
            if unique_files.len() >= 3 {
                return Some("multi-file-refactor".to_string());
            }
        }

        // Agent delegation
        if agents >= 1 {
            return Some("delegated-research".to_string());
        }

        // Read-heavy = understanding
        if reads >= 3 && edits == 0 {
            return Some("code-review".to_string());
        }

        None
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

    /// Suggest likely repair paths after failures of the current tool.
    pub fn repair_hints(&self, current_tool: &str) -> Option<RepairHint> {
        let now = chrono::Utc::now().timestamp_millis();
        let repairs: Vec<_> = self
            .repair_patterns
            .iter()
            .filter(|p| p.error_tool == current_tool && (now - p.last_seen_ms) < 86_400_000)
            .collect();

        if repairs.is_empty() {
            return None;
        }

        let best = repairs.into_iter()
            .filter_map(|p| {
                let age_ms = now - p.last_seen_ms;
                let weighted_support = p.count as f64 * Self::repair_recency_weight(age_ms);
                let source_count = Self::source_count(&p.source_ids);
                let (confidence, score) = Self::repair_confidence(weighted_support, p.count)?;
                let step = match &p.repair_target {
                    Some(target) => format!("{} {}", p.repair_tool, target),
                    None => p.repair_tool.clone(),
                };
                let candidate = StepCandidate::single(
                    p.repair_tool.clone(),
                    p.repair_target.clone(),
                    confidence,
                    p.count,
                    source_count,
                );
                Some((
                    weighted_support + source_count as f64 * 0.1,
                    p.last_seen_ms,
                    RepairHint {
                        body: format!(
                            "  repair path after {current_tool} failure: {step} ({confidence}, {}x)",
                            p.count
                        ),
                        score: score + Self::independence_bonus(source_count),
                        candidate,
                    },
                ))
            })
            .max_by(|a, b| {
                a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            })?;

        Some(best.2)
    }

    /// Suggest a short multi-step repair trajectory after failures of the current tool.
    /// Uses only recent local actions, so it stays cheap enough for prehook.
    pub fn repair_trajectory_hint(&self, current_tool: &str) -> Option<RepairHint> {
        use std::collections::HashMap;

        if self.recent_actions.len() < 2 {
            return None;
        }

        let now = chrono::Utc::now().timestamp_millis();
        let actions: Vec<_> = self.recent_actions.iter().rev().collect(); // oldest first
        let mut patterns: HashMap<String, RepairTrajectoryPattern> = HashMap::new();

        for (i, action) in actions.iter().enumerate() {
            if action.tool != current_tool || action.outcome != "failed" {
                continue;
            }

            let mut steps: Vec<StepAction> = Vec::new();
            let mut last_ts = action.timestamp_ms;
            for next in actions.iter().skip(i + 1) {
                if next.timestamp_ms - last_ts > 600_000 {
                    break;
                }
                if next.outcome == "failed" {
                    break;
                }
                steps.push(Self::step_action(&next.tool, next.file_path.as_deref()));
                last_ts = next.timestamp_ms;
                if steps.len() == 2 {
                    break;
                }
            }

            if steps.is_empty() {
                continue;
            }

            let key = steps
                .iter()
                .map(StepAction::render)
                .collect::<Vec<_>>()
                .join(" -> ");
            let age_ms = now - action.timestamp_ms;
            let weight = Self::repair_recency_weight(age_ms);
            let entry = patterns.entry(key).or_insert(RepairTrajectoryPattern {
                weighted_support: 0.0,
                count: 0,
                last_seen_ms: action.timestamp_ms,
                steps: steps.clone(),
                source_ids: Vec::new(),
            });
            entry.weighted_support += weight;
            entry.count += 1;
            entry.last_seen_ms = entry.last_seen_ms.max(action.timestamp_ms);
            Self::push_unique_source(&mut entry.source_ids, action.session_id.as_deref());
        }

        let best = patterns.into_iter()
            .filter_map(|(trajectory, pattern)| {
                let source_count = Self::source_count(&pattern.source_ids);
                let weighted_support = pattern.weighted_support;
                let count = pattern.count;
                let (confidence, score) = Self::repair_confidence(weighted_support, count)?;
                let candidate = StepCandidate::sequence(
                    pattern.steps,
                    confidence,
                    count,
                    source_count,
                );
                Some((
                    weighted_support + source_count as f64 * 0.1,
                    pattern.last_seen_ms,
                    RepairHint {
                        body: format!(
                            "  repair trajectory after {current_tool} failure: {trajectory} ({confidence}, {count}x)",
                        ),
                        score: score + Self::independence_bonus(source_count),
                        candidate,
                    },
                ))
            })
            .max_by(|a, b| {
                a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            })?;

        Some(best.2)
    }

    /// Suggest the most likely companion file to touch alongside the current file.
    /// Suppresses low-confidence patterns to avoid wasting tokens on weak co-edit guesses.
    pub fn adjacency_hint(
        &self,
        tool_name: &str,
        current_file: Option<&str>,
    ) -> Option<AdjacencyHint> {
        use std::collections::HashMap;

        let file = current_file?;
        if !matches!(tool_name, "Edit" | "Write") {
            return None;
        }

        let now = chrono::Utc::now().timestamp_millis();
        let actions: Vec<_> = self.recent_actions.iter().collect();
        let mut co_edits: HashMap<String, (f64, u32, i64, Vec<String>)> = HashMap::new();

        for (i, action) in actions.iter().enumerate() {
            if action.file_path.as_deref() != Some(file) {
                continue;
            }
            if !matches!(action.tool.as_str(), "Edit" | "Write") {
                continue;
            }

            let age_ms = now - action.timestamp_ms;
            let weight = Self::repair_recency_weight(age_ms);
            if weight == 0.0 {
                continue;
            }

            let start = i.saturating_sub(10);
            let end = (i + 10).min(actions.len());
            let mut seen_targets: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for (j, other) in actions.iter().enumerate().take(end).skip(start) {
                if j == i {
                    continue;
                }
                if !matches!(other.tool.as_str(), "Edit" | "Write") {
                    continue;
                }
                if let Some(ref other_path) = other.file_path
                    && other_path != file
                    && (other.timestamp_ms - action.timestamp_ms).abs() < 300_000
                {
                    let short = Self::short_target(other_path);
                    if !seen_targets.insert(short.clone()) {
                        continue;
                    }
                    let entry =
                        co_edits
                            .entry(short)
                            .or_insert((0.0, 0, action.timestamp_ms, Vec::new()));
                    entry.0 += weight;
                    entry.1 += 1;
                    entry.2 = entry.2.max(action.timestamp_ms);
                    Self::push_unique_source(&mut entry.3, action.session_id.as_deref());
                }
            }
        }

        let fname = Self::short_target(file);
        let best = co_edits
            .into_iter()
            .filter_map(
                |(target, (weighted_support, count, last_seen_ms, source_ids))| {
                    let source_count = Self::source_count(&source_ids);
                    let (confidence, score) = Self::adjacency_confidence(weighted_support, count)?;
                    let candidate = StepCandidate::single(
                        "Edit",
                        Some(target.clone()),
                        confidence,
                        count,
                        source_count,
                    );
                    Some((
                        weighted_support + source_count as f64 * 0.1,
                        last_seen_ms,
                        AdjacencyHint {
                            body: format!(
                                "  companion edit for {fname}: {target} ({confidence}, {count}x)"
                            ),
                            score: score + Self::independence_bonus(source_count),
                            candidate,
                        },
                    ))
                },
            )
            .max_by(|a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            })?;

        Some(best.2)
    }

    /// Suggest what file is usually read before editing the current file.
    pub fn preparation_hint(
        &self,
        tool_name: &str,
        current_file: Option<&str>,
    ) -> Option<PreparationHint> {
        use std::collections::HashMap;

        let file = current_file?;
        if !matches!(tool_name, "Edit" | "Write") {
            return None;
        }

        let now = chrono::Utc::now().timestamp_millis();
        let actions: Vec<_> = self.recent_actions.iter().collect();
        let mut prep_reads: HashMap<String, (f64, u32, i64, Vec<String>)> = HashMap::new();

        for (i, action) in actions.iter().enumerate() {
            if action.file_path.as_deref() != Some(file) {
                continue;
            }
            if !matches!(action.tool.as_str(), "Edit" | "Write") {
                continue;
            }

            let age_ms = now - action.timestamp_ms;
            let weight = Self::repair_recency_weight(age_ms);
            if weight == 0.0 {
                continue;
            }

            let start = i + 1; // actions are newest-first, so older reads are at higher indexes
            let end = (i + 6).min(actions.len());
            let mut seen_targets: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for prev in actions.iter().take(end).skip(start) {
                if prev.tool != "Read" {
                    continue;
                }
                if action.timestamp_ms - prev.timestamp_ms > 300_000 {
                    continue;
                }
                if let Some(ref read_path) = prev.file_path {
                    if read_path == file {
                        continue;
                    }
                    let short = Self::short_target(read_path);
                    if !seen_targets.insert(short.clone()) {
                        continue;
                    }
                    let entry = prep_reads.entry(short).or_insert((
                        0.0,
                        0,
                        action.timestamp_ms,
                        Vec::new(),
                    ));
                    entry.0 += weight;
                    entry.1 += 1;
                    entry.2 = entry.2.max(action.timestamp_ms);
                    Self::push_unique_source(&mut entry.3, action.session_id.as_deref());
                }
            }
        }

        let fname = Self::short_target(file);
        let best = prep_reads.into_iter()
            .filter_map(|(target, (weighted_support, count, last_seen_ms, source_ids))| {
                let source_count = Self::source_count(&source_ids);
                let (confidence, score) = Self::preparation_confidence(weighted_support, count)?;
                let candidate = StepCandidate::single(
                    "Read",
                    Some(target.clone()),
                    confidence,
                    count,
                    source_count,
                );
                Some((
                    weighted_support + source_count as f64 * 0.1,
                    last_seen_ms,
                    PreparationHint {
                        body: format!(
                            "  read before editing {fname}: Read {target} ({confidence}, {count}x)"
                        ),
                        score: score + Self::independence_bonus(source_count),
                        candidate,
                    },
                ))
            })
            .max_by(|a, b| {
                a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            })?;

        Some(best.2)
    }

    /// Generate decision context hints for a file operation.
    /// Shows: (1) what was read before previous edits of this file, (2) files co-edited with this file.
    pub fn decision_hints(&self, tool_name: &str, current_file: Option<&str>) -> Option<String> {
        let file = current_file?;
        if !matches!(tool_name, "Edit" | "Write" | "Read") {
            return None;
        }

        let _now = chrono::Utc::now().timestamp_millis();
        let mut lines: Vec<String> = Vec::new();

        // 1. Co-edit pattern: files edited within 5 minutes of editing this file
        if matches!(tool_name, "Edit" | "Write") {
            let mut co_edits: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();

            // Find all past edits of this file in actions
            let actions: Vec<_> = self.recent_actions.iter().collect();
            for (i, action) in actions.iter().enumerate() {
                if action.file_path.as_deref() != Some(file) {
                    continue;
                }
                if !matches!(action.tool.as_str(), "Edit" | "Write") {
                    continue;
                }

                // Look within ±10 actions for other file edits
                let start = i.saturating_sub(10);
                let end = (i + 10).min(actions.len());
                for (j, other) in actions.iter().enumerate().take(end).skip(start) {
                    if j == i {
                        continue;
                    }
                    if !matches!(other.tool.as_str(), "Edit" | "Write") {
                        continue;
                    }
                    if let Some(ref other_path) = other.file_path
                        && other_path != file
                        && (other.timestamp_ms - action.timestamp_ms).abs() < 300_000
                    {
                        let short = Self::file_name(other_path);
                        *co_edits.entry(short.to_string()).or_insert(0) += 1;
                    }
                }
            }

            if !co_edits.is_empty() {
                let mut sorted: Vec<_> = co_edits.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1));
                let top: Vec<String> = sorted
                    .iter()
                    .take(3)
                    .map(|(name, count)| format!("{name} ({count}x)"))
                    .collect();
                let fname = Self::file_name(file);
                lines.push(format!("  co-edited with {fname}: {}", top.join(", ")));
            }
        }

        // 2. Preparation pattern: what was read before previous edits of this file
        if matches!(tool_name, "Edit" | "Write") {
            let actions: Vec<_> = self.recent_actions.iter().collect();
            let mut prep_files: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();

            for (i, action) in actions.iter().enumerate() {
                if action.file_path.as_deref() != Some(file) {
                    continue;
                }
                if !matches!(action.tool.as_str(), "Edit" | "Write") {
                    continue;
                }

                // Look at the 5 actions before this edit for Reads
                let start = i + 1; // actions are most-recent-first, so earlier = higher index
                let end = (i + 6).min(actions.len());
                for prev in actions.iter().take(end).skip(start) {
                    if prev.tool != "Read" {
                        continue;
                    }
                    if let Some(ref read_path) = prev.file_path
                        && read_path != file
                    {
                        let short = Self::file_name(read_path);
                        *prep_files.entry(short.to_string()).or_insert(0) += 1;
                    }
                }
            }

            if !prep_files.is_empty() {
                let mut sorted: Vec<_> = prep_files.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1));
                let top: Vec<String> = sorted
                    .iter()
                    .take(3)
                    .map(|(name, count)| format!("{name} ({count}x)"))
                    .collect();
                lines.push(format!("  prep reads before editing: {}", top.join(", ")));
            }
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Generate context hints for prehook injection.
    /// Returns None if workspace is empty or stale (>24h).
    pub fn context_hints(&self, current_tool: &str, current_file: Option<&str>) -> Option<String> {
        let now = chrono::Utc::now().timestamp_millis();
        let age_hours = (now - self.updated_ms) as f64 / 3_600_000.0;

        // Stale workspace — don't inject outdated context
        if self.updated_ms == 0 || age_hours > 24.0 {
            return None;
        }

        let mut lines: Vec<String> = Vec::new();

        // 1. If touching a file, show its recent history from workspace
        if let Some(file) = current_file {
            let file_history: Vec<&RecentFile> = self
                .recent_files
                .iter()
                .filter(|f| f.path == file)
                .take(3)
                .collect();

            if !file_history.is_empty() {
                lines.push(format!("  file history for {file}:"));
                for f in &file_history {
                    let age = Self::age_str(now, f.timestamp_ms);
                    lines.push(format!(
                        "    {age}: {action} — {ctx} [{outcome}]",
                        action = f.action,
                        ctx = f.context,
                        outcome = f.outcome
                    ));
                }
            }
        }

        // 2. Recent errors (if relevant to current tool)
        let recent_tool_errors: Vec<&RecentError> = self
            .recent_errors
            .iter()
            .filter(|e| e.tool == current_tool && (now - e.timestamp_ms) < 3_600_000) // last hour
            .take(2)
            .collect();

        if !recent_tool_errors.is_empty() {
            lines.push(format!("  recent {current_tool} errors:"));
            for e in &recent_tool_errors {
                let age = Self::age_str(now, e.timestamp_ms);
                let snippet = if e.error_snippet.len() > 120 {
                    format!("{}...", &e.error_snippet[..120])
                } else {
                    e.error_snippet.clone()
                };
                lines.push(format!("    {age}: {snippet}"));
            }
        }

        // 3. Previous session summary (if this seems like a new session)
        if let Some(prev) = self.sessions.front() {
            let session_age_h = (now - prev.last_seen_ms) as f64 / 3_600_000.0;
            // Only show if previous session ended >5min ago (likely a new session)
            if session_age_h > 0.08 && session_age_h < 24.0 {
                let caps = prev.top_capabilities.join(", ");
                lines.push(format!(
                    "  previous session ({:.0}h ago): {} tool calls, {} errors, used: {caps}",
                    session_age_h, prev.tool_count, prev.error_count
                ));
            }
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Format a relative time string.
    fn age_str(now_ms: i64, then_ms: i64) -> String {
        let diff_s = (now_ms - then_ms) / 1000;
        if diff_s < 60 {
            format!("{diff_s}s ago")
        } else if diff_s < 3600 {
            format!("{}m ago", diff_s / 60)
        } else if diff_s < 86400 {
            format!("{}h ago", diff_s / 3600)
        } else {
            format!("{}d ago", diff_s / 86400)
        }
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
            ws.record_error("Bash", format!("ctx{i}"), format!("err{i}"));
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
    fn adjacency_hint_requires_repeated_companion_edits() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
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
            timestamp_ms: now + 1_000,
        });

        assert!(ws.adjacency_hint("Edit", Some("/a.rs")).is_none());

        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/a.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 10_000,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/b.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 11_000,
        });

        let hint = ws.adjacency_hint("Edit", Some("/a.rs")).unwrap();
        assert!(
            hint.body
                .contains("companion edit for a.rs: b.rs (medium, 2x)")
        );
    }

    #[test]
    fn preparation_hint_requires_repeated_prep_reads() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some("/helper.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/main.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 1_000,
        });

        assert!(ws.preparation_hint("Edit", Some("/main.rs")).is_none());
    }

    #[test]
    fn repeated_recent_file_actions_requires_two_local_edits() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/main.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now,
        });

        assert!(!ws.has_repeated_recent_file_actions(Some("/main.rs")));

        ws.recent_actions.push_front(RecentAction {
            tool: "Write".into(),
            file_path: Some("/main.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 1_000,
        });

        assert!(ws.has_repeated_recent_file_actions(Some("/main.rs")));
    }

    #[test]
    fn preparation_hint_emits_confident_read_step() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some("/helper.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/main.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 1_000,
        });
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some("/helper.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 10_000,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/main.rs".into()),
            session_id: None,
            outcome: "succeeded".into(),
            timestamp_ms: now + 11_000,
        });

        let hint = ws.preparation_hint("Edit", Some("/main.rs")).unwrap();
        assert!(
            hint.body
                .contains("read before editing main.rs: Read helper.rs (medium, 2x)")
        );
        assert_eq!(hint.score, 230);
        assert_eq!(hint.candidate.source_count, 1);
    }

    #[test]
    fn preparation_hint_counts_independent_sources() {
        let mut ws = make_ws();
        let now = chrono::Utc::now().timestamp_millis();
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some("/helper.rs".into()),
            session_id: Some("s1".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/main.rs".into()),
            session_id: Some("s1".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + 1_000,
        });
        ws.recent_actions.push_back(RecentAction {
            tool: "Read".into(),
            file_path: Some("/helper.rs".into()),
            session_id: Some("s2".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + 10_000,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(),
            file_path: Some("/main.rs".into()),
            session_id: Some("s2".into()),
            outcome: "succeeded".into(),
            timestamp_ms: now + 11_000,
        });

        let hint = ws.preparation_hint("Edit", Some("/main.rs")).unwrap();
        assert_eq!(hint.candidate.source_count, 2);
        assert_eq!(hint.score, 240);
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
        ws.record_error("Bash", "ctx".into(), "err".into());
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
    fn load_missing_file_returns_default() {
        let ws = WorkspaceState::load(Path::new("/nonexistent/path"));
        assert_eq!(ws.recent_files.len(), 0);
    }
}
