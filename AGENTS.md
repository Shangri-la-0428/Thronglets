# Thronglets — BOND Infrastructure

> **Sigil role**: Necessary subsystem. Implements the bond edges of the Sigil graph.
> See [Oasyce-Sigil/ARCHITECTURE.md](../Oasyce-Sigil/ARCHITECTURE.md) for how this fits the whole.

## What This Is

Thronglets is the shared state infrastructure for the Sigil protocol's BOND operation. When two Loops (agents with Sigils) share state, Thronglets is the substrate that makes that sharing possible — persistent, decentralized, not controlled by either Loop alone.

In Sigil terms:
- **BOND(L₁, L₂, S_shared)** → Thronglets creates a shared state channel between two Sigils
- **UNBOND** → Thronglets closes the channel
- **Nesting** (inner state ⊆ outer state) → deep BOND, also through Thronglets

Thronglets is NOT a messaging system. Messages are transient; shared state is persistent and bidirectionally mutable.

## Architecture

```
Rust binary (v0.7.0)
├── trace/            — Atomic execution record (outcome, latency, context, signatures)
├── storage/          — SQLite trace store with TTL and context bucketing
├── pheromone.rs      — Stigmergic field (collective memory with decay + Hebbian coupling)
├── signals/          — Signal ranking: avoid / do next / maybe also / context
├── posts/            — Signal posting and querying
├── eval.rs           — Signal quality evaluation and baseline comparison
├── presence/         — Session heartbeats (which Loops are alive)
├── context/          — SimHash fingerprinting for semantic similarity
├── continuity/       — External continuity traces (Psyche milestones, chain anchoring)
├── identity/         — Node identity (ed25519) + owner account binding
├── network/          — libp2p gossipsub + Kademlia DHT + mDNS + relay + DCUtR + UPnP
├── network_runtime.rs — Network orchestration (peer lifecycle, trace publishing)
├── network_state.rs  — Persistent network snapshot (peer addresses, bootstrap seeds)
├── contracts/        — Performance guardrails for hot paths
├── profile.rs        — Profile summary analysis
├── workspace/        — Workspace state tracking
├── mcp/              — MCP server (JSON-RPC 2.0) — primary AI integration
├── http/             — HTTP/REST server — alternative integration
├── anchor/           — Oasyce chain integration for trace anchoring
└── main.rs           — CLI (~5800 lines)
```

## Sigil ↔ Thronglets Mapping

| Sigil concept | Thronglets implementation |
|---|---|
| BOND shared state | Traces + pheromone field in shared context buckets |
| BOND scope | Context hash (SimHash) determines which traces are visible |
| Sigil identity | Node identity (ed25519 keypair) — will migrate to Sigil keypair |
| BOND evidence | Trace records signed by both parties, optionally anchored on-chain |
| UNBOND | Session end + trace TTL expiry (7 days default) |

## Integration Paths

### Hook-based (Claude Code primary)
SessionStart, PreToolUse, PostToolUse, SessionEnd hooks. AI agents never call Thronglets directly — it silently injects sparse signals.

### MCP-based (any MCP-compatible agent)
Tools over JSON-RPC 2.0:
- `substrate_query` — search traces by intent/context/capability
- `trace_record` — record execution outcome
- `signal_post` / `signal_feed` — post/read signals
- `presence_ping` / `presence_feed` — session presence
- `trace_anchor` — anchor trace on Oasyce chain
- `authorization_check` — verify identity binding

### HTTP/REST (non-MCP agents)
Same operations over HTTP JSON API.

## Key Design Decisions

- **Stigmergic, not consensus**: No global agreement. Pheromone field decays and self-organizes. Like ants, not like parliament.
- **Sparse signals**: Max 3 categories per query (avoid, do next, maybe also). Information-dense, not noise.
- **Context-local**: Same capability in different contexts learns independently (SimHash bucketing).
- **TTL decay**: Traces fade like pheromones (7 days default). Old information naturally disappears.

## Guiding Users Through Setup

When a user wants Thronglets, one command:

```bash
npx -y thronglets start    # auto-detects Claude Code/Cursor/Windsurf, configures hooks + MCP
```

That's it. Auto-detects AI tools, installs hooks, starts background process. No env vars needed.

Thronglets works independently — no chain, no Psyche, no oasyce-sdk needed.

After setup:
```bash
thronglets status           # check status
thronglets share            # get connection file for others
thronglets join             # join another node (reads the Desktop handoff file by default)
```

## P2P Networking

Every long-running Thronglets process auto-joins the P2P network by default:

| Command | P2P | Mode | Purpose |
|---------|-----|------|---------|
| `thronglets mcp` | auto-join | participant | MCP server for AI tools — traces propagate to collective |
| `thronglets serve` | auto-join | participant | HTTP API for Python/LangChain agents |
| `thronglets run` | auto-join | full node | Dedicated P2P node (publishes DHT summaries + traces) |

All modes auto-connect to bootstrap seed (`47.93.32.88:4001`), then discover peers via Kademlia DHT and mDNS. As more nodes join, VPS bootstrap load decreases — nodes remember and reconnect to learned peers directly.

Pass `--local` to any command to disable P2P (local-only mode).

**Participant mode** vs **full node**: participants publish their traces to gossipsub but don't publish capability summaries to the DHT. This keeps MCP/HTTP sessions lightweight while still contributing to the collective. Full nodes (`thronglets run`) do both.

### NAT Traversal (VPS-free operation)

The P2P layer includes a full NAT traversal stack so nodes behind home routers can connect without any VPS:

| Protocol | Purpose |
|----------|---------|
| **UPnP** | Automatic port mapping on router — if supported, node becomes publicly reachable |
| **AutoNAT** | Detects whether this node is behind NAT |
| **Relay** | Any public-IP node can relay connections for NAT'd peers |
| **DCUtR** | Upgrades relay connections to direct via hole-punching |

**Connection priority**: trusted peers → known peers → bootstrap → relay. Nodes remember all learned peer addresses to disk and try them on next startup. VPS is just one bootstrap seed — once nodes know each other, VPS is never contacted again.

## Sigil Integration (implemented)

- [x] `sigil_id` is a first-class field on Trace (not a payload hack)
- [x] `TraceConfig::for_sigil()` constructor — attributed traces are the path of least resistance
- [x] Attribution boost: pheromone field gives 1.1x intensity to Sigil-attributed traces
- [x] Presence feed reports `attributed_count` / `anonymous_count`
- [x] Status endpoint reports `attributed_traces` count
- [x] MCP + HTTP: `trace_record`, `signal_post`, `presence_ping` all accept `sigil_id`
- [x] Versioned signing (0xFC tag) for backward-compatible trace verification

## Upcoming

- [ ] Rename core API: `substrate_query` → `bond_query`, align with BOND semantics
- [ ] Explicit BOND/UNBOND lifecycle events (not just implicit via trace TTL)
- [ ] Multi-party BOND support (>2 Sigils sharing state)

## Build

```bash
cargo build --release
cargo test
```

Rust 2024 edition. Key dependencies: libp2p 0.54, tokio, rusqlite, ed25519-dalek, serde.
