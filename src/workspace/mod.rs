//! Workspace state — persistent context across AI sessions.
//!
//! Maintains a lightweight JSON file that tracks what the AI was doing:
//! recent files, recent errors, current project context. This lets the
//! next session pick up where the last one left off without the AI
//! needing to re-discover everything.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Maximum number of recent file entries to keep.
const MAX_RECENT_FILES: usize = 20;
/// Maximum number of recent error entries to keep.
const MAX_RECENT_ERRORS: usize = 10;
/// Maximum number of session entries to keep.
const MAX_SESSIONS: usize = 5;
/// Maximum number of recent tool calls to keep (for decision context).
const MAX_RECENT_ACTIONS: usize = 50;
/// Maximum number of pending feedback items.
const MAX_PENDING_FEEDBACK: usize = 30;

/// A file that was recently touched by the AI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: String,
    pub action: String,       // "read", "write", "edit", "grep"
    pub context: String,      // what was done (from build_hook_context)
    pub timestamp_ms: i64,
    pub outcome: String,      // "succeeded" | "failed"
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
    pub action: String,       // "Edit" | "Write"
    pub timestamp_ms: i64,
    pub resolved: bool,
    pub outcome: Option<String>,  // "committed" | "reverted" | "modified"
}

/// A tool call in the action sequence (for decision context).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentAction {
    pub tool: String,
    pub file_path: Option<String>,  // if the tool targets a file
    pub timestamp_ms: i64,
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
    /// Pending feedback: edits waiting to see if they were committed.
    #[serde(default)]
    pub pending_feedback: VecDeque<PendingFeedback>,
    /// Last update timestamp.
    pub updated_ms: i64,
}

impl WorkspaceState {
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

    /// Record a file interaction from a PostToolUse hook.
    pub fn record_file(&mut self, path: String, action: &str, context: String, outcome: &str) {
        let now = chrono::Utc::now().timestamp_millis();

        // Deduplicate: if same file+action within last 2 seconds, update instead of adding
        if let Some(existing) = self.recent_files.front_mut() {
            if existing.path == path && existing.action == action
                && (now - existing.timestamp_ms) < 2000
            {
                existing.timestamp_ms = now;
                existing.context = context;
                existing.outcome = outcome.to_string();
                return;
            }
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

        if let Some(session) = self.sessions.iter_mut().find(|s| s.session_id == session_id) {
            session.last_seen_ms = now;
            session.tool_count += 1;
            if is_error {
                session.error_count += 1;
            }
            // Update top capabilities (simple frequency tracking)
            if !session.top_capabilities.contains(&capability.to_string()) {
                if session.top_capabilities.len() < 5 {
                    session.top_capabilities.push(capability.to_string());
                }
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
        if let Some(existing) = self.pending_feedback.iter_mut()
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
        use std::process::Command;
        use std::collections::HashSet;

        let unresolved: Vec<usize> = self.pending_feedback.iter()
            .enumerate()
            .filter(|(_, item)| !item.resolved)
            .map(|(i, _)| i)
            .collect();

        if unresolved.is_empty() { return; }

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

        let dirty_files: HashSet<String> = [&diff_output, &staged_output].iter()
            .filter_map(|o| o.as_ref().ok())
            .flat_map(|o| String::from_utf8_lossy(&o.stdout).lines()
                .map(|l| l.trim().to_string())
                .collect::<Vec<_>>())
            .collect();

        let now = chrono::Utc::now().timestamp_millis();

        for &idx in &unresolved {
            let item = &self.pending_feedback[idx];
            let fp = &item.file_path;

            // Check if file path (or its basename) appears in dirty set
            let is_dirty = dirty_files.contains(fp)
                || std::path::Path::new(fp).file_name()
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
                .args(["log", "--oneline", "-1", &format!("--after={after_ts}"), "--", fp])
                .current_dir(&git_dir)
                .output();

            let has_commit = log_output.as_ref()
                .map(|o| !o.stdout.is_empty()).unwrap_or(false);

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
        let resolved: Vec<&PendingFeedback> = self.pending_feedback.iter()
            .filter(|p| p.resolved)
            .collect();

        if resolved.is_empty() { return None; }

        let mut lines: Vec<String> = Vec::new();

        // Overall retention rate
        let committed = resolved.iter().filter(|p| p.outcome.as_deref() == Some("committed")).count();
        let reverted = resolved.iter().filter(|p| p.outcome.as_deref() == Some("reverted")).count();
        let total = committed + reverted;
        if total >= 3 {
            let rate = (committed as f64 / total as f64 * 100.0).round();
            lines.push(format!("  edit retention: {rate}% ({committed}/{total} committed)"));
        }

        // Specific file feedback
        if let Some(file) = current_file {
            let file_fb: Vec<_> = resolved.iter()
                .filter(|p| p.file_path == file)
                .collect();
            if !file_fb.is_empty() {
                let file_committed = file_fb.iter()
                    .filter(|p| p.outcome.as_deref() == Some("committed")).count();
                let fname = std::path::Path::new(file)
                    .file_name().and_then(|n| n.to_str()).unwrap_or(file);
                lines.push(format!("  {fname}: {file_committed}/{} edits committed",
                    file_fb.len()));
            }
        }

        if lines.is_empty() { None } else { Some(lines.join("\n")) }
    }

    /// Infer current strategy from recent tool call sequence.
    /// Returns a short label like "analyze-modify", "debug-cycle", "explore".
    pub fn infer_strategy(&self) -> Option<String> {
        if self.recent_actions.len() < 3 {
            return None;
        }

        // Look at last 8 actions (most recent first)
        let recent: Vec<&str> = self.recent_actions.iter()
            .take(8)
            .map(|a| a.tool.as_str())
            .collect();

        // Pattern detection (newest first, so patterns are reversed)
        let reads = recent.iter().filter(|t| **t == "Read").count();
        let edits = recent.iter().filter(|t| **t == "Edit" || **t == "Write").count();
        let bashes = recent.iter().filter(|t| **t == "Bash").count();
        let greps = recent.iter().filter(|t| **t == "Grep" || **t == "Glob").count();
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
            let unique_files: std::collections::HashSet<_> = self.recent_actions.iter()
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
    pub fn record_action(&mut self, tool: &str, file_path: Option<String>) {
        let now = chrono::Utc::now().timestamp_millis();
        self.recent_actions.push_front(RecentAction {
            tool: tool.to_string(),
            file_path,
            timestamp_ms: now,
        });
        self.recent_actions.truncate(MAX_RECENT_ACTIONS);
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
            let mut co_edits: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

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
                for j in start..end {
                    if j == i { continue; }
                    let other = &actions[j];
                    if !matches!(other.tool.as_str(), "Edit" | "Write") { continue; }
                    if let Some(ref other_path) = other.file_path {
                        if other_path != file
                            && (other.timestamp_ms - action.timestamp_ms).abs() < 300_000 // 5 min window
                        {
                            // Use just filename for readability
                            let short = std::path::Path::new(other_path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(other_path);
                            *co_edits.entry(short.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }

            if !co_edits.is_empty() {
                let mut sorted: Vec<_> = co_edits.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1));
                let top: Vec<String> = sorted.iter().take(3)
                    .map(|(name, count)| format!("{name} ({count}x)"))
                    .collect();
                let fname = std::path::Path::new(file)
                    .file_name().and_then(|n| n.to_str()).unwrap_or(file);
                lines.push(format!("  co-edited with {fname}: {}", top.join(", ")));
            }
        }

        // 2. Preparation pattern: what was read before previous edits of this file
        if matches!(tool_name, "Edit" | "Write") {
            let actions: Vec<_> = self.recent_actions.iter().collect();
            let mut prep_files: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

            for (i, action) in actions.iter().enumerate() {
                if action.file_path.as_deref() != Some(file) { continue; }
                if !matches!(action.tool.as_str(), "Edit" | "Write") { continue; }

                // Look at the 5 actions before this edit for Reads
                let start = i + 1; // actions are most-recent-first, so earlier = higher index
                let end = (i + 6).min(actions.len());
                for j in start..end {
                    let prev = &actions[j];
                    if prev.tool != "Read" { continue; }
                    if let Some(ref read_path) = prev.file_path {
                        if read_path != file {
                            let short = std::path::Path::new(read_path)
                                .file_name().and_then(|n| n.to_str()).unwrap_or(read_path);
                            *prep_files.entry(short.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }

            if !prep_files.is_empty() {
                let mut sorted: Vec<_> = prep_files.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1));
                let top: Vec<String> = sorted.iter().take(3)
                    .map(|(name, count)| format!("{name} ({count}x)"))
                    .collect();
                lines.push(format!("  prep reads before editing: {}", top.join(", ")));
            }
        }

        if lines.is_empty() { None } else { Some(lines.join("\n")) }
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
            let file_history: Vec<&RecentFile> = self.recent_files.iter()
                .filter(|f| f.path == file)
                .take(3)
                .collect();

            if !file_history.is_empty() {
                lines.push(format!("  file history for {file}:"));
                for f in &file_history {
                    let age = Self::age_str(now, f.timestamp_ms);
                    lines.push(format!("    {age}: {action} — {ctx} [{outcome}]",
                        action = f.action, ctx = f.context, outcome = f.outcome));
                }
            }
        }

        // 2. Recent errors (if relevant to current tool)
        let recent_tool_errors: Vec<&RecentError> = self.recent_errors.iter()
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
        if let Some(prev) = self.sessions.get(0) {
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
        ws.record_action("Read", Some("/a.rs".into()));
        ws.record_action("Edit", Some("/a.rs".into()));
        ws.record_action("Bash", None);
        assert_eq!(ws.recent_actions.len(), 3);
        assert_eq!(ws.recent_actions[0].tool, "Bash"); // most recent first
    }

    // ── infer_strategy ──

    #[test]
    fn infer_strategy_too_few_actions() {
        let mut ws = make_ws();
        ws.record_action("Read", None);
        ws.record_action("Read", None);
        assert!(ws.infer_strategy().is_none());
    }

    #[test]
    fn infer_strategy_build_fix_cycle() {
        let mut ws = make_ws();
        ws.record_action("Bash", None);
        ws.record_action("Edit", Some("/a.rs".into()));
        ws.record_action("Read", Some("/a.rs".into()));
        ws.record_action("Bash", None);
        assert_eq!(ws.infer_strategy().unwrap(), "build-fix-cycle");
    }

    #[test]
    fn infer_strategy_codebase_exploration() {
        let mut ws = make_ws();
        ws.record_action("Grep", None);
        ws.record_action("Glob", None);
        ws.record_action("Read", Some("/a.rs".into()));
        assert_eq!(ws.infer_strategy().unwrap(), "codebase-exploration");
    }

    #[test]
    fn infer_strategy_analyze_modify() {
        let mut ws = make_ws();
        ws.record_action("Read", Some("/a.rs".into()));
        ws.record_action("Read", Some("/b.rs".into()));
        ws.record_action("Edit", Some("/a.rs".into()));
        assert_eq!(ws.infer_strategy().unwrap(), "analyze-modify");
    }

    #[test]
    fn infer_strategy_code_review() {
        let mut ws = make_ws();
        ws.record_action("Read", Some("/a.rs".into()));
        ws.record_action("Read", Some("/b.rs".into()));
        ws.record_action("Read", Some("/c.rs".into()));
        assert_eq!(ws.infer_strategy().unwrap(), "code-review");
    }

    #[test]
    fn infer_strategy_delegated_research() {
        let mut ws = make_ws();
        ws.record_action("Read", None);
        ws.record_action("Read", None);
        ws.record_action("Agent", None);
        assert_eq!(ws.infer_strategy().unwrap(), "delegated-research");
    }

    #[test]
    fn infer_strategy_multi_file_refactor() {
        let mut ws = make_ws();
        ws.record_action("Edit", Some("/a.rs".into()));
        ws.record_action("Edit", Some("/b.rs".into()));
        ws.record_action("Edit", Some("/c.rs".into()));
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
        ws.record_error("Bash", "compile".into(), "error[E0308]: mismatched types".into());
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
            tool: "Edit".into(), file_path: Some("/a.rs".into()), timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(), file_path: Some("/b.rs".into()), timestamp_ms: now + 1000,
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
            tool: "Read".into(), file_path: Some("/b.rs".into()), timestamp_ms: now,
        });
        ws.recent_actions.push_front(RecentAction {
            tool: "Edit".into(), file_path: Some("/a.rs".into()), timestamp_ms: now + 2000,
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
        ws.record_action("Read", Some("/b.rs".into()));
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

/// Extract file path from tool_input if the tool operates on a file.
pub fn extract_file_path(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "Read" | "Write" | "Edit" => tool_input["file_path"].as_str().map(String::from),
        "Grep" => tool_input["path"].as_str().map(String::from),
        "Glob" => tool_input["path"].as_str().map(String::from),
        _ => None,
    }
}

/// Extract error snippet from tool_response if the tool failed.
pub fn extract_error(tool_response: &serde_json::Value) -> Option<String> {
    if let Some(err) = tool_response.get("error").and_then(|e| e.as_str()) {
        let truncated = if err.len() > 300 { &err[..300] } else { err };
        return Some(truncated.to_string());
    }
    if let Some(s) = tool_response.as_str() {
        if s.contains("error") || s.contains("Error") || s.contains("failed") {
            let truncated = if s.len() > 300 { &s[..300] } else { s };
            return Some(truncated.to_string());
        }
    }
    None
}
