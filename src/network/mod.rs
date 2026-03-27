//! P2P network layer using libp2p.
//!
//! Each Thronglets node is a libp2p peer that:
//! - Discovers neighbors via mDNS (local) and Kademlia DHT (global)
//! - Propagates traces via gossipsub
//! - Stores received traces locally after verification

use libp2p::{
    gossipsub, identify, kad, mdns, noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use libp2p::futures::StreamExt;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn, debug};

use crate::trace::Trace;

/// Topic name for the global trace gossip channel.
const TRACE_TOPIC: &str = "thronglets/traces/v1";

/// Events emitted by the network layer to the node runtime.
#[derive(Debug)]
pub enum NetworkEvent {
    /// A new peer was discovered and connected.
    PeerConnected(PeerId),
    /// A peer disconnected.
    PeerDisconnected(PeerId),
    /// A trace was received from the network (already deserialized).
    TraceReceived(Trace),
}

/// Commands sent from the node runtime to the network layer.
#[derive(Debug)]
pub enum NetworkCommand {
    /// Publish a trace to the gossip network.
    PublishTrace(Trace),
    /// Get the list of connected peers.
    GetPeers(tokio::sync::oneshot::Sender<Vec<PeerId>>),
}

/// Combined libp2p behaviour for Thronglets.
#[derive(NetworkBehaviour)]
struct ThrongletsNetworkBehaviour {
    gossipsub: gossipsub::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    mdns: mdns::tokio::Behaviour,
    identify: identify::Behaviour,
}

/// Configuration for the network layer.
#[derive(Default)]
pub struct NetworkConfig {
    /// Port to listen on (0 = random).
    pub listen_port: u16,
    /// Bootstrap peer addresses.
    pub bootstrap_peers: Vec<Multiaddr>,
}

/// Start the network layer. Returns channels for communication with the node runtime.
///
/// - `event_tx`: network sends events (new peers, received traces) to runtime
/// - `cmd_rx`: runtime sends commands (publish trace, get peers) to network
pub async fn start(
    keypair: libp2p::identity::Keypair,
    config: NetworkConfig,
) -> Result<
    (mpsc::Sender<NetworkCommand>, mpsc::Receiver<NetworkEvent>),
    Box<dyn std::error::Error>,
> {
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
    let mut gossipsub_behaviour =
        gossipsub::Behaviour::new(gossipsub::MessageAuthenticity::Signed(keypair.clone()), gossipsub_config)
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

    let behaviour = ThrongletsNetworkBehaviour {
        gossipsub: gossipsub_behaviour,
        kademlia,
        mdns,
        identify,
    };

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    // Listen
    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", config.listen_port).parse()?;
    swarm.listen_on(listen_addr)?;

    // Connect to bootstrap peers
    for addr in &config.bootstrap_peers {
        info!(%addr, "Dialing bootstrap peer");
        if let Err(e) = swarm.dial(addr.clone()) {
            warn!(%addr, %e, "Failed to dial bootstrap peer");
        }
    }

    // Channels
    let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>(256);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<NetworkCommand>(256);

    // Spawn the network event loop
    tokio::spawn(async move {
        loop {
            tokio::select! {
                // Handle swarm events
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message { message, .. }
                        )) => {
                            match serde_json::from_slice::<Trace>(&message.data) {
                                Ok(trace) => {
                                    if trace.verify() && trace.verify_id() {
                                        debug!(trace_id = ?&trace.id[..4], "Received valid trace from network");
                                        let _ = event_tx.send(NetworkEvent::TraceReceived(trace)).await;
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
                                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
                                let _ = event_tx.send(NetworkEvent::PeerConnected(peer_id)).await;
                            }
                        }
                        SwarmEvent::Behaviour(ThrongletsNetworkBehaviourEvent::Mdns(
                            mdns::Event::Expired(peers)
                        )) => {
                            for (peer_id, _) in peers {
                                debug!(%peer_id, "mDNS: peer expired");
                                swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                                let _ = event_tx.send(NetworkEvent::PeerDisconnected(peer_id)).await;
                            }
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
                        NetworkCommand::PublishTrace(trace) => {
                            match serde_json::to_vec(&trace) {
                                Ok(data) => {
                                    let topic = gossipsub::IdentTopic::new(TRACE_TOPIC);
                                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                        warn!(%e, "Failed to publish trace to gossipsub");
                                    } else {
                                        debug!(trace_id = ?&trace.id[..4], "Published trace to network");
                                    }
                                }
                                Err(e) => {
                                    warn!(%e, "Failed to serialize trace for publishing");
                                }
                            }
                        }
                        NetworkCommand::GetPeers(reply) => {
                            let peers: Vec<PeerId> = swarm.connected_peers().cloned().collect();
                            let _ = reply.send(peers);
                        }
                    }
                }
            }
        }
    });

    Ok((cmd_tx, event_rx))
}
