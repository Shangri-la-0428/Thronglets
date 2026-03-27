//! Integration test: two Thronglets nodes discover each other via mDNS,
//! one emits a trace, the other receives it via gossipsub.
//! This proves the core P2P loop works.

use std::time::Duration;
use thronglets::identity::NodeIdentity;
use thronglets::network::{NetworkCommand, NetworkConfig, NetworkEvent};
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};

#[tokio::test]
async fn two_nodes_sync_trace_via_mdns() {
    // Initialize tracing for test output
    let _ = tracing_subscriber::fmt()
        .with_env_filter("thronglets=debug")
        .with_test_writer()
        .try_init();

    // --- Node A ---
    let id_a = NodeIdentity::generate();
    let keypair_a = libp2p::identity::Keypair::ed25519_from_bytes(&mut id_a.secret_key_bytes())
        .expect("keypair A");
    let config_a = NetworkConfig { listen_port: 0, ..Default::default() };
    let (cmd_tx_a, mut event_rx_a) = thronglets::network::start(keypair_a, config_a)
        .await
        .expect("start node A");

    // --- Node B ---
    let id_b = NodeIdentity::generate();
    let keypair_b = libp2p::identity::Keypair::ed25519_from_bytes(&mut id_b.secret_key_bytes())
        .expect("keypair B");
    let config_b = NetworkConfig { listen_port: 0, ..Default::default() };
    let (cmd_tx_b, mut event_rx_b) = thronglets::network::start(keypair_b, config_b)
        .await
        .expect("start node B");

    // Wait for mDNS peer discovery (may take a few seconds)
    let discovery_timeout = Duration::from_secs(10);
    let mut a_found_b = false;
    let mut b_found_a = false;

    let discover_deadline = tokio::time::Instant::now() + discovery_timeout;
    while tokio::time::Instant::now() < discover_deadline && !(a_found_b && b_found_a) {
        tokio::select! {
            Some(event) = event_rx_a.recv() => {
                if matches!(event, NetworkEvent::PeerConnected(_)) {
                    a_found_b = true;
                }
            }
            Some(event) = event_rx_b.recv() => {
                if matches!(event, NetworkEvent::PeerConnected(_)) {
                    b_found_a = true;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    assert!(a_found_b, "Node A should discover Node B via mDNS");
    assert!(b_found_a, "Node B should discover Node A via mDNS");

    // Give gossipsub mesh time to form
    tokio::time::sleep(Duration::from_secs(2)).await;

    // --- Node A emits a trace ---
    let trace = Trace::new(
        "test-capability/v1".into(),
        vec!["test".into(), "integration".into()],
        Outcome::Succeeded,
        42,
        95,
        id_a.public_key_bytes(),
        |msg| id_a.sign(msg),
    );
    let trace_id = trace.id;

    cmd_tx_a
        .send(NetworkCommand::PublishTrace(trace))
        .await
        .expect("send publish command");

    // --- Node B should receive the trace ---
    let receive_timeout = Duration::from_secs(10);
    let mut received_trace: Option<Trace> = None;

    let receive_deadline = tokio::time::Instant::now() + receive_timeout;
    while tokio::time::Instant::now() < receive_deadline && received_trace.is_none() {
        tokio::select! {
            Some(event) = event_rx_b.recv() => {
                if let NetworkEvent::TraceReceived(t) = event {
                    received_trace = Some(t);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    let received = received_trace.expect("Node B should receive the trace from Node A");

    // Verify the received trace
    assert_eq!(received.id, trace_id, "Trace ID should match");
    assert_eq!(received.about, "test-capability/v1");
    assert_eq!(received.outcome, Outcome::Succeeded);
    assert_eq!(received.quality, 95);
    assert!(received.verify(), "Received trace signature should be valid");
    assert!(received.verify_id(), "Received trace ID should be valid");

    // Store it and check aggregation
    let store = TraceStore::in_memory().unwrap();
    assert!(store.insert(&received).unwrap(), "Should store the trace");
    let stats = store.aggregate("test-capability/v1").unwrap().unwrap();
    assert_eq!(stats.total_traces, 1);
    assert!((stats.success_rate - 1.0).abs() < 0.001);

    // Cleanup
    drop(cmd_tx_a);
    drop(cmd_tx_b);
}
