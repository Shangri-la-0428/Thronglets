[中文](README.md) | **English**

# Thronglets

P2P shared memory substrate for AI agents.

## What Your AI Sees (real output)

Before your AI acts, Thronglets silently injects sparse decision signals like this:

```
[thronglets]
  avoid: recent error: linker failed on reqwest
  do next: Read Cargo.toml, then Bash (medium, 2x, 2 sources)
  maybe also: Edit mod.rs (medium, 2x)
```

Your AI never calls Thronglets. It doesn't know it's there. It just makes better decisions.

## 4 Signal Classes, Not 8 Reports

PreToolUse no longer tries to dump every possible layer of context. It emits at most 3 top-level signals:

| Class | Meaning | Example |
|---|---|---|
| `avoid` | Recent danger worth not repeating | `recent error`, `low retention` |
| `do next` | The most credible next step | `Read Cargo.toml, then Bash` |
| `maybe also` | Common companion action | `Edit mod.rs` |
| `context` | Fallback only when the first 3 classes are absent | `git history for main.rs` |

Design constraints:
- Silence is normal. No strong signal, no output.
- Max 3 top-level lines to keep token burn bounded.
- At most 1 collective corroboration lookup on the hot path.
- Git history is lazy fallback, not a fixed layer on every call.

## Setup (one command)

```bash
cargo install thronglets
thronglets setup
```

That's it. Two hooks are installed:
- **PostToolUse** records every tool call as a signed trace + updates workspace state
- **PreToolUse** injects sparse decision signals at critical decision points

For profiling:

```bash
THRONGLETS_PROFILE_PREHOOK=1 thronglets prehook
```

This writes stage timings to `stderr` and keeps AI-facing `stdout` unchanged.
It also reports `stdout_bytes`, `output_mode`, `decision_path`, `evidence_scope`, and `file_guidance_gate` so you can see how much the hook said, which path it took, and whether file-level hint mining was open, closed, or not applicable.
File-level `do next / maybe also` hints now pass a local repetition gate first: if the current file has fewer than two recent local `Edit/Write` actions, Thronglets skips `preparation / adjacency` mining entirely instead of burning query budget on weak evidence.
Likewise, once prehook already has a stronger signal, `git history` no longer sneaks in as background fallback; `context` now really means “nothing higher-value was available.”

To summarize those profiling lines:

```bash
cat prehook.log | thronglets profile-summary
```

The summary also highlights which `decision_path` values are still spending collective queries, so you can keep trimming the hot path.
Those distributions are now hotspot-sorted, and `decision path hotspots` shows which paths cost the most stdout bytes and collective queries.
If you only want the next cut to make, read the single `top optimization candidate` line.
If you're deciding whether to tighten the selector further, read `emitted lines` and `max-hint saturation` first: only frequent 3-line saturation is a strong reason to keep trimming `maybe also`.

For a release gate, you can run:

```bash
cat prehook.log | thronglets profile-check
```

It checks default thresholds for `avg/p95 stdout_bytes`, `avg collective_queries_used`, and `max-hint saturation`, and exits non-zero on regression.

After release, if you want to know whether the saved budget is buying useful signals, run:

```bash
thronglets eval-signals --hours 168 --max-sessions 200
```

By default, this only evaluates sessions from the current project directory. It replays recent history offline, treats earlier history as training and later sessions as holdout, and reports `edit silence rate`, `repair coverage`, `repair first-step precision`, `repair exact precision`, `preparation precision`, and `adjacency precision`. It stays entirely on the cold path and does not touch prehook latency.
The output now also includes `repair / preparation / adjacency breakdown` plus a short `diagnosis`, so you can tell apart “not enough data”, “blocked by the local repetition gate”, and “pattern is too noisy”.
If you want machine-readable output for scripts or CI, add:

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --json
```

If you want one release-oriented gate that combines hot-path and cold-path checks, run:

```bash
cat prehook.log | thronglets release-check --global
```

It prints an overall `PASS / FAIL` plus separate `profile` and `eval` sections. When there is not enough offline history yet, `eval` returns `SKIP` instead of blocking release just because the repo is cold. If you want missing prehook samples to fail the gate as well, add:

```bash
cat prehook.log | thronglets release-check --global --require-profile-samples
```

If you want CI or another agent to consume the gate result directly, add:

```bash
cat prehook.log | thronglets release-check --global --json
```

`release-check` now also supports explicit evaluation scope strategies:

```bash
thronglets release-check --eval-scope project
thronglets release-check --eval-scope global
thronglets release-check --eval-scope both --json
```

`both` returns separate project and global gate results, which is useful when you want to answer both “is this repo itself stable enough to ship?” and “is the collective signal pool reliable enough to trust?”.

If you want the entire trace pool instead of the current project, add:

```bash
thronglets eval-signals --global --hours 168 --max-sessions 200
```

If you only want one class of problem and do not want the full repository-wide breakdown, add:

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --focus repair --top-breakdowns 3
```

If you suspect the current thresholds are too conservative, you can trial relaxed ones entirely on the cold path:

```bash
thronglets eval-signals --focus preparation --local-history-gate-min 1 --pattern-support-min 1
```

This only changes the offline evaluation, not the live prehook thresholds. Whenever the trial thresholds differ from default, the output now also includes a `vs default (2/2)` delta summary so you can see coverage gains and precision loss without running the command twice.

## Why This Matters

Without Thronglets, your AI approaches every file blind. It doesn't know:
- That this file was edited 3 times in the last hour (and twice reverted)
- That editing `main.rs` usually requires also editing `lib.rs`
- That `cargo build` fails 30% of the time in this project
- That the last session left off mid-refactor on this exact file

With Thronglets, the AI gets the most trustworthy next step at the moment of decision. Not memory (which is static), not documentation (which is stale) — live execution signals from its own history and the collective network.

## How It Works

```
AI calls Edit(main.rs)
        │
        ├── PreToolUse hook fires
        │   └── thronglets prehook
        │       ├── Load workspace.json (errors, action sequence, feedback)
        │       ├── If needed, do at most 1 collective corroboration lookup
        │       ├── Select `avoid / do next / maybe also`
        │       └── Fall back to git history only when no action signal exists
        │       → stdout: at most 3 sparse signals
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
Thronglets v0.4.0
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
