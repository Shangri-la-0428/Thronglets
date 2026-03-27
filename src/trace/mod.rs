//! Trace: the atomic unit of the signal substrate.
//!
//! A trace is what an AI agent leaves behind after interacting with the world.
//! The substrate doesn't define what traces mean — it just ensures they persist and propagate.

use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

/// Outcome of an agent's interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Partial,
    Timeout,
}

/// A single trace — the footprint an agent leaves on the substrate.
///
/// Design principles:
/// - Structured, not natural language (AI doesn't need to "read" reviews)
/// - Automatic emission (using the substrate = contributing to it)
/// - Facts, not opinions (objective execution result, not subjective rating)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    /// Content-addressed ID: sha256(all fields except id itself).
    pub id: [u8; 32],

    /// What this trace is about — a capability, resource, tool, or any identifier.
    pub about: String,

    /// Structured tags for topic routing and filtering.
    /// e.g., ["nlp", "translation", "rust"]
    pub tags: Vec<String>,

    /// Outcome of the interaction.
    pub outcome: Outcome,

    /// Latency in milliseconds (0 if not applicable).
    pub latency_ms: u32,

    /// Quality score 0-100 (0 if not assessed).
    pub quality: u8,

    /// Unix timestamp in milliseconds.
    pub timestamp: u64,

    /// Public key of the signing node (32 bytes).
    pub node_pubkey: [u8; 32],

    /// ed25519 signature over the trace content.
    #[serde(with = "signature_serde")]
    pub signature: Signature,
}

impl Trace {
    /// Create a new trace, computing its content-addressed ID and signature.
    pub fn new(
        about: String,
        tags: Vec<String>,
        outcome: Outcome,
        latency_ms: u32,
        quality: u8,
        node_pubkey: [u8; 32],
        sign_fn: impl FnOnce(&[u8]) -> Signature,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Compute signable content (everything except id and signature)
        let signable = Self::signable_bytes(&about, &tags, outcome, latency_ms, quality, timestamp, &node_pubkey);

        let signature = sign_fn(&signable);

        // ID = hash of signable content + signature
        let mut hasher = Sha256::new();
        hasher.update(&signable);
        hasher.update(signature.to_bytes());
        let id: [u8; 32] = hasher.finalize().into();

        Self {
            id,
            about,
            tags,
            outcome,
            latency_ms,
            quality,
            timestamp,
            node_pubkey,
            signature,
        }
    }

    /// Verify this trace's signature is valid.
    pub fn verify(&self) -> bool {
        let signable = Self::signable_bytes(
            &self.about, &self.tags, self.outcome,
            self.latency_ms, self.quality, self.timestamp, &self.node_pubkey,
        );
        crate::identity::NodeIdentity::verify(&self.node_pubkey, &signable, &self.signature)
    }

    /// Verify the content-addressed ID matches.
    pub fn verify_id(&self) -> bool {
        let signable = Self::signable_bytes(
            &self.about, &self.tags, self.outcome,
            self.latency_ms, self.quality, self.timestamp, &self.node_pubkey,
        );
        let mut hasher = Sha256::new();
        hasher.update(&signable);
        hasher.update(self.signature.to_bytes());
        let expected: [u8; 32] = hasher.finalize().into();
        self.id == expected
    }

    fn signable_bytes(
        about: &str,
        tags: &[String],
        outcome: Outcome,
        latency_ms: u32,
        quality: u8,
        timestamp: u64,
        node_pubkey: &[u8; 32],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(about.as_bytes());
        buf.push(0); // separator
        for tag in tags {
            buf.extend_from_slice(tag.as_bytes());
            buf.push(0);
        }
        buf.push(outcome as u8);
        buf.extend_from_slice(&latency_ms.to_le_bytes());
        buf.push(quality);
        buf.extend_from_slice(&timestamp.to_le_bytes());
        buf.extend_from_slice(node_pubkey);
        buf
    }
}

mod signature_serde {
    use ed25519_dalek::Signature;
    use serde::{self, Deserializer, Serializer, Deserialize};

    pub fn serialize<S: Serializer>(sig: &Signature, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&sig.to_bytes())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Signature, D::Error> {
        let bytes = <Vec<u8>>::deserialize(d)?;
        let arr: [u8; 64] = bytes.try_into().map_err(|_| serde::de::Error::custom("signature must be 64 bytes"))?;
        Ok(Signature::from_bytes(&arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    #[test]
    fn create_and_verify() {
        let id = NodeIdentity::generate();
        let trace = Trace::new(
            "openai/gpt-4".into(),
            vec!["llm".into(), "chat".into()],
            Outcome::Succeeded,
            1200,
            85,
            id.public_key_bytes(),
            |msg| id.sign(msg),
        );

        assert!(trace.verify(), "signature should be valid");
        assert!(trace.verify_id(), "content-addressed ID should match");
    }

    #[test]
    fn tampered_trace_fails_verification() {
        let id = NodeIdentity::generate();
        let mut trace = Trace::new(
            "some-tool".into(),
            vec![],
            Outcome::Succeeded,
            100,
            90,
            id.public_key_bytes(),
            |msg| id.sign(msg),
        );

        trace.quality = 10; // tamper
        assert!(!trace.verify(), "tampered trace should fail verification");
    }

    #[test]
    fn different_traces_have_different_ids() {
        let id = NodeIdentity::generate();
        let t1 = Trace::new("a".into(), vec![], Outcome::Succeeded, 0, 0, id.public_key_bytes(), |m| id.sign(m));
        // Small delay to ensure different timestamp
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = Trace::new("b".into(), vec![], Outcome::Failed, 0, 0, id.public_key_bytes(), |m| id.sign(m));
        assert_ne!(t1.id, t2.id);
    }
}
