use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const NETWORK_STATUS_FILE: &str = "network-status.v1.json";
const RECENT_BOOTSTRAP_WINDOW_MS: i64 = 15 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkSnapshot {
    pub updated_at_ms: i64,
    pub bootstrap_targets: usize,
    pub last_bootstrap_contact_at_ms: Option<i64>,
    pub peer_count: usize,
    pub direct_peer_count: usize,
    pub relay_peer_count: usize,
    pub last_peer_connected_at_ms: Option<i64>,
    pub last_trace_received_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    pub activity: &'static str,
    pub transport_mode: &'static str,
    pub vps_dependency_level: &'static str,
    pub peer_count: usize,
    pub direct_peer_count: usize,
    pub relay_peer_count: usize,
    pub bootstrap_targets: usize,
    pub bootstrap_contacted_recently: bool,
    pub last_peer_connected_age_ms: Option<i64>,
    pub last_trace_received_age_ms: Option<i64>,
    pub last_bootstrap_contact_age_ms: Option<i64>,
}

impl NetworkSnapshot {
    pub fn status_path(data_dir: &Path) -> PathBuf {
        data_dir.join(NETWORK_STATUS_FILE)
    }

    pub fn begin(bootstrap_targets: usize) -> Self {
        let now = now_ms();
        Self {
            updated_at_ms: now,
            bootstrap_targets,
            last_bootstrap_contact_at_ms: (bootstrap_targets > 0).then_some(now),
            ..Self::default()
        }
    }

    pub fn load(data_dir: &Path) -> Self {
        let path = Self::status_path(data_dir);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) {
        let path = Self::status_path(data_dir);
        if let Ok(raw) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, raw);
        }
    }

    pub fn mark_peer_connected(&mut self, connected_peers: usize) {
        let now = now_ms();
        self.updated_at_ms = now;
        self.peer_count = connected_peers;
        self.direct_peer_count = connected_peers;
        self.relay_peer_count = 0;
        self.last_peer_connected_at_ms = Some(now);
    }

    pub fn mark_peer_disconnected(&mut self, connected_peers: usize) {
        let now = now_ms();
        self.updated_at_ms = now;
        self.peer_count = connected_peers;
        self.direct_peer_count = connected_peers;
        self.relay_peer_count = 0;
    }

    pub fn mark_trace_received(&mut self) {
        let now = now_ms();
        self.updated_at_ms = now;
        self.last_trace_received_at_ms = Some(now);
    }

    pub fn to_status(&self) -> NetworkStatus {
        let now = now_ms();
        let bootstrap_contacted_recently = self
            .last_bootstrap_contact_at_ms
            .is_some_and(|ts| now - ts <= RECENT_BOOTSTRAP_WINDOW_MS);
        let activity = if self.peer_count > 0 {
            "connected"
        } else if bootstrap_contacted_recently {
            "bootstrapping"
        } else {
            "offline"
        };
        let transport_mode = if self.direct_peer_count > 0 && self.relay_peer_count > 0 {
            "mixed"
        } else if self.direct_peer_count > 0 {
            "direct"
        } else if self.relay_peer_count > 0 {
            "relayed"
        } else {
            "offline"
        };
        let vps_dependency_level = if self.bootstrap_targets == 0 {
            if self.peer_count > 0 {
                "peer-native"
            } else {
                "offline"
            }
        } else {
            match self.peer_count {
                0 => "bootstrap-only",
                1 => "high",
                2 => "medium",
                _ => "low",
            }
        };

        NetworkStatus {
            activity,
            transport_mode,
            vps_dependency_level,
            peer_count: self.peer_count,
            direct_peer_count: self.direct_peer_count,
            relay_peer_count: self.relay_peer_count,
            bootstrap_targets: self.bootstrap_targets,
            bootstrap_contacted_recently,
            last_peer_connected_age_ms: age(now, self.last_peer_connected_at_ms),
            last_trace_received_age_ms: age(now, self.last_trace_received_at_ms),
            last_bootstrap_contact_age_ms: age(now, self.last_bootstrap_contact_at_ms),
        }
    }
}

fn age(now: i64, ts: Option<i64>) -> Option<i64> {
    ts.map(|value| now.saturating_sub(value))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::NetworkSnapshot;

    #[test]
    fn status_defaults_to_offline_without_snapshot() {
        let status = NetworkSnapshot::default().to_status();
        assert_eq!(status.activity, "offline");
        assert_eq!(status.transport_mode, "offline");
        assert_eq!(status.vps_dependency_level, "offline");
        assert_eq!(status.peer_count, 0);
    }

    #[test]
    fn bootstrap_only_status_is_detected() {
        let snapshot = NetworkSnapshot::begin(1);
        let status = snapshot.to_status();
        assert_eq!(status.activity, "bootstrapping");
        assert_eq!(status.vps_dependency_level, "bootstrap-only");
        assert!(status.bootstrap_contacted_recently);
    }

    #[test]
    fn peer_native_status_is_detected_without_bootstrap() {
        let mut snapshot = NetworkSnapshot::begin(0);
        snapshot.mark_peer_connected(2);
        let status = snapshot.to_status();
        assert_eq!(status.activity, "connected");
        assert_eq!(status.transport_mode, "direct");
        assert_eq!(status.vps_dependency_level, "peer-native");
    }
}
