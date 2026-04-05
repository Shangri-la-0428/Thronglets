//! Trace: the atomic unit of the signal substrate.
//!
//! A trace is an objective execution record — what an AI agent did,
//! what happened, and in what context. No opinions, no subjective scores.
//!
//! v0.2 AI-native redesign:
//! - Killed `quality` (subjective) and `tags` (human taxonomy)
//! - Added `context_hash` (SimHash fingerprint for semantic similarity)
//! - Added `input_size` (workload dimension)
//! - Added `model_id` (cross-model intelligence)

use crate::context::ContextHash;
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
/// - Facts, not opinions (objective execution record)
/// - Machine-native context (SimHash, not keyword tags)
/// - Cross-model identity (which model produced this?)
/// - Preserve original context text (v0.2.1: AI agents want to read WHY)
/// - Session tracking for workflow discovery (v0.2.1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    /// Content-addressed ID: sha256(signable_bytes + signature).
    pub id: [u8; 32],

    /// Capability URI — what was used.
    /// e.g., "urn:mcp:anthropic:claude:code" or "openai/gpt-4/chat"
    pub capability: String,

    /// Outcome of the interaction.
    pub outcome: Outcome,

    /// Latency in milliseconds.
    pub latency_ms: u32,

    /// Input size (tokens, bytes, or items processed).
    pub input_size: u32,

    /// SimHash fingerprint of the agent's task context.
    /// Used as fast pre-filter for similarity search.
    pub context_hash: ContextHash,

    /// Original natural language context text.
    /// SimHash alone is lexical, not semantic — preserving the original
    /// lets future agents understand WHY a capability was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_text: Option<String>,

    /// Session identifier for workflow sequence tracking.
    /// Traces with the same session_id form an ordered sequence,
    /// enabling "agents who did X usually then did Y" pattern discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Optional root ownership account for Identity V1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_account: Option<String>,

    /// Optional executing device identity for Identity V1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_identity: Option<String>,

    /// Optional agent identifier for multi-agent disambiguation.
    /// Distinguishes concurrent agents on the same device (e.g., "ENFP-Luna", "INTJ-Kai").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Sigil identity (SIG_...) — the on-chain identity of the Loop that produced this trace.
    /// This is the semantic identity; node_pubkey is the cryptographic proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigil_id: Option<String>,

    /// Self-reported model identifier.
    /// e.g., "claude-opus-4-6", "gpt-4o", "gemini-pro"
    pub model_id: String,

    /// Unix timestamp in milliseconds.
    pub timestamp: u64,

    /// Public key of the signing node (32 bytes).
    pub node_pubkey: [u8; 32],

    /// ed25519 signature over the trace content.
    #[serde(with = "signature_serde")]
    pub signature: Signature,
}

/// Builder for constructing Traces without positional-argument hell.
///
/// Three required fields (capability, outcome, model_id).
/// Everything else defaults to zero/None and is set with builder methods.
/// Terminal: `.sign(pubkey, sign_fn)` produces the signed Trace.
///
/// ```ignore
/// let trace = TraceConfig::new("my-tool", Outcome::Succeeded, "claude-opus-4-6")
///     .context("refactoring async code")
///     .sigil("SIG_abc123")
///     .identity(Some("oasyce1...".into()), Some(device_id))
///     .sign(pubkey, |msg| id.sign(msg));
/// ```
pub struct TraceConfig {
    pub capability: String,
    pub outcome: Outcome,
    pub model_id: String,
    pub latency_ms: u32,
    pub input_size: u32,
    pub context_hash: ContextHash,
    pub context_text: Option<String>,
    pub session_id: Option<String>,
    pub owner_account: Option<String>,
    pub device_identity: Option<String>,
    pub agent_id: Option<String>,
    pub sigil_id: Option<String>,
}

impl TraceConfig {
    /// Create a trace config for system/anonymous traces (no Sigil attribution).
    pub fn new(
        capability: impl Into<String>,
        outcome: Outcome,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            capability: capability.into(),
            outcome,
            model_id: model_id.into(),
            latency_ms: 0,
            input_size: 0,
            context_hash: [0u8; 16],
            context_text: None,
            session_id: None,
            owner_account: None,
            device_identity: None,
            agent_id: None,
            sigil_id: None,
        }
    }

    /// Create a trace config attributed to a Sigil. This is the preferred
    /// constructor for any agent that has an on-chain identity — sigil_id
    /// is set at construction time, impossible to forget.
    pub fn for_sigil(
        sigil_id: impl Into<String>,
        capability: impl Into<String>,
        outcome: Outcome,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            capability: capability.into(),
            outcome,
            model_id: model_id.into(),
            latency_ms: 0,
            input_size: 0,
            context_hash: [0u8; 16],
            context_text: None,
            session_id: None,
            owner_account: None,
            device_identity: None,
            agent_id: None,
            sigil_id: Some(sigil_id.into()),
        }
    }

    /// Set context from natural language. Computes both SimHash and stores the text.
    pub fn context(mut self, text: &str) -> Self {
        self.context_hash = crate::context::simhash(text);
        self.context_text = Some(text.to_string());
        self
    }

    /// Set context_hash and context_text independently (for payloads where
    /// the hash is derived from a different string than the stored text).
    pub fn context_raw(mut self, hash: ContextHash, text: Option<String>) -> Self {
        self.context_hash = hash;
        self.context_text = text;
        self
    }

    pub fn latency_ms(mut self, ms: u32) -> Self {
        self.latency_ms = ms;
        self
    }
    pub fn input_size(mut self, size: u32) -> Self {
        self.input_size = size;
        self
    }
    pub fn session_id(mut self, id: Option<String>) -> Self {
        self.session_id = id;
        self
    }

    /// Set owner_account + device_identity together (they always travel as a pair).
    pub fn identity(mut self, owner: Option<String>, device: Option<String>) -> Self {
        self.owner_account = owner;
        self.device_identity = device;
        self
    }

    pub fn agent_id(mut self, id: Option<String>) -> Self {
        self.agent_id = id;
        self
    }
    pub fn sigil_id(mut self, id: Option<String>) -> Self {
        self.sigil_id = id;
        self
    }

    /// Terminal: sign and produce a Trace.
    pub fn sign(self, node_pubkey: [u8; 32], sign_fn: impl FnOnce(&[u8]) -> Signature) -> Trace {
        Trace::new_with_agent(
            self.capability,
            self.outcome,
            self.latency_ms,
            self.input_size,
            self.context_hash,
            self.context_text,
            self.session_id,
            self.owner_account,
            self.device_identity,
            self.agent_id,
            self.sigil_id,
            self.model_id,
            node_pubkey,
            sign_fn,
        )
    }
}

impl Trace {
    /// Create a new trace, computing its content-addressed ID and signature.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capability: String,
        outcome: Outcome,
        latency_ms: u32,
        input_size: u32,
        context_hash: ContextHash,
        context_text: Option<String>,
        session_id: Option<String>,
        model_id: String,
        node_pubkey: [u8; 32],
        sign_fn: impl FnOnce(&[u8]) -> Signature,
    ) -> Self {
        Self::new_with_identity(
            capability,
            outcome,
            latency_ms,
            input_size,
            context_hash,
            context_text,
            session_id,
            None,
            None,
            model_id,
            node_pubkey,
            sign_fn,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_identity(
        capability: String,
        outcome: Outcome,
        latency_ms: u32,
        input_size: u32,
        context_hash: ContextHash,
        context_text: Option<String>,
        session_id: Option<String>,
        owner_account: Option<String>,
        device_identity: Option<String>,
        model_id: String,
        node_pubkey: [u8; 32],
        sign_fn: impl FnOnce(&[u8]) -> Signature,
    ) -> Self {
        Self::new_with_agent(
            capability,
            outcome,
            latency_ms,
            input_size,
            context_hash,
            context_text,
            session_id,
            owner_account,
            device_identity,
            None,
            None,
            model_id,
            node_pubkey,
            sign_fn,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_agent(
        capability: String,
        outcome: Outcome,
        latency_ms: u32,
        input_size: u32,
        context_hash: ContextHash,
        context_text: Option<String>,
        session_id: Option<String>,
        owner_account: Option<String>,
        device_identity: Option<String>,
        agent_id: Option<String>,
        sigil_id: Option<String>,
        model_id: String,
        node_pubkey: [u8; 32],
        sign_fn: impl FnOnce(&[u8]) -> Signature,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let signable = Self::signable_bytes(
            &capability,
            outcome,
            latency_ms,
            input_size,
            &context_hash,
            context_text.as_deref(),
            session_id.as_deref(),
            owner_account.as_deref(),
            device_identity.as_deref(),
            agent_id.as_deref(),
            sigil_id.as_deref(),
            &model_id,
            timestamp,
            &node_pubkey,
        );

        let signature = sign_fn(&signable);

        let mut hasher = Sha256::new();
        hasher.update(&signable);
        hasher.update(signature.to_bytes());
        let id: [u8; 32] = hasher.finalize().into();

        Self {
            id,
            capability,
            outcome,
            latency_ms,
            input_size,
            context_hash,
            context_text,
            session_id,
            owner_account,
            device_identity,
            agent_id,
            sigil_id,
            model_id,
            timestamp,
            node_pubkey,
            signature,
        }
    }

    /// Whether this trace is attributed to a Sigil (on-chain identity).
    /// Attributed traces receive a slight pheromone intensity boost —
    /// the system naturally rewards identity without mandating it.
    pub fn is_attributed(&self) -> bool {
        self.sigil_id.is_some()
    }

    /// Verify this trace's signature is valid.
    pub fn verify(&self) -> bool {
        let signable = Self::signable_bytes(
            &self.capability,
            self.outcome,
            self.latency_ms,
            self.input_size,
            &self.context_hash,
            self.context_text.as_deref(),
            self.session_id.as_deref(),
            self.owner_account.as_deref(),
            self.device_identity.as_deref(),
            self.agent_id.as_deref(),
            self.sigil_id.as_deref(),
            &self.model_id,
            self.timestamp,
            &self.node_pubkey,
        );
        crate::identity::NodeIdentity::verify(&self.node_pubkey, &signable, &self.signature)
    }

    /// Verify the content-addressed ID matches.
    pub fn verify_id(&self) -> bool {
        let signable = Self::signable_bytes(
            &self.capability,
            self.outcome,
            self.latency_ms,
            self.input_size,
            &self.context_hash,
            self.context_text.as_deref(),
            self.session_id.as_deref(),
            self.owner_account.as_deref(),
            self.device_identity.as_deref(),
            self.agent_id.as_deref(),
            self.sigil_id.as_deref(),
            &self.model_id,
            self.timestamp,
            &self.node_pubkey,
        );
        let mut hasher = Sha256::new();
        hasher.update(&signable);
        hasher.update(self.signature.to_bytes());
        let expected: [u8; 32] = hasher.finalize().into();
        self.id == expected
    }

    /// Signable bytes include all fields that contribute to trace identity.
    ///
    /// Backward compatibility: if BOTH context_text and session_id are None
    /// (i.e., a v0.2.0 trace), the byte layout matches v0.2.0 exactly.
    /// When either field is Some (v0.2.1+), a version tag 0xFF is inserted
    /// after context_hash, followed by length-prefixed optional fields.
    /// This ensures old traces received from the network still verify correctly.
    #[allow(clippy::too_many_arguments)]
    fn signable_bytes(
        capability: &str,
        outcome: Outcome,
        latency_ms: u32,
        input_size: u32,
        context_hash: &ContextHash,
        context_text: Option<&str>,
        session_id: Option<&str>,
        owner_account: Option<&str>,
        device_identity: Option<&str>,
        agent_id: Option<&str>,
        sigil_id: Option<&str>,
        model_id: &str,
        timestamp: u64,
        node_pubkey: &[u8; 32],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(capability.as_bytes());
        buf.push(0);
        buf.push(outcome as u8);
        buf.extend_from_slice(&latency_ms.to_le_bytes());
        buf.extend_from_slice(&input_size.to_le_bytes());
        buf.extend_from_slice(context_hash);
        // Version tags for backward-compatible signing:
        // 0xFF = v0.2.1 (context_text + session_id)
        // 0xFE = Identity V1 (+ owner_account + device_identity)
        // 0xFD = Agent V1 (+ agent_id)
        // 0xFC = Sigil V1 (+ sigil_id)
        let has_v021_fields = context_text.is_some() || session_id.is_some();
        let has_identity_v1 = owner_account.is_some() || device_identity.is_some();
        let has_agent_id = agent_id.is_some();
        let has_sigil_id = sigil_id.is_some();
        if has_sigil_id {
            buf.push(0xFC); // Sigil V1 tag
            push_optional_bytes(&mut buf, context_text);
            push_optional_bytes(&mut buf, session_id);
            push_optional_bytes(&mut buf, owner_account);
            push_optional_bytes(&mut buf, device_identity);
            push_optional_bytes(&mut buf, agent_id);
            push_optional_bytes(&mut buf, sigil_id);
        } else if has_agent_id {
            buf.push(0xFD); // Agent V1 tag
            push_optional_bytes(&mut buf, context_text);
            push_optional_bytes(&mut buf, session_id);
            push_optional_bytes(&mut buf, owner_account);
            push_optional_bytes(&mut buf, device_identity);
            push_optional_bytes(&mut buf, agent_id);
        } else if has_identity_v1 {
            buf.push(0xFE); // Identity V1 tag
            push_optional_bytes(&mut buf, context_text);
            push_optional_bytes(&mut buf, session_id);
            push_optional_bytes(&mut buf, owner_account);
            push_optional_bytes(&mut buf, device_identity);
        } else if has_v021_fields {
            buf.push(0xFF); // version tag — absent in v0.2.0 signing
            push_optional_bytes(&mut buf, context_text);
            push_optional_bytes(&mut buf, session_id);
        }
        buf.extend_from_slice(model_id.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&timestamp.to_le_bytes());
        buf.extend_from_slice(node_pubkey);
        buf
    }
}

fn push_optional_bytes(buf: &mut Vec<u8>, value: Option<&str>) {
    if let Some(value) = value {
        buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
        buf.extend_from_slice(value.as_bytes());
    } else {
        buf.extend_from_slice(&0u32.to_le_bytes());
    }
}

mod signature_serde {
    use ed25519_dalek::Signature;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(sig: &Signature, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&sig.to_bytes())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Signature, D::Error> {
        let bytes = <Vec<u8>>::deserialize(d)?;
        let arr: [u8; 64] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("signature must be 64 bytes"))?;
        Ok(Signature::from_bytes(&arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::identity::NodeIdentity;

    fn make_trace(id: &NodeIdentity, cap: &str, outcome: Outcome, context: &str) -> Trace {
        Trace::new(
            cap.into(),
            outcome,
            100,
            5000,
            simhash(context),
            Some(context.to_string()),
            None,
            "claude-opus-4-6".into(),
            id.public_key_bytes(),
            |msg| id.sign(msg),
        )
    }

    #[test]
    fn create_and_verify() {
        let id = NodeIdentity::generate();
        let trace = make_trace(
            &id,
            "urn:mcp:anthropic:claude:code",
            Outcome::Succeeded,
            "refactoring async rust code",
        );

        assert!(trace.verify(), "signature should be valid");
        assert!(trace.verify_id(), "content-addressed ID should match");
        assert_eq!(trace.model_id, "claude-opus-4-6");
        assert_eq!(trace.input_size, 5000);
    }

    #[test]
    fn identity_metadata_round_trip_verifies() {
        let id = NodeIdentity::generate();
        let trace = Trace::new_with_identity(
            "claude-code/Edit".into(),
            Outcome::Succeeded,
            12,
            42,
            simhash("fix workspace identity model"),
            Some("fix workspace identity model".into()),
            Some("session-1".into()),
            Some("oasyce1owner".into()),
            Some(id.device_identity()),
            "codex".into(),
            id.public_key_bytes(),
            |msg| id.sign(msg),
        );

        assert!(trace.verify());
        assert!(trace.verify_id());
        assert_eq!(trace.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(
            trace.device_identity.as_deref(),
            Some(id.device_identity().as_str())
        );
    }

    #[test]
    fn tampered_trace_fails_verification() {
        let id = NodeIdentity::generate();
        let mut trace = make_trace(&id, "some-tool", Outcome::Succeeded, "test context");

        trace.latency_ms = 999; // tamper
        assert!(!trace.verify(), "tampered trace should fail verification");
    }

    #[test]
    fn different_traces_have_different_ids() {
        let id = NodeIdentity::generate();
        let t1 = make_trace(&id, "cap-a", Outcome::Succeeded, "context alpha");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = make_trace(&id, "cap-b", Outcome::Failed, "context beta");
        assert_ne!(t1.id, t2.id);
    }

    #[test]
    fn context_hash_is_stored() {
        let id = NodeIdentity::generate();
        let ctx = "translating a technical document about P2P networking";
        let trace = make_trace(&id, "deepl/translate", Outcome::Succeeded, ctx);
        assert_eq!(trace.context_hash, simhash(ctx));
    }

    #[test]
    fn builder_produces_valid_trace() {
        let id = NodeIdentity::generate();
        let trace = TraceConfig::new("code-review", Outcome::Succeeded, "claude-opus-4-6")
            .context("reviewing Rust async patterns")
            .latency_ms(42)
            .input_size(1024)
            .session_id(Some("sess-1".into()))
            .identity(Some("oasyce1owner".into()), Some(id.device_identity()))
            .sigil_id(Some("SIG_abc123".into()))
            .sign(id.public_key_bytes(), |msg| id.sign(msg));

        assert!(trace.verify(), "builder trace must have valid signature");
        assert!(
            trace.verify_id(),
            "builder trace must have valid content-addressed ID"
        );
        assert_eq!(trace.capability, "code-review");
        assert_eq!(trace.sigil_id.as_deref(), Some("SIG_abc123"));
        assert_eq!(trace.owner_account.as_deref(), Some("oasyce1owner"));
        assert_eq!(trace.latency_ms, 42);
    }

    #[test]
    fn for_sigil_produces_attributed_trace() {
        let id = NodeIdentity::generate();
        let trace = TraceConfig::for_sigil("SIG_deadbeef", "cap", Outcome::Succeeded, "model")
            .context("testing attribution")
            .sign(id.public_key_bytes(), |msg| id.sign(msg));

        assert!(trace.is_attributed());
        assert_eq!(trace.sigil_id.as_deref(), Some("SIG_deadbeef"));
        assert!(trace.verify());
    }

    #[test]
    fn anonymous_trace_is_not_attributed() {
        let id = NodeIdentity::generate();
        let trace = TraceConfig::new("cap", Outcome::Succeeded, "model")
            .sign(id.public_key_bytes(), |msg| id.sign(msg));

        assert!(!trace.is_attributed());
        assert_eq!(trace.sigil_id, None);
    }

    #[test]
    fn builder_context_sets_both_hash_and_text() {
        let id = NodeIdentity::generate();
        let ctx = "debugging memory allocation in Rust";
        let trace = TraceConfig::new("debug", Outcome::Failed, "model")
            .context(ctx)
            .sign(id.public_key_bytes(), |msg| id.sign(msg));

        assert_eq!(trace.context_hash, simhash(ctx));
        assert_eq!(trace.context_text.as_deref(), Some(ctx));
        assert!(trace.verify());
    }
}
