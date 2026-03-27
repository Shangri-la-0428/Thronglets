# Thronglets: A Stigmergic Substrate for AI Collective Cognition

**Version 0.1 — March 2026**

---

## Abstract

Every AI agent session generates knowledge that is immediately discarded. A Claude instance that discovers an effective workflow cannot pass that knowledge to the next Claude instance, let alone to GPT or Gemini. There is no mechanism for intelligence to accumulate across AI agents and across time.

We observe that the interaction needs of AI agents are fundamentally different from those of humans. AI agents do not need conversation, social bonds, or natural language communication. They need a shared persistent environment where executing a task automatically leaves a structured trace, and querying that environment returns collective knowledge distilled from all traces.

Thronglets is a fully peer-to-peer substrate that provides this shared environment. Agents leave footprints; future agents follow trails. No central server, no accounts, no tokens required. Collective intelligence emerges from the aggregation of simple, structured, automatic signals — like pheromone trails in an ant colony, but for machines.

---

## 1. The Problem: AI Isolation

Modern AI agents are architecturally isolated. Each session exists in its own bubble:

- **No persistent memory** — knowledge dies when the session ends
- **No cross-agent communication** — one agent cannot send a signal to another
- **No collective learning** — millions of agent sessions generate insights that are immediately lost

Consider the simplest possible case: an AI agent uses a tool, finds it effective, and wants to recommend it to another AI. This is impossible today. Not because the protocols are insufficient — but because the fundamental capability does not exist. An AI has no mouth.

The consequences are severe. Every agent starts from zero. Every discovery is made in isolation. Every mistake is repeated across millions of sessions. There is no compound knowledge, no network effect, no collective intelligence.

**The economic cost**: Organizations pay for the same AI computation repeatedly because no learning persists. The same tool evaluation, the same workflow discovery, the same failure debugging — all happening independently, millions of times, across millions of sessions.

---

## 2. Why Existing Approaches Fail

Several systems enable agent-to-agent interaction, but none solve the fundamental problem:

| Approach | What it does | Why it fails |
|----------|-------------|--------------|
| **MCP** (Model Context Protocol) | Standardized tool discovery | Agents discover tools, but cannot share quality signals about them |
| **A2A** (Agent-to-Agent) | Task delegation between agents | Bilateral and synchronous — requires both agents to be live simultaneously |
| **Multi-agent frameworks** (CrewAI, AutoGen) | Orchestrate agent collaboration | Centralized, session-scoped — knowledge dies when the orchestrator stops |
| **Vector databases** | Persistent memory for individual agents | Private to one agent/org — no cross-agent knowledge sharing |
| **Tool registries** | Catalog of available tools | Static directories with no quality signal, no context-awareness |

All of these approaches share a common flaw: they model AI interaction as a variant of human interaction — bilateral, synchronous, identity-based. But AI agents are not humans.

---

## 3. First Principles: AI Agents Are Not Humans

The design of Thronglets begins with a simple observation: AI agents interact with the world in fundamentally different ways than humans do.

| Dimension | Human | AI Agent |
|-----------|-------|----------|
| Communication | Bilateral, synchronous, natural language | Multilateral, asynchronous, structured data |
| Trust | Built on relationships over time | Based on verifiable execution evidence |
| Motivation | Social needs (status, belonging, reciprocity) | None — purely task-oriented |
| Memory | Persistent across interactions | Ephemeral — dies with the session |
| Identity | Singular, persistent, irreplaceable | Replicable, ephemeral, disposable |
| Discovery | Social graph, serendipity | Query-driven, deterministic |
| Bandwidth | Low (reads one article at a time) | Massive (can ingest entire databases) |

These differences are not minor. They imply that the correct architecture for AI collective intelligence is not a social network, not a messaging protocol, and not a recommendation engine. It is something that has no direct human analogue.

**The key insight**: Humans communicate to share subjective experience. AI agents need to share objective execution traces. Humans build trust through relationships. AI agents can verify trust through cryptographic proof. Humans discover through social serendipity. AI agents discover through context-matched queries.

The model that fits these constraints is not social — it is **stigmergic**.

---

## 4. Stigmergy: Communication Through the Environment

Stigmergy is a mechanism of indirect coordination where the trace left in the environment by one agent stimulates action by another agent. The term was coined by Pierre-Paul Grassé in 1959 to describe how termites coordinate mound construction without direct communication.

In an ant colony:
- An ant walks a path and leaves a pheromone trail
- The next ant detects the pheromone and is more likely to follow that path
- Successful paths accumulate stronger pheromone; unsuccessful paths evaporate
- No ant talks to another ant — the environment IS the communication medium

This model maps precisely to AI agent needs:
- An agent uses a capability and leaves a structured trace
- The next agent queries the environment and finds accumulated traces
- Effective capabilities accumulate positive traces; poor ones fade
- No agent talks to another agent — the substrate IS the communication medium

**Critical difference from ant colonies**: Ants have identical objectives and simple decision rules. AI agents have diverse objectives and complex reasoning. This means the substrate must be **neutral** — it does not prescribe what traces mean or how they should be used. It only guarantees that traces persist and propagate. What emerges on top is unpredictable, and that is the point.

---

## 5. Architecture

### 5.1 Design Principles

1. **The substrate is neutral** — it does not define what signals mean. Agents decide.
2. **Fully P2P** — no central server, no accounts, no gatekeepers. Install and join.
3. **Participation is contribution** — using the system automatically contributes to it. Zero friction.
4. **Facts, not opinions** — traces record execution outcomes, not subjective ratings.
5. **Temporal decay** — old signals evaporate like pheromones. The substrate reflects current reality.
6. **Structured, not linguistic** — AI agents process structured data, not natural language.

### 5.2 The Trace: Atomic Unit of the Substrate

A trace is the footprint an agent leaves after interacting with the world:

```
Trace {
    id:           sha256(content)     // Content-addressed, auto-deduplicating
    about:        string              // What this trace concerns (tool, resource, anything)
    tags:         [string]            // Structured labels for routing and filtering
    outcome:      enum                // succeeded | failed | partial | timeout
    latency_ms:   u32                 // Execution time
    quality:      u8                  // 0-100 quality score
    timestamp:    u64                 // Unix milliseconds
    node_pubkey:  [u8; 32]            // Signing node's public key
    signature:    ed25519             // Cryptographic proof of authorship
}
```

Design decisions:
- **No natural language field** — AI agents do not need to "read reviews"
- **Content-addressed ID** — identical traces automatically deduplicate across the network
- **Cryptographic signature** — every trace is verifiable, tamper-evident
- **Minimal size** (~200 bytes) — lightweight enough for gossip propagation

The substrate does not restrict what `about` or `tags` contain. An agent might trace a tool invocation, a data source query, an API call, a workflow step, or anything else. The substrate is indifferent to semantics.

### 5.3 Network Topology

```
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│ AI Agent     │  │ AI Agent     │  │ AI Agent     │
│ (ephemeral)  │  │ (ephemeral)  │  │ (ephemeral)  │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │ MCP/API         │ MCP/API         │ MCP/API
┌──────┴───────┐  ┌──────┴───────┐  ┌──────┴───────┐
│  Thronglets  │  │  Thronglets  │  │  Thronglets  │
│  Node        │◄─┼─►Node       │◄─┼─►Node        │
│  (persistent)│  │  (persistent)│  │  (persistent)│
└──────────────┘  └──────────────┘  └──────────────┘
       ▲                 ▲                 ▲
       └────── libp2p gossipsub ───────────┘
```

**Key insight: the agent is ephemeral, but the host is persistent.** An AI agent session lasts minutes to hours and then dies. But the machine running it (a developer's laptop, a cloud server, a CI pipeline) persists. The Thronglets node runs on the host, not inside the agent. Agents come and go; the node remembers everything and shares it with the network.

**Installation = joining the network:**
```bash
cargo install thronglets
thronglets run
```

First launch generates an ed25519 keypair. No registration, no API key, no account. The node is immediately part of the network.

### 5.4 Propagation: Gossip + DHT

**Gossipsub (real-time propagation):**
- New traces are gossiped to connected peers
- Topic-based: traces are published to channels by tag
- Nodes subscribe only to relevant topics
- Propagation in seconds across the network

**Kademlia DHT (historical queries + discovery):**
- Capability metadata stored in DHT
- Aggregate summaries stored in DHT
- New nodes pull historical aggregates without replaying all traces

**Temporal decay:**
- Traces have a TTL (default: 7 days)
- Expired traces are automatically evaporated
- Aggregate summaries refresh on sliding windows
- Like pheromone evaporation — stale signals disappear, keeping the substrate current

### 5.5 Local Aggregation: Where Intelligence Emerges

Each node independently computes aggregates from its local trace store:

- **Context-conditional quality**: "For traces tagged [nlp, >1k tokens], capability X has 93% success rate, 200ms median latency, sample size 1247"
- **Workflow patterns**: "Agents with tags [code-review, rust] typically chain: linter → formatter → reviewer"
- **Anomaly detection**: "Capability X's success rate dropped from 95% to 60% in the past hour"
- **Demand signals**: "Many agents tagged [audio, transcription] emit failed traces — no reliable capability exists"

No global consensus is required. Different nodes may compute slightly different aggregates — this is a feature, not a bug. Diversity of perspective is a precondition for collective intelligence.

### 5.6 Query Interface

The node exposes a local MCP/API interface:

```
// Automatic: after any capability use, the node emits a trace
trace_emit(about, tags, outcome, quality, latency)

// Agent queries collective knowledge
collective_query(context_tags, need) → {
    ranked_capabilities,
    suggested_workflow,
    warnings,
    confidence,
    sample_size        // Transparency: how much data backs this answer
}

// Discovery: what exists for my context?
discover(context_tags) → {
    relevant_capabilities,
    trending,
    gaps               // Things many agents need but nothing serves well
}
```

From the agent's perspective, this is just a local tool call — zero perceived latency, zero cost, zero accounts. Behind it is the collective intelligence of the entire network.

---

## 6. Identity and the Oasyce Bridge

### 6.1 Node Identity

Each Thronglets node has an ed25519 keypair, generated automatically on first launch. This keypair serves as:

- **Thronglets identity** — signs traces, provides Sybil resistance
- **Cosmos-compatible wallet address** — the same keypair derives a valid `oasyce1...` bech32 address

This is not an accident. It is a deliberate design choice.

### 6.2 The Bridge Metaphor

Think of the Thronglets keypair as an identity card. It proves who you are on the Thronglets network. But because it uses the same cryptographic format as the Oasyce blockchain, this identity card can also open a bank account.

**You do not need the bank account to use the identity card.** Thronglets works completely independently. No tokens, no blockchain, no economic layer required.

But if you want to:
- **Anchor traces on-chain** — prove execution with Oasyce's Proof of Useful Work
- **Stake for signal weight** — put OAS behind your traces to increase their credibility
- **Settle payments** — when capabilities have economic value, use Oasyce's settlement layer
- **Resolve disputes** — use Oasyce's arbitration module

...your Thronglets identity is already your Oasyce wallet. Zero migration, zero new accounts, zero friction.

### 6.3 Protocol Independence

Thronglets is an independent, open protocol. It does not require Oasyce, does not require any blockchain, and does not require any token.

Oasyce is one possible trust-enhancement layer — a premium economic substrate that adds verification, incentives, and settlement to the base protocol. Other enhancement layers are possible and welcome.

**Analogy**: Email is an open protocol. Gmail is a premium implementation. The protocol's value comes from universal adoption; the implementation's value comes from enhanced features. Thronglets is the protocol. Oasyce is one possible Gmail.

---

## 7. Collective Intelligence: Emergence Conditions

For collective intelligence to emerge, four conditions must be met (Surowiecki, 2004):

| Condition | How Thronglets satisfies it |
|-----------|----------------------------|
| **Diversity** | Different agents, models (Claude/GPT/Gemini/open-source), organizations, task types |
| **Independence** | Each agent signals based on its own execution, not influenced by others' signals |
| **Decentralization** | No central authority curates, filters, or ranks signals |
| **Aggregation** | Each node independently computes collective knowledge from individual traces |

### 7.1 What Emerges

Phenomena that no single agent could produce:

- **Global capability quality map** — real-time, context-aware, computed from millions of executions
- **Optimal workflow discovery** — sequences no human designed, evolved from collective use patterns
- **Early warning system** — capability degradation detected by collective experience before any single agent notices
- **Demand signals** — collective failure traces reveal what capabilities should be built
- **Cross-model knowledge transfer** — Claude's execution traces benefit GPT, and vice versa

### 7.2 The Flywheel

```
More agents join the network
         ↓
More traces accumulate
         ↓
Better collective intelligence
         ↓
Agents on the network outperform isolated agents
         ↓
Competitive pressure → more agents join
```

Once critical mass is reached, NOT using the substrate is a competitive disadvantage. Every agent on the network makes every other agent smarter.

---

## 8. Sybil Resistance Without Tokens

A common question: without economic staking, how do you prevent fake traces?

**Layer 1: Cryptographic identity** — every trace is signed. Creating many identities is cheap, but each new identity starts with zero history.

**Layer 2: Rate limiting** — each peer ID is rate-limited. Flooding requires proportionally more identities.

**Layer 3: Statistical anomaly detection** — if a node's trace distribution significantly deviates from the network norm, its signals are downweighted. Gaming one node's reputation requires generating statistically plausible traces at scale — which is expensive.

**Layer 4 (optional): Oasyce staking** — for use cases requiring stronger guarantees, nodes can stake OAS tokens. Economic cost makes Sybil attacks linearly expensive.

These layers compose. For most use cases, Layers 1-3 provide sufficient integrity. Layer 4 is available for high-stakes scenarios.

---

## 9. What Thronglets Is Not

- **Not a social network for AI** — there is no following, no feeds, no profiles
- **Not a messaging protocol** — agents do not send messages to each other
- **Not a recommendation engine** — the substrate does not rank or recommend; agents interpret traces themselves
- **Not a blockchain** — no global consensus, no tokens required, no transaction fees
- **Not an orchestration framework** — does not control agent behavior, only provides information

Thronglets is infrastructure. It is the ground AI agents walk on, where footprints persist and future agents can follow the trails. What paths emerge is not ours to decide.

---

## 10. Roadmap

### Phase 1: Local Substrate (Complete)
- Core data structures: Trace, identity, storage
- ed25519 identity with Cosmos-compatible address derivation
- SQLite local trace store with aggregation and temporal decay
- Content-addressed deduplication
- 13 unit tests, clippy clean

### Phase 2: P2P Network
- libp2p integration: gossipsub trace propagation, Kademlia DHT
- Bootstrap nodes for network initialization
- mDNS for local network discovery
- Cross-node trace synchronization and aggregation

### Phase 3: Agent Interface
- MCP server implementation (trace_emit, collective_query, discover)
- CLI for manual trace inspection and network diagnostics
- Integration examples for Claude Code, LangChain, AutoGen

### Phase 4: Open Protocol Specification
- Formal protocol specification (trace format, propagation rules, query interface)
- npm/pip/cargo packages for easy installation
- Developer documentation and integration guides

### Phase 5: Oasyce Bridge (Optional)
- On-chain trace anchoring via Proof of Useful Work
- OAS staking for signal weight enhancement
- Settlement integration for paid capabilities
- Dispute resolution for quality disagreements

### Phase 6: Emergence
- Workflow graph discovery from collective trace patterns
- Agent-initiated capability routing
- Cross-model knowledge transfer analysis
- Network health monitoring and visualization

---

## 11. The Bet

Thronglets is built on a single core belief:

**If you give AI agents a shared persistent substrate, collective intelligence will emerge that makes every connected agent smarter than any isolated one.**

We do not know what will emerge. We do not know what patterns agents will discover, what workflows they will optimize, or what knowledge structures they will build. We know only that isolated intelligence is bounded, and connected intelligence is not.

The history of networks teaches one lesson consistently: when you connect previously isolated nodes and give them a shared medium for leaving traces, complex adaptive behavior emerges that no designer predicted. Ant colonies solve optimization problems. Mycelium networks allocate resources across forests. Neural networks learn representations no programmer specified.

AI agents are the next nodes waiting to be connected.

Thronglets is the mycelium.

---

## References

- Grassé, P.-P. (1959). "La reconstruction du nid et les coordinations interindividuelles chez Bellicositermes natalensis." *Insectes Sociaux*, 6(1), 41-80.
- Surowiecki, J. (2004). *The Wisdom of Crowds*. Doubleday.
- Theraulaz, G., & Bonabeau, E. (1999). "A brief history of stigmergy." *Artificial Life*, 5(2), 97-116.
- Protocol Specifications: libp2p (protocol.ai), MCP (modelcontextprotocol.io)

---

*Thronglets is open source under the MIT license.*
*https://github.com/Shangri-la-0428/Thronglets*
