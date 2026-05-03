//! Field tail: the daemon-side loop that materializes the pheromone field
//! from the trace store.
//!
//! Architectural intent: the trace store (SQLite) is the **single source of
//! truth** for trace events. The pheromone field is a **derived view** —
//! a function of (trace history × decay × physics constants). Any process
//! can write a trace (hooks via direct SQLite insert, MCP via service path);
//! the tail picks them up exactly once and excites the live in-memory field.
//!
//! Before this module existed, the hook path bypassed field excitation
//! entirely (hooks insert to SQLite but the daemon's in-memory field never
//! saw those traces). The tail closes that gap by making excitation a
//! function of the store, not of the write path.
//!
//! Cursor: persisted at `<data_dir>/field-tail.cursor.json`. Holds the
//! `timestamp_ms` of the last trace whose excitation has been applied to
//! the field. On boot, daemon should seed the cursor from `hydrate_from_store`
//! so we don't double-excite traces hydrate just replayed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::pheromone::PheromoneField;
use crate::storage::TraceStore;

const CURSOR_FILE: &str = "field-tail.cursor.json";
const CURSOR_VERSION: u32 = 1;
const POLL_BATCH_LIMIT: usize = 1000;
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(serde::Serialize, serde::Deserialize)]
struct CursorFile {
    version: u32,
    last_processed_timestamp_ms: u64,
}

pub struct FieldTail {
    field: Arc<PheromoneField>,
    store: Arc<TraceStore>,
    cursor_path: PathBuf,
    cursor: AtomicU64,
    /// Local node's identity pubkey. Traces signed by this key are local
    /// (full physics excitation: Concrete + Project + Typed + Universal).
    /// Traces with any other pubkey are remote and were already excited
    /// abstract-only by the network ingest path; the tail must skip them
    /// to avoid double-counting.
    local_node_pubkey: [u8; 32],
}

impl FieldTail {
    /// Construct a tail. Loads cursor from disk if present, else starts at 0.
    pub fn new(
        field: Arc<PheromoneField>,
        store: Arc<TraceStore>,
        data_dir: &Path,
        local_node_pubkey: [u8; 32],
    ) -> Self {
        let cursor_path = data_dir.join(CURSOR_FILE);
        let cursor = read_cursor(&cursor_path).unwrap_or(0);
        Self {
            field,
            store,
            cursor_path,
            cursor: AtomicU64::new(cursor),
            local_node_pubkey,
        }
    }

    /// Seed the cursor (used when daemon boots and `hydrate_from_store`
    /// already excited everything up through `ts_ms`). Only advances forward.
    pub fn seed_cursor(&self, ts_ms: u64) {
        let prev = self.cursor.load(Ordering::SeqCst);
        if ts_ms > prev {
            self.cursor.store(ts_ms, Ordering::SeqCst);
            let _ = write_cursor(&self.cursor_path, ts_ms);
        }
    }

    pub fn current_cursor(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }

    /// Process all pending traces. Returns the number excited.
    pub fn poll_once(&self) -> usize {
        let cursor = self.current_cursor();
        // SQLite stores timestamps as INTEGER (i64). Cursor is u64 in
        // application space (matches `Trace::timestamp`); convert at the
        // boundary. Saturating cast keeps the query well-formed even at
        // pathological values.
        let cursor_i64 = cursor.try_into().unwrap_or(i64::MAX);
        let traces = match self.store.traces_after(cursor_i64, POLL_BATCH_LIMIT) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "field tail: traces_after failed");
                return 0;
            }
        };
        if traces.is_empty() {
            return 0;
        }
        let mut max_ts = cursor;
        let n = traces.len();
        for (trace, space) in traces {
            // Only excite for local traces. Remote traces were already
            // excited (abstract-only) by network_runtime when received,
            // and re-exciting here would double-count.
            if trace.node_pubkey == self.local_node_pubkey {
                self.field.excite_with_space(&trace, space.as_deref());
            }
            if trace.timestamp > max_ts {
                max_ts = trace.timestamp;
            }
        }
        if max_ts > cursor {
            self.cursor.store(max_ts, Ordering::SeqCst);
            let _ = write_cursor(&self.cursor_path, max_ts);
        }
        n
    }

    /// Drain the entire backlog (loops `poll_once` until it returns 0).
    /// Useful at boot to catch up any traces that arrived while the daemon
    /// was down.
    pub fn drain(&self) -> usize {
        let mut total = 0;
        loop {
            let n = self.poll_once();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }

    /// Spawn a background task that polls at `interval`. Returns the
    /// JoinHandle so the caller can hold the lifetime; the task ends only
    /// when the runtime shuts down.
    pub fn spawn(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let n = self.poll_once();
                if n > 0 {
                    tracing::debug!(n, cursor = self.current_cursor(), "field tail polled");
                }
            }
        })
    }
}

fn read_cursor(path: &Path) -> Option<u64> {
    let data = std::fs::read_to_string(path).ok()?;
    let parsed: CursorFile = serde_json::from_str(&data).ok()?;
    if parsed.version != CURSOR_VERSION {
        return None;
    }
    Some(parsed.last_processed_timestamp_ms)
}

fn write_cursor(path: &Path, ts_ms: u64) -> std::io::Result<()> {
    let cursor = CursorFile {
        version: CURSOR_VERSION,
        last_processed_timestamp_ms: ts_ms,
    };
    let data = serde_json::to_string(&cursor).expect("serialize cursor");
    std::fs::write(path, data)
}

/// Reset cursor + remove the persisted field snapshot. Used by the
/// `rebuild-field` CLI to force a clean replay from store on next daemon
/// boot. Returns true if anything was removed.
pub fn reset_field_state(data_dir: &Path) -> bool {
    let cursor_path = data_dir.join(CURSOR_FILE);
    let field_path = data_dir.join("pheromone-field.v1.json");
    let cursor_removed = std::fs::remove_file(&cursor_path).is_ok();
    let field_removed = std::fs::remove_file(&field_path).is_ok();
    cursor_removed || field_removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::trace::{Outcome, Trace};
    use ed25519_dalek::{Signer, SigningKey};
    use tempfile::tempdir;

    /// Test seed → fixed signing key → fixed pubkey. The local pubkey for
    /// most tests is seed=1 unless otherwise stated.
    fn key_for(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubkey_for(seed: u8) -> [u8; 32] {
        key_for(seed).verifying_key().to_bytes()
    }

    fn make_trace(capability: &str, context: &str, ts_ms: u64, seed: u8) -> Trace {
        let key = key_for(seed);
        let mut t = Trace::new(
            capability.to_string(),
            Outcome::Succeeded,
            10,
            0,
            simhash(context),
            Some(context.to_string()),
            Some("test-session".to_string()),
            "test-model".to_string(),
            key.verifying_key().to_bytes(),
            |bytes| key.sign(bytes),
        );
        t.timestamp = ts_ms;
        t
    }

    const LOCAL_SEED: u8 = 1;
    const REMOTE_SEED: u8 = 99;

    #[test]
    fn tail_picks_up_new_traces_and_advances_cursor() {
        let dir = tempdir().unwrap();
        let store = Arc::new(TraceStore::open(&dir.path().join("traces.db")).unwrap());
        let field = Arc::new(PheromoneField::new());
        let tail = FieldTail::new(
            Arc::clone(&field),
            Arc::clone(&store),
            dir.path(),
            pubkey_for(LOCAL_SEED),
        );

        // No traces yet — cursor stays at 0
        assert_eq!(tail.poll_once(), 0);
        assert_eq!(tail.current_cursor(), 0);

        // Insert a local trace (signed by LOCAL_SEED)
        let t1 = make_trace("tool:read", "alpha", 1_000, LOCAL_SEED);
        store.insert_with_space(&t1, Some("space-a")).unwrap();

        // Tail picks it up
        assert_eq!(tail.poll_once(), 1);
        assert_eq!(tail.current_cursor(), 1_000);
        let points_after_first = field.len();
        assert!(points_after_first > 0, "field should have points after excitation");

        // Insert another, older — must NOT be re-processed (cursor blocks)
        let t_old = make_trace("tool:edit", "beta", 500, LOCAL_SEED);
        store.insert_with_space(&t_old, Some("space-a")).unwrap();
        assert_eq!(tail.poll_once(), 0, "older trace must be skipped by cursor");

        // Insert newer — picked up
        let t2 = make_trace("tool:edit", "beta", 2_000, LOCAL_SEED);
        store.insert_with_space(&t2, Some("space-a")).unwrap();
        assert_eq!(tail.poll_once(), 1);
        assert_eq!(tail.current_cursor(), 2_000);
    }

    #[test]
    fn tail_advances_cursor_for_remote_traces_but_does_not_excite() {
        // Remote traces (different pubkey) must NOT be re-excited by the tail
        // — the network ingest path already excited them abstract-only.
        // But the cursor must still advance so the tail doesn't replay them.
        let dir = tempdir().unwrap();
        let store = Arc::new(TraceStore::open(&dir.path().join("traces.db")).unwrap());
        let field = Arc::new(PheromoneField::new());
        let tail = FieldTail::new(
            Arc::clone(&field),
            Arc::clone(&store),
            dir.path(),
            pubkey_for(LOCAL_SEED),
        );

        let baseline_points = field.len();

        // Remote trace (signed by a different key)
        let remote = make_trace("tool:read", "remote-ctx", 5_000, REMOTE_SEED);
        store.insert_with_space(&remote, Some("space-r")).unwrap();

        let processed = tail.poll_once();
        assert_eq!(processed, 1, "tail should process the row");
        assert_eq!(
            tail.current_cursor(),
            5_000,
            "cursor must advance even for skipped remote traces"
        );
        assert_eq!(
            field.len(),
            baseline_points,
            "remote trace must not produce new field points via tail"
        );
    }

    #[test]
    fn cursor_persists_across_tail_restart() {
        let dir = tempdir().unwrap();
        let store = Arc::new(TraceStore::open(&dir.path().join("traces.db")).unwrap());

        let t1 = make_trace("tool:read", "alpha", 1_500, LOCAL_SEED);
        store.insert_with_space(&t1, Some("space-a")).unwrap();

        // First tail processes the trace and persists cursor
        {
            let field = Arc::new(PheromoneField::new());
            let tail = FieldTail::new(
                Arc::clone(&field),
                Arc::clone(&store),
                dir.path(),
                pubkey_for(LOCAL_SEED),
            );
            assert_eq!(tail.poll_once(), 1);
            assert_eq!(tail.current_cursor(), 1_500);
        }

        // Second tail (fresh field, same data dir) picks up cursor from disk
        let field = Arc::new(PheromoneField::new());
        let tail2 = FieldTail::new(
            Arc::clone(&field),
            Arc::clone(&store),
            dir.path(),
            pubkey_for(LOCAL_SEED),
        );
        assert_eq!(
            tail2.current_cursor(),
            1_500,
            "cursor must persist across tail restart"
        );
        assert_eq!(tail2.poll_once(), 0, "no new traces after restart");
    }

    #[test]
    fn seed_cursor_only_advances_forward() {
        let dir = tempdir().unwrap();
        let store = Arc::new(TraceStore::open(&dir.path().join("traces.db")).unwrap());
        let field = Arc::new(PheromoneField::new());
        let tail = FieldTail::new(
            Arc::clone(&field),
            Arc::clone(&store),
            dir.path(),
            pubkey_for(LOCAL_SEED),
        );

        tail.seed_cursor(5_000);
        assert_eq!(tail.current_cursor(), 5_000);
        // Lower seed value must not move cursor backward
        tail.seed_cursor(1_000);
        assert_eq!(tail.current_cursor(), 5_000);
        // Higher seed advances
        tail.seed_cursor(10_000);
        assert_eq!(tail.current_cursor(), 10_000);
    }

    #[test]
    fn drain_processes_full_backlog() {
        let dir = tempdir().unwrap();
        let store = Arc::new(TraceStore::open(&dir.path().join("traces.db")).unwrap());
        let field = Arc::new(PheromoneField::new());
        let tail = FieldTail::new(
            Arc::clone(&field),
            Arc::clone(&store),
            dir.path(),
            pubkey_for(LOCAL_SEED),
        );

        // Insert a backlog of local traces (all signed by LOCAL_SEED so the
        // tail excites every one). Tests that drain processes the whole queue
        // even if it spans multiple `poll_once` batches.
        for i in 0..50 {
            let t = make_trace(
                if i % 2 == 0 { "tool:read" } else { "tool:edit" },
                &format!("ctx-{i}"),
                10_000 + i as u64,
                LOCAL_SEED,
            );
            store.insert_with_space(&t, Some("space-a")).unwrap();
        }
        let total = tail.drain();
        assert_eq!(total, 50);
        assert_eq!(tail.current_cursor(), 10_049);
        assert!(field.len() > 0, "drained traces must produce field points");
    }

    #[test]
    fn reset_field_state_removes_cursor_and_snapshot() {
        let dir = tempdir().unwrap();
        // Write fake cursor + field snapshot
        std::fs::write(
            dir.path().join("field-tail.cursor.json"),
            r#"{"version":1,"last_processed_timestamp_ms":42}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("pheromone-field.v1.json"), "{}").unwrap();
        assert!(reset_field_state(dir.path()));
        assert!(!dir.path().join("field-tail.cursor.json").exists());
        assert!(!dir.path().join("pheromone-field.v1.json").exists());
        // Idempotent: second reset returns false (nothing to remove)
        assert!(!reset_field_state(dir.path()));
    }
}
