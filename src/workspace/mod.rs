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
