//! P2P network layer using libp2p.
//!
//! Each Thronglets node is a libp2p peer that:
//! - Discovers neighbors via mDNS (local) and Kademlia DHT (global)
//! - Propagates traces via gossipsub
//! - Stores received traces locally after verification

use libp2p::futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, SwarmBuilder, autonat, dcutr, gossipsub, identify, kad, mdns, noise, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, upnp, yamux,
};
use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::storage::AggregateStats;
use crate::trace::Trace;

/// Topic name for the global trace gossip channel.
const TRACE_TOPIC: &str = "thronglets/traces/v1";

/// DHT key prefix for capability summaries.
const DHT_CAP_PREFIX: &str = "/thronglets/cap/v1/";
const KNOWN_PEER_BOOTSTRAP_FALLBACK_DELAY: Duration = Duration::from_secs(5);
const TRUSTED_PEER_BOOTSTRAP_FALLBACK_DELAY: Duration = Duration::from_secs(15);

/// Events emitted by the network layer to the node runtime.
#[derive(Debug)]
pub enum NetworkEvent {
    /// Bootstrap peers were actively dialed as a fallback path.
    BootstrapContacted { targets: usize },
    /// A new peer was discovered and connected.
    PeerConnected {
        peer_id: PeerId,
        address: Option<Multiaddr>,
    },
    /// A peer address was observed and can be reused as a future seed.
    PeerObserved { peer_id: PeerId, address: Multiaddr },
    /// A peer identified itself, including its device identity when derivable.
    PeerIdentified {
        peer_id: PeerId,
        device_identity: Option<String>,
        listen_addrs: Vec<Multiaddr>,
    },
    /// A peer disconnected.
    PeerDisconnected(PeerId),
    /// A trace was received from the network (already deserialized).
    TraceReceived {
        trace: Box<Trace>,
        source_peer: PeerId,
    },
}

/// A capability summary retrieved from the DHT.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DhtCapabilitySummary {
    pub capability: String,
    pub stats: AggregateStats,
    pub node_count: u32,
    pub updated_at: u64,
}

/// Commands sent from the node runtime to the network layer.
#[derive(Debug)]
pub enum NetworkCommand {
    /// Publish a trace to the gossip network.
    /// When `space` is set, also publishes to the space-specific topic.
    PublishTrace {
        trace: Box<Trace>,
        space: Option<String>,
    },
    /// Subscribe to a space-specific gossipsub topic.
    SubscribeSpace(String),
    /// Unsubscribe from a space-specific gossipsub topic.
    UnsubscribeSpace(String),
    /// Get the list of connected peers.
    GetPeers(tokio::sync::oneshot::Sender<Vec<PeerId>>),
    /// Shut down the network event loop.
    Shutdown,
    /// Publish a capability summary to the DHT.
    PublishSummary {
        capability: String,
        stats: AggregateStats,
    },
    /// Query the DHT for a capability summary.
    QuerySummary {
        capability: String,
        reply: tokio::sync::oneshot::Sender<Option<DhtCapabilitySummary>>,
    },
}

/// Build a space-specific gossipsub topic name.
fn space_topic(space: &str) -> String {
    format!("{TRACE_TOPIC}/{space}")
}

/// Combined libp2p behaviour for Thronglets.
///
/// Includes full NAT traversal stack so nodes behind home routers can
/// connect to each other without a VPS:
/// - relay: any public-IP node can relay for NAT'd peers
/// - dcutr: upgrades relay connections to direct via hole-punching
/// - autonat: detects whether this node is publicly reachable
/// - upnp: attempts UPnP port mapping on the local router
#[derive(NetworkBehaviour)]
struct ThrongletsNetworkBehaviour {
    gossipsub: gossipsub::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    mdns: mdns::tokio::Behaviour,
    identify: identify::Behaviour,
    relay_server: relay::Behaviour,
    relay_client: relay::client::Behaviour,
    dcutr: dcutr::Behaviour,
    autonat: autonat::Behaviour,
    upnp: upnp::tokio::Behaviour,
}

/// Configuration for the network layer.
#[derive(Default)]
pub struct NetworkConfig {
    /// Port to listen on (0 = random).
    pub listen_port: u16,
    /// Bootstrap peer addresses.
    pub bootstrap_peers: Vec<Multiaddr>,
    /// Same-owner / trusted peer addresses to try before generic remembered peers.
    pub trusted_peers: Vec<Multiaddr>,
    /// Previously observed peer addresses to try before bootstrap fallback.
    pub known_peers: Vec<Multiaddr>,
}

/// Start the network layer. Returns channels for communication with the node runtime.
///
/// - `event_tx`: network sends events (new peers, received traces) to runtime
/// - `cmd_rx`: runtime sends commands (publish trace, get peers) to network
pub async fn start(
    keypair: libp2p::identity::Keypair,
    config: NetworkConfig,
) -> Result<(mpsc::Sender<NetworkCommand>, mpsc::Receiver<NetworkEvent>), Box<dyn std::error::Error>>
{
    let local_peer_id = PeerId::from(keypair.public());
    info!(%local_peer_id, "Starting Thronglets network node");

    // Build gossipsub
    let message_id_fn = |message: &gossipsub::Message| {
        let mut hasher = DefaultHasher::new();
        message.data.hash(&mut hasher);
        gossipsub::MessageId::from(hasher.finish().to_string())
    };
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(5))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .message_id_fn(message_id_fn)
        .build()
        .map_err(|e| format!("gossipsub config error: {e}"))?;
    let mut gossipsub_behaviour = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(keypair.clone()),
        gossipsub_config,
    )
    .map_err(|e| format!("gossipsub behaviour error: {e}"))?;

    // Subscribe to the trace topic
    let topic = gossipsub::IdentTopic::new(TRACE_TOPIC);
    gossipsub_behaviour.subscribe(&topic)?;

    // Build Kademlia
    let kademlia = kad::Behaviour::new(local_peer_id, kad::store::MemoryStore::new(local_peer_id));

    // Build mDNS
    let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id)?;

    // Build identify
    let identify = identify::Behaviour::new(identify::Config::new(
        "/thronglets/0.1.0".into(),
        keypair.public(),
    ));

    // Build relay server — any node can relay for NAT'd peers.
    // NAT'd nodes won't be chosen as relays because others can't reach them.
    let relay_server = relay::Behaviour::new(local_peer_id, Default::default());

    // Build autonat — detect if behind NAT
    let autonat = autonat::Behaviour::new(local_peer_id, Default::default());

    // Build UPnP — attempt automatic port mapping on the router
    let upnp = upnp::tokio::Behaviour::default();

    // Swarm with relay client transport (required for connecting through relays)
    let mut swarm = SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|_keypair, relay_client| {
            Ok(ThrongletsNetworkBehaviour {
                gossipsub: gossipsub_behaviour,
                kademlia,
                mdns,
                identify,
                relay_server,
                relay_client,
                dcutr: dcutr::Behaviour::new(local_peer_id),
                autonat,
                upnp,
            })
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    // Listen
    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", config.listen_port).parse()?;
    swarm.listen_on(listen_addr)?;

    // Channels
    let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>(256);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<NetworkCommand>(256);

    let (mut bootstrap_fallback_at, contacted_bootstrap) = dial_peer_first(
        &mut swarm,
        &config.trusted_peers,
        &config.known_peers,
        &config.bootstrap_peers,
    );
    if contacted_bootstrap {
        let _ = event_tx.try_send(NetworkEvent::BootstrapContacted {
            targets: config.bootstrap_peers.len(),
        });
    }

    // Pending DHT query replies
    let mut pending_dht_queries: std::collections::HashMap<
        kad::QueryId,
        (
            String,
            tokio::sync::oneshot::Sender<Option<DhtCapabilitySummary>>,
        ),
    > = std::collections::HashMap::new();
    let mut connected_peers: HashSet<PeerId> = HashSet::new();

    // Spawn the network event loop
    tokio::spawn(async move {
        loop {
            tokio::select! {
                // Handle swarm events
                _ = async {
                    if let Some(deadline) = bootstrap_fallback_at {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if connected_peers.is_empty() && !config.bootstrap_peers.is_empty() {
                        dial_bootstrap_peers(&mut swarm, &config.bootstrap_peers);
                        let _ = event_tx
                            .send(NetworkEvent::BootstrapContacted {
                                targets: config.bootstrap_peers.len(),
                            })
                            .await;
                    }
                    bootstrap_fallback_at = None;
                }
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message {
                                propagation_source,
                                message,
                                ..
                            }
                        )) => {
                            match serde_json::from_slice::<Trace>(&message.data) {
                                Ok(trace) => {
                                    if trace.verify() && trace.verify_id() {
                                        debug!(trace_id = ?&trace.id[..4], "Received valid trace from network");
                                        let _ = event_tx
                                            .send(NetworkEvent::TraceReceived {
                                                trace: Box::new(trace),
                                                source_peer: propagation_source,
                                            })
                                            .await;
                                    } else {
                                        warn!("Received invalid trace from network, dropping");
                                    }
                                }
                                Err(e) => {
                                    warn!(%e, "Failed to deserialize gossip message as Trace");
                                }
                            }
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Mdns(
                            mdns::Event::Discovered(peers)
                        )) => {
                            for (peer_id, addr) in peers {
                                info!(%peer_id, %addr, "mDNS: discovered peer");
                                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                                let _ = event_tx
                                    .send(NetworkEvent::PeerObserved {
                                        peer_id,
                                        address: addr,
                                    })
                                    .await;
                            }
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Mdns(
                            mdns::Event::Expired(peers)
                        )) => {
                            for (peer_id, _) in peers {
                                debug!(%peer_id, "mDNS: peer expired");
                                swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                            }
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Identify(
                            identify::Event::Received { peer_id, info, .. }
                        )) => {
                            let device_identity = identify_device_identity(&info.public_key);
                            let _ = event_tx
                                .send(NetworkEvent::PeerIdentified {
                                    peer_id,
                                    device_identity,
                                    listen_addrs: info.listen_addrs,
                                })
                                .await;
                        }
                        SwarmEvent::ConnectionEstablished {
                            peer_id,
                            endpoint,
                            ..
                        } => {
                            if connected_peers.insert(peer_id) {
                                bootstrap_fallback_at = None;
                                debug!(%peer_id, "Connection established");
                                let remote_address = endpoint.get_remote_address().clone();
                                let _ = event_tx
                                    .send(NetworkEvent::PeerObserved {
                                        peer_id,
                                        address: remote_address.clone(),
                                    })
                                    .await;
                                let _ = event_tx
                                    .send(NetworkEvent::PeerConnected {
                                        peer_id,
                                        address: Some(remote_address),
                                    })
                                    .await;
                            }
                        }
                        SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                            if num_established == 0 && connected_peers.remove(&peer_id) {
                                debug!(%peer_id, "Connection closed");
                                if connected_peers.is_empty() {
                                    let (next_fallback_at, contacted_bootstrap) = dial_peer_first(
                                        &mut swarm,
                                        &config.trusted_peers,
                                        &config.known_peers,
                                        &config.bootstrap_peers,
                                    );
                                    bootstrap_fallback_at = next_fallback_at;
                                    if contacted_bootstrap {
                                        let _ = event_tx
                                            .send(NetworkEvent::BootstrapContacted {
                                                targets: config.bootstrap_peers.len(),
                                            })
                                            .await;
                                    }
                                }
                                let _ = event_tx.send(NetworkEvent::PeerDisconnected(peer_id)).await;
                            }
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Kademlia(
                            kad::Event::OutboundQueryProgressed {
                                id,
                                result: kad::QueryResult::GetRecord(result),
                                ..
                            }
                        )) => {
                            if let Some((capability, reply)) = pending_dht_queries.remove(&id) {
                                let summary = match result {
                                    Ok(kad::GetRecordOk::FoundRecord(rec)) => {
                                        serde_json::from_slice::<DhtCapabilitySummary>(&rec.record.value)
                                            .ok()
                                    }
                                    _ => {
                                        debug!(%capability, "DHT query returned no results");
                                        None
                                    }
                                };
                                let _ = reply.send(summary);
                            }
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Kademlia(
                            kad::Event::OutboundQueryProgressed {
                                result: kad::QueryResult::PutRecord(result),
                                ..
                            }
                        )) => {
                            match result {
                                Ok(kad::PutRecordOk { key }) => {
                                    debug!(?key, "Published capability summary to DHT");
                                }
                                Err(e) => {
                                    debug!(?e, "Failed to publish summary to DHT");
                                }
                            }
                        }
                        // Relay server events
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::RelayServer(
                            relay::Event::ReservationReqAccepted { src_peer_id, .. }
                        )) => {
                            info!(%src_peer_id, "Relay: accepted reservation from peer");
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::RelayServer(
                            relay::Event::CircuitReqAccepted { src_peer_id, dst_peer_id, .. }
                        )) => {
                            debug!(%src_peer_id, %dst_peer_id, "Relay: circuit established");
                        }
                        // DCUtR: direct connection upgrade through relay (hole punching)
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Dcutr(
                            dcutr::Event { remote_peer_id, result }
                        )) => {
                            match result {
                                Ok(_) => info!(%remote_peer_id, "DCUtR: upgraded to direct connection"),
                                Err(ref e) => debug!(%remote_peer_id, %e, "DCUtR: hole-punch failed, keeping relay"),
                            }
                        }
                        // AutoNAT: detect if we're publicly reachable
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Autonat(
                            autonat::Event::StatusChanged { old, new }
                        )) => {
                            info!(?old, ?new, "AutoNAT: reachability status changed");
                        }
                        // UPnP: automatic port mapping on the router
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Upnp(
                            upnp::Event::NewExternalAddr(addr)
                        )) => {
                            info!(%addr, "UPnP: mapped external address");
                            swarm.add_external_address(addr);
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Upnp(
                            upnp::Event::ExpiredExternalAddr(addr)
                        )) => {
                            info!(%addr, "UPnP: external address mapping expired");
                            swarm.remove_external_address(&addr);
                        }
                        SwarmEvent::NewListenAddr { address, .. } => {
                            info!(%address, "Listening on");
                        }
                        _ => {}
                    }
                }
                // Handle commands from the runtime
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        NetworkCommand::PublishTrace { trace, space } => {
                            match serde_json::to_vec(&trace) {
                                Ok(data) => {
                                    // Always publish to global topic
                                    let global = gossipsub::IdentTopic::new(TRACE_TOPIC);
                                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(global, data.clone()) {
                                        warn!(%e, "Failed to publish trace to global topic");
                                    }
                                    // Also publish to space-specific topic if set
                                    if let Some(ref space) = space {
                                        let st = gossipsub::IdentTopic::new(space_topic(space));
                                        let _ = swarm.behaviour_mut().gossipsub.publish(st, data);
                                    }
                                    debug!(trace_id = ?&trace.id[..4], ?space, "Published trace to network");
                                }
                                Err(e) => {
                                    warn!(%e, "Failed to serialize trace for publishing");
                                }
                            }
                        }
                        NetworkCommand::SubscribeSpace(space) => {
                            let topic = gossipsub::IdentTopic::new(space_topic(&space));
                            match swarm.behaviour_mut().gossipsub.subscribe(&topic) {
                                Ok(true) => info!(%space, "Subscribed to space topic"),
                                Ok(false) => debug!(%space, "Already subscribed to space topic"),
                                Err(e) => warn!(%e, %space, "Failed to subscribe to space topic"),
                            }
                        }
                        NetworkCommand::UnsubscribeSpace(space) => {
                            let topic = gossipsub::IdentTopic::new(space_topic(&space));
                            match swarm.behaviour_mut().gossipsub.unsubscribe(&topic) {
                                Ok(true) => info!(%space, "Unsubscribed from space topic"),
                                Ok(false) => debug!(%space, "Was not subscribed to space topic"),
                                Err(e) => warn!(%e, %space, "Failed to unsubscribe from space topic"),
                            }
                        }
                        NetworkCommand::GetPeers(reply) => {
                            let peers: Vec<PeerId> = swarm.connected_peers().cloned().collect();
                            let _ = reply.send(peers);
                        }
                        NetworkCommand::Shutdown => {
                            debug!("Shutting down network event loop");
                            break;
                        }
                        NetworkCommand::PublishSummary { capability, stats } => {
                            let key_str = format!("{DHT_CAP_PREFIX}{capability}");
                            let summary = DhtCapabilitySummary {
                                capability: capability.clone(),
                                stats,
                                node_count: 1,
                                updated_at: chrono::Utc::now().timestamp_millis() as u64,
                            };
                            match serde_json::to_vec(&summary) {
                                Ok(value) => {
                                    let record = kad::Record {
                                        key: kad::RecordKey::new(&key_str),
                                        value,
                                        publisher: Some(local_peer_id),
                                        expires: Some(std::time::Instant::now() + Duration::from_secs(3600)),
                                    };
                                    if let Err(e) = swarm.behaviour_mut().kademlia.put_record(
                                        record,
                                        kad::Quorum::One,
                                    ) {
                                        warn!(%e, %capability, "Failed to publish summary to DHT");
                                    } else {
                                        debug!(%capability, "Publishing capability summary to DHT");
                                    }
                                }
                                Err(e) => warn!(%e, "Failed to serialize capability summary"),
                            }
                        }
                        NetworkCommand::QuerySummary { capability, reply } => {
                            let key_str = format!("{DHT_CAP_PREFIX}{capability}");
                            let query_id = swarm.behaviour_mut().kademlia.get_record(
                                kad::RecordKey::new(&key_str),
                            );
                            pending_dht_queries.insert(query_id, (capability, reply));
                        }
                    }
                }
            }
        }
    });

    Ok((cmd_tx, event_rx))
}

fn identify_device_identity(public_key: &libp2p::identity::PublicKey) -> Option<String> {
    let ed25519 = public_key.clone().try_into_ed25519().ok()?;
    Some(crate::identity::NodeIdentity::device_identity_from_pubkey(
        &ed25519.to_bytes(),
    ))
}

fn dial_peer_first(
    swarm: &mut libp2p::Swarm<ThrongletsNetworkBehaviour>,
    trusted_peers: &[Multiaddr],
    known_peers: &[Multiaddr],
    bootstrap_peers: &[Multiaddr],
) -> (Option<tokio::time::Instant>, bool) {
    dial_trusted_peers(swarm, trusted_peers);
    dial_known_peers(swarm, known_peers);
    let fallback_delay = bootstrap_fallback_delay(
        trusted_peers.len(),
        known_peers.len(),
        bootstrap_peers.len(),
    );
    let contacted_bootstrap = fallback_delay.is_some_and(|delay| {
        if delay.is_zero() {
            dial_bootstrap_peers(swarm, bootstrap_peers);
            true
        } else {
            false
        }
    });
    let fallback_at = fallback_delay.and_then(|delay| {
        if delay.is_zero() {
            None
        } else {
            Some(tokio::time::Instant::now() + delay)
        }
    });
    (fallback_at, contacted_bootstrap)
}

fn dial_trusted_peers(
    swarm: &mut libp2p::Swarm<ThrongletsNetworkBehaviour>,
    trusted_peers: &[Multiaddr],
) {
    for addr in trusted_peers {
        info!(%addr, "Dialing trusted peer");
        if let Err(e) = swarm.dial(addr.clone()) {
            warn!(%addr, %e, "Failed to dial trusted peer");
        }
    }
}

fn dial_known_peers(
    swarm: &mut libp2p::Swarm<ThrongletsNetworkBehaviour>,
    known_peers: &[Multiaddr],
) {
    for addr in known_peers {
        info!(%addr, "Dialing known peer");
        if let Err(e) = swarm.dial(addr.clone()) {
            warn!(%addr, %e, "Failed to dial known peer");
        }
    }
}

fn dial_bootstrap_peers(
    swarm: &mut libp2p::Swarm<ThrongletsNetworkBehaviour>,
    bootstrap_peers: &[Multiaddr],
) {
    for addr in bootstrap_peers {
        info!(%addr, "Dialing bootstrap peer");
        if let Err(e) = swarm.dial(addr.clone()) {
            warn!(%addr, %e, "Failed to dial bootstrap peer");
        }
    }
}

fn bootstrap_fallback_delay(
    trusted_peer_count: usize,
    known_peer_count: usize,
    bootstrap_peer_count: usize,
) -> Option<Duration> {
    if bootstrap_peer_count == 0 {
        None
    } else if trusted_peer_count > 0 {
        Some(TRUSTED_PEER_BOOTSTRAP_FALLBACK_DELAY)
    } else if known_peer_count > 0 {
        Some(KNOWN_PEER_BOOTSTRAP_FALLBACK_DELAY)
    } else {
        Some(Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::bootstrap_fallback_delay;
    use std::time::Duration;

    #[test]
    fn bootstrap_is_immediate_without_known_peers() {
        assert_eq!(bootstrap_fallback_delay(0, 0, 1), Some(Duration::ZERO));
    }

    #[test]
    fn bootstrap_is_delayed_when_known_peers_exist() {
        assert_eq!(
            bootstrap_fallback_delay(0, 2, 1),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn bootstrap_is_more_delayed_when_trusted_peers_exist() {
        assert_eq!(
            bootstrap_fallback_delay(2, 2, 1),
            Some(Duration::from_secs(15))
        );
    }

    #[test]
    fn bootstrap_is_disabled_without_targets() {
        assert_eq!(bootstrap_fallback_delay(2, 0, 0), None);
    }
}
