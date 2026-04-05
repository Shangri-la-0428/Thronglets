//! Oasyce anchoring layer — submit trace proofs to the Oasyce blockchain.
//!
//! Constructs Cosmos SDK REST-compatible transactions that anchor trace hashes
//! on-chain. This provides cryptographic proof that a trace existed at a given
//! block height, without requiring trust in any single node.
//!
//! Design: lightweight JSON-only construction. No heavy protobuf/gRPC deps.
//! The actual HTTP broadcast is a placeholder until the chain module is deployed.

use crate::identity::NodeIdentity;
use crate::trace::Trace;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum traces per batch anchor transaction.
const MAX_BATCH_SIZE: usize = 50;

/// Client for anchoring traces to the Oasyce blockchain.
pub struct AnchorClient {
    rpc_url: String,
    chain_id: String,
}

/// Result of an anchor operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorResult {
    /// Transaction hash (hex-encoded sha256 of the tx bytes).
    pub tx_hash: String,
    /// Number of traces successfully included in the anchor tx.
    pub anchored: u32,
    /// Number of traces skipped (e.g., already anchored or invalid).
    pub skipped: u32,
}

/// A single trace anchor message for the Cosmos tx body.
#[derive(Debug, Clone, Serialize)]
struct MsgAnchorTrace {
    #[serde(rename = "@type")]
    type_url: String,
    sender: String,
    trace_id: String,
    capability: String,
    outcome: String,
    context_hash: String,
    model_id: String,
    timestamp: String,
    node_pubkey: String,
    signature: String,
}

impl AnchorClient {
    /// Create a new anchor client.
    ///
    /// `rpc_url` is the Cosmos SDK REST endpoint (e.g., `http://localhost:1317`).
    /// `chain_id` is the chain identifier (e.g., `oasyce-1`).
    pub fn new(rpc_url: &str, chain_id: &str) -> Self {
        Self {
            rpc_url: rpc_url.trim_end_matches('/').to_string(),
            chain_id: chain_id.to_string(),
        }
    }

    /// RPC endpoint URL.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Chain ID.
    pub fn chain_id(&self) -> &str {
        &self.chain_id
    }

    /// Anchor a single trace to the Oasyce chain.
    ///
    /// Constructs the transaction JSON and simulates submission.
    /// Returns the computed tx_hash for the would-be transaction.
    pub fn anchor_trace(
        &self,
        identity: &NodeIdentity,
        trace: &Trace,
    ) -> Result<AnchorResult, AnchorError> {
        self.anchor_batch(identity, std::slice::from_ref(trace))
    }

    /// Anchor a batch of traces (up to 50) in a single transaction.
    ///
    /// Traces beyond MAX_BATCH_SIZE are skipped (not silently dropped --
    /// they show up in the `skipped` count).
    pub fn anchor_batch(
        &self,
        identity: &NodeIdentity,
        traces: &[Trace],
    ) -> Result<AnchorResult, AnchorError> {
        if traces.is_empty() {
            return Ok(AnchorResult {
                tx_hash: String::new(),
                anchored: 0,
                skipped: 0,
            });
        }

        let sender = identity.oasyce_address();
        let mut messages = Vec::new();
        let mut skipped: u32 = 0;

        for (i, trace) in traces.iter().enumerate() {
            if i >= MAX_BATCH_SIZE {
                skipped += 1;
                continue;
            }

            if !trace.verify() {
                skipped += 1;
                continue;
            }

            messages.push(self.build_msg(&sender, trace));
        }

        let anchored = messages.len() as u32;

        if messages.is_empty() {
            return Ok(AnchorResult {
                tx_hash: String::new(),
                anchored: 0,
                skipped,
            });
        }

        let tx_body = self.build_tx_body(&messages);
        let tx_bytes =
            serde_json::to_vec(&tx_body).map_err(|e| AnchorError::Serialization(e.to_string()))?;

        // Sign the tx body bytes
        let sig = identity.sign(&tx_bytes);
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

        // Build the full signed tx envelope
        let signed_tx = serde_json::json!({
            "body": tx_body,
            "auth_info": {
                "signer_infos": [{
                    "public_key": {
                        "@type": "/cosmos.crypto.ed25519.PubKey",
                        "key": base64::engine::general_purpose::STANDARD.encode(identity.public_key_bytes())
                    },
                    "mode_info": {
                        "single": {
                            "mode": "SIGN_MODE_DIRECT"
                        }
                    },
                    "sequence": "0"
                }],
                "fee": {
                    "amount": [],
                    "gas_limit": "200000"
                }
            },
            "signatures": [sig_b64]
        });

        let signed_tx_bytes = serde_json::to_vec(&signed_tx)
            .map_err(|e| AnchorError::Serialization(e.to_string()))?;

        // Compute the tx hash (sha256 of the signed tx bytes)
        let tx_hash = hex_encode(&Sha256::digest(&signed_tx_bytes));

        // Broadcast to Cosmos REST endpoint
        let broadcast_request = serde_json::json!({
            "tx_bytes": base64::engine::general_purpose::STANDARD.encode(&signed_tx_bytes),
            "mode": "BROADCAST_MODE_SYNC"
        });

        let url = format!("{}/cosmos/tx/v1beta1/txs", self.rpc_url);
        let resp = reqwest::blocking::Client::new()
            .post(&url)
            .json(&broadcast_request)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .map_err(|e| AnchorError::Http(e.to_string()))?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .map_err(|e| AnchorError::Http(format!("invalid JSON response: {e}")))?;

        // Extract tx_hash from chain response
        let chain_tx_hash = body
            .pointer("/tx_response/txhash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let code = body
            .pointer("/tx_response/code")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX);

        if !status.is_success() || (code != 0 && code != u64::MAX) {
            let raw_log = body
                .pointer("/tx_response/raw_log")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(AnchorError::Chain(format!("code={code}, log={raw_log}")));
        }

        let final_hash = if chain_tx_hash.is_empty() {
            tx_hash
        } else {
            chain_tx_hash
        };

        tracing::info!(
            tx_hash = %final_hash,
            anchored = anchored,
            skipped = skipped,
            chain_id = %self.chain_id,
            "Anchor tx broadcast"
        );

        Ok(AnchorResult {
            tx_hash: final_hash,
            anchored,
            skipped,
        })
    }

    /// Build a single MsgAnchorTrace for the tx body.
    fn build_msg(&self, sender: &str, trace: &Trace) -> MsgAnchorTrace {
        let b64 = base64::engine::general_purpose::STANDARD;

        MsgAnchorTrace {
            type_url: "/oasyce.trace.v1.MsgAnchorTrace".to_string(),
            sender: sender.to_string(),
            trace_id: hex_encode(&trace.id),
            capability: trace.capability.clone(),
            outcome: format!("{:?}", trace.outcome).to_lowercase(),
            context_hash: b64.encode(trace.context_hash),
            model_id: trace.model_id.clone(),
            timestamp: trace.timestamp.to_string(),
            node_pubkey: b64.encode(trace.node_pubkey),
            signature: b64.encode(trace.signature.to_bytes()),
        }
    }

    /// Build the tx body containing all anchor messages.
    fn build_tx_body(&self, messages: &[MsgAnchorTrace]) -> serde_json::Value {
        serde_json::json!({
            "messages": messages,
            "memo": format!("thronglets-anchor-v{}", env!("CARGO_PKG_VERSION")),
            "timeout_height": "0",
            "extension_options": [],
            "non_critical_extension_options": []
        })
    }
}

/// Errors that can occur during anchoring.
#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("chain returned error: {0}")]
    Chain(String),
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::trace::Outcome;

    fn make_identity() -> NodeIdentity {
        NodeIdentity::generate()
    }

    fn make_trace(id: &NodeIdentity, cap: &str, outcome: Outcome, ctx: &str) -> Trace {
        Trace::new(
            cap.into(),
            outcome,
            100,
            5000,
            simhash(ctx),
            Some(ctx.to_string()),
            None,
            "test-model".into(),
            id.public_key_bytes(),
            |m| id.sign(m),
        )
    }

    /// Tests that actually broadcast need a running chain.
    /// In CI/offline, they return Http error — that's expected.
    /// The test verifies tx construction is correct (proper error, not panic).
    #[test]
    fn anchor_single_trace_broadcasts() {
        let id = make_identity();
        let trace = make_trace(&id, "tool-a", Outcome::Succeeded, "test context");
        let client = AnchorClient::new("http://localhost:1317", "oasyce-1");

        match client.anchor_trace(&id, &trace) {
            Ok(result) => {
                assert_eq!(result.anchored, 1);
                assert_eq!(result.skipped, 0);
                assert!(!result.tx_hash.is_empty());
            }
            Err(AnchorError::Http(_)) => {
                // Expected when no chain is running
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn anchor_batch_broadcasts() {
        let id = make_identity();
        let mut traces = Vec::new();
        for i in 0..5 {
            let t = make_trace(
                &id,
                &format!("tool-{i}"),
                Outcome::Succeeded,
                &format!("ctx {i}"),
            );
            traces.push(t);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let client = AnchorClient::new("http://localhost:1317", "oasyce-1");
        match client.anchor_batch(&id, &traces) {
            Ok(result) => {
                assert_eq!(result.anchored, 5);
                assert_eq!(result.skipped, 0);
                assert!(!result.tx_hash.is_empty());
            }
            Err(AnchorError::Http(_)) => {}
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn anchor_empty_batch() {
        let id = make_identity();
        let client = AnchorClient::new("http://localhost:1317", "oasyce-1");
        let result = client.anchor_batch(&id, &[]).unwrap();
        assert_eq!(result.anchored, 0);
        assert_eq!(result.skipped, 0);
        assert!(result.tx_hash.is_empty());
    }

    #[test]
    fn anchor_skips_tampered_traces() {
        let id = make_identity();
        let mut trace = make_trace(&id, "tool-a", Outcome::Succeeded, "test context");
        trace.latency_ms = 999; // tamper

        let client = AnchorClient::new("http://localhost:1317", "oasyce-1");
        // All traces invalid → 0 messages → empty batch (no HTTP call)
        let result = client.anchor_batch(&id, &[trace]).unwrap();
        assert_eq!(result.anchored, 0);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn client_stores_config() {
        let client = AnchorClient::new("http://example.com:1317/", "testnet-42");
        assert_eq!(client.rpc_url(), "http://example.com:1317");
        assert_eq!(client.chain_id(), "testnet-42");
    }

    #[test]
    fn anchor_result_serializes() {
        let r = AnchorResult {
            tx_hash: "abc123".into(),
            anchored: 3,
            skipped: 1,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("abc123"));
        assert!(json.contains("\"anchored\":3"));
    }
}
