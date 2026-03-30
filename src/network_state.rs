use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const NETWORK_STATUS_FILE: &str = "network-status.v1.json";
const RECENT_BOOTSTRAP_WINDOW_MS: i64 = 15 * 60 * 1000;
const MAX_KNOWN_PEERS: usize = 64;
const MAX_PEER_ADDRESSES: usize = 8;
const MAX_PEER_SEEDS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedPeer {
    pub peer_id: String,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
}

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
    pub peers: Vec<ObservedPeer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_peer_seeds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peer_seeds: Vec<String>,
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
    pub known_peer_count: usize,
    pub trusted_peer_seed_count: usize,
    pub peer_seed_count: usize,
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

    pub fn mark_bootstrap_start(&mut self, bootstrap_targets: usize) {
        let now = now_ms();
        self.updated_at_ms = now;
        self.bootstrap_targets = bootstrap_targets;
        self.last_bootstrap_contact_at_ms = (bootstrap_targets > 0).then_some(now);
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

    pub fn mark_peer_connected(&mut self, peer_id: impl Into<String>, connected_peers: usize) {
        let now = now_ms();
        self.updated_at_ms = now;
        self.peer_count = connected_peers;
        self.direct_peer_count = connected_peers;
        self.relay_peer_count = 0;
        self.last_peer_connected_at_ms = Some(now);
        self.touch_peer(peer_id.into(), now, true);
    }

    pub fn mark_peer_disconnected(&mut self, peer_id: &str, connected_peers: usize) {
        let now = now_ms();
        self.updated_at_ms = now;
        self.peer_count = connected_peers;
        self.direct_peer_count = connected_peers;
        self.relay_peer_count = 0;
        self.touch_peer(peer_id.to_string(), now, false);
    }

    pub fn observe_peer_address(&mut self, peer_id: impl Into<String>, address: impl Into<String>) {
        let peer_id = peer_id.into();
        let address = address.into();
        let now = now_ms();
        self.updated_at_ms = now;
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.peer_id == peer_id) {
            peer.last_seen_at_ms = now;
            push_unique_front(&mut peer.addresses, address, MAX_PEER_ADDRESSES);
            return;
        }

        let mut addresses = Vec::new();
        push_unique_front(&mut addresses, address, MAX_PEER_ADDRESSES);
        self.peers.push(ObservedPeer {
            peer_id,
            first_seen_at_ms: now,
            last_seen_at_ms: now,
            connected: false,
            addresses,
        });
        self.peers
            .sort_by(|a, b| b.last_seen_at_ms.cmp(&a.last_seen_at_ms));
        self.peers.truncate(MAX_KNOWN_PEERS);
    }

    pub fn merge_peer_seeds<I>(&mut self, seeds: I)
    where
        I: IntoIterator<Item = String>,
    {
        let now = now_ms();
        self.updated_at_ms = now;
        for seed in seeds {
            let trimmed = seed.trim();
            if trimmed.is_empty()
                || self
                    .trusted_peer_seeds
                    .iter()
                    .any(|existing| existing == trimmed)
            {
                continue;
            }
            push_unique_front(&mut self.peer_seeds, trimmed.to_string(), MAX_PEER_SEEDS);
        }
    }

    pub fn merge_trusted_peer_seeds<I>(&mut self, seeds: I)
    where
        I: IntoIterator<Item = String>,
    {
        let now = now_ms();
        self.updated_at_ms = now;
        for seed in seeds {
            let trimmed = seed.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(index) = self
                .peer_seeds
                .iter()
                .position(|existing| existing == trimmed)
            {
                self.peer_seeds.remove(index);
            }
            push_unique_front(
                &mut self.trusted_peer_seeds,
                trimmed.to_string(),
                MAX_PEER_SEEDS,
            );
        }
    }

    pub fn peer_seed_addresses(&self, limit: usize) -> Vec<String> {
        let mut seeds = self.peer_seeds.clone();
        for peer in &self.peers {
            for address in peer.addresses.iter().rev() {
                push_unique_front(&mut seeds, address.clone(), MAX_PEER_SEEDS.max(limit));
            }
        }
        for seed in self.trusted_peer_seeds.iter().rev() {
            push_unique_front(&mut seeds, seed.clone(), MAX_PEER_SEEDS.max(limit));
        }
        seeds.truncate(limit);
        seeds
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
            known_peer_count: self.peers.len(),
            trusted_peer_seed_count: self.trusted_peer_seeds.len(),
            peer_seed_count: self.trusted_peer_seeds.len() + self.peer_seeds.len(),
            last_peer_connected_age_ms: age(now, self.last_peer_connected_at_ms),
            last_trace_received_age_ms: age(now, self.last_trace_received_at_ms),
            last_bootstrap_contact_age_ms: age(now, self.last_bootstrap_contact_at_ms),
        }
    }

    fn touch_peer(&mut self, peer_id: String, now: i64, connected: bool) {
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.peer_id == peer_id) {
            peer.last_seen_at_ms = now;
            peer.connected = connected;
            return;
        }

        self.peers.push(ObservedPeer {
            peer_id,
            first_seen_at_ms: now,
            last_seen_at_ms: now,
            connected,
            addresses: Vec::new(),
        });
        self.peers
            .sort_by(|a, b| b.last_seen_at_ms.cmp(&a.last_seen_at_ms));
        self.peers.truncate(MAX_KNOWN_PEERS);
    }
}

fn push_unique_front(values: &mut Vec<String>, value: String, limit: usize) {
    if let Some(index) = values.iter().position(|existing| existing == &value) {
        values.remove(index);
    }
    values.insert(0, value);
    values.truncate(limit);
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
        snapshot.mark_peer_connected("peer-a", 2);
        let status = snapshot.to_status();
        assert_eq!(status.activity, "connected");
        assert_eq!(status.transport_mode, "direct");
        assert_eq!(status.vps_dependency_level, "peer-native");
    }

    #[test]
    fn snapshot_tracks_known_peers() {
        let mut snapshot = NetworkSnapshot::begin(1);
        snapshot.mark_peer_connected("peer-a", 1);
        snapshot.mark_peer_connected("peer-b", 2);
        snapshot.mark_peer_disconnected("peer-a", 1);

        assert_eq!(snapshot.peers.len(), 2);
        let peer_a = snapshot
            .peers
            .iter()
            .find(|peer| peer.peer_id == "peer-a")
            .unwrap();
        let peer_b = snapshot
            .peers
            .iter()
            .find(|peer| peer.peer_id == "peer-b")
            .unwrap();
        assert!(!peer_a.connected);
        assert!(peer_b.connected);
    }

    #[test]
    fn snapshot_tracks_peer_seed_addresses() {
        let mut snapshot = NetworkSnapshot::begin(1);
        snapshot.merge_peer_seeds([
            "/ip4/10.0.0.1/tcp/4001".to_string(),
            "/ip4/10.0.0.2/tcp/4001".to_string(),
        ]);
        snapshot.merge_trusted_peer_seeds(["/ip4/10.0.0.42/tcp/4001".to_string()]);
        snapshot.observe_peer_address("peer-a", "/ip4/10.0.0.3/tcp/4001");
        snapshot.observe_peer_address("peer-a", "/ip4/10.0.0.4/tcp/4001");
        snapshot.observe_peer_address("peer-b", "/ip4/10.0.0.5/tcp/4001");

        let status = snapshot.to_status();
        assert_eq!(status.trusted_peer_seed_count, 1);
        assert_eq!(status.peer_seed_count, 3);

        let seeds = snapshot.peer_seed_addresses(4);
        assert_eq!(seeds.len(), 4);
        assert_eq!(seeds[0], "/ip4/10.0.0.42/tcp/4001");
    }

    #[test]
    fn trusted_peer_seeds_take_priority_over_generic_seeds() {
        let mut snapshot = NetworkSnapshot::begin(1);
        snapshot.merge_peer_seeds([
            "/ip4/10.0.0.1/tcp/4001".to_string(),
            "/ip4/10.0.0.2/tcp/4001".to_string(),
        ]);
        snapshot.merge_trusted_peer_seeds([
            "/ip4/10.0.0.9/tcp/4001".to_string(),
            "/ip4/10.0.0.8/tcp/4001".to_string(),
        ]);

        let seeds = snapshot.peer_seed_addresses(4);
        assert_eq!(seeds[0], "/ip4/10.0.0.8/tcp/4001");
        assert_eq!(seeds[1], "/ip4/10.0.0.9/tcp/4001");
    }
}
