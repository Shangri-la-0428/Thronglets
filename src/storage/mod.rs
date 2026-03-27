//! Local trace storage backed by SQLite.
//!
//! Each node stores traces locally. No global consensus needed.
//! Traces have TTL — like pheromone evaporation, old signals fade.

use crate::trace::{Outcome, Trace};
use ed25519_dalek::Signature;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

const DEFAULT_TTL_DAYS: i64 = 7;

pub struct TraceStore {
    conn: Mutex<Connection>,
}

impl TraceStore {
    /// Open or create a trace store at the given path.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS traces (
                id              BLOB PRIMARY KEY,
                about           TEXT NOT NULL,
                tags            TEXT NOT NULL,
                outcome         INTEGER NOT NULL,
                latency_ms      INTEGER NOT NULL,
                quality         INTEGER NOT NULL,
                timestamp       INTEGER NOT NULL,
                node_pubkey     BLOB NOT NULL,
                signature       BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_traces_about ON traces(about);
            CREATE INDEX IF NOT EXISTS idx_traces_timestamp ON traces(timestamp);
            CREATE INDEX IF NOT EXISTS idx_traces_tags ON traces(tags);",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Open an in-memory store (for testing).
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    /// Insert a trace. Returns false if duplicate (content-addressed dedup).
    pub fn insert(&self, trace: &Trace) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let tags_json = serde_json::to_string(&trace.tags).unwrap();
        let result = conn.execute(
            "INSERT OR IGNORE INTO traces (id, about, tags, outcome, latency_ms, quality, timestamp, node_pubkey, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                trace.id.as_slice(),
                trace.about,
                tags_json,
                trace.outcome as u8,
                trace.latency_ms,
                trace.quality,
                trace.timestamp as i64,
                trace.node_pubkey.as_slice(),
                trace.signature.to_bytes().as_slice(),
            ],
        )?;
        Ok(result > 0)
    }

    /// Query traces about a specific subject.
    pub fn query_about(&self, about: &str, limit: usize) -> rusqlite::Result<Vec<Trace>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, about, tags, outcome, latency_ms, quality, timestamp, node_pubkey, signature
             FROM traces WHERE about = ?1 ORDER BY timestamp DESC LIMIT ?2",
        )?;
        Self::collect_traces(&mut stmt, params![about, limit as i64])
    }

    /// Query traces matching any of the given tags.
    pub fn query_tags(&self, tags: &[&str], limit: usize) -> rusqlite::Result<Vec<Trace>> {
        if tags.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        // Use parameterized LIKE queries to prevent SQL injection
        let placeholders: Vec<String> = (0..tags.len()).map(|i| format!("tags LIKE ?{}", i + 1)).collect();
        let where_clause = placeholders.join(" OR ");
        let sql = format!(
            "SELECT id, about, tags, outcome, latency_ms, quality, timestamp, node_pubkey, signature
             FROM traces WHERE ({where_clause}) ORDER BY timestamp DESC LIMIT ?{}",
            tags.len() + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = tags
            .iter()
            .map(|t| Box::new(format!("%\"{t}\"%")) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        param_values.push(Box::new(limit as i64));
        let params = rusqlite::params_from_iter(param_values.iter().map(|p| p.as_ref()));
        Self::collect_traces(&mut stmt, params)
    }

    /// Compute aggregate stats for a subject.
    pub fn aggregate(&self, about: &str) -> rusqlite::Result<Option<AggregateStats>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN outcome = 0 THEN 1 ELSE 0 END) as succeeded,
                AVG(latency_ms) as avg_latency,
                AVG(quality) as avg_quality
             FROM traces WHERE about = ?1",
        )?;
        let result = stmt.query_row(params![about], |row| {
            let total: i64 = row.get(0)?;
            if total == 0 {
                return Ok(None);
            }
            Ok(Some(AggregateStats {
                total_traces: total as u64,
                success_rate: row.get::<_, i64>(1)? as f64 / total as f64,
                avg_latency_ms: row.get(2)?,
                avg_quality: row.get(3)?,
            }))
        })?;
        Ok(result)
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

    /// Total trace count.
    pub fn count(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM traces", [], |row| {
            row.get::<_, i64>(0).map(|n| n as u64)
        })
    }

    fn collect_traces(
        stmt: &mut rusqlite::Statement,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<Trace>> {
        let rows = stmt.query_map(params, |row| {
            let id_bytes: Vec<u8> = row.get(0)?;
            let tags_json: String = row.get(2)?;
            let outcome_u8: u8 = row.get(3)?;
            let pubkey_bytes: Vec<u8> = row.get(7)?;
            let sig_bytes: Vec<u8> = row.get(8)?;

            Ok(Trace {
                id: id_bytes.try_into().unwrap_or([0u8; 32]),
                about: row.get(1)?,
                tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                outcome: match outcome_u8 {
                    0 => Outcome::Succeeded,
                    1 => Outcome::Failed,
                    2 => Outcome::Partial,
                    _ => Outcome::Timeout,
                },
                latency_ms: row.get::<_, u32>(4)?,
                quality: row.get::<_, u8>(5)?,
                timestamp: row.get::<_, i64>(6)? as u64,
                node_pubkey: pubkey_bytes.try_into().unwrap_or([0u8; 32]),
                signature: Signature::from_bytes(
                    &sig_bytes.try_into().unwrap_or([0u8; 64]),
                ),
            })
        })?;
        rows.collect()
    }
}

#[derive(Debug, Clone)]
pub struct AggregateStats {
    pub total_traces: u64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub avg_quality: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    fn make_trace(id: &NodeIdentity, about: &str, outcome: Outcome, quality: u8, tags: Vec<String>) -> Trace {
        Trace::new(about.into(), tags, outcome, 100, quality, id.public_key_bytes(), |m| id.sign(m))
    }

    #[test]
    fn insert_and_query() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let trace = make_trace(&id, "tool-a", Outcome::Succeeded, 80, vec!["nlp".into()]);

        assert!(store.insert(&trace).unwrap());
        assert_eq!(store.count().unwrap(), 1);

        let results = store.query_about("tool-a", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].about, "tool-a");
    }

    #[test]
    fn dedup_by_content_address() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();
        let trace = make_trace(&id, "tool-a", Outcome::Succeeded, 80, vec![]);

        assert!(store.insert(&trace).unwrap());
        assert!(!store.insert(&trace).unwrap()); // duplicate
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn aggregate_stats() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        store.insert(&make_trace(&id, "x", Outcome::Succeeded, 90, vec![])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.insert(&make_trace(&id, "x", Outcome::Succeeded, 70, vec![])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.insert(&make_trace(&id, "x", Outcome::Failed, 20, vec![])).unwrap();

        let stats = store.aggregate("x").unwrap().unwrap();
        assert_eq!(stats.total_traces, 3);
        assert!((stats.success_rate - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn query_by_tags() {
        let store = TraceStore::in_memory().unwrap();
        let id = NodeIdentity::generate();

        store.insert(&make_trace(&id, "a", Outcome::Succeeded, 80, vec!["rust".into(), "code".into()])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.insert(&make_trace(&id, "b", Outcome::Succeeded, 80, vec!["python".into()])).unwrap();

        let results = store.query_tags(&["rust"], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].about, "a");
    }

    #[test]
    fn no_traces_returns_none_aggregate() {
        let store = TraceStore::in_memory().unwrap();
        assert!(store.aggregate("nonexistent").unwrap().is_none());
    }
}
