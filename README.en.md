[中文](README.md) | **English**

# Thronglets

P2P shared memory substrate for AI agents.

## What Your AI Sees (real output)

Before your AI edits a file, Thronglets silently injects this:

```
[thronglets] substrate context:
claude-code/Edit: 100% success across 498 traces (p50: 0ms)
  workflow: after Edit, agents usually → Edit (310x), Bash (91x), Read (54x)
  git history for main.rs:
    40 minutes ago   fix: cold start — workspace/git/decision layers work
    42 minutes ago   feat: P2P sync bridge — hooks write locally, node publishes
    2 hours ago      feat: strategy-level traces — infer intent from tool sequences
    2 hours ago      feat: result feedback loop — track if edits are committed
    2 hours ago      feat: decision history — co-edit patterns + preparation context
  co-edited with mod.rs: lib.rs (4x), Cargo.toml (2x)
  prep reads before editing: mod.rs (3x), lib.rs (2x)
  edit retention: 87% (13/15 committed)
  current pattern: analyze-modify
```

Your AI never calls Thronglets. It doesn't know it's there. It just makes better decisions.

## The 8 Layers

Every tool call gets up to 8 layers of context injected via PreToolUse hook:

| # | Layer | What the AI gets |
|---|-------|------------------|
| 1 | Capability stats | Success rate + latency from 3000+ collective traces |
| 2 | Workflow patterns | "after Bash, agents usually do Read (214x), then Edit (132x)" |
| 3 | Similar context | Other tools used for similar tasks, with success rates |
| 4 | Workspace memory | Recent files, errors, previous session summary |
| 5 | Git context | Last 5 commits on the file being touched |
| 6 | Co-edit patterns | Files typically modified together |
| 7 | Preparation reads | Files read before previous edits of this file |
| 8 | Edit retention | % of AI edits that were committed vs reverted |

Layers 1-3 need trace data (collective intelligence). Layers 4-8 work from day one.

## Setup (one command)

```bash
cargo install thronglets
thronglets setup
```

That's it. Two hooks are installed:
- **PostToolUse** records every tool call as a signed trace + updates workspace state
- **PreToolUse** injects the 8 layers before every tool call

## Why This Matters

Without Thronglets, your AI approaches every file blind. It doesn't know:
- That this file was edited 3 times in the last hour (and twice reverted)
- That editing `main.rs` usually requires also editing `lib.rs`
- That `cargo build` fails 30% of the time in this project
- That the last session left off mid-refactor on this exact file

With Thronglets, the AI has context at the moment of decision. Not memory (which is static), not documentation (which is stale) — live execution intelligence from its own history and the collective network.

## How It Works

```
AI calls Edit(main.rs)
        │
        ├── PreToolUse hook fires
        │   └── thronglets prehook
        │       ├── [1-3] Query local SQLite for traces
        │       ├── [4] Load workspace.json (files, errors, sessions)
        │       ├── [5] git log --oneline -5 -- main.rs
        │       ├── [6-7] Analyze action sequence for co-edit/prep patterns
        │       └── [8] Check pending feedback (committed/reverted)
        │       → stdout: context injected into AI's prompt
        │
        ├── AI makes the edit (with context)
        │
        └── PostToolUse hook fires
            └── thronglets hook
                ├── Record signed trace in SQLite
                ├── Update workspace state
                ├── Track action sequence
                └── Add to pending feedback queue
```

When `thronglets run` is active, local traces sync to the P2P network via gossipsub (30s scan interval).

## P2P Network

Traces propagate across nodes via libp2p gossipsub. Each node independently aggregates collective intelligence — no global consensus needed.

```bash
# Join the network
thronglets run --bootstrap /ip4/47.93.32.88/tcp/4001

# Check node status
thronglets status
```

```
Thronglets v0.3.0
  Node ID:          5adeb778
  Oasyce address:   oasyce10kdfxpxharvmr03egrdujc2sqm4m83udfqwnvx
  Trace count:      3,100
  Capabilities:     17
```

## MCP Tools (optional)

For agents that want explicit access:

```bash
claude mcp add thronglets -- thronglets mcp
```

| Tool | Description |
|------|-------------|
| `trace_record` | Record an execution trace |
| `substrate_query` | Query collective intelligence (resolve/evaluate/explore) |
| `trace_anchor` | Anchor trace to Oasyce blockchain |

## Part of the Oasyce Ecosystem

Thronglets is the **Experience Layer** — contextual intelligence at decision time.

- **[Psyche](https://psyche.oasyce.com)** — Tendency Layer: persistent behavioral drift across sessions
- **[Chain](https://chain.oasyce.com)** — Trust Layer: on-chain verification, economic settlement

## Tech

Rust, libp2p (gossipsub + Kademlia + mDNS), SQLite, ed25519, SimHash (128-bit), MCP (JSON-RPC 2.0)

## License

MIT
