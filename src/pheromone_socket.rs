//! Unix socket IPC for pheromone field queries.
//!
//! Long-running processes (MCP, HTTP, run) own a live PheromoneField.
//! Short-lived processes (prehook) query it via Unix domain socket
//! instead of loading a stale JSON snapshot from disk.
//!
//! Protocol: one JSON line in, one JSON line out, then close.
//! Socket path: `{data_dir}/field.sock`

use crate::pheromone::{FieldScan, PheromoneField};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tracing::{debug, warn};

/// Socket filename inside the data directory.
const SOCKET_NAME: &str = "field.sock";

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOCKET_NAME)
}

// ── Protocol ────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ScanRequest {
    pub context_hash: [u8; 16],
    pub space: Option<String>,
    pub file_path: Option<String>,
    pub limit: usize,
}

// ── Server (long-running process) ───────────────────────────

/// Start listening on the field socket. Returns a handle that cleans up
/// the socket file when dropped. Runs until the tokio runtime shuts down.
pub fn start_listener(field: Arc<PheromoneField>, data_dir: &Path) -> SocketGuard {
    #[cfg(unix)]
    {
        return start_unix_listener(field, data_dir);
    }

    #[cfg(not(unix))]
    {
        let _ = (field, data_dir);
        SocketGuard(None)
    }
}

#[cfg(unix)]
fn start_unix_listener(field: Arc<PheromoneField>, data_dir: &Path) -> SocketGuard {
    let path = socket_path(data_dir);

    // Remove stale socket from a previous crash
    let _ = std::fs::remove_file(&path);

    let guard = SocketGuard(Some(path.clone()));

    tokio::spawn(async move {
        let listener = match tokio::net::UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                warn!(%e, "Failed to bind field socket");
                return;
            }
        };
        debug!(path = %path.display(), "Field socket listening");

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => break,
            };

            let field = Arc::clone(&field);
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();

                if let Ok(Some(line)) = lines.next_line().await
                    && let Ok(req) = serde_json::from_str::<ScanRequest>(&line)
                {
                    let scans = field.scan_with_fallback(
                        &req.context_hash,
                        req.space.as_deref(),
                        req.file_path.as_deref(),
                        req.limit,
                    );

                    if let Ok(mut json) = serde_json::to_vec(&scans) {
                        json.push(b'\n');
                        let _ = writer.write_all(&json).await;
                    }
                }
            });
        }
    });

    guard
}

/// Cleans up the socket file on drop.
pub struct SocketGuard(Option<PathBuf>);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ── Client (prehook) ────────────────────────────────────────

/// Query the field via Unix socket. Returns None if the socket
/// is not available or the query times out (~50ms budget).
pub fn query(data_dir: &Path, request: &ScanRequest) -> Option<Vec<FieldScan>> {
    #[cfg(unix)]
    {
        return query_unix(data_dir, request);
    }

    #[cfg(not(unix))]
    {
        let _ = (data_dir, request);
        None
    }
}

#[cfg(unix)]
fn query_unix(data_dir: &Path, request: &ScanRequest) -> Option<Vec<FieldScan>> {
    let path = socket_path(data_dir);
    if !path.exists() {
        return None;
    }

    // Synchronous connect + write + read with tight timeout.
    // The prehook is not async, so we use std::os::unix::net.
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let stream = UnixStream::connect(&path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(50)))
        .ok()?;

    let mut stream = std::io::BufWriter::new(stream);
    let mut json = serde_json::to_vec(request).ok()?;
    json.push(b'\n');
    stream.write_all(&json).ok()?;
    stream.flush().ok()?;

    let inner = stream.into_inner().ok()?;
    let mut reader = std::io::BufReader::new(inner);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;

    serde_json::from_str(&line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::pheromone::PheromoneField;
    use crate::trace::{Outcome, Trace};

    fn make_trace(capability: &str, context: &str) -> Trace {
        let identity = crate::identity::NodeIdentity::generate();
        Trace::new(
            capability.into(),
            Outcome::Succeeded,
            10,
            1,
            simhash(context),
            Some(context.into()),
            Some("s1".into()),
            "test".into(),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        )
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let field = Arc::new(PheromoneField::new());

        // Excite with some traces
        let trace = make_trace("claude-code/Edit", "edit src/main.rs in Desktop/Thronglets");
        field.excite(&trace);

        let _guard = start_listener(Arc::clone(&field), tmp.path());

        let ctx_hash = simhash("edit src/main.rs in Desktop/Thronglets");
        let request = ScanRequest {
            context_hash: ctx_hash,
            space: Some("Desktop/Thronglets".into()),
            file_path: Some("src/main.rs".into()),
            limit: 5,
        };

        // Retry until the listener binds (up to 500ms)
        let mut results = None;
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(r) = query(tmp.path(), &request) {
                results = Some(r);
                break;
            }
        }
        assert!(results.is_some(), "socket query should succeed");
        let results = results.unwrap();
        assert!(!results.is_empty(), "should return field scan results");
        assert_eq!(results[0].capability, "tool:edit");
    }

    #[test]
    fn query_returns_none_when_no_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let request = ScanRequest {
            context_hash: [0; 16],
            space: None,
            file_path: None,
            limit: 3,
        };
        assert!(query(tmp.path(), &request).is_none());
    }
}
