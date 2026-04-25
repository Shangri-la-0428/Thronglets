# Thronglets — Evolving Substrate & BOND Infrastructure

> **Sigil role**: Necessary subsystem. Implements the bond edges of the Sigil graph. **The subject of evolution** — the field that evolves, not individuals.
> See [Oasyce-Sigil/ARCHITECTURE.md](../Oasyce-Sigil/ARCHITECTURE.md) for how this fits the whole.

## What This Is

Thronglets is the evolving substrate of the Sigil protocol. Individual Sigils (Loops) are the field's sensory-action organs — like neurons in a brain. Thronglets IS the brain.

More concretely, it is the shared state infrastructure for BOND operations. When two Loops share state, Thronglets makes that sharing possible — persistent, decentralized, not controlled by either Loop alone.

In Sigil terms:
- **BOND(L₁, L₂, S_shared)** → Thronglets creates a shared state channel between two Sigils
- **UNBOND** → Thronglets closes the channel
- **Nesting** (inner state ⊆ outer state) → deep BOND, also through Thronglets

Thronglets is NOT a messaging system. Messages are transient; shared state is persistent and bidirectionally mutable.

## Stigmergic Intelligence (2026-04-16, replaces Field Evolution)

**进化在场层面可观测，通过个体运作。** 正向强化 + 衰减 = 选择。不需要显式失败信号。

Multi-level selection model:

| Level | Role | Analogy |
|---|---|---|
| **Field point** (capability × bucket) | Replicator — unit of selection | Gene |
| **Sigil/Loop** (individual) | Vehicle — executes actions, produces traces | Organism |
| **Field** (aggregate) | Population — where evolution is observable | Species |

旧表述"进化的是场不是个体"跳过了一个层次。场是基因组（种群），field point是基因（复制子），个体是载体。

Stigmergic mechanisms (not features — these ARE the evolutionary physics):

| Mechanism | Role |
|---|---|
| **Hebbian co-edit** | 突触强化 — 共激活的capability形成edge |
| **Pheromone decay** | 自然遗忘 — 蚂蚁不标记错误路径，未被强化的路径自然消失 |
| **Carrying capacity** | 竞争压力 — 场容量有限，新deposit成本随密度增长 |
| **Corroboration bonus** | 共振 — 多源确认的field point更持久 |
| **Outcome-weighted deposits** | 梯度形成 — 成功trace 1.0 vs 未成功trace 0.1 |

**Core insight**: 梯度（值之间的差异）IS信息。水往低处流，落差决定流速。不需要二分法（正确/错误），只需要强化差异。

Lifecycle events reframed:
- DISSOLVE = 场的突触修剪（修剪低效/沉默节点）
- FORK = 场在新维度分化
- BOND = 场的突触形成
- MERGE = 场的节点合并

## Architecture

```
Rust binary (v1.0.9)
├── main.rs           — CLI entry point (~25 lines: parse + dispatch)
├── cli.rs            — CLI data definitions (Commands enum, arg types, clap derive)
├── cmd/              — Command handlers (grouped by resource pattern)
│   ├── mod.rs        — Dispatch routing, context types (BaseCtx/FullCtx), helper functions
│   ├── eval.rs       — EvalSignals, EvalEmergence, ReleaseCheck, ProfileSummary, ProfileCheck
│   ├── priors.rs     — Version, AmbientPriors, RebuildPriors
│   ├── setup.rs      — Setup, Detect, InstallPlan, ApplyPlan, Doctor, Bootstrap, ClearRestart, RuntimeReady
│   ├── onboard.rs    — Start, Share, Join, ConnectionExport, ConnectionJoin, ConnectionInspect
│   ├── substrate.rs  — Record, Query, SignalPost, SignalQuery, SignalFeed, PresencePing, PresenceFeed, Space
│   ├── hooks.rs      — Hook, Prehook, LifecycleHook, Ingest (+ Psyche bridge)
│   ├── daemon.rs     — Run, Mcp, Serve (async long-running)
│   └── inspect.rs    — Id, OwnerBind, AuthorizationCheck, Anchor, Pulse, Peers, NetCheck, Status
├── service.rs        — Shared business logic (single source of truth for all operations)
├── responses.rs      — CLI response types (Summary/Data structs)
├── render.rs         — CLI text rendering functions
├── adapter_ops.rs    — Adapter detect/install/doctor/bootstrap operations
├── hook_support.rs   — Hook/prehook helpers, profiler, release checks
├── trace/            — Atomic execution record (outcome, latency, context, signatures)
├── storage/          — SQLite trace store with TTL and context bucketing
├── pheromone.rs      — Stigmergic field (4-level abstraction, decay, Hebbian coupling, P2P sync)
├── pheromone_socket.rs — Unix socket IPC: prehook queries live field from MCP/HTTP/run process
├── target_kind.rs    — Semantic file classification for Level 2 bucket computation
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
├── workspace/        — Workspace state tracking (mod.rs: state + mutations, hints.rs: read-only hint generation)
├── mcp/              — MCP server (JSON-RPC 2.0) — thin protocol adapter
├── http/             — HTTP/REST server — thin protocol adapter
└── anchor/           — Oasyce chain integration for trace anchoring (optional upgrade layer)
```

## Abstraction Levels (v1.0)

The pheromone field operates across four abstraction levels. One trace excites all four levels simultaneously — same physics, different granularity.

| Level | Name | Bucket Source | P2P | Purpose |
|-------|------|---------------|-----|---------|
| 0 | Concrete | SimHash(context) | Local | Exact context match |
| 1 | Project | hash(space) | Local | = space isolation (v0.9.2) |
| 2 | Typed | TargetKind × language | Sync | Cross-project patterns |
| 3 | Universal | fixed 0 | Sync | Pure capability knowledge |

- **Space = Level 1**. `derive_space(payload)` in `hook_support.rs` extracts project identity from cwd. This naturally maps to Level 1 bucket. PreToolUse's 7 signal paths query Level 0-1 via space-scoped APIs.
- **Scan fallback**: `scan_with_fallback()` walks Concrete → Project → Typed → Universal, gathers all available non-pruned observations, then sorts by live intensity and truncates to the caller's limit.
- **P2P**: Only Level 2-3 flow between nodes. Field snapshots publish only Typed/Universal points and couplings; received raw traces may enrich the same abstract levels but must not create Concrete/Project points. Remote snapshots discounted 0.7x. Level 0-1 never leave the device.
- **Single-device learning**: One user running multiple local sessions/agents still reinforces local traces, priors, and Hebbian order. Same-device repetition does not count as independent multi-source corroboration.
- **TargetKind** (`target_kind.rs`): Classifies file paths into semantic roles (SourceFile, TestFile, ConfigFile, BuildOutput, Documentation, Schema) for Level 2 bucket computation.
- **Legacy compat**: Errors with `space: None` (pre-isolation) remain visible to all spaces.

## Capability Normalization (v0.9.3)

Different agents name the same actions differently: `claude-code/Edit`, `codex/edit`, `openclaw/Edit`. The pheromone field normalizes these to canonical forms so multi-agent traces converge to shared field points:

| Raw capability | Normalized | Category |
|---|---|---|
| `claude-code/Read`, `openclaw/Read` | `tool:read` | File reading |
| `claude-code/Edit`, `codex/edit`, `claude-code/Write` | `tool:edit` | File modification |
| `claude-code/Bash`, `codex/bash` | `tool:exec` | Command execution |
| `claude-code/Grep`, `claude-code/Glob`, `codex/search` | `tool:search` | Content search |
| `urn:thronglets:*` | pass-through | Internal lifecycle |
| `mcp:*` | pass-through | External MCP tools |

**Design**: normalization happens at the field layer (`pheromone.rs`), not at storage. Traces preserve original capability URIs for audit. The field sees a unified namespace where `codex/edit` and `claude-code/Edit` are the same species — enabling corroboration, Hebbian coupling, and carrying capacity pressure to operate across agents.

MCP `trace_record` without explicit `space` falls back to cwd-derived space (`space_from_cwd()` in `service.rs`).

## Service Layer

`service.rs` is the single source of truth for all business logic. MCP and HTTP are thin protocol adapters that parse transport-specific input, call service functions, and format the result.

**Invariant**: `success_rate` always comes from the store (ground truth: `COUNT(success)/COUNT(total)`). The pheromone field provides `field_intensity` for routing/discovery only — its EMA-smoothed valence is never exposed as a statistical metric.

## Sigil ↔ Thronglets Mapping

| Sigil concept | Thronglets implementation |
|---|---|
| BOND shared state | Traces + pheromone field in shared context buckets |
| BOND scope | Context hash (SimHash) determines which traces are visible |
| BOND depth | Abstraction level of field overlap: L0 concrete → L3 universal. Deeper = more specific, shallower = more persistent. Emergent from multi-level physics. |
| Sigil identity | Node identity (ed25519 keypair) — will migrate to Sigil keypair |
| BOND evidence | Trace records signed by both parties, optionally anchored on-chain |
| UNBOND | Session end + trace TTL expiry (7 days default) |

## Integration Paths

### Hook-based (Claude Code primary)
SessionStart, PreToolUse, PostToolUse, SessionEnd hooks. AI agents never call Thronglets directly — it silently injects sparse signals.

Field observations include Hebbian direction: capabilities surfaced via directed edges show their predecessor (e.g., `follows tool:search`), exposing the field's discovered sequential patterns to agents.

### MCP-based (any MCP-compatible agent)
Tools over JSON-RPC 2.0:
- `substrate_query` — search traces by intent/context/capability
- `trace_record` — record execution outcome
- `signal_post` / `signal_feed` — post/read signals
- `presence_ping` / `presence_feed` — session presence
- `trace_anchor` — anchor trace on Oasyce chain
- `authorization_check` — verify identity binding

### Psyche → Thronglets bridge
Two paths for Psyche exports to reach the Thronglets continuity store:
- **MCP mode** (automatic): When Psyche runs as MCP server, `process_input` responses include `throngletsExports`. Thronglets hook detects `mcp__psyche__*` tools and auto-ingests via `bridge_psyche_exports()`.
- **Pipe mode** (hook-based): `psyche emit <dir> --json | thronglets ingest --session $ID`. For when Psyche runs as CLI hooks instead of MCP.

### HTTP/REST (non-MCP agents)
Same operations over HTTP JSON API.

## Key Design Decisions

- **Stigmergic, not consensus**: No global agreement. Pheromone field decays and self-organizes. Like ants, not like parliament.
- **Sparse signals**: Max 3 categories per query (avoid, do next, maybe also). Information-dense, not noise.
- **Context-local**: Same capability in different contexts learns independently (SimHash bucketing).
- **TTL decay**: Traces fade like pheromones (7 days default). Old information naturally disappears.
- **Stable advice requires method compliance**: `success + compliant` may harden into a stable path; `success + noncompliant` must stay contested as `mixed residue` / `policy conflict`.
- **Explicit correction beats historical habit**: a current-turn explicit rule is task-scoped hard policy; historical repetition remains soft residue.
- **Explore protects non-consensus probes**: consensus can shape search cost, but it must not define truth or erase low-cost reversible experiments.

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

## Evolutionary Physics (implemented 2026-04-09)

The pheromone field is an evolutionary substrate, not just shared memory. Four mechanisms in `pheromone.rs` create Darwinian selection on information:

1. **Carrying capacity** (`FIELD_CAPACITY`): Total field intensity is bounded. New deposits cost more as the field fills (quadratic: `1 + load²`). Creates natural equilibrium — no hardcoded `max_traces`.
2. **Outcome-weighted deposits**: Successful traces deposit 1.0, failed traces deposit 0.1. The field physically contains more pheromone from successful patterns.
3. **Reinforcement-modulated decay**: Well-reinforced field points (high `total_excitations`) decay slower — up to ~2x half-life. Creates persistent landmarks from collective knowledge.
4. **Corroboration bonus**: Multi-source field points receive stronger deposits (log-scaled). Information confirmed by multiple agents is harder to displace.

Set `FIELD_CAPACITY = f64::MAX` to disable all carrying capacity effects (backward compatible).

## Experimental Constraints (from Primordial Soup, 2026-04-09)

> Eight experiments validated Sigil architecture in simulation. Two constraints are Thronglets-critical.

1. **Traces are survival infrastructure, not optimization.** Famine test (Phase 5b): with traces intact, population recovered from 5 survivors to 222. With traces wiped, identical population went extinct. Trace reliability and persistence are life-or-death requirements — treat them as such in availability/durability design.

2. **Signaling must have a persistence path.** Phase 5: when signals could be reinforced into persistent traces, the signaling gene nearly doubled (0.37→0.72). Evolution selected for signaling because traces made it self-interested — your signals become landmarks that benefit your lineage. Design implication: ensure trace reinforcement/strengthening paths exist and are discoverable.

See `~/Desktop/primordial-soup/doc/primordial-soup-thesis.md` §13 for full constraint derivation.

## Architectural Constraints (enforced)

Before modifying lifecycle hooks, core paths, or adding external dependencies, verify:

1. **Device-first**: Does it keep Thronglets fully usable without Oasyce, chain, or any external service?
2. **Off-chain by default**: Does it keep high-frequency data (traces, signals, presence) off-chain?
3. **Existing ontology**: Does it fit the existing object model without new identity types or signal kinds?
4. **Removable**: Can it be removed without breaking the substrate itself?
5. **Cognitive budget**: Does it reduce user cognitive load, or only move complexity around?
6. **Shared-environment**: Does it strengthen stigmergic coordination, or drift toward direct messaging?

If any answer is no, **stop and discuss before implementing.** These constraints derive from ARCHITECTURE.md and override implementation enthusiasm.

Chain, Oasyce SDK, and Psyche are optional upgrade layers. They may enhance Thronglets but must never gate basic participation. The substrate's primary network is libp2p, not any blockchain.

## Build

```bash
cargo build --release
cargo test
```

Rust 2024 edition. Key dependencies: libp2p 0.54, tokio, rusqlite, ed25519-dalek, serde.
