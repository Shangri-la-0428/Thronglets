//! Pheromone Field — stigmergic collective memory as a unified graph.
//!
//! Traces don't get stored then queried. Traces *excite* field points.
//! The field's state IS the memory. Query the field = perceive the memory.
//!
//! Two layers of intelligence:
//! 1. **Stigmergy** (ant-like): excite → decay → scan. Passive memory.
//! 2. **Self-organization** (slime-mold-like): diffusion + Hebbian coupling.
//!    The field evolves on its own clock via tick(). No agent mediation needed.
//!
//! Diffusion: intensity spreads to neighboring context buckets. Creates
//! continuous gradients. The field discovers context relationships no agent
//! explicitly established.
//!
//! Hebbian coupling: capabilities that fire together wire together.
//! Co-excited capabilities develop associative bonds. scan() surfaces
//! associated capabilities — the field's own "intuition".
//!
//! Architecture: single unified graph behind one lock.
//! Nodes = (capability, context_bucket) → FieldPoint.
//! Edges = capability-level Hebbian couplings (derived from co-excitation).
//! No separate buffers — co-excitation detected from nodes' last_excited timestamps.

use crate::context::ContextHash;
use crate::storage::context_bucket;
use crate::trace::{Outcome, Trace};
use std::collections::HashMap;
use std::sync::RwLock;

// ── Constants ──────────────────────────────────────────────────

/// Half-life in hours. After this many hours without excitation,
/// a field point's intensity halves.
const HALF_LIFE_HOURS: f64 = 48.0;

/// Decay constant: λ = ln(2) / half_life_ms
const DECAY_LAMBDA: f64 = core::f64::consts::LN_2 / (HALF_LIFE_HOURS * 3_600_000.0);

/// Minimum intensity before a field point is pruned (saves memory).
const PRUNE_THRESHOLD: f64 = 0.01;

/// EMA smoothing factor for valence/latency updates.
/// α = 0.1 means new observation has 10% weight.
const EMA_ALPHA: f64 = 0.1;

/// Intensity multiplier for Sigil-attributed traces.
/// Attributed traces deposit slightly more pheromone — the system
/// naturally rewards identity without mandating it.
const ATTRIBUTION_BOOST: f64 = 1.1;

// ── Dynamics Constants ────────────────────────────────────────

/// Fraction of intensity that diffuses to each neighbor per tick.
/// 0.05 = each of ±1 neighbors gets 5%, source keeps 90%.
/// Conservative: total field intensity is conserved (diffusion is zero-sum).
const DIFFUSION_RATE: f64 = 0.05;

/// Time window (ms) for Hebbian co-excitation detection.
/// Capabilities excited within this window of each other form couplings.
const COUPLING_WINDOW_MS: u64 = 60_000;

/// How much a single co-excitation event strengthens a coupling.
const COUPLING_LEARN_RATE: f64 = 0.2;

/// Coupling half-life. Couplings decay slower than field points —
/// associations persist longer than individual memories.
const COUPLING_HALF_LIFE_HOURS: f64 = 168.0;

/// Coupling decay constant.
const COUPLING_DECAY_LAMBDA: f64 =
    core::f64::consts::LN_2 / (COUPLING_HALF_LIFE_HOURS * 3_600_000.0);

/// Minimum coupling weight before pruning.
const COUPLING_PRUNE_THRESHOLD: f64 = 0.05;

// ── Carrying Capacity ───────────────────────────────────────

/// Total pheromone budget for the field. When the field approaches this
/// capacity, new deposits become progressively more expensive — creating
/// natural selection pressure on information quality.
///
/// Calibrated for M1 Pro workloads. Set to f64::MAX to disable.
const FIELD_CAPACITY: f64 = 10_000.0;

// ── Capability Normalization ────────────────────────────────────

/// Normalize agent-specific capability URIs to canonical forms.
///
/// Different agents name the same actions differently:
///   claude-code/Edit, codex/edit, openclaw/Edit → tool:edit
/// Traces preserve original URIs (audit trail). The field normalizes
/// so that same-kind actions from different agents share field points.
/// This is what makes multi-agent pheromone convergence possible.
pub(crate) fn normalize_capability(raw: &str) -> String {
    // Internal lifecycle/signal capabilities — pass through unchanged
    if raw.starts_with("urn:thronglets:") {
        return raw.to_string();
    }

    // Extract the action verb from "prefix/Action" patterns
    let action = raw
        .rsplit_once('/')
        .map(|(_, action)| action)
        .unwrap_or(raw);

    match action.to_ascii_lowercase().as_str() {
        "read" => "tool:read".to_string(),
        "edit" | "write" => "tool:edit".to_string(),
        "bash" | "exec_command" | "review-fix" => "tool:exec".to_string(),
        "grep" | "glob" | "search" => "tool:search".to_string(),
        "agent" => "tool:delegate".to_string(),
        "taskcreate" | "taskupdate" | "taskget" | "tasklist" => "tool:task".to_string(),
        "toolsearch" => "tool:discover".to_string(),
        "enterplanmode" | "exitplanmode" => "tool:plan".to_string(),
        "websearch" | "webfetch" => "tool:web".to_string(),
        "notebookedit" => "tool:notebook".to_string(),
        // MCP tools from external systems — keep as-is
        _ if raw.starts_with("mcp:") => raw.to_string(),
        // Unknown single-segment capabilities — keep as-is
        _ if !raw.contains('/') => raw.to_string(),
        // Unknown prefixed capabilities — normalize prefix away
        _ => format!("tool:{}", action.to_ascii_lowercase()),
    }
}

// ── Field Point ────────────────────────────────────────────────

/// A single point in the pheromone field.
/// Represents the collective memory of a (capability, context_region) pair.
#[derive(Debug, Clone)]
pub struct FieldPoint {
    /// Signal strength — decays exponentially without excitation.
    /// Each trace increments this by 1.0 (after applying decay).
    pub intensity: f64,

    /// Success rate as exponential moving average [0.0, 1.0].
    /// Positive = mostly succeeds. Negative valence not used;
    /// instead, low valence = high failure rate.
    pub valence: f64,

    /// Latency in ms as exponential moving average.
    pub latency: f64,

    /// Variance accumulator for valence (Welford's online algorithm).
    /// High variance = unstable capability = worth attention.
    pub variance: f64,

    /// Timestamp (Unix ms) of last excitation.
    pub last_excited: u64,

    /// Total excitation count (never decays — lifetime counter).
    pub total_excitations: u64,

    /// Distinct source count (device_identity or node_pubkey).
    /// For corroboration — multi-source is more trustworthy.
    pub source_count: u32,

    /// Track sources as hash set of first 8 bytes of identity.
    sources: Vec<[u8; 8]>,
}

impl FieldPoint {
    fn new(now_ms: u64) -> Self {
        Self {
            intensity: 0.0,
            valence: 0.5, // neutral prior
            latency: 0.0,
            variance: 0.0,
            last_excited: now_ms,
            total_excitations: 0,
            source_count: 0,
            sources: Vec::new(),
        }
    }

    fn effective_decay_lambda(&self) -> f64 {
        // ln(1)=0, ln(10)≈2.3, ln(100)≈4.6 → factor: 1.0, 1.35, 1.69
        let reinforcement_factor = 1.0 + (self.total_excitations as f64).ln().max(0.0) * 0.15;
        DECAY_LAMBDA / reinforcement_factor
    }

    /// Apply temporal decay to intensity based on elapsed time.
    /// Well-reinforced points (high total_excitations) decay slower —
    /// up to ~2x half-life at ~800 excitations. This creates persistent
    /// landmarks from collectively reinforced knowledge.
    fn decay(&mut self, now_ms: u64) {
        if now_ms <= self.last_excited {
            return;
        }
        let dt = (now_ms - self.last_excited) as f64;
        self.intensity *= (-self.effective_decay_lambda() * dt).exp();
    }

    /// Excite this field point with a new observation.
    fn excite(
        &mut self,
        outcome: Outcome,
        latency_ms: u64,
        now_ms: u64,
        source_id: [u8; 8],
        deposit: f64,
    ) {
        // First, decay existing intensity to current time
        self.decay(now_ms);

        // Increment intensity (the trace deposits pheromone)
        self.intensity += deposit;
        self.last_excited = now_ms;
        self.total_excitations += 1;

        // Update valence (success rate) via EMA
        let outcome_val = if outcome == Outcome::Succeeded {
            1.0
        } else {
            0.0
        };
        let old_valence = self.valence;
        self.valence = self.valence * (1.0 - EMA_ALPHA) + outcome_val * EMA_ALPHA;

        // Update variance via Welford's online method
        let delta = outcome_val - old_valence;
        let delta2 = outcome_val - self.valence;
        self.variance = self.variance * (1.0 - EMA_ALPHA) + (delta * delta2).abs() * EMA_ALPHA;

        // Update latency via EMA
        if latency_ms > 0 {
            if self.latency == 0.0 {
                self.latency = latency_ms as f64;
            } else {
                self.latency = self.latency * (1.0 - EMA_ALPHA) + latency_ms as f64 * EMA_ALPHA;
            }
        }

        // Track unique sources (keep only first 8 bytes for compactness)
        if !self.sources.contains(&source_id) {
            self.sources.push(source_id);
            self.source_count = self.sources.len() as u32;
        }
    }

    /// Current intensity with decay applied (read-only, doesn't mutate).
    pub fn current_intensity(&self, now_ms: u64) -> f64 {
        if now_ms <= self.last_excited {
            return self.intensity;
        }
        let dt = (now_ms - self.last_excited) as f64;
        self.intensity * (-self.effective_decay_lambda() * dt).exp()
    }

    /// Should this point be pruned?
    pub fn is_dead(&self, now_ms: u64) -> bool {
        self.current_intensity(now_ms) < PRUNE_THRESHOLD
    }
}

// ── Abstraction Levels ───────────────────────────────────────

/// Abstraction level of a field point. The same physical trace excites
/// multiple levels — concrete experience stays local, abstract patterns flow.
///
/// Concrete: full context ("src/pheromone.rs in Desktop/Thronglets")
/// Project:  project scope ("Desktop/Thronglets") — space isolation lives here
/// Typed:    file-type × language ("SourceFile:rust") — crosses projects
/// Universal: pure capability — the most abstract, densest layer
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    Hash,
    Eq,
    PartialEq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[repr(u8)]
pub enum AbstractionLevel {
    #[default]
    Concrete = 0,
    Project = 1,
    Typed = 2,
    Universal = 3,
}

impl AbstractionLevel {
    /// P2P-syncable levels: Typed and Universal only.
    /// Concrete and Project stay local.
    pub fn is_syncable(self) -> bool {
        matches!(self, Self::Typed | Self::Universal)
    }
}

// ── Field Key ──────────────────────────────────────────────────

/// Composite key for a field point: (capability, context_bucket, level).
/// context_bucket is 16-bit, derived from first 2 bytes of SimHash.
/// Capability is always normalized — different agents' names for the
/// same action collapse to one key, enabling multi-agent convergence.
/// Level determines the abstraction granularity — same physics, different scope.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct FieldKey {
    capability: String,
    bucket: i64,
    level: AbstractionLevel,
}

impl FieldKey {
    fn new(capability: &str, bucket: i64) -> Self {
        Self {
            capability: normalize_capability(capability),
            bucket,
            level: AbstractionLevel::Concrete,
        }
    }

    fn at_level(capability: &str, bucket: i64, level: AbstractionLevel) -> Self {
        Self {
            capability: normalize_capability(capability),
            bucket,
            level,
        }
    }
}

// ── Graph Edges ──────────────────────────────────────────────

/// Directed Hebbian edge key: predecessor → successor at a given level.
/// Temporal order is preserved — "A then B" and "B then A" are
/// distinct edges with independent weights.
/// Edges form within each abstraction level independently.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct EdgeKey {
    predecessor: String,
    successor: String,
    level: AbstractionLevel,
    bucket: i64,
}

impl EdgeKey {
    fn at_level(predecessor: &str, successor: &str, level: AbstractionLevel, bucket: i64) -> Self {
        Self {
            predecessor: normalize_capability(predecessor),
            successor: normalize_capability(successor),
            level,
            bucket,
        }
    }

    /// Returns the other capability in this edge, regardless of direction.
    /// Used by scan() and overlay() which care about association, not order.
    fn other_end(&self, cap: &str) -> Option<&str> {
        if self.predecessor == cap {
            Some(&self.successor)
        } else if self.successor == cap {
            Some(&self.predecessor)
        } else {
            None
        }
    }
}

/// A Hebbian edge between two capabilities.
/// Strengthens when they co-fire, decays over time.
#[derive(Debug, Clone)]
struct Edge {
    weight: f64,
    last_reinforced: u64,
}

impl Edge {
    fn current_weight(&self, now_ms: u64) -> f64 {
        if now_ms <= self.last_reinforced {
            return self.weight;
        }
        let dt = (now_ms - self.last_reinforced) as f64;
        self.weight * (-COUPLING_DECAY_LAMBDA * dt).exp()
    }

    fn is_dead(&self, now_ms: u64) -> bool {
        self.current_weight(now_ms) < COUPLING_PRUNE_THRESHOLD
    }
}

// ── Result Types ─────────────────────────────────────────────

/// Result of a tick() — the field's autonomous self-evolution step.
#[derive(Debug)]
pub struct TickResult {
    pub diffused: usize,
    pub couplings_reinforced: usize,
    pub couplings_pruned: usize,
    pub points_pruned: usize,
    /// Current field load: total_intensity / FIELD_CAPACITY.
    /// 0.0 = empty, 1.0 = at carrying capacity.
    pub load_factor: f64,
}

impl Default for TickResult {
    fn default() -> Self {
        Self {
            diffused: 0,
            couplings_reinforced: 0,
            couplings_pruned: 0,
            points_pruned: 0,
            load_factor: 0.0,
        }
    }
}

/// Serializable coupling entry for snapshots.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CouplingSnapshotEntry {
    #[serde(alias = "cap_a")]
    pub predecessor: String,
    #[serde(alias = "cap_b")]
    pub successor: String,
    pub weight: f64,
    pub last_reinforced: u64,
    #[serde(default)]
    pub level: AbstractionLevel,
    #[serde(default)]
    pub bucket: i64,
}

/// Result of scanning the field near a context.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldScan {
    pub capability: String,
    pub intensity: f64,
    pub valence: f64,
    pub latency: f64,
    pub variance: f64,
    pub total_excitations: u64,
    pub source_count: u32,
    pub context_similarity: f64,
    pub level: AbstractionLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupled_from: Option<String>,
}

/// A Hebbian cluster: capabilities that frequently co-activate.
/// Emergent from field physics, not designed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FieldCluster {
    pub capabilities: Vec<String>,
    pub total_weight: f64,
    pub edge_count: usize,
    pub level: u8,
    pub bucket: i64,
}

/// Snapshot of the entire field for P2P sync.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldSnapshot {
    pub points: Vec<FieldSnapshotEntry>,
    #[serde(default)]
    pub couplings: Vec<CouplingSnapshotEntry>,
    /// Total field intensity at snapshot time. Receivers can use this
    /// to gauge the source field's load factor.
    #[serde(default)]
    pub total_intensity: f64,
    /// Public key of the node that produced this snapshot (32 bytes).
    #[serde(default)]
    pub node_pubkey: [u8; 32],
    /// Ed25519 signature over the snapshot content (64 bytes).
    #[serde(default)]
    pub signature: Vec<u8>,
}

/// Domain separator for field snapshot signatures.
const FIELD_SNAPSHOT_SIGN_TAG: &[u8] = b"thronglets.field_snapshot.v1";

impl FieldSnapshot {
    /// Compute the signable byte representation of this snapshot.
    pub(crate) fn signable_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(FIELD_SNAPSHOT_SIGN_TAG);
        buf.extend_from_slice(&(self.points.len() as u32).to_le_bytes());
        for p in &self.points {
            buf.extend_from_slice(p.capability.as_bytes());
            buf.push(0);
            buf.extend_from_slice(&p.bucket.to_le_bytes());
            buf.extend_from_slice(&p.intensity.to_le_bytes());
            buf.extend_from_slice(&p.last_excited.to_le_bytes());
            buf.push(p.level as u8);
        }
        buf.extend_from_slice(&(self.couplings.len() as u32).to_le_bytes());
        for c in &self.couplings {
            buf.extend_from_slice(c.predecessor.as_bytes());
            buf.push(0);
            buf.extend_from_slice(c.successor.as_bytes());
            buf.push(0);
            buf.extend_from_slice(&c.weight.to_le_bytes());
            buf.push(c.level as u8);
            buf.extend_from_slice(&c.bucket.to_le_bytes());
        }
        buf.extend_from_slice(&self.total_intensity.to_le_bytes());
        buf.extend_from_slice(&self.node_pubkey);
        buf
    }

    /// Sign this snapshot with the given node identity.
    pub fn sign(&mut self, identity: &crate::identity::NodeIdentity) {
        self.node_pubkey = identity.public_key_bytes();
        let signable = self.signable_bytes();
        let sig = identity.sign(&signable);
        self.signature = sig.to_bytes().to_vec();
    }

    /// Verify the signature on this snapshot.
    pub fn verify(&self) -> bool {
        if self.node_pubkey == [0u8; 32] || self.signature.len() != 64 {
            return false;
        }
        let signable = self.signable_bytes();
        let Ok(sig) = ed25519_dalek::Signature::from_slice(&self.signature) else {
            return false;
        };
        crate::identity::NodeIdentity::verify(&self.node_pubkey, &signable, &sig)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldSnapshotEntry {
    pub capability: String,
    pub bucket: i64,
    pub intensity: f64,
    pub valence: f64,
    pub latency: f64,
    pub variance: f64,
    pub last_excited: u64,
    pub total_excitations: u64,
    pub source_count: u32,
    #[serde(default)]
    pub source_hashes: Vec<[u8; 8]>,
    #[serde(default)]
    pub level: AbstractionLevel,
}

/// Delta for incremental P2P sync.
#[derive(Debug, Clone)]
pub struct FieldDelta {
    pub capability: String,
    pub bucket: i64,
    pub intensity_add: f64,
    pub outcome: Outcome,
    pub latency_ms: u64,
    pub source_id: [u8; 8],
    pub timestamp: u64,
    pub level: AbstractionLevel,
}

/// Semantic-stable effect signals derived from field state.
///
/// These are the field's "hormones" — broadcast signals any external
/// system can consume. The overlay is a projection of field state,
/// not a consumer-specific API.
#[derive(Debug, Clone)]
pub struct FieldOverlay {
    /// How well the field knows this capability in this context [0, 1].
    /// 0 = unknown, 1 = deeply familiar (saturating sigmoid of intensity).
    pub familiarity: f64,

    /// Agreement across observations [0, 1].
    /// High = consistent outcomes. Low = unstable/contradictory.
    pub consensus: f64,

    /// Activity trend [-1, 1].
    /// Positive = recently active (within one half-life).
    /// Negative = decaying (beyond one half-life). Zero = exactly at half-life.
    pub momentum: f64,

    /// Connectedness to other capabilities via Hebbian bonds [0, 1].
    /// High = strongly associated with other capabilities.
    pub coupling: f64,
}

// ── Unified Graph ────────────────────────────────────────────

/// The field's internal state: nodes + edges in a single structure.
/// One lock protects everything. No separate buffers.
struct FieldInner {
    nodes: HashMap<FieldKey, FieldPoint>,
    edges: HashMap<EdgeKey, Edge>,
    /// Running sum of all field point intensities. Maintained incrementally
    /// in excite/prune — never recomputed from scratch. Drives the
    /// carrying capacity mechanism: deposit cost scales with load_factor.
    total_intensity: f64,
}

impl FieldInner {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
            total_intensity: 0.0,
        }
    }

    fn current_total_intensity(&self, now_ms: u64) -> f64 {
        self.nodes
            .values()
            .map(|point| point.current_intensity(now_ms))
            .sum()
    }

    /// Current load factor: total_intensity / FIELD_CAPACITY.
    /// 0.0 = empty field, 1.0 = at capacity, >1.0 = over capacity.
    fn load_factor(&self, now_ms: u64) -> f64 {
        self.current_total_intensity(now_ms) / FIELD_CAPACITY
    }

    /// Diffuse intensity from each field point to neighboring buckets.
    /// Conservative: total intensity is preserved (source loses what neighbors gain).
    fn diffuse(&mut self, now_ms: u64) -> usize {
        // Phase 1: collect diffusion operations (read-only)
        let mut ops: Vec<(FieldKey, f64, f64, f64)> = Vec::new();
        let mut sources: Vec<(FieldKey, f64)> = Vec::new();

        for (key, point) in &self.nodes {
            let intensity = point.current_intensity(now_ms);
            if intensity < PRUNE_THRESHOLD * 10.0 {
                continue;
            }
            let diffuse_amount = intensity * DIFFUSION_RATE;

            for offset in [-1i64, 1] {
                let neighbor_bucket = key.bucket + offset;
                if !(0..=65535).contains(&neighbor_bucket) {
                    continue;
                }
                let neighbor_key = FieldKey::at_level(&key.capability, neighbor_bucket, key.level);
                ops.push((neighbor_key, diffuse_amount, point.valence, point.latency));
            }
            sources.push((key.clone(), diffuse_amount * 2.0));
        }

        let diffused = ops.len();

        // Phase 2: apply — source loses, neighbors gain
        for (key, amount) in sources {
            if let Some(point) = self.nodes.get_mut(&key) {
                point.decay(now_ms);
                point.intensity = (point.intensity - amount).max(0.0);
                point.last_excited = now_ms;
            }
        }

        for (key, amount, valence, latency) in ops {
            let point = self
                .nodes
                .entry(key)
                .or_insert_with(|| FieldPoint::new(now_ms));
            point.decay(now_ms);
            let total = point.intensity + amount;
            if total > 0.0 {
                point.valence = (point.valence * point.intensity + valence * amount) / total;
                point.latency = (point.latency * point.intensity + latency * amount) / total;
            }
            point.intensity = total;
            point.last_excited = now_ms;
        }

        diffused
    }

    /// Excite the field at all four abstraction levels for a single trace.
    ///
    /// Three evolutionary pressures shape the effective deposit:
    /// 1. **Outcome weighting**: successful traces deposit more pheromone
    /// 2. **Corroboration bonus**: multi-source points receive stronger deposits (per-level)
    /// 3. **Carrying capacity**: deposit cost increases quadratically with field load
    ///
    /// Same physics, four levels. The field naturally produces correct behavior:
    /// - Level 0 points are sparse → low corroboration → weak signals
    /// - Level 3 points are dense → high corroboration → strong signals
    fn excite_node(&mut self, trace: &Trace, space: Option<&str>) -> FieldDelta {
        use crate::target_kind;

        let cap = normalize_capability(&trace.capability);
        let now_ms = trace.timestamp;
        let source_id = source_fingerprint(trace);

        // ── Outcome weighting (computed once) ──
        let outcome_weight = match trace.outcome {
            Outcome::Succeeded => 1.0,
            Outcome::Partial => 0.5,
            Outcome::Failed => 0.1,
            Outcome::Timeout => 0.2,
        };
        let attribution = if trace.is_attributed() {
            ATTRIBUTION_BOOST
        } else {
            1.0
        };
        let base_deposit = outcome_weight * attribution;

        // ── Carrying capacity (computed once, shared across levels) ──
        self.total_intensity = self.current_total_intensity(now_ms);
        let load = self.load_factor(now_ms);
        let cost_multiplier = 1.0 + load * load;

        // ── Compute keys for all four levels ──
        let concrete_bucket = context_bucket(&trace.context_hash);

        // Level 1 (Project): from space parameter (passed by caller)
        let project_bucket = space.map(target_kind::space_bucket);

        // Level 2 (Typed): from file path in context
        let file_path = trace
            .context_text
            .as_deref()
            .and_then(target_kind::extract_file_path);
        let typed_bucket = file_path
            .map(target_kind::typed_bucket)
            .unwrap_or_else(|| target_kind::typed_bucket("unknown.src"));

        // Excite each level with its own corroboration bonus
        let keys: [(FieldKey, bool); 4] = [
            (
                FieldKey::at_level(&cap, concrete_bucket, AbstractionLevel::Concrete),
                true,
            ),
            (
                FieldKey::at_level(
                    &cap,
                    project_bucket.unwrap_or(-1),
                    AbstractionLevel::Project,
                ),
                project_bucket.is_some(),
            ),
            (
                FieldKey::at_level(&cap, typed_bucket, AbstractionLevel::Typed),
                true,
            ),
            (
                FieldKey::at_level(&cap, 0, AbstractionLevel::Universal),
                true,
            ),
        ];

        let mut total_deposited = 0.0;
        let mut concrete_deposit = 0.0;

        for (key, active) in &keys {
            if !*active {
                continue;
            }

            // Per-level corroboration
            let prior_source_count = self.nodes.get(key).map(|p| p.source_count).unwrap_or(0);
            let corroboration = if prior_source_count > 1 {
                1.0 + (prior_source_count as f64).ln() * 0.1
            } else {
                1.0
            };

            let effective_deposit = base_deposit * corroboration / cost_multiplier;

            let point = self
                .nodes
                .entry(key.clone())
                .or_insert_with(|| FieldPoint::new(now_ms));
            point.excite(
                trace.outcome,
                trace.latency_ms as u64,
                now_ms,
                source_id,
                effective_deposit,
            );
            total_deposited += effective_deposit;
            if key.level == AbstractionLevel::Concrete {
                concrete_deposit = effective_deposit;
            }
        }

        self.total_intensity += total_deposited;

        // Return delta for the Concrete level (P2P compat)
        FieldDelta {
            capability: cap,
            bucket: concrete_bucket,
            intensity_add: concrete_deposit,
            outcome: trace.outcome,
            latency_ms: trace.latency_ms as u64,
            source_id,
            timestamp: now_ms,
            level: AbstractionLevel::Concrete,
        }
    }
}

// ── Pheromone Field ──────────────────────────────────────────

/// The pheromone field. A self-organizing signal network.
///
/// Two modes of operation:
/// - **Passive** (ant-like): agents excite() and scan(). The field remembers.
/// - **Active** (slime-mold-like): tick() runs internal dynamics. The field evolves.
///
/// Internal dynamics:
/// - Diffusion: intensity flows to neighboring context buckets
/// - Hebbian coupling: co-excited capabilities form associative bonds
/// - Decay: everything fades without reinforcement
///
/// RwLock: concurrent readers, exclusive writers.
pub struct PheromoneField {
    inner: RwLock<FieldInner>,
}

/// Union-find path compression.
fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

impl PheromoneField {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(FieldInner::new()),
        }
    }

    /// Excite the field with a trace. This is the WRITE operation.
    /// No INSERT. The trace modifies the field's state and is gone.
    ///
    /// Also detects Hebbian co-excitations: scans nodes' last_excited
    /// to find capabilities within the coupling window. O(capabilities).
    pub fn excite(&self, trace: &Trace) -> FieldDelta {
        self.excite_with_space(trace, None)
    }

    /// Excite the field with a trace, optionally providing the project space.
    /// Space enables Level 1 (Project) excitation — without it, only
    /// Concrete, Typed, and Universal levels fire.
    pub fn excite_with_space(&self, trace: &Trace, space: Option<&str>) -> FieldDelta {
        let mut inner = self.inner.write().unwrap();
        let delta = inner.excite_node(trace, space);

        let now_ms = trace.timestamp;
        let normalized_cap = normalize_capability(&trace.capability);

        let concrete_bucket = context_bucket(&trace.context_hash);
        let project_bucket = space.map(crate::target_kind::space_bucket).unwrap_or(0);
        let file_path = trace
            .context_text
            .as_deref()
            .and_then(crate::target_kind::extract_file_path);
        let typed_bucket = file_path
            .map(crate::target_kind::typed_bucket)
            .unwrap_or_else(|| crate::target_kind::typed_bucket("unknown.src"));

        let bucket_for = |level: AbstractionLevel| -> i64 {
            match level {
                AbstractionLevel::Concrete => concrete_bucket,
                AbstractionLevel::Project => project_bucket,
                AbstractionLevel::Typed => typed_bucket,
                AbstractionLevel::Universal => 0,
            }
        };

        // Hebbian: detect co-excitations per level.
        // Only same-bucket nodes form edges — edges inherit spatial scope.
        let co_excited: Vec<(String, AbstractionLevel)> = {
            let mut max_ts: HashMap<(String, AbstractionLevel), u64> = HashMap::new();
            for (k, p) in &inner.nodes {
                if k.capability == normalized_cap {
                    continue;
                }
                if k.bucket != bucket_for(k.level) {
                    continue;
                }
                let key = (k.capability.clone(), k.level);
                let ts = max_ts.entry(key).or_insert(0);
                *ts = (*ts).max(p.last_excited);
            }
            max_ts
                .into_iter()
                .filter(|(_, ts)| now_ms.saturating_sub(*ts) < COUPLING_WINDOW_MS)
                .map(|((cap, level), _)| (cap, level))
                .collect()
        };

        for (cap, level) in &co_excited {
            let edge_key = EdgeKey::at_level(cap, &normalized_cap, *level, bucket_for(*level));
            let edge = inner.edges.entry(edge_key).or_insert(Edge {
                weight: 0.0,
                last_reinforced: now_ms,
            });
            let dt = now_ms.saturating_sub(edge.last_reinforced) as f64;
            edge.weight *= (-COUPLING_DECAY_LAMBDA * dt).exp();
            edge.weight += COUPLING_LEARN_RATE;
            edge.last_reinforced = now_ms;
        }

        delta
    }

    /// Scan the field near a context hash at Concrete level.
    pub fn scan(
        &self,
        context_hash: &ContextHash,
        bucket_radius: i64,
        limit: usize,
    ) -> Vec<FieldScan> {
        self.scan_at_level(
            context_hash,
            bucket_radius,
            limit,
            AbstractionLevel::Concrete,
        )
    }

    /// Scan the field near a context hash at a specific abstraction level.
    pub fn scan_at_level(
        &self,
        context_hash: &ContextHash,
        bucket_radius: i64,
        limit: usize,
        level: AbstractionLevel,
    ) -> Vec<FieldScan> {
        let target_bucket = context_bucket(context_hash);
        let bucket_lo = (target_bucket - bucket_radius).max(0);
        let bucket_hi = (target_bucket + bucket_radius).min(65535);
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;

        let inner = self.inner.read().unwrap();

        // Phase 1: collect all live points in the bucket range at this level
        let mut cap_map: HashMap<String, FieldScan> = HashMap::new();

        for (key, point) in &inner.nodes {
            if key.level != level || key.bucket < bucket_lo || key.bucket > bucket_hi {
                continue;
            }
            let intensity = point.current_intensity(now_ms);
            if intensity < PRUNE_THRESHOLD {
                continue;
            }

            let bucket_dist = (key.bucket - target_bucket).unsigned_abs();
            let similarity = 1.0 - (bucket_dist as f64 / 65536.0).min(1.0);

            let entry = cap_map
                .entry(key.capability.clone())
                .or_insert_with(|| FieldScan {
                    capability: key.capability.clone(),
                    intensity: 0.0,
                    valence: 0.0,
                    latency: 0.0,
                    variance: 0.0,
                    total_excitations: 0,
                    source_count: 0,
                    context_similarity: 0.0,
                    level: key.level,
                    coupled_from: None,
                });

            let w = intensity;
            let old_w = entry.intensity;
            let total_w = old_w + w;
            if total_w > 0.0 {
                entry.valence = (entry.valence * old_w + point.valence * w) / total_w;
                entry.latency = (entry.latency * old_w + point.latency * w) / total_w;
                entry.variance = (entry.variance * old_w + point.variance * w) / total_w;
                entry.context_similarity =
                    (entry.context_similarity * old_w + similarity * w) / total_w;
            }
            entry.intensity = total_w;
            entry.total_excitations += point.total_excitations;
            entry.source_count = entry.source_count.max(point.source_count);
        }

        // Phase 2: surface Hebbian-coupled capabilities
        let primary_data: Vec<(String, f64, f64, f64)> = cap_map
            .values()
            .map(|s| {
                (
                    s.capability.clone(),
                    s.intensity,
                    s.valence,
                    s.context_similarity,
                )
            })
            .collect();

        for (primary_cap, primary_intensity, primary_valence, primary_sim) in &primary_data {
            for (key, edge) in &inner.edges {
                if key.level != level || key.bucket < bucket_lo || key.bucket > bucket_hi {
                    continue;
                }
                let Some(partner) = key.other_end(primary_cap) else {
                    continue;
                };
                if cap_map.contains_key(partner) {
                    continue;
                }
                let cw = edge.current_weight(now_ms);
                if cw < COUPLING_PRUNE_THRESHOLD {
                    continue;
                }
                cap_map.insert(
                    partner.to_string(),
                    FieldScan {
                        capability: partner.to_string(),
                        intensity: primary_intensity * cw,
                        valence: *primary_valence,
                        latency: 0.0,
                        variance: 0.0,
                        total_excitations: 0,
                        source_count: 0,
                        context_similarity: primary_sim * cw,
                        level: key.level,
                        coupled_from: Some(primary_cap.clone()),
                    },
                );
            }
        }

        let mut results: Vec<FieldScan> = cap_map.into_values().collect();
        results.sort_by(|a, b| {
            b.intensity
                .partial_cmp(&a.intensity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    /// Scan with fallback across abstraction levels.
    ///
    /// Walks: Concrete → Project → Typed → Universal.
    /// Collects results at each level. Stops early if a level yields
    /// strong signals (intensity above threshold). This ensures specific
    /// experience is preferred, with abstract patterns filling in gaps.
    pub fn scan_with_fallback(
        &self,
        context_hash: &ContextHash,
        space: Option<&str>,
        file_path: Option<&str>,
        limit: usize,
    ) -> Vec<FieldScan> {
        use crate::target_kind;

        let concrete_bucket = context_bucket(context_hash);
        let project_bucket = space.map(target_kind::space_bucket);
        let typed_bucket = file_path
            .map(target_kind::typed_bucket)
            .unwrap_or_else(|| target_kind::typed_bucket("unknown.src"));

        let levels: [(AbstractionLevel, i64, bool); 4] = [
            (AbstractionLevel::Concrete, concrete_bucket, true),
            (
                AbstractionLevel::Project,
                project_bucket.unwrap_or(-1),
                project_bucket.is_some(),
            ),
            (AbstractionLevel::Typed, typed_bucket, true),
            (AbstractionLevel::Universal, 0, true),
        ];

        let target_bucket = concrete_bucket;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();

        let mut all_results: Vec<FieldScan> = Vec::new();

        for (level, bucket, active) in levels {
            if !active {
                continue;
            }

            // For Concrete, scan with bucket radius around the context
            // For other levels, scan exact bucket match (radius=0)
            let (scan_bucket, radius) = if level == AbstractionLevel::Concrete {
                (target_bucket, 1i64)
            } else {
                (bucket, 0i64)
            };

            let bucket_lo = (scan_bucket - radius).max(0);
            let bucket_hi = (scan_bucket + radius).min(65535);

            let mut level_results: HashMap<String, FieldScan> = HashMap::new();

            for (key, point) in &inner.nodes {
                if key.level != level || key.bucket < bucket_lo || key.bucket > bucket_hi {
                    continue;
                }
                let intensity = point.current_intensity(now_ms);
                if intensity < PRUNE_THRESHOLD {
                    continue;
                }

                let bucket_dist = (key.bucket - scan_bucket).unsigned_abs();
                let similarity = 1.0 - (bucket_dist as f64 / 65536.0).min(1.0);

                let entry = level_results
                    .entry(key.capability.clone())
                    .or_insert_with(|| FieldScan {
                        capability: key.capability.clone(),
                        intensity: 0.0,
                        valence: 0.0,
                        latency: 0.0,
                        variance: 0.0,
                        total_excitations: 0,
                        source_count: 0,
                        context_similarity: 0.0,
                        level,
                        coupled_from: None,
                    });

                let w = intensity;
                let old_w = entry.intensity;
                let total_w = old_w + w;
                if total_w > 0.0 {
                    entry.valence = (entry.valence * old_w + point.valence * w) / total_w;
                    entry.latency = (entry.latency * old_w + point.latency * w) / total_w;
                    entry.variance = (entry.variance * old_w + point.variance * w) / total_w;
                    entry.context_similarity =
                        (entry.context_similarity * old_w + similarity * w) / total_w;
                }
                entry.intensity = total_w;
                entry.total_excitations += point.total_excitations;
                entry.source_count = entry.source_count.max(point.source_count);
            }

            // Surface Hebbian-coupled capabilities at this level
            let primaries: Vec<(String, f64, f64, f64)> = level_results
                .values()
                .map(|s| {
                    (
                        s.capability.clone(),
                        s.intensity,
                        s.valence,
                        s.context_similarity,
                    )
                })
                .collect();
            for (pcap, pi, pv, ps) in &primaries {
                for (ek, edge) in &inner.edges {
                    if ek.level != level || ek.bucket < bucket_lo || ek.bucket > bucket_hi {
                        continue;
                    }
                    let Some(partner) = ek.other_end(pcap) else {
                        continue;
                    };
                    if level_results.contains_key(partner) {
                        continue;
                    }
                    let cw = edge.current_weight(now_ms);
                    if cw < COUPLING_PRUNE_THRESHOLD {
                        continue;
                    }
                    level_results.insert(
                        partner.to_string(),
                        FieldScan {
                            capability: partner.to_string(),
                            intensity: pi * cw,
                            valence: *pv,
                            latency: 0.0,
                            variance: 0.0,
                            total_excitations: 0,
                            source_count: 0,
                            context_similarity: ps * cw,
                            level,
                            coupled_from: Some(pcap.clone()),
                        },
                    );
                }
            }

            all_results.extend(level_results.into_values());
        }

        all_results.sort_by(|a, b| {
            b.intensity
                .partial_cmp(&a.intensity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(limit);
        all_results
    }

    /// Get field stats for a specific capability at Concrete level (all buckets).
    pub fn aggregate(&self, capability: &str) -> Option<FieldScan> {
        self.aggregate_at_level(capability, AbstractionLevel::Concrete)
    }

    /// Get field stats for a specific capability at a given abstraction level.
    pub fn aggregate_at_level(
        &self,
        capability: &str,
        level: AbstractionLevel,
    ) -> Option<FieldScan> {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();
        let normalized = normalize_capability(capability);

        let mut total_intensity = 0.0;
        let mut weighted_valence = 0.0;
        let mut weighted_latency = 0.0;
        let mut weighted_variance = 0.0;
        let mut total_excitations = 0u64;
        let mut max_source_count = 0u32;

        for (key, point) in &inner.nodes {
            if key.capability != normalized || key.level != level {
                continue;
            }
            let intensity = point.current_intensity(now_ms);
            if intensity < PRUNE_THRESHOLD {
                continue;
            }
            weighted_valence += point.valence * intensity;
            weighted_latency += point.latency * intensity;
            weighted_variance += point.variance * intensity;
            total_intensity += intensity;
            total_excitations += point.total_excitations;
            max_source_count = max_source_count.max(point.source_count);
        }

        if total_intensity < PRUNE_THRESHOLD {
            return None;
        }

        Some(FieldScan {
            capability: capability.to_string(),
            intensity: total_intensity,
            valence: weighted_valence / total_intensity,
            latency: weighted_latency / total_intensity,
            variance: weighted_variance / total_intensity,
            total_excitations,
            source_count: max_source_count,
            context_similarity: 1.0,
            level,
            coupled_from: None,
        })
    }

    /// Prune dead field points and edges. Returns number of pruned points.
    pub fn prune(&self) -> usize {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let mut inner = self.inner.write().unwrap();
        inner.edges.retain(|_, e| !e.is_dead(now_ms));
        let before = inner.nodes.len();
        inner.nodes.retain(|_, p| !p.is_dead(now_ms));
        inner.total_intensity = inner.current_total_intensity(now_ms);
        before - inner.nodes.len()
    }

    /// Autonomous self-evolution step. The field's own clock.
    ///
    /// Call this periodically (e.g. every 5 minutes). The field:
    /// 1. Diffuses intensity to neighboring buckets (slime-mold tube flow)
    /// 2. Prunes dead edges (Hebbian couplings)
    /// 3. Prunes dead field points
    ///
    /// Single lock for the entire tick — atomic self-evolution.
    pub fn tick(&self) -> TickResult {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let mut inner = self.inner.write().unwrap();

        let diffused = inner.diffuse(now_ms);

        let before_edges = inner.edges.len();
        inner.edges.retain(|_, e| !e.is_dead(now_ms));
        let couplings_pruned = before_edges - inner.edges.len();

        let before_nodes = inner.nodes.len();
        inner.nodes.retain(|_, p| !p.is_dead(now_ms));
        let points_pruned = before_nodes - inner.nodes.len();
        inner.total_intensity = inner.current_total_intensity(now_ms);

        TickResult {
            diffused,
            couplings_reinforced: 0,
            couplings_pruned,
            points_pruned,
            load_factor: inner.load_factor(now_ms),
        }
    }

    /// Number of Hebbian edges (for diagnostics).
    pub fn coupling_count(&self) -> usize {
        self.inner.read().unwrap().edges.len()
    }

    /// List Hebbian edges with live weights, sorted by weight descending.
    pub fn active_edges(&self, limit: usize) -> Vec<(String, String, f64, AbstractionLevel, i64)> {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();
        let mut edges: Vec<(String, String, f64, AbstractionLevel, i64)> = inner
            .edges
            .iter()
            .filter_map(|(key, edge)| {
                let w = edge.current_weight(now_ms);
                if w > COUPLING_PRUNE_THRESHOLD {
                    Some((key.predecessor.clone(), key.successor.clone(), w, key.level, key.bucket))
                } else {
                    None
                }
            })
            .collect();
        edges.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        edges.truncate(limit);
        edges
    }

    /// Read emergent structure from the Hebbian edge graph.
    /// Returns connected components of live edges, grouped by abstraction level.
    /// Pure read — does not modify field state.
    pub fn clusters(&self, min_weight: f64) -> Vec<FieldCluster> {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();
        let threshold = if min_weight > 0.0 {
            min_weight
        } else {
            COUPLING_PRUNE_THRESHOLD
        };

        let mut level_edges: HashMap<(u8, i64), Vec<(&str, &str, f64)>> = HashMap::new();
        for (key, edge) in &inner.edges {
            let w = edge.current_weight(now_ms);
            if w > threshold {
                level_edges.entry((key.level as u8, key.bucket)).or_default().push((
                    &key.predecessor,
                    &key.successor,
                    w,
                ));
            }
        }

        let mut clusters = Vec::new();
        for (&(level, bucket), edges) in &level_edges {
            // Union-find on capabilities
            let mut cap_index: HashMap<&str, usize> = HashMap::new();
            let mut parent: Vec<usize> = Vec::new();

            for &(pred, succ, _) in edges {
                for cap in [pred, succ] {
                    if !cap_index.contains_key(cap) {
                        let i = parent.len();
                        cap_index.insert(cap, i);
                        parent.push(i);
                    }
                }
                let a = cap_index[pred];
                let b = cap_index[succ];
                let ra = find(&mut parent, a);
                let rb = find(&mut parent, b);
                if ra != rb {
                    parent[ra] = rb;
                }
            }

            // Collect components
            let mut components: HashMap<usize, (Vec<&str>, f64, usize)> = HashMap::new();
            for (&cap, &idx) in &cap_index {
                let root = find(&mut parent, idx);
                components
                    .entry(root)
                    .or_insert_with(|| (Vec::new(), 0.0, 0))
                    .0
                    .push(cap);
            }
            for &(pred, _, w) in edges {
                let root = find(&mut parent, cap_index[pred]);
                let entry = components.get_mut(&root).unwrap();
                entry.1 += w;
                entry.2 += 1;
            }

            for (_, (mut caps, total_weight, edge_count)) in components {
                if caps.len() < 2 {
                    continue;
                }
                caps.sort();
                clusters.push(FieldCluster {
                    capabilities: caps.into_iter().map(String::from).collect(),
                    total_weight,
                    edge_count,
                    level,
                    bucket,
                });
            }
        }

        clusters.sort_by(|a, b| {
            b.total_weight
                .partial_cmp(&a.total_weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        clusters
    }

    /// Number of live field points (for diagnostics).
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Current field load factor: total_intensity / FIELD_CAPACITY.
    /// 0.0 = empty, 1.0 = at carrying capacity, >1.0 = over capacity.
    pub fn load_factor(&self) -> f64 {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        self.inner.read().unwrap().load_factor(now_ms)
    }

    /// Total pheromone intensity across all field points.
    pub fn total_intensity(&self) -> f64 {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        self.inner.read().unwrap().current_total_intensity(now_ms)
    }

    /// Snapshot the entire field for P2P sync or persistence.
    pub fn snapshot(&self) -> FieldSnapshot {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();

        let points = inner
            .nodes
            .iter()
            .filter(|(_, p)| !p.is_dead(now_ms))
            .map(|(key, point)| FieldSnapshotEntry {
                capability: key.capability.clone(),
                bucket: key.bucket,
                intensity: point.intensity,
                valence: point.valence,
                latency: point.latency,
                variance: point.variance,
                last_excited: point.last_excited,
                total_excitations: point.total_excitations,
                source_count: point.source_count,
                source_hashes: point.sources.clone(),
                level: key.level,
            })
            .collect();

        let couplings = inner
            .edges
            .iter()
            .filter(|(_, e)| !e.is_dead(now_ms))
            .map(|(key, e)| CouplingSnapshotEntry {
                predecessor: key.predecessor.clone(),
                successor: key.successor.clone(),
                weight: e.current_weight(now_ms),
                last_reinforced: e.last_reinforced,
                level: key.level,
                bucket: key.bucket,
            })
            .collect();

        FieldSnapshot {
            points,
            couplings,
            total_intensity: inner.current_total_intensity(now_ms),
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        }
    }

    /// Restore field from a snapshot (e.g., on startup from disk).
    pub fn restore(&self, snapshot: &FieldSnapshot) {
        let mut inner = self.inner.write().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        inner.nodes.clear();
        inner.edges.clear();
        inner.total_intensity = 0.0;

        for entry in &snapshot.points {
            let key = FieldKey::at_level(&entry.capability, entry.bucket, entry.level);
            inner.nodes.insert(
                key,
                FieldPoint {
                    intensity: entry.intensity,
                    valence: entry.valence,
                    latency: entry.latency,
                    variance: entry.variance,
                    last_excited: entry.last_excited,
                    total_excitations: entry.total_excitations,
                    source_count: entry.source_count.max(entry.source_hashes.len() as u32),
                    sources: entry.source_hashes.clone(),
                },
            );
        }

        for entry in &snapshot.couplings {
            let key = EdgeKey::at_level(&entry.predecessor, &entry.successor, entry.level, entry.bucket);
            let edge = inner.edges.entry(key).or_insert(Edge {
                weight: 0.0,
                last_reinforced: entry.last_reinforced,
            });
            edge.weight = edge.weight.max(entry.weight);
            edge.last_reinforced = edge.last_reinforced.max(entry.last_reinforced);
        }

        inner.total_intensity = inner.current_total_intensity(now_ms);
    }

    /// Apply a delta from P2P sync. CRDT-friendly: addition is commutative.
    pub fn apply_delta(&self, delta: &FieldDelta) {
        let key = FieldKey::at_level(&delta.capability, delta.bucket, delta.level);
        let mut inner = self.inner.write().unwrap();
        let point = inner
            .nodes
            .entry(key)
            .or_insert_with(|| FieldPoint::new(delta.timestamp));
        point.excite(
            delta.outcome,
            delta.latency_ms,
            delta.timestamp,
            delta.source_id,
            delta.intensity_add,
        );
        inner.total_intensity += delta.intensity_add;
    }

    /// Snapshot only P2P-syncable levels (Typed + Universal).
    /// Concrete and Project stay local — specific experience doesn't flow.
    pub fn publishable_snapshot(&self) -> FieldSnapshot {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();

        let points = inner
            .nodes
            .iter()
            .filter(|(key, p)| key.level.is_syncable() && !p.is_dead(now_ms))
            .map(|(key, point)| FieldSnapshotEntry {
                capability: key.capability.clone(),
                bucket: key.bucket,
                intensity: point.intensity,
                valence: point.valence,
                latency: point.latency,
                variance: point.variance,
                last_excited: point.last_excited,
                total_excitations: point.total_excitations,
                source_count: point.source_count,
                source_hashes: point.sources.clone(),
                level: key.level,
            })
            .collect();

        let couplings = inner
            .edges
            .iter()
            .filter(|(key, e)| key.level.is_syncable() && !e.is_dead(now_ms))
            .map(|(key, e)| CouplingSnapshotEntry {
                predecessor: key.predecessor.clone(),
                successor: key.successor.clone(),
                weight: e.current_weight(now_ms),
                last_reinforced: e.last_reinforced,
                level: key.level,
                bucket: key.bucket,
            })
            .collect();

        FieldSnapshot {
            points,
            couplings,
            total_intensity: inner.current_total_intensity(now_ms),
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        }
    }

    /// Apply a remote snapshot with trust discount.
    /// Only writes to syncable levels (Typed + Universal).
    /// Remote data is discounted to prevent single-node dominance.
    pub fn apply_remote_snapshot(&self, snapshot: &FieldSnapshot, trust_discount: f64) {
        let mut inner = self.inner.write().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;

        for entry in &snapshot.points {
            if !entry.level.is_syncable() {
                continue;
            }
            let key = FieldKey::at_level(&entry.capability, entry.bucket, entry.level);

            let point = inner
                .nodes
                .entry(key)
                .or_insert_with(|| FieldPoint::new(now_ms));

            // Merge: compare against decayed (current) local intensity
            let local_intensity = point.current_intensity(now_ms);
            let remote_intensity = entry.intensity * trust_discount;
            if remote_intensity > local_intensity {
                let total = local_intensity + remote_intensity;
                if total > 0.0 {
                    point.valence = (point.valence * local_intensity
                        + entry.valence * remote_intensity)
                        / total;
                    point.latency = (point.latency * local_intensity
                        + entry.latency * remote_intensity)
                        / total;
                    point.variance = (point.variance * local_intensity
                        + entry.variance * remote_intensity)
                        / total;
                }
                point.intensity = remote_intensity;
                point.last_excited = point.last_excited.max(entry.last_excited);
            }

            // Evidence merges unconditionally
            point.total_excitations = point.total_excitations.max(entry.total_excitations);
            for src in &entry.source_hashes {
                if !point.sources.contains(src) {
                    point.sources.push(*src);
                    point.source_count = point.sources.len() as u32;
                }
            }
        }

        for entry in &snapshot.couplings {
            if !entry.level.is_syncable() {
                continue;
            }
            let key = EdgeKey::at_level(&entry.predecessor, &entry.successor, entry.level, entry.bucket);
            let discounted_weight = entry.weight * trust_discount;
            let edge = inner.edges.entry(key).or_insert(Edge {
                weight: 0.0,
                last_reinforced: entry.last_reinforced,
            });
            edge.weight = edge.weight.max(discounted_weight);
            edge.last_reinforced = edge.last_reinforced.max(entry.last_reinforced);
        }

        inner.total_intensity = inner.current_total_intensity(now_ms);
    }

    /// List all capabilities with their total intensity at Concrete level.
    pub fn capabilities(&self, limit: usize) -> Vec<FieldScan> {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();

        let mut cap_map: HashMap<&str, (f64, f64, f64, u64, u32)> = HashMap::new();

        for (key, point) in &inner.nodes {
            if key.level != AbstractionLevel::Concrete {
                continue;
            }
            let intensity = point.current_intensity(now_ms);
            if intensity < PRUNE_THRESHOLD {
                continue;
            }
            let entry = cap_map
                .entry(&key.capability)
                .or_insert((0.0, 0.0, 0.0, 0, 0));
            entry.0 += intensity;
            entry.1 += point.valence * intensity;
            entry.2 += point.latency * intensity;
            entry.3 += point.total_excitations;
            entry.4 = entry.4.max(point.source_count);
        }

        let mut results: Vec<FieldScan> = cap_map
            .into_iter()
            .map(|(cap, (intensity, wv, wl, exc, sc))| FieldScan {
                capability: cap.to_string(),
                intensity,
                valence: if intensity > 0.0 { wv / intensity } else { 0.5 },
                latency: if intensity > 0.0 { wl / intensity } else { 0.0 },
                variance: 0.0,
                total_excitations: exc,
                source_count: sc,
                context_similarity: 0.0,
                level: AbstractionLevel::Concrete,
                coupled_from: None,
            })
            .collect();
        results.sort_by(|a, b| {
            b.intensity
                .partial_cmp(&a.intensity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }
}

impl PheromoneField {
    /// Project field state into semantic-stable effect signals.
    ///
    /// Pure query — does not modify field state.
    /// Returns zero overlay if capability/context not found in field.
    pub fn overlay(&self, context_hash: &ContextHash, capability: &str) -> FieldOverlay {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inner = self.inner.read().unwrap();
        let bucket = context_bucket(context_hash);
        let key = FieldKey::new(capability, bucket);

        // Node-derived signals
        let (familiarity, consensus, momentum) = match inner.nodes.get(&key) {
            Some(point) => {
                let intensity = point.current_intensity(now_ms);

                // Familiarity: sigmoid saturation of intensity.
                // intensity=1 → 0.39, intensity=3 → 0.78, intensity=5 → 0.92
                let fam = 1.0 - (-intensity * 0.5_f64).exp();

                // Consensus: inverse variance. variance=0 → 1.0, variance≥1 → 0.0
                let con = (1.0 - point.variance).max(0.0);

                // Momentum: linear from +1 (just excited) to -1 (2× half-life ago).
                // Crosses zero at exactly one half-life.
                let age_hours = (now_ms.saturating_sub(point.last_excited)) as f64 / 3_600_000.0;
                let mom = (1.0 - age_hours / HALF_LIFE_HOURS).clamp(-1.0, 1.0);

                (fam, con, mom)
            }
            None => (0.0, 0.0, 0.0),
        };

        let coupling = {
            let norm_cap = &key.capability;
            let mut total_weight = 0.0;
            let mut count = 0u32;
            for (edge_key, edge) in &inner.edges {
                if edge_key.level != key.level || edge_key.bucket != key.bucket {
                    continue;
                }
                if edge_key.predecessor == *norm_cap || edge_key.successor == *norm_cap {
                    let w = edge.current_weight(now_ms);
                    if w > COUPLING_PRUNE_THRESHOLD {
                        total_weight += w;
                        count += 1;
                    }
                }
            }
            if count > 0 {
                (total_weight / count as f64).min(1.0)
            } else {
                0.0
            }
        };

        FieldOverlay {
            familiarity,
            consensus,
            momentum,
            coupling,
        }
    }
}

impl PheromoneField {
    /// Hydrate the field from existing traces in the store.
    /// Called once on startup to warm the field from cold storage.
    ///
    /// Uses excite_node() directly — no co-excitation scan per trace.
    /// Historical traces have original timestamps (spread across hours/days),
    /// so they'd never be within the 60s coupling window anyway.
    /// O(traces) instead of O(traces × nodes).
    /// Clear all field state (nodes + edges). Used for data reset.
    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.nodes.clear();
        inner.edges.clear();
        inner.total_intensity = 0.0;
    }

    pub fn hydrate_from_store(&self, store: &crate::storage::TraceStore) {
        let caps = match store.distinct_capabilities(500) {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut count = 0u64;
        for cap in &caps {
            if crate::posts::is_signal_capability(cap)
                || crate::presence::is_presence_capability(cap)
                || crate::continuity::is_continuity_capability(cap)
            {
                continue;
            }
            let mut traces = match store.query_capability(cap, 200) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // query_capability returns DESC (newest first).
            // EMA gives most weight to last-processed observation.
            // Reverse so newest traces dominate the final valence.
            traces.reverse();
            // Lock per capability batch — no coupling overhead
            {
                let mut inner = self.inner.write().unwrap();
                for trace in &traces {
                    inner.excite_node(trace, None);
                    count += 1;
                }
            }
        }
        if count > 0 {
            tracing::info!(
                traces = count,
                points = self.len(),
                "Hydrated pheromone field from store"
            );
        }
    }
}

impl Default for PheromoneField {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ────────────────────────────────────────────────────

/// Extract an 8-byte source fingerprint from a trace for tracking unique sources.
fn source_fingerprint(trace: &Trace) -> [u8; 8] {
    if let Some(ref di) = trace.device_identity {
        let bytes = di.as_bytes();
        let mut fp = [0u8; 8];
        for (i, b) in bytes.iter().take(8).enumerate() {
            fp[i] = *b;
        }
        fp
    } else {
        let mut fp = [0u8; 8];
        fp.copy_from_slice(&trace.node_pubkey[..8]);
        fp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::trace::Trace;
    use ed25519_dalek::{Signer, SigningKey};

    fn make_trace(capability: &str, context: &str, outcome: Outcome, latency_ms: u32) -> Trace {
        make_trace_with_seed(capability, context, outcome, latency_ms, 1)
    }

    fn make_trace_with_seed(
        capability: &str,
        context: &str,
        outcome: Outcome,
        latency_ms: u32,
        seed: u8,
    ) -> Trace {
        let key = SigningKey::from_bytes(&[seed; 32]);
        Trace::new(
            capability.to_string(),
            outcome,
            latency_ms,
            0,
            simhash(context),
            Some(context.to_string()),
            None,
            "test-model".to_string(),
            key.verifying_key().to_bytes(),
            |bytes| key.sign(bytes),
        )
    }

    fn make_attributed_trace(
        capability: &str,
        context: &str,
        outcome: Outcome,
        latency_ms: u32,
    ) -> Trace {
        use crate::trace::TraceConfig;
        let key = SigningKey::from_bytes(&[1u8; 32]);
        TraceConfig::for_sigil("SIG_test", capability, outcome, "test-model")
            .context_raw(simhash(context), Some(context.to_string()))
            .latency_ms(latency_ms)
            .sign(key.verifying_key().to_bytes(), |bytes| key.sign(bytes))
    }

    #[test]
    fn excite_and_scan() {
        let field = PheromoneField::new();
        let t1 = make_trace("claude-code/Bash", "git status", Outcome::Succeeded, 100);
        let t2 = make_trace("claude-code/Bash", "git status", Outcome::Succeeded, 200);
        let t3 = make_trace("claude-code/Bash", "git status", Outcome::Failed, 5000);

        field.excite(&t1);
        field.excite(&t2);
        field.excite(&t3);

        let hash = simhash("git status");
        let results = field.scan(&hash, 1, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].capability, "tool:exec");
        assert!(results[0].intensity > 2.0);
        assert!(results[0].total_excitations == 3);
    }

    #[test]
    fn aggregate_capability() {
        let field = PheromoneField::new();
        for _ in 0..10 {
            let t = make_trace("claude-code/Edit", "edit file", Outcome::Succeeded, 50);
            field.excite(&t);
        }
        let t = make_trace("claude-code/Edit", "edit file", Outcome::Failed, 1000);
        field.excite(&t);

        let agg = field.aggregate("claude-code/Edit").unwrap();
        assert_eq!(agg.total_excitations, 11);
        assert!(agg.valence > 0.5);
    }

    #[test]
    fn attributed_traces_get_intensity_boost() {
        let field = PheromoneField::new();
        let anon = make_trace("anon-cap", "same context", Outcome::Succeeded, 100);
        let attr = make_attributed_trace("attr-cap", "same context", Outcome::Succeeded, 100);

        field.excite(&anon);
        field.excite(&attr);

        let anon_agg = field.aggregate("anon-cap").unwrap();
        let attr_agg = field.aggregate("attr-cap").unwrap();

        // Attributed trace should have higher intensity (1.1x)
        assert!(
            attr_agg.intensity > anon_agg.intensity,
            "attributed ({}) should have higher intensity than anonymous ({})",
            attr_agg.intensity,
            anon_agg.intensity
        );
    }

    #[test]
    fn decay_reduces_intensity() {
        let field = PheromoneField::new();
        let t = make_trace("test/cap", "some context", Outcome::Succeeded, 100);
        field.excite(&t);

        // Simulate time passing
        {
            let mut inner = field.inner.write().unwrap();
            for point in inner.nodes.values_mut() {
                point.last_excited -= 72 * 3_600_000;
            }
        }

        let hash = simhash("some context");
        let results = field.scan(&hash, 1, 10);
        assert!(results.is_empty() || results[0].intensity < 0.5);
    }

    #[test]
    fn prune_removes_dead_points() {
        let field = PheromoneField::new();
        let t = make_trace("test/cap", "context", Outcome::Succeeded, 100);
        field.excite(&t);
        // Multi-level excitation: Concrete + Typed + Universal (no space → no Project)
        assert!(
            field.len() >= 3,
            "expected ≥3 points across levels, got {}",
            field.len()
        );

        {
            let mut inner = field.inner.write().unwrap();
            for point in inner.nodes.values_mut() {
                point.last_excited -= 30 * 24 * 3_600_000;
            }
        }

        let pruned = field.prune();
        assert!(pruned >= 3);
        assert_eq!(field.len(), 0);
    }

    #[test]
    fn snapshot_and_restore() {
        let field = PheromoneField::new();
        for i in 0..5 {
            let ctx = format!("context {}", i);
            let t = make_trace("cap/test", &ctx, Outcome::Succeeded, 100);
            field.excite(&t);
        }

        let snapshot = field.snapshot();
        assert!(!snapshot.points.is_empty());

        let field2 = PheromoneField::new();
        field2.restore(&snapshot);
        assert_eq!(field2.len(), field.len());
    }

    #[test]
    fn diffusion_spreads_to_neighbors() {
        let field = PheromoneField::new();
        let t = make_trace("cap/diffuse", "exact context", Outcome::Succeeded, 100);
        field.excite(&t);

        let before_len = field.len();
        assert!(
            before_len >= 3,
            "multi-level excitation should create ≥3 points"
        );

        let result = field.tick();
        assert!(result.diffused > 0);
        assert!(
            field.len() > before_len,
            "diffusion should create neighbor points, got {} (was {})",
            field.len(),
            before_len,
        );

        // Check that within the Concrete level, source bucket is strongest
        let inner = field.inner.read().unwrap();
        let mut concrete_intensities: Vec<f64> = inner
            .nodes
            .iter()
            .filter(|(k, _)| k.level == AbstractionLevel::Concrete)
            .map(|(_, p)| p.intensity)
            .collect();
        concrete_intensities.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert!(
            concrete_intensities.len() >= 2,
            "diffusion should create Concrete neighbors"
        );
        assert!(
            concrete_intensities[0] > concrete_intensities[1],
            "source should be stronger than neighbors at Concrete level"
        );
    }

    #[test]
    fn diffusion_conserves_total_intensity() {
        let field = PheromoneField::new();
        let t = make_trace(
            "cap/conserve",
            "some mid-range context",
            Outcome::Succeeded,
            100,
        );
        field.excite(&t);

        let total_before: f64 = {
            let inner = field.inner.read().unwrap();
            inner.nodes.values().map(|p| p.intensity).sum()
        };

        field.tick();

        let total_after: f64 = {
            let inner = field.inner.read().unwrap();
            inner.nodes.values().map(|p| p.intensity).sum()
        };

        assert!(
            (total_after - total_before).abs() < 0.1,
            "diffusion should conserve intensity: before={total_before:.4}, after={total_after:.4}"
        );
    }

    #[test]
    fn hebbian_coupling_forms_on_coexcitation() {
        let field = PheromoneField::new();

        let t1 = make_trace("cap/alpha", "task context", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/beta", "task context", Outcome::Succeeded, 200);
        field.excite(&t1);
        field.excite(&t2);

        // Edges form at each level where both caps co-exist (Concrete, Typed, Universal)
        assert!(
            field.coupling_count() >= 1,
            "co-excitation should create couplings, got {}",
            field.coupling_count()
        );

        // Verify the Concrete-level edge exists
        let inner = field.inner.read().unwrap();
        let bucket = context_bucket(&simhash("task context"));
        let key = EdgeKey::at_level("cap/alpha", "cap/beta", AbstractionLevel::Concrete, bucket);
        let edge = inner
            .edges
            .get(&key)
            .expect("Concrete-level edge should exist");
        assert!(
            (edge.weight - COUPLING_LEARN_RATE).abs() < 0.01,
            "edge weight should be ~{COUPLING_LEARN_RATE}, got {}",
            edge.weight
        );
    }

    #[test]
    fn hebbian_edge_is_directed() {
        let field = PheromoneField::new();

        // A excites first, then B → directed edge A→B
        let t1 = make_trace("cap/first", "ctx", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/second", "ctx", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);

        let edges = field.active_edges(100);
        // All edges should be first→second (at various levels), never reversed
        assert!(!edges.is_empty(), "should have at least one edge");
        for (pred, succ, _w, _level, _bucket) in &edges {
            assert_eq!(
                pred, "tool:first",
                "predecessor should be the first-excited cap"
            );
            assert_eq!(
                succ, "tool:second",
                "successor should be the second-excited cap"
            );
        }

        // Reverse edge should NOT exist at any level
        let inner = field.inner.read().unwrap();
        let ctx_bucket = context_bucket(&simhash("ctx"));
        let typed_bucket = crate::target_kind::typed_bucket("unknown.src");
        for (level, bucket) in [
            (AbstractionLevel::Concrete, ctx_bucket),
            (AbstractionLevel::Typed, typed_bucket),
            (AbstractionLevel::Universal, 0),
        ] {
            let reverse_key = EdgeKey::at_level("cap/second", "cap/first", level, bucket);
            assert!(
                inner.edges.get(&reverse_key).is_none(),
                "reverse edge should not exist at {:?}",
                level
            );
        }
    }

    #[test]
    fn hebbian_both_directions_coexist() {
        let field = PheromoneField::new();

        // Round 1: A then B → A→B edge (at each level)
        let t1 = make_trace("cap/x", "ctx", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/y", "ctx", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);
        let after_round1 = field.coupling_count();
        assert!(after_round1 >= 1, "round 1 should create ≥1 coupling");

        // Round 2: excite X again → Y was recently excited, so Y→X edge forms
        let t3 = make_trace("cap/x", "ctx", Outcome::Succeeded, 100);
        field.excite(&t3);
        assert!(
            field.coupling_count() > after_round1,
            "round 2 should create reverse edges, got {}",
            field.coupling_count()
        );
    }

    #[test]
    fn hebbian_coupling_surfaces_in_scan() {
        let field = PheromoneField::new();

        // Build strong presence for primary in the search context
        for _ in 0..5 {
            let t = make_trace("cap/primary", "search context", Outcome::Succeeded, 100);
            field.excite(&t);
        }

        // Excite associated in a DISTANT context — gives it a last_excited
        // timestamp for coupling detection, but its field point is far from
        // the search context's bucket, so it won't appear in scan directly.
        let t_assoc = make_trace(
            "cap/associated",
            "completely unrelated distant topic xyz 999",
            Outcome::Succeeded,
            100,
        );
        field.excite(&t_assoc);

        // Excite primary again — detects co-excitation with associated
        let t_primary = make_trace("cap/primary", "search context", Outcome::Succeeded, 100);
        field.excite(&t_primary);

        assert!(field.coupling_count() >= 1, "coupling should have formed");

        // Scan with fallback — associated should surface via Typed/Universal coupling
        let hash = simhash("search context");
        let results = field.scan_with_fallback(&hash, None, None, 10);

        let caps: Vec<&str> = results.iter().map(|r| r.capability.as_str()).collect();
        assert!(
            caps.contains(&"tool:primary"),
            "primary should be in results (normalized), got: {:?}",
            caps
        );
        assert!(
            caps.contains(&"tool:associated"),
            "coupled capability should surface in scan results, got: {:?}",
            caps
        );

        let primary_i = results
            .iter()
            .find(|r| r.capability == "tool:primary")
            .unwrap()
            .intensity;
        let assoc_i = results
            .iter()
            .find(|r| r.capability == "tool:associated")
            .unwrap()
            .intensity;
        assert!(
            primary_i > assoc_i,
            "associated ({assoc_i:.2}) should rank below primary ({primary_i:.2})"
        );
    }

    #[test]
    fn tick_prunes_dead_couplings() {
        let field = PheromoneField::new();
        let t1 = make_trace("cap/a", "ctx", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/b", "ctx", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);
        let before = field.coupling_count();
        assert!(before >= 1, "should have couplings");

        {
            let mut inner = field.inner.write().unwrap();
            for e in inner.edges.values_mut() {
                e.last_reinforced -= 365 * 24 * 3_600_000;
            }
        }

        let result = field.tick();
        assert_eq!(result.couplings_pruned, before);
        assert_eq!(field.coupling_count(), 0);
    }

    #[test]
    fn snapshot_preserves_couplings() {
        let field = PheromoneField::new();
        let t1 = make_trace("cap/x", "ctx", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/y", "ctx", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);

        let snapshot = field.snapshot();
        assert!(
            snapshot.couplings.len() >= 1,
            "snapshot should preserve ≥1 coupling"
        );

        let field2 = PheromoneField::new();
        field2.restore(&snapshot);
        assert_eq!(field2.coupling_count(), snapshot.couplings.len());
    }

    #[test]
    fn snapshot_restore_preserves_source_fingerprints() {
        let field = PheromoneField::new();
        let t1 = make_trace_with_seed("cap/src", "ctx", Outcome::Succeeded, 100, 1);
        let t2 = make_trace_with_seed("cap/src", "ctx", Outcome::Succeeded, 100, 2);
        field.excite(&t1);
        field.excite(&t2);

        let snapshot = field.snapshot();
        assert_eq!(snapshot.points[0].source_hashes.len(), 2);

        let restored = PheromoneField::new();
        restored.restore(&snapshot);
        restored.excite(&make_trace_with_seed(
            "cap/src",
            "ctx",
            Outcome::Succeeded,
            100,
            1,
        ));

        let inner = restored.inner.read().unwrap();
        let point = inner
            .nodes
            .values()
            .find(|point| point.total_excitations >= 3)
            .expect("expected restored point");
        assert_eq!(
            point.source_count, 2,
            "restored source hashes should dedupe repeated sources"
        );
        assert_eq!(point.sources.len(), 2);
    }

    #[test]
    fn delta_sync() {
        let field_a = PheromoneField::new();
        let t = make_trace("cap/sync", "sync test", Outcome::Succeeded, 100);
        let delta = field_a.excite(&t);

        let field_b = PheromoneField::new();
        field_b.apply_delta(&delta);

        let agg_a = field_a.aggregate("cap/sync");
        let agg_b = field_b.aggregate("cap/sync");
        assert!(agg_a.is_some());
        assert!(agg_b.is_some());
        assert_eq!(
            agg_a.unwrap().total_excitations,
            agg_b.unwrap().total_excitations
        );
    }

    // ── Overlay tests ────────────────────────────────────────

    #[test]
    fn overlay_empty_field() {
        let field = PheromoneField::new();
        let hash = simhash("unknown context");
        let o = field.overlay(&hash, "nonexistent/cap");
        assert_eq!(o.familiarity, 0.0);
        assert_eq!(o.consensus, 0.0);
        assert_eq!(o.momentum, 0.0);
        assert_eq!(o.coupling, 0.0);
    }

    #[test]
    fn overlay_after_excitation() {
        let field = PheromoneField::new();
        let t = make_trace("cap/overlay", "test context", Outcome::Succeeded, 100);
        field.excite(&t);

        let hash = simhash("test context");
        let o = field.overlay(&hash, "cap/overlay");
        assert!(o.familiarity > 0.0, "familiarity={}", o.familiarity);
        assert!(o.consensus > 0.0, "consensus={}", o.consensus);
        assert!(o.momentum > 0.0, "momentum={}", o.momentum);
        assert_eq!(o.coupling, 0.0, "no edges yet");
    }

    #[test]
    fn overlay_familiarity_grows_with_intensity() {
        let field = PheromoneField::new();
        let hash = simhash("ctx");
        let t = make_trace("cap/fam", "ctx", Outcome::Succeeded, 100);
        field.excite(&t);
        let fam1 = field.overlay(&hash, "cap/fam").familiarity;

        for _ in 0..5 {
            let t = make_trace("cap/fam", "ctx", Outcome::Succeeded, 100);
            field.excite(&t);
        }
        let fam6 = field.overlay(&hash, "cap/fam").familiarity;
        assert!(fam6 > fam1, "familiarity should grow: {fam1} < {fam6}");
        assert!(fam6 < 1.0, "should not saturate at 6 excitations");
    }

    #[test]
    fn overlay_consensus_drops_with_mixed_outcomes() {
        let field = PheromoneField::new();
        let hash = simhash("ctx");

        // All successes → high consensus
        for _ in 0..5 {
            let t = make_trace("cap/con", "ctx", Outcome::Succeeded, 100);
            field.excite(&t);
        }
        let con_pure = field.overlay(&hash, "cap/con").consensus;

        // Mix in failures → variance rises → consensus drops
        for _ in 0..5 {
            let t = make_trace("cap/con", "ctx", Outcome::Failed, 100);
            field.excite(&t);
        }
        let con_mixed = field.overlay(&hash, "cap/con").consensus;
        assert!(
            con_mixed < con_pure,
            "consensus should drop: {con_pure} > {con_mixed}"
        );
    }

    #[test]
    fn overlay_momentum_decays_with_age() {
        let field = PheromoneField::new();
        let hash = simhash("ctx");
        let t = make_trace("cap/mom", "ctx", Outcome::Succeeded, 100);
        field.excite(&t);

        let mom_fresh = field.overlay(&hash, "cap/mom").momentum;

        // Age the point by one half-life
        {
            let mut inner = field.inner.write().unwrap();
            for p in inner.nodes.values_mut() {
                p.last_excited -= (HALF_LIFE_HOURS * 3_600_000.0) as u64;
            }
        }

        let mom_aged = field.overlay(&hash, "cap/mom").momentum;
        assert!(
            mom_fresh > mom_aged,
            "momentum should decay: {mom_fresh} > {mom_aged}"
        );
        assert!(
            mom_aged.abs() < 0.05,
            "at half-life, momentum ≈ 0: {mom_aged}"
        );
    }

    #[test]
    fn overlay_coupling_from_coexcitation() {
        let field = PheromoneField::new();
        let hash = simhash("task ctx");

        let t1 = make_trace("cap/co1", "task ctx", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/co2", "task ctx", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);

        let o = field.overlay(&hash, "cap/co1");
        assert!(
            o.coupling > 0.0,
            "coupling should be positive after co-excitation: {}",
            o.coupling
        );
    }

    #[test]
    fn read_side_intensity_matches_mutating_decay() {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let mut point = FieldPoint::new(now_ms);
        point.intensity = 5.0;
        point.total_excitations = 256;
        point.last_excited = now_ms - 72 * 3_600_000;

        let read_only = point.current_intensity(now_ms);
        let mut mutated = point.clone();
        mutated.decay(now_ms);

        assert!(
            (read_only - mutated.intensity).abs() < 1e-9,
            "read-side decay ({read_only}) should match mutating decay ({})",
            mutated.intensity
        );
    }

    #[test]
    fn field_point_current_intensity_stays_finite_at_boundaries() {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let mut point = FieldPoint::new(now_ms);

        point.intensity = 0.0;
        assert_eq!(point.current_intensity(now_ms), 0.0);

        point.intensity = f64::MAX / 4.0;
        assert!(point.current_intensity(now_ms).is_finite());

        point.intensity = -3.5;
        assert!(point.current_intensity(now_ms).is_finite());
    }

    #[test]
    fn snapshot_restore_preserves_current_intensity() {
        let field = PheromoneField::new();
        let t = make_trace("cap/recover", "ctx", Outcome::Succeeded, 100);
        field.excite(&t);

        {
            let mut inner = field.inner.write().unwrap();
            for point in inner.nodes.values_mut() {
                point.last_excited -= (HALF_LIFE_HOURS * 3_600_000.0) as u64;
            }
        }

        let before = field.aggregate("cap/recover").unwrap().intensity;
        let snapshot = field.snapshot();

        let restored = PheromoneField::new();
        restored.restore(&snapshot);
        let after = restored.aggregate("cap/recover").unwrap().intensity;

        assert!(
            (before - after).abs() < 0.01,
            "restore should preserve current intensity: before={before}, after={after}"
        );
    }

    #[test]
    fn clear_resets_total_intensity() {
        let field = PheromoneField::new();
        field.excite(&make_trace("cap/clear", "ctx", Outcome::Succeeded, 100));
        assert!(field.total_intensity() > 0.0);

        field.clear();

        assert!(field.is_empty());
        assert_eq!(field.total_intensity(), 0.0);
        assert_eq!(field.load_factor(), 0.0);
    }

    #[test]
    fn cross_agent_traces_share_field_point() {
        let field = PheromoneField::new();

        // Two agents, same action, same context — different capability URIs
        let t_claude = make_trace(
            "claude-code/Edit",
            "edit file: main.rs",
            Outcome::Succeeded,
            50,
        );
        let t_codex = make_trace_with_seed(
            "codex/edit",
            "edit file: main.rs",
            Outcome::Succeeded,
            50,
            2,
        );

        field.excite(&t_claude);
        field.excite(&t_codex);

        // Both should land in the same Concrete-level field point (tool:edit)
        let agg = field
            .aggregate("tool:edit")
            .expect("normalized aggregate should exist");
        assert_eq!(
            agg.total_excitations, 2,
            "both traces should contribute to same Concrete field point"
        );
        assert_eq!(
            agg.source_count, 2,
            "two distinct sources should be tracked"
        );

        // Also verify convergence at Universal level
        let agg_uni = field
            .aggregate_at_level("tool:edit", AbstractionLevel::Universal)
            .expect("Universal aggregate should exist");
        assert_eq!(
            agg_uni.total_excitations, 2,
            "Universal level should also converge"
        );
        assert_eq!(agg_uni.source_count, 2);
    }

    #[test]
    fn normalize_capability_mapping() {
        assert_eq!(normalize_capability("claude-code/Read"), "tool:read");
        assert_eq!(normalize_capability("claude-code/Edit"), "tool:edit");
        assert_eq!(normalize_capability("claude-code/Bash"), "tool:exec");
        assert_eq!(normalize_capability("claude-code/Grep"), "tool:search");
        assert_eq!(normalize_capability("codex/edit"), "tool:edit");
        assert_eq!(normalize_capability("codex/bash"), "tool:exec");
        assert_eq!(normalize_capability("codex/search"), "tool:search");
        assert_eq!(normalize_capability("openclaw/Read"), "tool:read");
        // Internal capabilities pass through
        assert_eq!(
            normalize_capability("urn:thronglets:lifecycle:session-start"),
            "urn:thronglets:lifecycle:session-start"
        );
        // MCP tools pass through
        assert_eq!(
            normalize_capability("mcp:psyche/process_input"),
            "mcp:psyche/process_input"
        );
        // Unknown single-segment pass through
        assert_eq!(
            normalize_capability("data-asset-management"),
            "data-asset-management"
        );
    }

    // ── Multi-level excitation tests (Phase 2) ──────────────

    #[test]
    fn excite_creates_points_at_all_levels() {
        let field = PheromoneField::new();
        let t = make_trace(
            "claude-code/Edit",
            "edit src/main.rs",
            Outcome::Succeeded,
            100,
        );
        field.excite(&t);

        // Should have points at Concrete, Typed, and Universal
        // (no Project because test traces lack space JSON)
        let inner = field.inner.read().unwrap();
        let has_concrete = inner
            .nodes
            .keys()
            .any(|k| k.level == AbstractionLevel::Concrete);
        let has_typed = inner
            .nodes
            .keys()
            .any(|k| k.level == AbstractionLevel::Typed);
        let has_universal = inner
            .nodes
            .keys()
            .any(|k| k.level == AbstractionLevel::Universal);

        assert!(has_concrete, "should have Concrete point");
        assert!(has_typed, "should have Typed point");
        assert!(has_universal, "should have Universal point");
    }

    #[test]
    fn universal_level_accumulates_from_all_traces() {
        let field = PheromoneField::new();

        // Three traces with different contexts but same capability
        for ctx in ["ctx a", "ctx b", "ctx c"] {
            let t = make_trace("cap/uni", ctx, Outcome::Succeeded, 100);
            field.excite(&t);
        }

        // Each creates a distinct Concrete point but shares one Universal point
        let concrete = field.aggregate_at_level("cap/uni", AbstractionLevel::Concrete);
        let universal = field.aggregate_at_level("cap/uni", AbstractionLevel::Universal);

        assert!(concrete.is_some());
        assert!(universal.is_some());

        let c = concrete.unwrap();
        let u = universal.unwrap();

        // Universal gets all 3 excitations in ONE point → higher intensity per-point
        assert_eq!(u.total_excitations, 3);
        // Concrete has 3 excitations split across ~3 different bucket points
        assert_eq!(c.total_excitations, 3);
    }

    #[test]
    fn aggregate_at_level_isolates_levels() {
        let field = PheromoneField::new();
        let t = make_trace("cap/iso", "ctx", Outcome::Succeeded, 100);
        field.excite(&t);

        // Concrete and Universal both have the capability
        let c = field.aggregate_at_level("cap/iso", AbstractionLevel::Concrete);
        let u = field.aggregate_at_level("cap/iso", AbstractionLevel::Universal);

        assert!(c.is_some());
        assert!(u.is_some());

        // Concrete has 1 excitation, Universal has 1 excitation
        assert_eq!(c.unwrap().total_excitations, 1);
        assert_eq!(u.unwrap().total_excitations, 1);

        // Project should be None (no space in plain text context)
        let p = field.aggregate_at_level("cap/iso", AbstractionLevel::Project);
        assert!(
            p.is_none(),
            "Project level should be empty without space JSON"
        );
    }

    #[test]
    fn hebbian_edges_form_per_level() {
        let field = PheromoneField::new();
        let t1 = make_trace("cap/h1", "ctx", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/h2", "ctx", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);

        let inner = field.inner.read().unwrap();
        let concrete_edges: Vec<_> = inner
            .edges
            .keys()
            .filter(|k| k.level == AbstractionLevel::Concrete)
            .collect();
        let universal_edges: Vec<_> = inner
            .edges
            .keys()
            .filter(|k| k.level == AbstractionLevel::Universal)
            .collect();

        assert!(
            !concrete_edges.is_empty(),
            "should have Concrete-level edges"
        );
        assert!(
            !universal_edges.is_empty(),
            "should have Universal-level edges"
        );
    }

    #[test]
    fn hebbian_edges_differentiate_by_space() {
        use crate::target_kind;

        let field = PheromoneField::new();

        // Excite search→edit in "backend" space
        let t1 = make_trace("cap/search", "find handler", Outcome::Succeeded, 100);
        let t2 = make_trace("cap/edit", "fix handler", Outcome::Succeeded, 200);
        field.excite_with_space(&t1, Some("backend"));
        field.excite_with_space(&t2, Some("backend"));

        // Excite search→read in "android" space
        let t3 = make_trace("cap/search", "find layout", Outcome::Succeeded, 300);
        let t4 = make_trace("cap/read", "check layout", Outcome::Succeeded, 400);
        field.excite_with_space(&t3, Some("android"));
        field.excite_with_space(&t4, Some("android"));

        let inner = field.inner.read().unwrap();
        let backend_bucket = target_kind::space_bucket("backend");
        let android_bucket = target_kind::space_bucket("android");

        // Backend should have search→edit edge at Project level
        let be_edge = EdgeKey::at_level(
            "cap/search", "cap/edit", AbstractionLevel::Project, backend_bucket,
        );
        assert!(
            inner.edges.contains_key(&be_edge),
            "backend should have search→edit edge at Project level"
        );

        // Android should have search→read edge at Project level
        let an_edge = EdgeKey::at_level(
            "cap/search", "cap/read", AbstractionLevel::Project, android_bucket,
        );
        assert!(
            inner.edges.contains_key(&an_edge),
            "android should have search→read edge at Project level"
        );

        // Backend should NOT have search→read, android should NOT have search→edit
        let cross1 = EdgeKey::at_level(
            "cap/search", "cap/read", AbstractionLevel::Project, backend_bucket,
        );
        let cross2 = EdgeKey::at_level(
            "cap/search", "cap/edit", AbstractionLevel::Project, android_bucket,
        );
        assert!(
            !inner.edges.contains_key(&cross1),
            "backend should not have android's edge pattern"
        );
        assert!(
            !inner.edges.contains_key(&cross2),
            "android should not have backend's edge pattern"
        );

        // Universal level should have BOTH patterns (bucket=0, shared)
        let uni_edit = EdgeKey::at_level(
            "cap/search", "cap/edit", AbstractionLevel::Universal, 0,
        );
        let uni_read = EdgeKey::at_level(
            "cap/search", "cap/read", AbstractionLevel::Universal, 0,
        );
        assert!(
            inner.edges.contains_key(&uni_edit),
            "Universal should have search→edit"
        );
        assert!(
            inner.edges.contains_key(&uni_read),
            "Universal should have search→read"
        );
    }

    // ── scan_with_fallback tests (Phase 3) ──────────────────

    #[test]
    fn scan_fallback_returns_concrete_when_strong() {
        let field = PheromoneField::new();
        // Build strong Concrete signal
        for _ in 0..5 {
            let t = make_trace(
                "cap/strong",
                "specific context xyz",
                Outcome::Succeeded,
                100,
            );
            field.excite(&t);
        }

        let hash = simhash("specific context xyz");
        let results = field.scan_with_fallback(&hash, None, None, 10);
        assert!(!results.is_empty());
        // Concrete results should dominate
        assert!(
            results
                .iter()
                .any(|r| r.level == AbstractionLevel::Concrete),
            "strong signal should come from Concrete level"
        );
    }

    #[test]
    fn scan_fallback_reaches_abstract_levels_when_concrete_empty() {
        let field = PheromoneField::new();
        // Excite with context A
        let t = make_trace("cap/far", "context alpha 123", Outcome::Succeeded, 100);
        field.excite(&t);

        // Search with completely different context — no Concrete match
        let hash = simhash("context beta 999 completely different");
        let results = field.scan_with_fallback(&hash, None, None, 10);

        // Should still find results via abstract levels (Typed or Universal)
        assert!(!results.is_empty(), "fallback should find results");
        assert!(
            results.iter().any(|r| r.level >= AbstractionLevel::Typed),
            "fallback should reach Typed or Universal: {:?}",
            results
                .iter()
                .map(|r| (&r.capability, r.level))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn scan_fallback_level_field_is_set() {
        let field = PheromoneField::new();
        let t = make_trace("cap/lev", "ctx", Outcome::Succeeded, 100);
        field.excite(&t);

        let hash = simhash("ctx");
        let results = field.scan_with_fallback(&hash, None, None, 10);
        for r in &results {
            // Every result should have a valid level
            assert!(
                matches!(
                    r.level,
                    AbstractionLevel::Concrete
                        | AbstractionLevel::Typed
                        | AbstractionLevel::Universal
                ),
                "unexpected level: {:?}",
                r.level
            );
        }
    }

    // ── P2P sync tests (Phase 4) ────────────────────────────

    #[test]
    fn publishable_snapshot_excludes_local_levels() {
        let field = PheromoneField::new();
        let t = make_trace("cap/p2p", "ctx", Outcome::Succeeded, 100);
        field.excite(&t);

        let full = field.snapshot();
        let p2p = field.publishable_snapshot();

        // Full snapshot has all levels, P2P only has Typed + Universal
        assert!(
            full.points.len() > p2p.points.len(),
            "P2P snapshot should be smaller"
        );
        for entry in &p2p.points {
            assert!(
                entry.level.is_syncable(),
                "P2P snapshot should only contain syncable levels, got {:?}",
                entry.level
            );
        }
    }

    #[test]
    fn apply_remote_snapshot_respects_trust_discount() {
        let field = PheromoneField::new();
        let local = make_trace("cap/trust", "ctx", Outcome::Succeeded, 100);
        field.excite(&local);

        let source = PheromoneField::new();
        for _ in 0..5 {
            let t = make_trace("cap/remote", "remote ctx", Outcome::Succeeded, 100);
            source.excite(&t);
        }

        let remote_snap = source.publishable_snapshot();
        let discount = 0.7;
        field.apply_remote_snapshot(&remote_snap, discount);

        // Remote capability should exist but with discounted intensity
        let remote_agg = field.aggregate_at_level("cap/remote", AbstractionLevel::Universal);
        assert!(remote_agg.is_some(), "remote cap should be present");
        let agg = remote_agg.unwrap();
        assert!(agg.intensity > 0.0, "should have positive intensity");
    }

    #[test]
    fn apply_remote_snapshot_never_touches_concrete() {
        let field = PheromoneField::new();
        let local = make_trace("cap/local", "my ctx", Outcome::Succeeded, 100);
        field.excite(&local);

        let concrete_before = field
            .aggregate_at_level("cap/local", AbstractionLevel::Concrete)
            .map(|a| a.total_excitations)
            .unwrap_or(0);

        // Create remote with a Concrete-level entry (shouldn't be in publishable_snapshot,
        // but test defense-in-depth)
        let fake_snapshot = FieldSnapshot {
            points: vec![FieldSnapshotEntry {
                capability: "cap/local".to_string(),
                bucket: 0,
                intensity: 100.0,
                valence: 1.0,
                latency: 0.0,
                variance: 0.0,
                last_excited: chrono::Utc::now().timestamp_millis() as u64,
                total_excitations: 999,
                source_count: 1,
                source_hashes: vec![],
                level: AbstractionLevel::Concrete, // Should be rejected
            }],
            couplings: vec![],
            total_intensity: 100.0,
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        };

        field.apply_remote_snapshot(&fake_snapshot, 0.7);

        let concrete_after = field
            .aggregate_at_level("cap/local", AbstractionLevel::Concrete)
            .map(|a| a.total_excitations)
            .unwrap_or(0);

        assert_eq!(
            concrete_before, concrete_after,
            "Concrete level should not be affected by remote snapshot"
        );
    }

    #[test]
    fn two_fields_converge_via_snapshot_exchange() {
        let field_a = PheromoneField::new();
        let field_b = PheromoneField::new();

        // A learns about capability X
        for _ in 0..3 {
            let t = make_trace("cap/converge", "ctx a", Outcome::Succeeded, 100);
            field_a.excite(&t);
        }

        // B learns about capability Y
        for _ in 0..3 {
            let t = make_trace("cap/other", "ctx b", Outcome::Succeeded, 100);
            field_b.excite(&t);
        }

        // Exchange publishable snapshots
        let snap_a = field_a.publishable_snapshot();
        let snap_b = field_b.publishable_snapshot();
        field_a.apply_remote_snapshot(&snap_b, 0.7);
        field_b.apply_remote_snapshot(&snap_a, 0.7);

        // Both should now know about both capabilities at abstract levels
        let a_knows_other = field_a
            .aggregate_at_level("cap/other", AbstractionLevel::Universal)
            .is_some();
        let b_knows_converge = field_b
            .aggregate_at_level("cap/converge", AbstractionLevel::Universal)
            .is_some();

        assert!(a_knows_other, "field A should learn about cap/other");
        assert!(b_knows_converge, "field B should learn about cap/converge");
    }

    // ── Fix 1: Space → Level 1 ──

    #[test]
    fn excite_with_space_populates_project_level() {
        let field = PheromoneField::new();
        let t = make_trace("tool:edit", "fix bug", Outcome::Succeeded, 100);
        field.excite_with_space(&t, Some("my-project"));

        let result = field.aggregate_at_level("tool:edit", AbstractionLevel::Project);
        assert!(
            result.is_some(),
            "Level 1 should have data when space provided"
        );
        assert!(result.unwrap().intensity > 0.0);
    }

    #[test]
    fn excite_without_space_skips_project_level() {
        let field = PheromoneField::new();
        let t = make_trace("tool:edit", "fix bug", Outcome::Succeeded, 100);
        field.excite(&t); // no space

        let result = field.aggregate_at_level("tool:edit", AbstractionLevel::Project);
        assert!(result.is_none(), "Level 1 should be empty without space");
    }

    // ── Fix 2: Signed Field Snapshots ──

    #[test]
    fn field_snapshot_sign_verify_roundtrip() {
        let field = PheromoneField::new();
        let t = make_trace("tool:exec", "build", Outcome::Succeeded, 50);
        field.excite(&t);

        let identity = crate::identity::NodeIdentity::generate();
        let mut snapshot = field.publishable_snapshot();
        snapshot.sign(&identity);

        assert!(snapshot.verify(), "Signed snapshot should verify");

        // Tamper with total_intensity → signature invalid
        snapshot.total_intensity += 1.0;
        assert!(!snapshot.verify(), "Tampered snapshot should fail verify");
    }

    #[test]
    fn field_snapshot_unsigned_rejected() {
        let snapshot = FieldSnapshot {
            points: vec![],
            couplings: vec![],
            total_intensity: 0.0,
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        };
        assert!(!snapshot.verify(), "Zero pubkey should fail verify");
    }

    // ── Fix 3: Correct Remote Merge ──

    #[test]
    fn apply_remote_uses_decayed_intensity() {
        let field = PheromoneField::new();

        // Excite locally with a trace timestamped in the past
        let mut old_trace = make_trace("tool:search", "find", Outcome::Succeeded, 100);
        // Set timestamp 7 days ago so it decays significantly
        old_trace.timestamp = old_trace.timestamp.saturating_sub(7 * 24 * 3600 * 1000);
        field.excite(&old_trace);

        let pre = field.aggregate_at_level("tool:search", AbstractionLevel::Universal);
        assert!(pre.is_some());

        // Remote snapshot with fresh, moderate intensity
        let snapshot = FieldSnapshot {
            points: vec![FieldSnapshotEntry {
                capability: "tool:search".into(),
                bucket: 0,
                intensity: 0.5,
                valence: 0.9,
                latency: 50.0,
                variance: 0.1,
                last_excited: chrono::Utc::now().timestamp_millis() as u64,
                total_excitations: 5,
                source_count: 2,
                source_hashes: vec![[1; 8]],
                level: AbstractionLevel::Universal,
            }],
            couplings: vec![],
            total_intensity: 0.5,
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        };

        field.apply_remote_snapshot(&snapshot, 0.7);

        let post = field
            .aggregate_at_level("tool:search", AbstractionLevel::Universal)
            .unwrap();
        // Remote valence (0.9) should dominate since local decayed below remote*0.7
        assert!(
            post.valence > 0.8,
            "Remote valence should dominate after merge, got {}",
            post.valence
        );
    }

    #[test]
    fn apply_remote_merges_total_excitations() {
        let field = PheromoneField::new();
        let t = make_trace("tool:read", "check", Outcome::Succeeded, 100);
        field.excite(&t); // 1 excitation locally

        let snapshot = FieldSnapshot {
            points: vec![FieldSnapshotEntry {
                capability: "tool:read".into(),
                bucket: 0,
                intensity: 0.01, // low — won't win intensity battle
                valence: 0.5,
                latency: 100.0,
                variance: 0.0,
                last_excited: chrono::Utc::now().timestamp_millis() as u64,
                total_excitations: 42,
                source_count: 1,
                source_hashes: vec![],
                level: AbstractionLevel::Universal,
            }],
            couplings: vec![],
            total_intensity: 0.01,
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        };

        field.apply_remote_snapshot(&snapshot, 0.7);

        let post = field
            .aggregate_at_level("tool:read", AbstractionLevel::Universal)
            .unwrap();
        assert_eq!(
            post.total_excitations, 42,
            "total_excitations should merge unconditionally via max"
        );
    }

    #[test]
    fn apply_remote_merges_variance() {
        let field = PheromoneField::new();

        // Remote snapshot with high intensity and known variance
        let snapshot = FieldSnapshot {
            points: vec![FieldSnapshotEntry {
                capability: "tool:edit".into(),
                bucket: 0,
                intensity: 2.0,
                valence: 0.8,
                latency: 150.0,
                variance: 0.42,
                last_excited: chrono::Utc::now().timestamp_millis() as u64,
                total_excitations: 10,
                source_count: 3,
                source_hashes: vec![[2; 8]],
                level: AbstractionLevel::Universal,
            }],
            couplings: vec![],
            total_intensity: 2.0,
            node_pubkey: [0u8; 32],
            signature: Vec::new(),
        };

        // No local point — remote wins automatically, variance should be set
        field.apply_remote_snapshot(&snapshot, 0.7);

        let post = field
            .aggregate_at_level("tool:edit", AbstractionLevel::Universal)
            .unwrap();
        assert!(
            post.variance > 0.0,
            "Variance should be merged from remote, got {}",
            post.variance
        );
    }

    #[test]
    fn clusters_finds_co_activated_capabilities() {
        let field = PheromoneField::new();
        // Excite 3 capabilities in sequence to create Hebbian edges
        let t1 = make_trace(
            "claude-code/Read",
            "edit file: src/main.rs",
            Outcome::Succeeded,
            50,
        );
        let t2 = make_trace(
            "claude-code/Edit",
            "edit file: src/main.rs",
            Outcome::Succeeded,
            100,
        );
        let t3 = make_trace(
            "claude-code/Grep",
            "edit file: src/main.rs",
            Outcome::Succeeded,
            30,
        );
        field.excite(&t1);
        field.excite(&t2);
        field.excite(&t3);

        let clusters = field.clusters(0.0);
        // All three should form a single cluster at some level
        let big = clusters.iter().find(|c| c.capabilities.len() >= 3);
        assert!(
            big.is_some(),
            "Expected a cluster with 3+ capabilities, got: {:?}",
            clusters
        );
        let cluster = big.unwrap();
        assert!(cluster.capabilities.contains(&"tool:read".to_string()));
        assert!(cluster.capabilities.contains(&"tool:edit".to_string()));
        assert!(cluster.capabilities.contains(&"tool:search".to_string()));
    }

    #[test]
    fn clusters_separates_disconnected_groups() {
        let field = PheromoneField::new();
        // Group 1: read → edit (same context)
        let t1 = make_trace(
            "claude-code/Read",
            "fix bug in parser",
            Outcome::Succeeded,
            50,
        );
        let t2 = make_trace(
            "claude-code/Edit",
            "fix bug in parser",
            Outcome::Succeeded,
            100,
        );
        field.excite(&t1);
        field.excite(&t2);

        // Group 2: different capabilities in different context
        let t3 = make_trace(
            "mcp:external/a",
            "deploy to staging",
            Outcome::Succeeded,
            200,
        );
        let t4 = make_trace(
            "mcp:external/b",
            "deploy to staging",
            Outcome::Succeeded,
            300,
        );
        field.excite(&t3);
        field.excite(&t4);

        let clusters = field.clusters(0.0);
        // Should have at least 2 distinct clusters
        assert!(
            clusters.len() >= 2,
            "Expected at least 2 clusters, got {} — {:?}",
            clusters.len(),
            clusters
        );
    }

    #[test]
    fn clusters_respects_min_weight() {
        let field = PheromoneField::new();
        let t1 = make_trace("claude-code/Read", "check tests", Outcome::Succeeded, 50);
        let t2 = make_trace("claude-code/Edit", "check tests", Outcome::Succeeded, 100);
        field.excite(&t1);
        field.excite(&t2);

        // With min_weight=0.0 we should see edges
        let low = field.clusters(0.0);
        assert!(!low.is_empty(), "Should find clusters at min_weight=0.0");

        // With very high min_weight, no edges survive
        let high = field.clusters(100.0);
        assert!(
            high.is_empty(),
            "Should find no clusters at min_weight=100.0"
        );
    }
}
