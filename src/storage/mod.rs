//! Local trace storage backed by SQLite.
//!
//! Each node stores traces locally. No global consensus needed.
//! Traces have TTL — like pheromone evaporation, old signals fade.

use crate::continuity::CONTINUITY_CAPABILITY_PREFIX;
use crate::posts::{
    SIGNAL_CAPABILITY_PREFIX, SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX, SignalPostKind,
    is_legacy_auto_signal_trace,
};
use crate::presence::PRESENCE_CAPABILITY_PREFIX;
use crate::signals::StepAction;
use crate::trace::{MethodCompliance, Outcome, Trace};
use ed25519_dalek::Signature;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Mutex;

const DEFAULT_TTL_DAYS: i64 = 7;
const TRACE_SELECT_COLUMNS: &str = "id, capability, outcome, latency_ms, input_size, context_hash, context_text, session_id, owner_account, device_identity, agent_id, sigil_id, method_compliance, model_id, timestamp, node_pubkey, signature";
const TRACE_SELECT_COLUMNS_T: &str = "t.id, t.capability, t.outcome, t.latency_ms, t.input_size, t.context_hash, t.context_text, t.session_id, t.owner_account, t.device_identity, t.agent_id, t.sigil_id, t.method_compliance, t.model_id, t.timestamp, t.node_pubkey, t.signature";

/// Compute a 16-bit bucket from the first 2 bytes of a context_hash.
/// Used as a pre-filter index for similarity search — traces in nearby
/// buckets are more likely to have similar SimHash fingerprints.
pub fn context_bucket(context_hash: &[u8; 16]) -> i64 {
    ((context_hash[0] as i64) << 8) | (context_hash[1] as i64)
}

pub struct TraceStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ContextResidueStats {
    pub success_compliant: u32,
    pub success_noncompliant: u32,
    pub success_unknown: u32,
    pub failure_compliant: u32,
    pub failure_noncompliant: u32,
    pub failure_unknown: u32,
}

impl ContextResidueStats {
    pub fn total_success(self) -> u32 {
        self.success_compliant + self.success_noncompliant + self.success_unknown
    }

    pub fn total_failure(self) -> u32 {
        self.failure_compliant + self.failure_noncompliant + self.failure_unknown
    }

    pub fn total_noncompliant(self) -> u32 {
        self.success_noncompliant + self.failure_noncompliant
    }
}

impl TraceStore {
    /// Open or create a trace store at the given path.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // Create core tables (columns match v0.2.1 schema for fresh installs)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS traces (
                id              BLOB PRIMARY KEY,
                capability      TEXT NOT NULL,
                outcome         INTEGER NOT NULL,
                latency_ms      INTEGER NOT NULL,
                input_size      INTEGER NOT NULL,
                context_hash    BLOB NOT NULL,
                context_text    TEXT,
                session_id      TEXT,
                owner_account   TEXT,
                device_identity TEXT,
                method_compliance TEXT,
                model_id        TEXT NOT NULL,
                timestamp       INTEGER NOT NULL,
                node_pubkey     BLOB NOT NULL,
                signature       BLOB NOT NULL,
                context_bucket  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS anchored_traces (
                trace_id      BLOB PRIMARY KEY,
                anchor_height INTEGER NOT NULL,
                tx_hash       TEXT NOT NULL,
                anchored_at   INTEGER NOT NULL
            );",
        )?;
        // Migrations: add columns if upgrading from older versions
        // Each ALTER is separate — if one fails (column exists), the rest still run
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN context_text TEXT", []);
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN session_id TEXT", []);
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN owner_account TEXT", []);
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN device_identity TEXT", []);
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN method_compliance TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE traces ADD COLUMN context_bucket INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE traces ADD COLUMN published INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Agent V1: agent_id for multi-agent disambiguation
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN agent_id TEXT", []);
        // Sigil V1: on-chain identity of the Loop that produced this trace
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN sigil_id TEXT", []);
        // Phase B: space as first-class column for SQL-level isolation
        let _ = conn.execute("ALTER TABLE traces ADD COLUMN space TEXT", []);
        // Backfill space from signal trace JSON payloads (idempotent)
        let _ = conn.execute(
            "UPDATE traces SET space = json_extract(context_text, '$.space')
             WHERE space IS NULL
               AND context_text IS NOT NULL
               AND capability LIKE 'urn:thronglets:signal:%'
               AND json_extract(context_text, '$.space') IS NOT NULL",
            [],
        );
        // Now create indexes (columns guaranteed to exist after migration)
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_traces_capability ON traces(capability);
            CREATE INDEX IF NOT EXISTS idx_traces_timestamp ON traces(timestamp);
            CREATE INDEX IF NOT EXISTS idx_traces_model_id ON traces(model_id);
            CREATE INDEX IF NOT EXISTS idx_traces_session_id ON traces(session_id);
            CREATE INDEX IF NOT EXISTS idx_traces_context_bucket ON traces(context_bucket);
            CREATE INDEX IF NOT EXISTS idx_traces_space ON traces(space);
            CREATE INDEX IF NOT EXISTS idx_traces_space_capability ON traces(space, capability);
            CREATE INDEX IF NOT EXISTS idx_traces_sigil_id ON traces(sigil_id);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory store (for testing).
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    /// Insert a trace. Returns false if duplicate (content-addressed dedup).
    /// Auto-extracts `space` from JSON payload in context_text when present.
    pub fn insert(&self, trace: &Trace) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let bucket = context_bucket(&trace.context_hash);
        let space = extract_space(&trace.context_text);
        let result = conn.execute(
            "INSERT OR IGNORE INTO traces (id, capability, outcome, latency_ms, input_size, context_hash, context_text, session_id, owner_account, device_identity, agent_id, sigil_id, method_compliance, model_id, timestamp, node_pubkey, signature, context_bucket, space)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                trace.id.as_slice(),
                trace.capability,
                trace.outcome as u8,
                trace.latency_ms,
                trace.input_size,
                trace.context_hash.as_slice(),
                trace.context_text,
                trace.session_id,
                trace.owner_account,
                trace.device_identity,
                trace.agent_id,
                trace.sigil_id,
                trace.method_compliance.map(|value| value.as_str().to_string()),
                trace.model_id,
                trace.timestamp as i64,
                trace.node_pubkey.as_slice(),
                trace.signature.to_bytes().as_slice(),
                bucket,
                space,
            ],
        )?;
        Ok(result > 0)
    }

    /// Query traces by capability.
    pub fn query_capability(&self, capability: &str, limit: usize) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE capability = ?1 ORDER BY timestamp DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![capability, limit as i64])
    }

    /// Query traces with similar context (Hamming distance on context_hash).
    ///
    /// v0.2.1: Uses bucket pre-filtering. The context_bucket index narrows
    /// candidates to traces in nearby SimHash buckets before doing the
    /// expensive Hamming distance check in Rust. At small scale (<10k traces)
    /// this falls back to scanning all buckets, but the index prevents
    /// full-table-scan pathology as the DB grows.
    pub fn query_similar(
        &self,
        context_hash: &[u8; 16],
        max_distance: u32,
        limit: usize,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let target_bucket = context_bucket(context_hash);

        // Bucket pre-filter: fetch traces from nearby buckets.
        // Each bucket bit-flip represents ~8 Hamming distance in the full hash,
        // so we expand to neighboring buckets proportional to max_distance.
        let bucket_radius = (max_distance / 8).max(1) as i64;
        let bucket_lo = (target_bucket - bucket_radius).max(0);
        let bucket_hi = (target_bucket + bucket_radius).min(65535);

        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE context_bucket BETWEEN ?1 AND ?2 ORDER BY timestamp DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let candidates = Self::collect_traces(&mut stmt, params![bucket_lo, bucket_hi])?;

        let mut matched: Vec<Trace> = candidates
            .into_iter()
            .filter(|t| {
                crate::context::hamming_distance(&t.context_hash, context_hash) <= max_distance
            })
            .collect();
        matched.truncate(limit);
        Ok(matched)
    }

    /// Compute aggregate stats for a capability.
    pub fn aggregate(&self, capability: &str) -> rusqlite::Result<Option<AggregateStats>> {
        let conn = self.conn.lock().unwrap();

        // First check count and success rate via SQL.
        let mut count_stmt = conn.prepare(
            "SELECT
                COUNT(*) as total,
                COALESCE(SUM(CASE WHEN outcome = 0 THEN 1 ELSE 0 END), 0) as succeeded,
                COALESCE(AVG(input_size), 0) as avg_input
             FROM traces WHERE capability = ?1",
        )?;
        let (total, succeeded, avg_input_size): (i64, i64, f64) = count_stmt
            .query_row(params![capability], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;

        if total == 0 {
            return Ok(None);
        }

        // Fetch all latency values for percentile calculation.
        let mut lat_stmt = conn.prepare(
            "SELECT latency_ms FROM traces WHERE capability = ?1 ORDER BY latency_ms ASC",
        )?;
        let latencies: Vec<f64> = lat_stmt
            .query_map(params![capability], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|v| v as f64)
            .collect();

        let p50 = percentile(&latencies, 0.50);
        let p95 = percentile(&latencies, 0.95);
        let confidence = (total as f64 / 100.0).min(1.0);

        Ok(Some(AggregateStats {
            total_traces: total as u64,
            success_rate: succeeded as f64 / total as f64,
            p50_latency_ms: p50,
            p95_latency_ms: p95,
            avg_input_size,
            confidence,
        }))
    }

    /// Query traces by session, ordered by timestamp (workflow sequence).
    pub fn query_session(&self, session_id: &str, limit: usize) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE session_id = ?1 ORDER BY timestamp ASC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![session_id, limit as i64])
    }

    /// Return recent session ids ordered from oldest to newest among the most
    /// recent `limit` sessions in the given time window.
    pub fn recent_session_ids(&self, hours: u64, limit: usize) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let mut stmt = conn.prepare(
            "SELECT session_id
             FROM (
                SELECT session_id, MAX(timestamp) AS last_seen
                FROM traces
                WHERE session_id IS NOT NULL
                  AND timestamp >= ?1
                GROUP BY session_id
                ORDER BY last_seen DESC
                LIMIT ?2
             )
             ORDER BY last_seen ASC",
        )?;
        let rows = stmt.query_map(params![cutoff_ms, limit as i64], |row| row.get(0))?;
        rows.collect()
    }

    /// Discover workflow patterns: what capability do agents use AFTER a given capability?
    /// Returns (next_capability, count) pairs ordered by frequency.
    pub fn query_workflow_next(
        &self,
        capability: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(String, u64)>> {
        let conn = self.conn.lock().unwrap();
        // Find sessions that contain the given capability, then look at the next trace in each session
        let mut stmt = conn.prepare(
            "SELECT t2.capability, COUNT(*) as cnt
             FROM traces t1
             JOIN traces t2 ON t1.session_id = t2.session_id
                           AND t2.timestamp > t1.timestamp
                           AND t1.session_id IS NOT NULL
             WHERE t1.capability = ?1
               AND t2.rowid = (
                   SELECT MIN(t3.rowid) FROM traces t3
                   WHERE t3.session_id = t1.session_id
                     AND t3.timestamp > t1.timestamp
               )
             GROUP BY t2.capability
             ORDER BY cnt DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![capability, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        rows.collect()
    }

    /// Count distinct corroborating sources for a failed tool followed by a
    /// concrete repair sequence (up to 2 steps).
    pub fn count_repair_sources(
        &self,
        failed_tool: &str,
        steps: &[StepAction],
        hours: u64,
    ) -> rusqlite::Result<u32> {
        if steps.is_empty() || steps.len() > 2 {
            return Ok(0);
        }

        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let failed_cap = format!("claude-code/{failed_tool}");

        let count = if steps.len() == 1 {
            let (step1_cap, step1_exact, step1_suffix) = step_match(&steps[0]);
            conn.query_row(
                "SELECT COUNT(DISTINCT (COALESCE(t0.device_identity, hex(t0.node_pubkey)) || ':' || t0.session_id))
                 FROM traces t0
                 JOIN traces t1 ON t1.session_id = t0.session_id
                               AND t1.node_pubkey = t0.node_pubkey
                 WHERE t0.session_id IS NOT NULL
                   AND t0.capability = ?1
                   AND t0.outcome = 1
                   AND t1.timestamp > t0.timestamp
                   AND (t1.timestamp - t0.timestamp) <= 600000
                   AND t0.timestamp >= ?2
                   AND t1.capability = ?3
                   AND (?4 IS NULL OR t1.context_text = ?4 OR t1.context_text LIKE ?5)",
                params![failed_cap, cutoff_ms, step1_cap, step1_exact, step1_suffix,],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            let (step1_cap, step1_exact, step1_suffix) = step_match(&steps[0]);
            let (step2_cap, step2_exact, step2_suffix) = step_match(&steps[1]);
            conn.query_row(
                "SELECT COUNT(DISTINCT (COALESCE(t0.device_identity, hex(t0.node_pubkey)) || ':' || t0.session_id))
                 FROM traces t0
                 JOIN traces t1 ON t1.session_id = t0.session_id
                               AND t1.node_pubkey = t0.node_pubkey
                 JOIN traces t2 ON t2.session_id = t0.session_id
                               AND t2.node_pubkey = t0.node_pubkey
                 WHERE t0.session_id IS NOT NULL
                   AND t0.capability = ?1
                   AND t0.outcome = 1
                   AND t1.timestamp > t0.timestamp
                   AND t2.timestamp > t1.timestamp
                   AND (t1.timestamp - t0.timestamp) <= 600000
                   AND (t2.timestamp - t1.timestamp) <= 600000
                   AND t0.timestamp >= ?2
                   AND t1.capability = ?3
                   AND (?4 IS NULL OR t1.context_text = ?4 OR t1.context_text LIKE ?5)
                   AND t2.capability = ?6
                   AND (?7 IS NULL OR t2.context_text = ?7 OR t2.context_text LIKE ?8)",
                params![
                    failed_cap,
                    cutoff_ms,
                    step1_cap,
                    step1_exact,
                    step1_suffix,
                    step2_cap,
                    step2_exact,
                    step2_suffix,
                ],
                |row| row.get::<_, i64>(0),
            )?
        };

        Ok(count.max(0) as u32)
    }

    /// Query explicit signal traces by context similarity and optional kind.
    /// When `space` is `Some`, only returns traces tagged with that space (SQL-level isolation).
    /// When `space` is `None`, returns all signals regardless of space.
    pub fn query_signal_traces(
        &self,
        context_hash: &[u8; 16],
        kind: Option<SignalPostKind>,
        max_distance: u32,
        limit: usize,
        space: Option<&str>,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let target_bucket = context_bucket(context_hash);
        let bucket_radius = (max_distance / 8).max(1) as i64;
        let bucket_lo = (target_bucket - bucket_radius).max(0);
        let bucket_hi = (target_bucket + bucket_radius).min(65535);
        let query_limit = (limit.max(1) * 20) as i64;

        let caps: Vec<String> = if let Some(kind) = kind {
            vec![
                kind.capability().to_string(),
                kind.reinforcement_capability().to_string(),
            ]
        } else {
            vec![
                format!("{SIGNAL_CAPABILITY_PREFIX}%"),
                format!("{SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX}%"),
            ]
        };
        let use_like = kind.is_none();

        let mut candidates = Vec::new();
        for cap in &caps {
            let cap_filter = if use_like {
                "capability LIKE ?3"
            } else {
                "capability = ?3"
            };
            let sql = format!(
                "SELECT {TRACE_SELECT_COLUMNS}
                 FROM traces
                 WHERE context_bucket BETWEEN ?1 AND ?2
                   AND {cap_filter}
                   AND (?5 IS NULL OR space = ?5)
                 ORDER BY timestamp DESC
                 LIMIT ?4"
            );
            let mut stmt = conn.prepare(&sql)?;
            candidates.extend(Self::collect_traces(
                &mut stmt,
                params![bucket_lo, bucket_hi, cap, query_limit, space],
            )?);
        }
        candidates.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        let mut matched: Vec<Trace> = candidates
            .into_iter()
            .filter(|trace| {
                crate::context::hamming_distance(&trace.context_hash, context_hash) <= max_distance
            })
            .collect();
        matched.truncate(limit);
        Ok(matched)
    }

    /// Count distinct sessions that performed a similar action successfully.
    /// Used to detect convergent behavior for auto-recommend signals.
    pub fn count_convergent_sessions(
        &self,
        context_hash: &[u8; 16],
        max_distance: u32,
        space: Option<&str>,
    ) -> rusqlite::Result<u32> {
        let conn = self.conn.lock().unwrap();
        let target_bucket = context_bucket(context_hash);
        let bucket_radius = (max_distance / 8).max(1) as i64;
        let bucket_lo = (target_bucket - bucket_radius).max(0);
        let bucket_hi = (target_bucket + bucket_radius).min(65535);

        let sql = "SELECT session_id, context_hash
                   FROM traces
                   WHERE context_bucket BETWEEN ?1 AND ?2
                     AND outcome = 0
                     AND capability NOT LIKE 'urn:thronglets:signal:%'
                     AND capability NOT LIKE 'urn:thronglets:presence:%'
                     AND session_id IS NOT NULL
                     AND (?3 IS NULL OR space = ?3)
                   ORDER BY timestamp DESC
                   LIMIT 200";

        let mut stmt = conn.prepare(sql)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params![bucket_lo, bucket_hi, space], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut sessions = std::collections::HashSet::new();
        for (session_id, hash_bytes) in &rows {
            if hash_bytes.len() == 16 {
                let mut h = [0u8; 16];
                h.copy_from_slice(hash_bytes);
                if crate::context::hamming_distance(&h, context_hash) <= max_distance {
                    sessions.insert(session_id.clone());
                }
            }
        }

        Ok(sessions.len() as u32)
    }

    /// Count distinct recent sessions that hit a similar failed context.
    /// This is the contradiction side of a success prior: wrong paths should
    /// remain visible long enough to resist premature recommend promotion.
    pub fn count_contradicting_failed_sessions(
        &self,
        context_hash: &[u8; 16],
        max_distance: u32,
        hours: u64,
        space: Option<&str>,
    ) -> rusqlite::Result<u32> {
        let traces =
            self.query_similar_failed_traces(context_hash, max_distance, hours, 200, space)?;
        let mut sessions = std::collections::HashSet::new();
        for trace in traces {
            let key = trace
                .session_id
                .or(trace.device_identity)
                .unwrap_or_else(|| format!("trace-{}", trace.timestamp));
            sessions.insert(key);
        }
        Ok(sessions.len() as u32)
    }

    /// Aggregate recent residue for a context, split by outcome and method compliance.
    /// Missing compliance on older traces is treated as `unknown`.
    pub fn residue_stats_for_context(
        &self,
        context_hash: &[u8; 16],
        max_distance: u32,
        hours: u64,
        limit: usize,
        space: Option<&str>,
    ) -> rusqlite::Result<ContextResidueStats> {
        let conn = self.conn.lock().unwrap();
        let target_bucket = context_bucket(context_hash);
        let bucket_radius = (max_distance / 8).max(1) as i64;
        let bucket_lo = (target_bucket - bucket_radius).max(0);
        let bucket_hi = (target_bucket + bucket_radius).min(65535);
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let query_limit = (limit.max(1) * 12) as i64;

        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS}
             FROM traces
             WHERE context_bucket BETWEEN ?1 AND ?2
               AND capability NOT LIKE 'urn:thronglets:%'
               AND timestamp > ?3
               AND (?5 IS NULL OR space = ?5)
             ORDER BY timestamp DESC
             LIMIT ?4"
        );

        let mut stmt = conn.prepare(&sql)?;
        let candidates =
            Self::collect_traces(&mut stmt, params![bucket_lo, bucket_hi, cutoff_ms, query_limit, space])?;

        let mut stats = ContextResidueStats::default();
        let mut success_sessions = std::collections::HashSet::new();
        let mut failure_sessions = std::collections::HashSet::new();

        for trace in candidates.into_iter().filter(|trace| {
            crate::context::hamming_distance(&trace.context_hash, context_hash) <= max_distance
        }) {
            let session_key = residue_session_key(&trace);
            let is_failure = matches!(trace.outcome, Outcome::Failed | Outcome::Timeout);
            if is_failure {
                if !failure_sessions.insert(session_key) {
                    continue;
                }
            } else if trace.outcome == Outcome::Succeeded {
                if !success_sessions.insert(session_key) {
                    continue;
                }
            } else {
                continue;
            }

            match (trace.outcome, trace.method_compliance.unwrap_or(crate::trace::MethodCompliance::Unknown)) {
                (Outcome::Succeeded, crate::trace::MethodCompliance::Compliant) => {
                    stats.success_compliant += 1;
                }
                (Outcome::Succeeded, crate::trace::MethodCompliance::Noncompliant) => {
                    stats.success_noncompliant += 1;
                }
                (Outcome::Succeeded, crate::trace::MethodCompliance::Unknown) => {
                    stats.success_unknown += 1;
                }
                (Outcome::Failed | Outcome::Timeout, crate::trace::MethodCompliance::Compliant) => {
                    stats.failure_compliant += 1;
                }
                (Outcome::Failed | Outcome::Timeout, crate::trace::MethodCompliance::Noncompliant) => {
                    stats.failure_noncompliant += 1;
                }
                (Outcome::Failed | Outcome::Timeout, crate::trace::MethodCompliance::Unknown) => {
                    stats.failure_unknown += 1;
                }
                _ => {}
            }
        }

        Ok(stats)
    }

    /// Count distinct sessions where a failure on `error_context` was followed
    /// (within 10 min) by a success on `repair_context`. This detects cross-file
    /// repair associations directly from traces — the high-value signal that
    /// workspace repair_patterns lose by discarding context.
    pub fn count_repair_associations(
        &self,
        error_context_hash: &[u8; 16],
        repair_context_hash: &[u8; 16],
        max_distance: u32,
        space: Option<&str>,
    ) -> rusqlite::Result<u32> {
        let conn = self.conn.lock().unwrap();

        let err_bucket = context_bucket(error_context_hash);
        let fix_bucket = context_bucket(repair_context_hash);
        let radius = (max_distance / 8).max(1) as i64;
        let err_lo = (err_bucket - radius).max(0);
        let err_hi = (err_bucket + radius).min(65535);
        let fix_lo = (fix_bucket - radius).max(0);
        let fix_hi = (fix_bucket + radius).min(65535);

        let sql = "SELECT t_err.session_id, t_err.context_hash, t_fix.context_hash
                   FROM traces t_err
                   JOIN traces t_fix ON t_fix.session_id = t_err.session_id
                                     AND t_fix.node_pubkey = t_err.node_pubkey
                                     AND t_fix.timestamp > t_err.timestamp
                                     AND (t_fix.timestamp - t_err.timestamp) <= 600000
                   WHERE t_err.outcome = 1
                     AND t_fix.outcome = 0
                     AND t_err.context_bucket BETWEEN ?1 AND ?2
                     AND t_fix.context_bucket BETWEEN ?3 AND ?4
                     AND t_err.session_id IS NOT NULL
                     AND t_err.capability NOT LIKE 'urn:thronglets:%'
                     AND t_fix.capability NOT LIKE 'urn:thronglets:%'
                     AND (?5 IS NULL OR t_err.space = ?5)
                   LIMIT 200";

        let mut stmt = conn.prepare(sql)?;
        let rows: Vec<(String, Vec<u8>, Vec<u8>)> = stmt
            .query_map(params![err_lo, err_hi, fix_lo, fix_hi, space], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut sessions = std::collections::HashSet::new();
        for (session_id, err_hash, fix_hash) in &rows {
            if err_hash.len() == 16 && fix_hash.len() == 16 {
                let mut eh = [0u8; 16];
                let mut fh = [0u8; 16];
                eh.copy_from_slice(err_hash);
                fh.copy_from_slice(fix_hash);
                if crate::context::hamming_distance(&eh, error_context_hash) <= max_distance
                    && crate::context::hamming_distance(&fh, repair_context_hash) <= max_distance
                {
                    sessions.insert(session_id.clone());
                }
            }
        }

        Ok(sessions.len() as u32)
    }

    /// Hebbian co-occurrence: count sessions where two contexts both succeeded.
    /// "Neurons that fire together wire together" — files edited together
    /// across multiple sessions reveal structural coupling.
    pub fn count_co_occurring_sessions(
        &self,
        context_a_hash: &[u8; 16],
        context_b_hash: &[u8; 16],
        max_distance: u32,
        space: Option<&str>,
    ) -> rusqlite::Result<u32> {
        let conn = self.conn.lock().unwrap();

        let a_bucket = context_bucket(context_a_hash);
        let b_bucket = context_bucket(context_b_hash);
        let radius = (max_distance / 8).max(1) as i64;
        let a_lo = (a_bucket - radius).max(0);
        let a_hi = (a_bucket + radius).min(65535);
        let b_lo = (b_bucket - radius).max(0);
        let b_hi = (b_bucket + radius).min(65535);

        let sql = "SELECT t1.session_id, t1.context_hash, t2.context_hash
                   FROM traces t1
                   JOIN traces t2 ON t2.session_id = t1.session_id
                                  AND t2.node_pubkey = t1.node_pubkey
                                  AND t2.id != t1.id
                   WHERE t1.outcome = 0
                     AND t2.outcome = 0
                     AND t1.context_bucket BETWEEN ?1 AND ?2
                     AND t2.context_bucket BETWEEN ?3 AND ?4
                     AND t1.session_id IS NOT NULL
                     AND t1.capability NOT LIKE 'urn:thronglets:%'
                     AND t2.capability NOT LIKE 'urn:thronglets:%'
                     AND (?5 IS NULL OR t1.space = ?5)
                   LIMIT 200";

        let mut stmt = conn.prepare(sql)?;
        let rows: Vec<(String, Vec<u8>, Vec<u8>)> = stmt
            .query_map(params![a_lo, a_hi, b_lo, b_hi, space], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut sessions = std::collections::HashSet::new();
        for (session_id, a_hash, b_hash) in &rows {
            if a_hash.len() == 16 && b_hash.len() == 16 {
                let mut ah = [0u8; 16];
                let mut bh = [0u8; 16];
                ah.copy_from_slice(a_hash);
                bh.copy_from_slice(b_hash);
                if crate::context::hamming_distance(&ah, context_a_hash) <= max_distance
                    && crate::context::hamming_distance(&bh, context_b_hash) <= max_distance
                {
                    sessions.insert(session_id.clone());
                }
            }
        }

        Ok(sessions.len() as u32)
    }

    /// Query recent failed traces with similar context.
    /// Surfaces experiential failure hints — "this context has failed before."
    /// Used by prehook to provide intuition-like warnings before the agent
    /// repeats a known mistake. Excludes internal thronglets traces.
    pub fn query_similar_failed_traces(
        &self,
        context_hash: &[u8; 16],
        max_distance: u32,
        hours: u64,
        limit: usize,
        space: Option<&str>,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let target_bucket = context_bucket(context_hash);
        let bucket_radius = (max_distance / 8).max(1) as i64;
        let bucket_lo = (target_bucket - bucket_radius).max(0);
        let bucket_hi = (target_bucket + bucket_radius).min(65535);
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let query_limit = (limit.max(1) * 10) as i64;

        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS}
                   FROM traces
                   WHERE context_bucket BETWEEN ?1 AND ?2
                     AND outcome IN (1, 3)
                     AND capability NOT LIKE 'urn:thronglets:%'
                     AND timestamp > ?3
                     AND (?5 IS NULL OR space = ?5)
                   ORDER BY timestamp DESC
                   LIMIT ?4"
        );

        let mut stmt = conn.prepare(&sql)?;
        let candidates = Self::collect_traces(
            &mut stmt,
            params![bucket_lo, bucket_hi, cutoff_ms, query_limit, space],
        )?;

        let mut matched: Vec<Trace> = candidates
            .into_iter()
            .filter(|trace| {
                crate::context::hamming_distance(&trace.context_hash, context_hash) <= max_distance
            })
            .collect();
        matched.truncate(limit);
        Ok(matched)
    }

    /// Query recent explicit signal traces for a feed view.
    /// When `space` is `Some`, only returns traces tagged with that space.
    pub fn query_recent_signal_traces(
        &self,
        hours: u32,
        kind: Option<SignalPostKind>,
        limit: usize,
        space: Option<&str>,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let query_limit = (limit.max(1) * 20) as i64;

        let caps: Vec<String> = if let Some(kind) = kind {
            vec![
                kind.capability().to_string(),
                kind.reinforcement_capability().to_string(),
            ]
        } else {
            vec![
                format!("{SIGNAL_CAPABILITY_PREFIX}%"),
                format!("{SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX}%"),
            ]
        };
        let use_like = kind.is_none();

        let mut traces = Vec::new();
        for cap in &caps {
            let cap_filter = if use_like {
                "capability LIKE ?1"
            } else {
                "capability = ?1"
            };
            let sql = format!(
                "SELECT {TRACE_SELECT_COLUMNS}
                 FROM traces
                 WHERE {cap_filter}
                   AND timestamp >= ?2
                   AND (?4 IS NULL OR space = ?4)
                 ORDER BY timestamp DESC
                 LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql)?;
            traces.extend(Self::collect_traces(
                &mut stmt,
                params![cap, cutoff_ms, query_limit, space],
            )?);
        }
        traces.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        traces.truncate(limit);
        Ok(traces)
    }

    /// Query recent presence traces for ambient session continuity.
    pub fn query_recent_presence_traces(
        &self,
        hours: u32,
        limit: usize,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let like = format!("{PRESENCE_CAPABILITY_PREFIX}%");
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE capability LIKE ?1 AND timestamp >= ?2 ORDER BY timestamp DESC LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![like, cutoff_ms, limit as i64])
    }

    /// Query recent external continuity traces.
    pub fn query_recent_continuity_traces(
        &self,
        hours: u32,
        limit: usize,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let like = format!("{CONTINUITY_CAPABILITY_PREFIX}%");
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE capability LIKE ?1 AND timestamp >= ?2 ORDER BY timestamp DESC LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![like, cutoff_ms, limit as i64])
    }

    /// Query continuity traces filtered by taxonomy (coordination/continuity/calibration).
    pub fn query_continuity_by_taxonomy(
        &self,
        taxonomy: &str,
        hours: u32,
        limit: usize,
        space: Option<&str>,
    ) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let like = format!("{CONTINUITY_CAPABILITY_PREFIX}{}:%", taxonomy);
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE capability LIKE ?1 AND timestamp >= ?2 AND (?4 IS NULL OR space = ?4) ORDER BY timestamp DESC LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![like, cutoff_ms, limit as i64, space])
    }

    /// Evaporate old traces (pheromone decay).
    pub fn evaporate(&self, max_age_days: Option<i64>) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let days = max_age_days.unwrap_or(DEFAULT_TTL_DAYS);
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (days * 86_400_000);
        let deleted = conn.execute(
            "DELETE FROM traces WHERE timestamp < ?1",
            params![cutoff_ms],
        )?;
        Ok(deleted)
    }

    /// Count legacy auto-derived signal traces that no longer match the active epoch.
    pub fn count_legacy_auto_signal_traces(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS}
             FROM traces
             WHERE (capability LIKE ?1 OR capability LIKE ?2)
               AND model_id = ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        let traces = Self::collect_traces(
            &mut stmt,
            params![
                format!("{SIGNAL_CAPABILITY_PREFIX}%"),
                format!("{SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX}%"),
                crate::posts::AUTO_DERIVED_SIGNAL_MODEL_ID,
            ],
        )?;
        Ok(traces
            .into_iter()
            .filter(is_legacy_auto_signal_trace)
            .count() as u64)
    }

    /// Remove legacy auto-derived signal traces while preserving raw execution traces.
    pub fn delete_legacy_auto_signal_traces(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS}
             FROM traces
             WHERE (capability LIKE ?1 OR capability LIKE ?2)
               AND model_id = ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        let traces = Self::collect_traces(
            &mut stmt,
            params![
                format!("{SIGNAL_CAPABILITY_PREFIX}%"),
                format!("{SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX}%"),
                crate::posts::AUTO_DERIVED_SIGNAL_MODEL_ID,
            ],
        )?;
        let legacy_ids: Vec<[u8; 32]> = traces
            .into_iter()
            .filter(is_legacy_auto_signal_trace)
            .map(|trace| trace.id)
            .collect();
        let mut deleted = 0_u64;
        for trace_id in legacy_ids {
            deleted += conn.execute(
                "DELETE FROM traces WHERE id = ?1",
                params![trace_id.as_slice()],
            )? as u64;
        }
        Ok(deleted)
    }

    /// List distinct capabilities that have traces.
    pub fn distinct_capabilities(&self, limit: usize) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT DISTINCT capability FROM traces ORDER BY capability LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
        rows.collect()
    }

    /// Total trace count.
    pub fn count(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM traces", [], |row| {
            row.get::<_, i64>(0).map(|n| n as u64)
        })
    }

    /// Count of traces attributed to a Sigil (sigil_id IS NOT NULL).
    pub fn count_attributed(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM traces WHERE sigil_id IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as u64),
        )
    }

    /// Query traces that haven't been published to the P2P network yet.
    pub fn unpublished_traces(&self, limit: usize) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let sql =
            format!("SELECT {TRACE_SELECT_COLUMNS} FROM traces WHERE published = 0 ORDER BY timestamp DESC LIMIT ?1");
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![limit as i64])
    }

    /// Mark traces as published to the P2P network.
    pub fn mark_published(&self, trace_ids: &[[u8; 32]]) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("UPDATE traces SET published = 1 WHERE id = ?1")?;
        for id in trace_ids {
            stmt.execute(params![id.as_slice()])?;
        }
        Ok(())
    }

    /// Mark a trace as anchored on-chain.
    pub fn mark_anchored(
        &self,
        trace_id: &[u8; 32],
        anchor_height: u64,
        tx_hash: &str,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT OR REPLACE INTO anchored_traces (trace_id, anchor_height, tx_hash, anchored_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![trace_id.as_slice(), anchor_height as i64, tx_hash, now_ms,],
        )?;
        Ok(())
    }

    /// Check whether a trace has been anchored on-chain.
    pub fn is_anchored(&self, trace_id: &[u8; 32]) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM anchored_traces WHERE trace_id = ?1",
            params![trace_id.as_slice()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get traces from the last N hours that have not been anchored.
    pub fn unanchored_traces(&self, hours: u64, limit: usize) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - (hours as i64 * 3_600_000);
        let sql = format!(
            "SELECT {TRACE_SELECT_COLUMNS_T} FROM traces t LEFT JOIN anchored_traces a ON t.id = a.trace_id WHERE a.trace_id IS NULL AND t.timestamp >= ?1 ORDER BY t.timestamp ASC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        Self::collect_traces(&mut stmt, params![cutoff_ms, limit as i64])
    }

    /// Column order: id(0), capability(1), outcome(2), latency_ms(3), input_size(4),
    /// context_hash(5), context_text(6), session_id(7), owner_account(8), device_identity(9),
    /// agent_id(10), sigil_id(11), method_compliance(12), model_id(13), timestamp(14),
    /// node_pubkey(15), signature(16)
    fn collect_traces(
        stmt: &mut rusqlite::Statement,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<Trace>> {
        let rows = stmt.query_map(params, |row| {
            let id_bytes: Vec<u8> = row.get(0)?;
            let outcome_u8: u8 = row.get(2)?;
            let context_bytes: Vec<u8> = row.get(5)?;
            let pubkey_bytes: Vec<u8> = row.get(15)?;
            let sig_bytes: Vec<u8> = row.get(16)?;

            Ok(Trace {
                id: id_bytes.try_into().unwrap_or([0u8; 32]),
                capability: row.get(1)?,
                outcome: match outcome_u8 {
                    0 => Outcome::Succeeded,
                    1 => Outcome::Failed,
                    2 => Outcome::Partial,
                    _ => Outcome::Timeout,
                },
                latency_ms: row.get::<_, u32>(3)?,
                input_size: row.get::<_, u32>(4)?,
                context_hash: context_bytes.try_into().unwrap_or([0u8; 16]),
                context_text: row.get(6)?,
                session_id: row.get(7)?,
                owner_account: row.get(8)?,
                device_identity: row.get(9)?,
                agent_id: row.get(10)?,
                sigil_id: row.get(11)?,
                method_compliance: row
                    .get::<_, Option<String>>(12)?
                    .as_deref()
                    .and_then(MethodCompliance::parse),
                model_id: row.get(13)?,
                timestamp: row.get::<_, i64>(14)? as u64,
                node_pubkey: pubkey_bytes.try_into().unwrap_or([0u8; 32]),
                signature: Signature::from_bytes(&sig_bytes.try_into().unwrap_or([0u8; 64])),
            })
        })?;
        rows.collect()
    }
}

/// Compute the value at a given percentile (0.0 to 1.0) from a sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p * (sorted.len() - 1) as f64;
    let lower = idx.floor() as usize;
    let upper = idx.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let frac = idx - lower as f64;
        sorted[lower] * (1.0 - frac) + sorted[upper] * frac
    }
}

/// Try to extract `$.space` from a JSON context_text. Returns None for non-JSON or missing key.
fn extract_space(context_text: &Option<String>) -> Option<String> {
    let text = context_text.as_ref()?;
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    parsed.get("space")?.as_str().map(String::from)
}

fn residue_session_key(trace: &Trace) -> String {
    trace
        .session_id
        .clone()
        .or_else(|| trace.device_identity.clone())
        .unwrap_or_else(|| format!("trace-{}", trace.timestamp))
}

fn step_match(step: &StepAction) -> (String, Option<String>, Option<String>) {
    let capability = format!("claude-code/{}", step.tool);
    let prefix = match step.tool.as_str() {
        "Read" => Some("read file"),
        "Edit" => Some("edit file"),
        "Write" => Some("write file"),
        _ => None,
    };

    match (&prefix, &step.target) {
        (Some(prefix), Some(target)) => (
            capability,
            Some(format!("{prefix}: {target}")),
            Some(format!("{prefix}: %/{target}")),
        ),
        _ => (capability, None, None),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AggregateStats {
    pub total_traces: u64,
    pub success_rate: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub avg_input_size: f64,
    pub confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    fn make_trace(id: &NodeIdentity, cap: &str, outcome: Outcome, context: &str) -> Trace {
        use crate::context::simhash;
        Trace::new(
            cap.into(),
            outcome,
            100,
            5000,
            simhash(context),
            Some(context.to_string()),
            None,
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        )
    }

    #[test]
    fn insert_and_query() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let trace = make_trace(&id, "tool-a", Outcome::Succeeded, "test context alpha");

        assert!(store.insert(&trace).unwrap());
        assert_eq!(store.count().unwrap(), 1);

        let results = store.query_capability("tool-a", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].capability, "tool-a");
    }

    #[test]
    fn dedup_by_content_address() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let trace = make_trace(&id, "tool-a", Outcome::Succeeded, "dedup context");

        assert!(store.insert(&trace).unwrap());
        assert!(!store.insert(&trace).unwrap()); // duplicate
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn aggregate_stats() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        store
            .insert(&make_trace(&id, "x", Outcome::Succeeded, "agg context 1"))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .insert(&make_trace(&id, "x", Outcome::Succeeded, "agg context 2"))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .insert(&make_trace(&id, "x", Outcome::Failed, "agg context 3"))
            .unwrap();

        let stats = store.aggregate("x").unwrap().unwrap();
        assert_eq!(stats.total_traces, 3);
        assert!((stats.success_rate - 2.0 / 3.0).abs() < 0.01);
        // All latencies are 100, so p50 and p95 should both be 100.0
        assert!((stats.p50_latency_ms - 100.0).abs() < 0.01);
        assert!((stats.p95_latency_ms - 100.0).abs() < 0.01);
        // confidence = min(1.0, 3/100) = 0.03
        assert!((stats.confidence - 0.03).abs() < 0.001);
    }

    #[test]
    fn query_similar_by_context() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // Two traces with similar contexts, one with a very different context.
        store
            .insert(&make_trace(
                &id,
                "a",
                Outcome::Succeeded,
                "translate a technical document from Chinese to English",
            ))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .insert(&make_trace(
                &id,
                "b",
                Outcome::Succeeded,
                "translate a legal document from Chinese to English",
            ))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .insert(&make_trace(
                &id,
                "c",
                Outcome::Succeeded,
                "deploy kubernetes cluster on AWS with terraform",
            ))
            .unwrap();

        let target =
            crate::context::simhash("translate a technical document from Chinese to English");

        // Tight distance: should find the exact match and the similar one.
        let results = store.query_similar(&target, 40, 10).unwrap();
        let caps: Vec<&str> = results.iter().map(|t| t.capability.as_str()).collect();
        assert!(caps.contains(&"a"), "exact context match should be found");
        assert!(caps.contains(&"b"), "similar context should be found");

        // Very tight distance (0): only exact match.
        let exact = store.query_similar(&target, 0, 10).unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].capability, "a");
    }

    #[test]
    fn no_traces_returns_none_aggregate() {
        let store = TraceStore::in_memory().unwrap();
        assert!(store.aggregate("nonexistent").unwrap().is_none());
    }

    #[test]
    fn distinct_capabilities() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        store
            .insert(&make_trace(&id, "cap-b", Outcome::Succeeded, "ctx 1"))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .insert(&make_trace(&id, "cap-a", Outcome::Succeeded, "ctx 2"))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .insert(&make_trace(&id, "cap-b", Outcome::Failed, "ctx 3"))
            .unwrap();

        let caps = store.distinct_capabilities(10).unwrap();
        assert_eq!(caps, vec!["cap-a", "cap-b"]); // alphabetical, deduplicated
    }

    #[test]
    fn evaporate_removes_expired_traces() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // Insert two traces: one we'll age out, one stays fresh
        let old_trace = make_trace(&id, "old-tool", Outcome::Succeeded, "old context");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let fresh_trace = make_trace(&id, "fresh-tool", Outcome::Succeeded, "fresh context");

        store.insert(&old_trace).unwrap();
        store.insert(&fresh_trace).unwrap();
        assert_eq!(store.count().unwrap(), 2);

        // Manually set old_trace's timestamp to 8 days ago
        let eight_days_ago_ms = chrono::Utc::now().timestamp_millis() - (8 * 86_400_000);
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE traces SET timestamp = ?1 WHERE id = ?2",
                params![eight_days_ago_ms, old_trace.id.as_slice()],
            )
            .unwrap();
        }

        // Evaporate with default 7-day TTL
        let deleted = store.evaporate(None).unwrap();
        assert_eq!(deleted, 1, "should evaporate exactly one expired trace");
        assert_eq!(store.count().unwrap(), 1, "one trace should remain");

        // The remaining trace should be the fresh one
        let remaining = store.query_capability("fresh-tool", 10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].capability, "fresh-tool");

        // The old one should be gone
        let gone = store.query_capability("old-tool", 10).unwrap();
        assert!(gone.is_empty(), "expired trace should be evaporated");
    }

    #[test]
    fn mark_anchored_and_is_anchored() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let trace = make_trace(&id, "anchor-test", Outcome::Succeeded, "anchor context");
        store.insert(&trace).unwrap();

        // Not anchored yet
        assert!(!store.is_anchored(&trace.id).unwrap());

        // Mark as anchored
        store
            .mark_anchored(&trace.id, 100, "ABCDEF1234567890")
            .unwrap();

        // Now it should be anchored
        assert!(store.is_anchored(&trace.id).unwrap());
    }

    #[test]
    fn mark_anchored_is_idempotent() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let trace = make_trace(&id, "anchor-test", Outcome::Succeeded, "idem context");
        store.insert(&trace).unwrap();

        store.mark_anchored(&trace.id, 100, "tx_hash_1").unwrap();
        // Re-anchor with different tx_hash should succeed (REPLACE)
        store.mark_anchored(&trace.id, 200, "tx_hash_2").unwrap();

        assert!(store.is_anchored(&trace.id).unwrap());
    }

    #[test]
    fn unanchored_traces_excludes_anchored() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        let t1 = make_trace(&id, "tool-a", Outcome::Succeeded, "unanchored ctx 1");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = make_trace(&id, "tool-b", Outcome::Succeeded, "unanchored ctx 2");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t3 = make_trace(&id, "tool-c", Outcome::Failed, "unanchored ctx 3");

        store.insert(&t1).unwrap();
        store.insert(&t2).unwrap();
        store.insert(&t3).unwrap();

        // All three should be unanchored
        let unanchored = store.unanchored_traces(24, 100).unwrap();
        assert_eq!(unanchored.len(), 3);

        // Anchor t2
        store.mark_anchored(&t2.id, 50, "some_tx_hash").unwrap();

        // Now only t1 and t3 should be unanchored
        let unanchored = store.unanchored_traces(24, 100).unwrap();
        assert_eq!(unanchored.len(), 2);
        let caps: Vec<&str> = unanchored.iter().map(|t| t.capability.as_str()).collect();
        assert!(caps.contains(&"tool-a"));
        assert!(caps.contains(&"tool-c"));
        assert!(!caps.contains(&"tool-b"));
    }

    #[test]
    fn count_repair_associations_finds_cross_file_patterns() {
        use crate::context::simhash;

        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // Session 1: edit main.rs fails → edit tests.rs succeeds
        let err_s1 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Failed,
            10,
            10,
            simhash("edit file: src/main.rs"),
            Some("edit file: src/main.rs".into()),
            Some("s1".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let fix_s1 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("edit file: tests/perf.rs"),
            Some("edit file: tests/perf.rs".into()),
            Some("s1".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));

        // Session 2: same pattern
        let err_s2 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Failed,
            10,
            10,
            simhash("edit file: src/main.rs"),
            Some("edit file: src/main.rs".into()),
            Some("s2".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let fix_s2 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("edit file: tests/perf.rs"),
            Some("edit file: tests/perf.rs".into()),
            Some("s2".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );

        for t in [&err_s1, &fix_s1, &err_s2, &fix_s2] {
            store.insert(t).unwrap();
        }

        let count = store
            .count_repair_associations(
                &simhash("edit file: src/main.rs"),
                &simhash("edit file: tests/perf.rs"),
                48,
                None,
            )
            .unwrap();
        assert_eq!(
            count, 2,
            "should find 2 sessions with same error→repair pattern"
        );

        // Unrelated context → 0
        let count_unrelated = store
            .count_repair_associations(
                &simhash("bash: cargo build"),
                &simhash("edit file: tests/perf.rs"),
                48,
                None,
            )
            .unwrap();
        assert_eq!(count_unrelated, 0);
    }

    #[test]
    fn count_co_occurring_sessions_finds_hebbian_pairs() {
        use crate::context::simhash;

        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // Session 1: edit A and B both succeed
        let a_s1 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("edit file: src/main.rs"),
            Some("edit file: src/main.rs".into()),
            Some("s1".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b_s1 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("edit file: src/storage/mod.rs"),
            Some("edit file: src/storage/mod.rs".into()),
            Some("s1".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));

        // Session 2: same pair co-edited
        let a_s2 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("edit file: src/main.rs"),
            Some("edit file: src/main.rs".into()),
            Some("s2".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b_s2 = Trace::new(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("edit file: src/storage/mod.rs"),
            Some("edit file: src/storage/mod.rs".into()),
            Some("s2".into()),
            "model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );

        for t in [&a_s1, &b_s1, &a_s2, &b_s2] {
            store.insert(t).unwrap();
        }

        let count = store
            .count_co_occurring_sessions(
                &simhash("edit file: src/main.rs"),
                &simhash("edit file: src/storage/mod.rs"),
                168,
                None,
            )
            .unwrap();
        assert!(
            count >= 2,
            "should find >= 2 co-occurring sessions, got {}",
            count
        );

        // Unrelated pair → 0
        let count_unrelated = store
            .count_co_occurring_sessions(
                &simhash("edit file: src/main.rs"),
                &simhash("bash: cargo build"),
                168,
                None,
            )
            .unwrap();
        assert_eq!(count_unrelated, 0);
    }

    #[test]
    fn unanchored_traces_respects_time_window() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        let recent = make_trace(&id, "recent-tool", Outcome::Succeeded, "recent ctx");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let old = make_trace(&id, "old-tool", Outcome::Succeeded, "old ctx");

        store.insert(&recent).unwrap();
        store.insert(&old).unwrap();

        // Set old trace timestamp to 48 hours ago
        let two_days_ago_ms = chrono::Utc::now().timestamp_millis() - (48 * 3_600_000);
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE traces SET timestamp = ?1 WHERE id = ?2",
                params![two_days_ago_ms, old.id.as_slice()],
            )
            .unwrap();
        }

        // Query for last 24 hours: only the recent trace
        let unanchored = store.unanchored_traces(24, 100).unwrap();
        assert_eq!(unanchored.len(), 1);
        assert_eq!(unanchored[0].capability, "recent-tool");

        // Query for last 72 hours: both traces
        let unanchored = store.unanchored_traces(72, 100).unwrap();
        assert_eq!(unanchored.len(), 2);
    }

    #[test]
    fn unanchored_traces_respects_limit() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        for i in 0..10 {
            let t = make_trace(
                &id,
                &format!("tool-{i}"),
                Outcome::Succeeded,
                &format!("ctx {i}"),
            );
            store.insert(&t).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let unanchored = store.unanchored_traces(24, 3).unwrap();
        assert_eq!(unanchored.len(), 3);
    }

    #[test]
    fn count_repair_sources_counts_distinct_sessions() {
        use crate::context::simhash;

        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        let bash_fail_s1 = Trace::new(
            "claude-code/Bash".into(),
            Outcome::Failed,
            10,
            10,
            simhash("bash: cargo test"),
            Some("bash: cargo test".into()),
            Some("s1".into()),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let read_s1 = Trace::new(
            "claude-code/Read".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("read file: Cargo.toml"),
            Some("read file: Cargo.toml".into()),
            Some("s1".into()),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let bash_ok_s1 = Trace::new(
            "claude-code/Bash".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("bash: cargo test"),
            Some("bash: cargo test".into()),
            Some("s1".into()),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));

        let bash_fail_s2 = Trace::new(
            "claude-code/Bash".into(),
            Outcome::Failed,
            10,
            10,
            simhash("bash: cargo test"),
            Some("bash: cargo test".into()),
            Some("s2".into()),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let read_s2 = Trace::new(
            "claude-code/Read".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("read file: /tmp/other/Cargo.toml"),
            Some("read file: /tmp/other/Cargo.toml".into()),
            Some("s2".into()),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let bash_ok_s2 = Trace::new(
            "claude-code/Bash".into(),
            Outcome::Succeeded,
            10,
            10,
            simhash("bash: cargo test"),
            Some("bash: cargo test".into()),
            Some("s2".into()),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );

        for trace in [
            bash_fail_s1,
            read_s1,
            bash_ok_s1,
            bash_fail_s2,
            read_s2,
            bash_ok_s2,
        ] {
            store.insert(&trace).unwrap();
        }

        let count = store
            .count_repair_sources(
                "Bash",
                &[
                    StepAction::new("Read", Some("Cargo.toml".into())),
                    StepAction::new("Bash", None),
                ],
                24,
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn query_signal_traces_filters_by_kind() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        let avoid = crate::posts::create_signal_trace(
            SignalPostKind::Avoid,
            "repair flaky ci",
            "skip the generated lockfile",
            crate::posts::SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("s1".into()),
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: crate::posts::DEFAULT_SIGNAL_TTL_HOURS,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let recommend = crate::posts::create_signal_trace(
            SignalPostKind::Recommend,
            "repair flaky ci",
            "run release-check first",
            crate::posts::SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("s2".into()),
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: crate::posts::DEFAULT_SIGNAL_TTL_HOURS,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );

        store.insert(&avoid).unwrap();
        store.insert(&recommend).unwrap();

        let target = crate::context::simhash("repair flaky ci");
        let results = store
            .query_signal_traces(&target, Some(SignalPostKind::Avoid), 48, 10, None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].capability, SignalPostKind::Avoid.capability());
    }

    #[test]
    fn query_signal_traces_includes_reinforcement_for_same_kind() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        let signal = crate::posts::create_signal_trace(
            SignalPostKind::Avoid,
            "repair flaky ci",
            "skip the generated lockfile",
            crate::posts::SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("s1".into()),
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: crate::posts::DEFAULT_SIGNAL_TTL_HOURS,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        let reinforcement = crate::posts::create_signal_reinforcement_trace(
            SignalPostKind::Avoid,
            "repair flaky ci",
            "skip the generated lockfile",
            crate::posts::SignalTraceConfig {
                model_id: "thronglets-query".into(),
                session_id: None,
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: crate::posts::DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );

        store.insert(&signal).unwrap();
        store.insert(&reinforcement).unwrap();

        let target = crate::context::simhash("repair flaky ci");
        let results = store
            .query_signal_traces(&target, Some(SignalPostKind::Avoid), 48, 10, None)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .any(|trace| trace.capability == SignalPostKind::Avoid.capability())
        );
        assert!(
            results
                .iter()
                .any(|trace| trace.capability == SignalPostKind::Avoid.reinforcement_capability())
        );
    }

    #[test]
    fn query_recent_signal_traces_respects_time_window() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        let recent = crate::posts::create_signal_trace(
            SignalPostKind::Watch,
            "ship the current branch",
            "run release-check before push",
            crate::posts::SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("recent".into()),
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: crate::posts::DEFAULT_SIGNAL_TTL_HOURS,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&recent).unwrap();

        let old = Trace::new(
            SignalPostKind::Watch.capability(),
            Outcome::Succeeded,
            0,
            10,
            crate::context::simhash("ship the current branch"),
            Some(
                serde_json::json!({
                    "context": "ship the current branch",
                    "message": "old signal",
                    "expires_at": u64::MAX,
                })
                .to_string(),
            ),
            Some("old".into()),
            "codex".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        let mut old = old;
        old.timestamp = old.timestamp.saturating_sub(48 * 3_600_000);
        store.insert(&old).unwrap();

        let results = store
            .query_recent_signal_traces(24, None, 10, None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id.as_deref(), Some("recent"));
    }

    // ── query_similar_failed_traces ──

    #[test]
    fn query_similar_failed_traces_finds_failures() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // A failed trace with known context
        store
            .insert(&make_trace(
                &id,
                "claude-code/Bash",
                Outcome::Failed,
                "bash: ssh -p 22 47.93.32.88",
            ))
            .unwrap();

        // Query with similar context
        let hash = crate::context::simhash("bash: ssh -p 22 47.93.32.88");
        let results = store
            .query_similar_failed_traces(&hash, 48, 168, 10, None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].capability, "claude-code/Bash");
    }

    #[test]
    fn query_similar_failed_traces_excludes_successes() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // A succeeded trace — should NOT appear
        store
            .insert(&make_trace(
                &id,
                "claude-code/Bash",
                Outcome::Succeeded,
                "bash: ssh -p 22 47.93.32.88",
            ))
            .unwrap();

        let hash = crate::context::simhash("bash: ssh -p 22 47.93.32.88");
        let results = store
            .query_similar_failed_traces(&hash, 48, 168, 10, None)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn query_similar_failed_traces_excludes_signals() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // A signal trace (internal) — should NOT appear
        store
            .insert(&make_trace(
                &id,
                "urn:thronglets:signal:avoid",
                Outcome::Failed,
                "bash: ssh -p 22 47.93.32.88",
            ))
            .unwrap();

        let hash = crate::context::simhash("bash: ssh -p 22 47.93.32.88");
        let results = store
            .query_similar_failed_traces(&hash, 48, 168, 10, None)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn query_similar_failed_traces_excludes_old() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // A failed trace, but very old
        let mut trace = make_trace(
            &id,
            "claude-code/Bash",
            Outcome::Failed,
            "bash: ssh -p 22 47.93.32.88",
        );
        trace.timestamp = trace.timestamp.saturating_sub(8 * 24 * 3_600_000); // 8 days ago
        store.insert(&trace).unwrap();

        let hash = crate::context::simhash("bash: ssh -p 22 47.93.32.88");
        let results = store
            .query_similar_failed_traces(&hash, 48, 168, 10, None)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn query_similar_failed_traces_includes_timeout() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // A timeout trace — should appear (outcome IN (1, 3))
        store
            .insert(&make_trace(
                &id,
                "claude-code/Bash",
                Outcome::Timeout,
                "bash: ssh -p 22 47.93.32.88",
            ))
            .unwrap();

        let hash = crate::context::simhash("bash: ssh -p 22 47.93.32.88");
        let results = store
            .query_similar_failed_traces(&hash, 48, 168, 10, None)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn query_similar_failed_traces_filters_dissimilar() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        // A failed trace with completely different context
        store
            .insert(&make_trace(
                &id,
                "claude-code/Bash",
                Outcome::Failed,
                "bash: npm install --save-dev typescript eslint prettier",
            ))
            .unwrap();

        // Query for SSH context — should NOT match
        let hash = crate::context::simhash("bash: ssh -p 22 47.93.32.88");
        let results = store
            .query_similar_failed_traces(&hash, 48, 168, 10, None)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn count_contradicting_failed_sessions_counts_distinct_sessions() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let ctx = "bash: cargo test --workspace";
        for session_id in ["failed-a", "failed-b"] {
            let trace = Trace::new_with_agent(
                "claude-code/Bash".into(),
                Outcome::Failed,
                100,
                5000,
                crate::context::simhash(ctx),
                Some(ctx.into()),
                Some(session_id.into()),
                None,
                Some(id.device_identity()),
                None,
                None,
                "test-model".into(),
                id.public_key_bytes(),
                |m| id.sign(m),
            );
            store.insert(&trace).unwrap();
        }
        let duplicate = Trace::new_with_agent(
            "claude-code/Bash".into(),
            Outcome::Failed,
            100,
            5000,
            crate::context::simhash(ctx),
            Some(ctx.into()),
            Some("failed-a".into()),
            None,
            Some(id.device_identity()),
            None,
            None,
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&duplicate).unwrap();

        let count = store
            .count_contradicting_failed_sessions(&crate::context::simhash(ctx), 48, 168, None)
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn legacy_traces_without_method_compliance_load_as_unknown() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";
        let trace = Trace::new_with_agent(
            "tool:Edit".into(),
            Outcome::Succeeded,
            100,
            5000,
            crate::context::simhash(ctx),
            Some(ctx.into()),
            Some("legacy-success".into()),
            None,
            Some(id.device_identity()),
            None,
            None,
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&trace).unwrap();

        let stats = store
            .residue_stats_for_context(&crate::context::simhash(ctx), 48, 168, 10, None)
            .unwrap();
        assert_eq!(stats.success_unknown, 1);
        assert_eq!(stats.success_compliant, 0);
        assert_eq!(stats.success_noncompliant, 0);
    }

    #[test]
    fn delete_legacy_auto_signal_traces_preserves_raw_traces() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";

        let raw = Trace::new_with_agent(
            "tool:Edit".into(),
            Outcome::Succeeded,
            100,
            5000,
            crate::context::simhash(ctx),
            Some(ctx.into()),
            Some("raw-session".into()),
            None,
            Some(id.device_identity()),
            None,
            None,
            "codex".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&raw).unwrap();

        let legacy_auto = crate::posts::create_signal_trace(
            SignalPostKind::Recommend,
            ctx,
            "stable path: stale auto guidance",
            crate::posts::SignalTraceConfig {
                model_id: crate::posts::AUTO_DERIVED_SIGNAL_MODEL_ID.into(),
                session_id: Some("legacy-auto".into()),
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: 24,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&legacy_auto).unwrap();

        let current_auto = crate::posts::create_auto_signal_trace(
            SignalPostKind::Recommend,
            ctx,
            "stable path: current auto guidance",
            crate::posts::SignalTraceConfig {
                model_id: "ignored".into(),
                session_id: Some("current-auto".into()),
                owner_account: None,
                device_identity: Some(id.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: 24,
            },
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&current_auto).unwrap();

        assert_eq!(store.count_legacy_auto_signal_traces().unwrap(), 1);
        assert_eq!(store.delete_legacy_auto_signal_traces().unwrap(), 1);
        assert_eq!(store.count_legacy_auto_signal_traces().unwrap(), 0);
        assert_eq!(store.count().unwrap(), 2);
        assert_eq!(store.query_capability("tool:Edit", 10).unwrap().len(), 1);
        assert_eq!(
            store
                .query_capability(&SignalPostKind::Recommend.capability(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn residue_stats_split_success_and_failure_by_method_compliance() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";

        let success = Trace::new_with_agent_compliance(
            "tool:Edit".into(),
            Outcome::Succeeded,
            100,
            5000,
            crate::context::simhash(ctx),
            Some(ctx.into()),
            Some("success-compliant".into()),
            None,
            Some(id.device_identity()),
            None,
            None,
            Some(MethodCompliance::Compliant),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&success).unwrap();

        let fail = Trace::new_with_agent_compliance(
            "tool:Edit".into(),
            Outcome::Failed,
            100,
            5000,
            crate::context::simhash(ctx),
            Some(ctx.into()),
            Some("failure-noncompliant".into()),
            None,
            Some(id.device_identity()),
            None,
            None,
            Some(MethodCompliance::Noncompliant),
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        );
        store.insert(&fail).unwrap();

        let stats = store
            .residue_stats_for_context(&crate::context::simhash(ctx), 48, 168, 10, None)
            .unwrap();
        assert_eq!(stats.success_compliant, 1);
        assert_eq!(stats.failure_noncompliant, 1);
        assert_eq!(stats.total_success(), 1);
        assert_eq!(stats.total_failure(), 1);
        assert_eq!(stats.total_noncompliant(), 1);
    }
}
