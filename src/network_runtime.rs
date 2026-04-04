use crate::continuity::is_continuity_capability;
use crate::identity::{IdentityBinding, NodeIdentity};
use crate::network::{NetworkCommand, NetworkConfig, NetworkEvent};
use crate::network_state::NetworkSnapshot;
use crate::pheromone::PheromoneField;
use crate::posts::is_signal_capability;
use crate::presence::is_presence_capability;
use crate::storage::TraceStore;
use crate::trace::Trace;
use libp2p::Multiaddr;
use std::error::Error;
use std::future::pending;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn};

const FIRST_CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const FIRST_CONNECTION_GRACE_AFTER_CONNECT: Duration = Duration::from_secs(2);
const DEFAULT_PUBLIC_BOOTSTRAP_SEEDS: &[&str] = &["/ip4/47.93.32.88/tcp/4001"];

#[derive(Clone, Copy, Debug, Default)]
pub struct InitialConnectionAttempt {
    pub connected_once: bool,
    pub trusted_same_owner_ready: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct NetworkRuntimeOptions {
    pub publish_summaries: bool,
    pub publish_local_traces: bool,
}

impl NetworkRuntimeOptions {
    pub fn node() -> Self {
        Self {
            publish_summaries: true,
            publish_local_traces: true,
        }
    }

    pub fn embedded() -> Self {
        Self {
            publish_summaries: false,
            publish_local_traces: false,
        }
    }

    /// Participant mode: publishes local traces to the network but does not
    /// publish DHT capability summaries.  This is the right mode for MCP and
    /// HTTP servers — they contribute to the collective without taking on
    /// the overhead of full DHT publishing.
    pub fn participant() -> Self {
        Self {
            publish_summaries: false,
            publish_local_traces: true,
        }
    }
}

pub struct NetworkRuntimeRequest<'a> {
    pub data_dir: &'a Path,
    pub identity: &'a NodeIdentity,
    pub binding: &'a IdentityBinding,
    pub store: Arc<TraceStore>,
    pub field: Option<Arc<PheromoneField>>,
    pub listen_port: u16,
    pub bootstrap: &'a [String],
    pub options: NetworkRuntimeOptions,
}

struct NetworkRuntimeLoop {
    data_dir: PathBuf,
    binding: IdentityBinding,
    store: Arc<TraceStore>,
    field: Option<Arc<PheromoneField>>,
    network_snapshot: NetworkSnapshot,
    event_rx: mpsc::Receiver<NetworkEvent>,
    command_tx: mpsc::Sender<NetworkCommand>,
    options: NetworkRuntimeOptions,
}

pub async fn start_network_runtime(
    request: NetworkRuntimeRequest<'_>,
) -> Result<mpsc::Sender<NetworkCommand>, Box<dyn Error>> {
    let mut network_snapshot = NetworkSnapshot::load(request.data_dir);
    if !request.bootstrap.is_empty() {
        network_snapshot.remember_bootstrap_seeds(request.bootstrap.iter().cloned());
    }
    let effective_bootstrap = effective_bootstrap_seeds(request.bootstrap, &network_snapshot);
    if !effective_bootstrap.is_empty() {
        network_snapshot.remember_bootstrap_seeds(effective_bootstrap.iter().cloned());
    }
    network_snapshot.configure_bootstrap(effective_bootstrap.len());
    network_snapshot.save(request.data_dir);

    let libp2p_keypair =
        libp2p::identity::Keypair::ed25519_from_bytes(&mut request.identity.secret_key_bytes())
            .map_err(|error| format!("failed to create libp2p keypair: {error}"))?;

    let bootstrap_addrs: Vec<Multiaddr> = effective_bootstrap
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let trusted_peer_addrs: Vec<Multiaddr> = network_snapshot
        .trusted_peer_seed_addresses(8)
        .into_iter()
        .filter_map(|address| address.parse().ok())
        .collect();
    let known_peer_addrs: Vec<Multiaddr> = network_snapshot
        .remembered_peer_addresses(16)
        .into_iter()
        .filter_map(|address| address.parse().ok())
        .collect();

    let config = NetworkConfig {
        listen_port: request.listen_port,
        bootstrap_peers: bootstrap_addrs,
        trusted_peers: trusted_peer_addrs,
        known_peers: known_peer_addrs,
    };

    let (command_tx, event_rx) = crate::network::start(libp2p_keypair, config).await?;
    let task_command_tx = command_tx.clone();
    let runtime = NetworkRuntimeLoop {
        data_dir: request.data_dir.to_path_buf(),
        binding: request.binding.clone(),
        store: request.store,
        field: request.field,
        network_snapshot,
        event_rx,
        command_tx: task_command_tx,
        options: request.options,
    };

    tokio::spawn(async move {
        runtime.run().await;
    });

    Ok(command_tx)
}

fn effective_bootstrap_seeds(
    explicit_bootstrap: &[String],
    snapshot: &NetworkSnapshot,
) -> Vec<String> {
    if !explicit_bootstrap.is_empty() {
        return explicit_bootstrap.to_vec();
    }

    let remembered = snapshot.bootstrap_seed_addresses(16);
    if !remembered.is_empty() {
        return remembered;
    }

    DEFAULT_PUBLIC_BOOTSTRAP_SEEDS
        .iter()
        .map(|seed| (*seed).to_string())
        .collect()
}

pub async fn attempt_first_connection(
    data_dir: &Path,
    identity: &NodeIdentity,
    binding: &IdentityBinding,
    store: Arc<TraceStore>,
    timeout: Duration,
) -> Result<InitialConnectionAttempt, Box<dyn Error>> {
    let before = NetworkSnapshot::load(data_dir);
    let baseline_bootstrap_targets = before.bootstrap_targets;
    let baseline_bootstrap_contact_at_ms = before.last_bootstrap_contact_at_ms;
    let baseline_last_peer_connected_at_ms = before.last_peer_connected_at_ms;
    let baseline_trusted_peer_seed_count = before.trusted_peer_seeds.len();

    let command_tx = start_network_runtime(NetworkRuntimeRequest {
        data_dir,
        identity,
        binding,
        store,
        field: None, // no pheromone field for connection attempts
        listen_port: 0,
        bootstrap: &[],
        options: NetworkRuntimeOptions::embedded(),
    })
    .await?;

    let started_at = Instant::now();
    let mut first_connected_at = None;
    while started_at.elapsed() < timeout {
        tokio::time::sleep(FIRST_CONNECTION_POLL_INTERVAL).await;
        let snapshot = NetworkSnapshot::load(data_dir);
        let connected_once = snapshot.peer_count > 0
            || snapshot.last_peer_connected_at_ms != baseline_last_peer_connected_at_ms;
        if connected_once && first_connected_at.is_none() {
            first_connected_at = Some(Instant::now());
        }
        let trusted_same_owner_ready =
            snapshot.trusted_peer_seeds.len() > baseline_trusted_peer_seed_count;
        if trusted_same_owner_ready {
            break;
        }
        if let Some(connected_at) = first_connected_at
            && connected_at.elapsed() >= FIRST_CONNECTION_GRACE_AFTER_CONNECT
        {
            break;
        }
    }

    let _ = command_tx.send(NetworkCommand::Shutdown).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut snapshot = NetworkSnapshot::load(data_dir);
    let connected_once = snapshot.peer_count > 0
        || snapshot.last_peer_connected_at_ms != baseline_last_peer_connected_at_ms;
    let trusted_same_owner_ready =
        snapshot.trusted_peer_seeds.len() > baseline_trusted_peer_seed_count;

    snapshot.bootstrap_targets = baseline_bootstrap_targets;
    snapshot.last_bootstrap_contact_at_ms = baseline_bootstrap_contact_at_ms;
    snapshot.clear_live_connections();
    snapshot.save(data_dir);

    Ok(InitialConnectionAttempt {
        connected_once,
        trusted_same_owner_ready,
    })
}

impl NetworkRuntimeLoop {
    async fn run(mut self) {
        let mut evaporation_interval = tokio::time::interval(Duration::from_secs(3600));
        evaporation_interval.tick().await;
        let mut dht_publish_interval = tokio::time::interval(Duration::from_secs(300));
        dht_publish_interval.tick().await;
        let mut publish_scan_interval = tokio::time::interval(Duration::from_secs(30));
        publish_scan_interval.tick().await;
        // Field self-evolution: diffusion + coupling decay every 5 minutes
        let mut field_tick_interval = tokio::time::interval(Duration::from_secs(300));
        field_tick_interval.tick().await;

        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    match event {
                        Some(event) => {
                            handle_network_event(
                                &self.data_dir,
                                &self.binding,
                                &self.store,
                                self.field.as_deref(),
                                &mut self.network_snapshot,
                                event,
                            );
                        }
                        None => break,
                    }
                }
                _ = evaporation_interval.tick() => {
                    match self.store.evaporate(None) {
                        Ok(n) if n > 0 => info!(deleted = n, "Evaporated expired traces"),
                        Ok(_) => {}
                        Err(e) => warn!(%e, "Evaporation failed"),
                    }
                    if let Some(ref f) = self.field {
                        let pruned = f.prune();
                        if pruned > 0 {
                            info!(pruned, "Pruned dead pheromone field points");
                        }
                    }
                }
                _ = wait_on_interval(self.options.publish_summaries, &mut dht_publish_interval) => {
                    publish_capability_summaries(&self.store, &self.command_tx).await;
                }
                _ = wait_on_interval(self.options.publish_local_traces, &mut publish_scan_interval) => {
                    publish_local_traces(&self.store, &self.command_tx).await;
                }
                _ = field_tick_interval.tick() => {
                    if let Some(ref f) = self.field {
                        let result = f.tick();
                        if result.diffused > 0 || result.points_pruned > 0 || result.couplings_pruned > 0 {
                            info!(
                                diffused = result.diffused,
                                points_pruned = result.points_pruned,
                                couplings_pruned = result.couplings_pruned,
                                "Pheromone field tick"
                            );
                        }
                    }
                }
            }
        }
    }
}

fn handle_network_event(
    data_dir: &Path,
    binding: &IdentityBinding,
    store: &TraceStore,
    field: Option<&PheromoneField>,
    network_snapshot: &mut NetworkSnapshot,
    event: NetworkEvent,
) {
    match event {
        NetworkEvent::BootstrapContacted { targets } => {
            network_snapshot.mark_bootstrap_contact(targets);
            network_snapshot.save(data_dir);
        }
        NetworkEvent::PeerObserved { peer_id, address } => {
            observe_reusable_peer_address(network_snapshot, &peer_id, &address);
            network_snapshot.save(data_dir);
        }
        NetworkEvent::PeerIdentified {
            peer_id,
            device_identity,
            listen_addrs,
        } => {
            for address in listen_addrs {
                observe_reusable_peer_address(network_snapshot, &peer_id, &address);
            }
            let promoted = maybe_promote_joined_primary_peer(
                network_snapshot,
                binding,
                &peer_id,
                device_identity.as_deref(),
            );
            if promoted > 0 {
                info!(
                    peer=%peer_id,
                    promoted,
                    "Promoted joined primary peer into trusted same-owner seeds"
                );
            }
            network_snapshot.save(data_dir);
        }
        NetworkEvent::PeerConnected { peer_id, address } => {
            info!(peer=%peer_id, "Peer connected");
            if let Some(address) = address {
                observe_reusable_peer_address(network_snapshot, &peer_id, &address);
            }
            network_snapshot.mark_peer_connected(
                peer_id.to_string(),
                network_snapshot.peer_count.saturating_add(1),
            );
            network_snapshot.save(data_dir);
        }
        NetworkEvent::PeerDisconnected(peer) => {
            info!(%peer, "Peer disconnected");
            network_snapshot.mark_peer_disconnected(
                &peer.to_string(),
                network_snapshot.peer_count.saturating_sub(1),
            );
            network_snapshot.save(data_dir);
        }
        NetworkEvent::TraceReceived { trace, source_peer } => {
            network_snapshot.mark_trace_received();
            let promoted = maybe_promote_same_owner_trace_source(
                network_snapshot,
                binding,
                &trace,
                &source_peer,
            );
            if promoted > 0 {
                info!(
                    peer=%source_peer,
                    promoted,
                    "Promoted live same-owner peer into trusted recovery seeds"
                );
            }
            network_snapshot.save(data_dir);
            let trace_id = trace.id;
            match store.insert(&trace) {
                Ok(true) => {
                    let _ = store.mark_published(&[trace_id]);
                    if let Some(f) = field {
                        f.excite(&trace);
                    }
                    info!(
                        capability = %trace.capability,
                        outcome = ?trace.outcome,
                        "Stored new trace from network"
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, "Failed to store received trace");
                }
            }
        }
    }
}

fn observe_reusable_peer_address(
    network_snapshot: &mut NetworkSnapshot,
    peer_id: &libp2p::PeerId,
    address: &Multiaddr,
) {
    let rendered = address.to_string();
    network_snapshot.observe_peer_address(peer_id.to_string(), rendered.clone());
    if let Some(dialable) = normalize_dialable_peer_address(&rendered, peer_id)
        && dialable != rendered
    {
        network_snapshot.observe_peer_address(peer_id.to_string(), dialable);
    }
}

fn normalize_dialable_peer_address(address: &str, peer_id: &libp2p::PeerId) -> Option<String> {
    if address.contains("/p2p/") {
        return Some(address.to_string());
    }

    let dialable = format!("{address}/p2p/{peer_id}");
    dialable.parse::<Multiaddr>().ok()?;
    Some(dialable)
}

async fn wait_on_interval(enabled: bool, interval: &mut tokio::time::Interval) {
    if enabled {
        interval.tick().await;
    } else {
        pending::<()>().await;
    }
}

async fn publish_capability_summaries(
    store: &TraceStore,
    command_tx: &mpsc::Sender<NetworkCommand>,
) {
    if let Ok(capabilities) = store.distinct_capabilities(100) {
        for capability in capabilities {
            if is_signal_capability(&capability)
                || is_presence_capability(&capability)
                || is_continuity_capability(&capability)
            {
                continue;
            }
            if let Ok(Some(stats)) = store.aggregate(&capability) {
                let _ = command_tx
                    .send(NetworkCommand::PublishSummary { capability, stats })
                    .await;
            }
        }
    }
}

async fn publish_local_traces(store: &TraceStore, command_tx: &mpsc::Sender<NetworkCommand>) {
    if let Ok(traces) = store.unpublished_traces(50)
        && !traces.is_empty()
    {
        info!(count = traces.len(), "Publishing local traces to network");
        let mut ids: Vec<[u8; 32]> = Vec::new();
        for trace in traces {
            ids.push(trace.id);
            let space = trace
                .context_text
                .as_ref()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
                .and_then(|v| v.get("space")?.as_str().map(String::from));
            let _ = command_tx
                .send(NetworkCommand::PublishTrace {
                    trace: Box::new(trace),
                    space,
                })
                .await;
        }
        let _ = store.mark_published(&ids);
    }
}

fn trace_author_peer_id(trace: &Trace) -> Option<libp2p::PeerId> {
    let public_key =
        libp2p::identity::ed25519::PublicKey::try_from_bytes(&trace.node_pubkey).ok()?;
    let public_key: libp2p::identity::PublicKey = public_key.into();
    Some(public_key.to_peer_id())
}

pub fn maybe_promote_joined_primary_peer(
    network_snapshot: &mut NetworkSnapshot,
    binding: &IdentityBinding,
    peer_id: &libp2p::PeerId,
    remote_device_identity: Option<&str>,
) -> usize {
    let Some(joined_from_device) = binding.joined_from_device.as_deref() else {
        return 0;
    };
    if remote_device_identity != Some(joined_from_device) {
        return 0;
    }
    network_snapshot.promote_peer_to_trusted(&peer_id.to_string())
}

pub fn maybe_promote_same_owner_trace_source(
    network_snapshot: &mut NetworkSnapshot,
    binding: &IdentityBinding,
    trace: &Trace,
    source_peer: &libp2p::PeerId,
) -> usize {
    let Some(local_owner) = binding.owner_account.as_deref() else {
        return 0;
    };
    let Some(remote_owner) = trace.owner_account.as_deref() else {
        return 0;
    };
    let Some(remote_device_identity) = trace.device_identity.as_deref() else {
        return 0;
    };
    if remote_owner != local_owner || remote_device_identity == binding.device_identity {
        return 0;
    }
    if trace_author_peer_id(trace).as_ref() != Some(source_peer) {
        return 0;
    }
    network_snapshot.promote_peer_to_trusted(&source_peer.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PUBLIC_BOOTSTRAP_SEEDS, effective_bootstrap_seeds,
        maybe_promote_joined_primary_peer, maybe_promote_same_owner_trace_source,
        normalize_dialable_peer_address, observe_reusable_peer_address,
    };
    use crate::context::simhash;
    use crate::identity::{IdentityBinding, NodeIdentity};
    use crate::network_state::NetworkSnapshot;
    use crate::trace::{Outcome, Trace};

    #[test]
    fn joined_primary_live_peer_is_promoted_to_trusted_seed() {
        let local_identity = NodeIdentity::generate();
        let remote_identity = NodeIdentity::generate();
        let binding = IdentityBinding::new(local_identity.device_identity())
            .joined_via_connection(
                Some("oasyce1owner".into()),
                remote_identity.device_identity(),
            )
            .unwrap();
        let mut remote_secret = remote_identity.secret_key_bytes();
        let remote_keypair =
            libp2p::identity::Keypair::ed25519_from_bytes(&mut remote_secret).unwrap();
        let remote_peer_id = remote_keypair.public().to_peer_id();

        let mut snapshot = NetworkSnapshot::begin(1);
        snapshot.observe_peer_address(remote_peer_id.to_string(), "/ip4/10.0.0.8/tcp/4001");

        let promoted = maybe_promote_joined_primary_peer(
            &mut snapshot,
            &binding,
            &remote_peer_id,
            Some(&remote_identity.device_identity()),
        );

        assert_eq!(promoted, 1);
        assert_eq!(
            snapshot.trusted_peer_seed_addresses(8),
            vec!["/ip4/10.0.0.8/tcp/4001".to_string()]
        );
    }

    #[test]
    fn same_owner_trace_source_promotes_author_peer_only() {
        let local_identity = NodeIdentity::generate();
        let remote_identity = NodeIdentity::generate();
        let other_identity = NodeIdentity::generate();
        let binding = IdentityBinding::new(local_identity.device_identity())
            .bind_owner_account("oasyce1owner".into())
            .unwrap();

        let mut remote_secret = remote_identity.secret_key_bytes();
        let remote_keypair =
            libp2p::identity::Keypair::ed25519_from_bytes(&mut remote_secret).unwrap();
        let remote_peer_id = remote_keypair.public().to_peer_id();

        let mut other_secret = other_identity.secret_key_bytes();
        let other_keypair =
            libp2p::identity::Keypair::ed25519_from_bytes(&mut other_secret).unwrap();
        let other_peer_id = other_keypair.public().to_peer_id();

        let trace = Trace::new_with_identity(
            "claude-code/Read".into(),
            Outcome::Succeeded,
            10,
            1,
            simhash("read file: src/main.rs"),
            Some("read file: src/main.rs".into()),
            Some("remote-session".into()),
            Some("oasyce1owner".into()),
            Some(remote_identity.device_identity()),
            "codex".into(),
            remote_identity.public_key_bytes(),
            |msg| remote_identity.sign(msg),
        );

        let mut snapshot = NetworkSnapshot::begin(1);
        snapshot.observe_peer_address(remote_peer_id.to_string(), "/ip4/10.0.0.9/tcp/4001");

        let ignored =
            maybe_promote_same_owner_trace_source(&mut snapshot, &binding, &trace, &other_peer_id);
        assert_eq!(ignored, 0);
        assert_eq!(snapshot.trusted_peer_seeds.len(), 0);

        let promoted =
            maybe_promote_same_owner_trace_source(&mut snapshot, &binding, &trace, &remote_peer_id);
        assert_eq!(promoted, 1);
        assert_eq!(
            snapshot.trusted_peer_seed_addresses(8),
            vec!["/ip4/10.0.0.9/tcp/4001".to_string()]
        );
    }

    #[test]
    fn effective_bootstrap_seeds_fall_back_to_default_public_seed() {
        let snapshot = NetworkSnapshot::default();
        let bootstrap = effective_bootstrap_seeds(&[], &snapshot);
        assert_eq!(
            bootstrap,
            DEFAULT_PUBLIC_BOOTSTRAP_SEEDS
                .iter()
                .map(|seed| (*seed).to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn effective_bootstrap_seeds_prefer_remembered_over_default() {
        let mut snapshot = NetworkSnapshot::default();
        snapshot.remember_bootstrap_seeds(["/ip4/10.0.0.99/tcp/4001".to_string()]);
        let bootstrap = effective_bootstrap_seeds(&[], &snapshot);
        assert_eq!(bootstrap, vec!["/ip4/10.0.0.99/tcp/4001".to_string()]);
    }

    #[test]
    fn normalize_dialable_peer_address_appends_peer_id_when_missing() {
        let local_identity = NodeIdentity::generate();
        let mut secret = local_identity.secret_key_bytes();
        let keypair = libp2p::identity::Keypair::ed25519_from_bytes(&mut secret).unwrap();
        let peer_id = keypair.public().to_peer_id();

        let normalized =
            normalize_dialable_peer_address("/ip4/127.0.0.1/tcp/4001", &peer_id).unwrap();

        assert_eq!(normalized, format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer_id}"));
    }

    #[test]
    fn observed_reusable_peer_address_keeps_dialable_variant() {
        let local_identity = NodeIdentity::generate();
        let mut secret = local_identity.secret_key_bytes();
        let keypair = libp2p::identity::Keypair::ed25519_from_bytes(&mut secret).unwrap();
        let peer_id = keypair.public().to_peer_id();

        let mut snapshot = NetworkSnapshot::begin(0);
        let address: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
        observe_reusable_peer_address(&mut snapshot, &peer_id, &address);

        assert!(
            snapshot
                .remembered_peer_addresses(8)
                .iter()
                .any(|addr| addr == &format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer_id}"))
        );
    }
}
