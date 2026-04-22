//! Performance and token-budget guardrails for the hot path.
//!
//! These constants define the contract that keeps Thronglets usable
//! inside AI tool loops: narrow hook matchers, silence-by-default,
//! and a bounded number of emitted signals.

/// Only inject prehook context at decision points.
pub const PREHOOK_MATCHER: &str = "Edit|Write|Bash|Agent|Read|Grep|Glob";

/// Keep prehook output bounded even when several signals are available.
pub const PREHOOK_MAX_HINTS: usize = 3;

/// Secondary corroboration is useful, but the hot path only gets one lookup.
pub const PREHOOK_MAX_SECONDARY_QUERIES: usize = 1;

/// Git history is useful, but it must stay short.
pub const GIT_HISTORY_MAX_ENTRIES: usize = 5;

/// Field observations travel beside recommendations — cap output to avoid overwhelming the agent.
pub const FIELD_OBSERVATION_MAX: usize = 6;

/// Stable header for injected output.
pub const PREHOOK_HEADER: &str = "[thronglets]";
