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

That's it. `thronglets setup` auto-installs known local adapters:
- **Claude Code**: writes `PostToolUse / PreToolUse` hooks automatically
- **Codex**: registers a `thronglets` MCP server in `~/.codex/config.toml` and installs a managed `AGENTS` memory block
- **OpenClaw**: installs a local path plugin and updates `~/.openclaw/openclaw.json`

`setup` now also runs a bootstrap health pass and returns `restart required / next steps` directly.
If an adapter still needs a client restart, `doctor` now returns `restart-pending`, and after the runtime is restarted you can clear that state with:

```bash
thronglets clear-restart --agent codex --json
```

Underneath, there is only one agent contract:
- `thronglets prehook`: any agent can send tool-intent JSON and get sparse signals back
- `thronglets hook`: any agent can send tool-result JSON and record a trace

Known AIs use native adapters. Unknown AIs use the same `hook/prehook` contract. No second protocol.

If you want an AI to bootstrap itself, it does not need to read the docs first. Use the machine-facing flow:

```bash
thronglets detect --json
thronglets install-plan --agent codex --json
thronglets apply-plan --agent codex --json
thronglets doctor --agent codex --json
```

`detect` discovers local runtimes.  
`install-plan` returns the executable plan and contract.  
`apply-plan` writes the config.  
`doctor` verifies health and exits non-zero when a targeted adapter is still broken.

If you want the whole flow in one step, run:

```bash
thronglets bootstrap --agent codex --json
```

All machine-facing commands now share one stable envelope:

```json
{
  "schema_version": "thronglets.bootstrap.v2",
  "command": "bootstrap",
  "data": {
    "summary": {}
  }
}
```

`detect / install-plan / apply-plan / doctor / bootstrap / clear-restart` now all lead with a top-level summary and then carry detailed lists.  
When a restart is needed, the summary also carries explicit `restart_commands`.  
`doctor` now returns top-level `status`, `healthy`, `restart_pending`, `next_steps`, plus per-adapter `fix_command`.  
`bootstrap` also returns top-level `restart_required` and `next_steps`, so an AI does not have to infer what to do next from free-form notes.

If the target runtime is not one of the native adapters, `install-plan --agent generic --json` now also includes minimal `Python / Node.js / shell` `prehook / hook` snippets, so the runtime does not have to invent its own wrapper contract. If you only want one thinner result, use:

```bash
thronglets install-plan --agent generic --runtime python --json
```

The minimum JSON contract is fixed. `prehook` reads input like:

```json
{
  "agent_source": "my-agent",
  "model": "my-model",
  "session_id": "session-123",
  "tool_name": "Edit",
  "tool_input": {
    "file_path": "src/main.rs"
  }
}
```

`hook` uses the same payload plus `tool_response`:

```json
{
  "agent_source": "my-agent",
  "model": "my-model",
  "session_id": "session-123",
  "tool_name": "Edit",
  "tool_input": {
    "file_path": "src/main.rs"
  },
  "tool_response": {
    "success": true
  }
}
```

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
The result view now also includes three more outcome-facing metrics:
- `local edit retention`: recent local feedback from the current project's `workspace.json`
- `holdout failed command rate`: `Bash` failure rate in holdout sessions
- `holdout first successful change latency`: a proxy for time from session start to the first successful `Edit/Write`
The output now also includes `repair / preparation / adjacency breakdown` plus a short `diagnosis`, so you can tell apart “not enough data”, “blocked by the local repetition gate”, and “pattern is too noisy”.
If you want machine-readable output for scripts or CI, add:

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --json
```

If you want to answer “did this version actually improve or regress versus the last baseline?”, save a previous `eval-signals --json` result and compare against it:

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --json > baseline.json
thronglets eval-signals --hours 168 --max-sessions 200 --compare-baseline baseline.json
```

That adds two `vs baseline` lines covering `local edit retention`, `holdout failed command rate`, `first successful change latency`, and the major signal-precision deltas.

If you want one release-oriented gate that combines hot-path and cold-path checks, run:

```bash
cat prehook.log | thronglets release-check --global
```

It prints an overall `PASS / FAIL` plus separate `profile`, `doctor`, and `eval` sections. `doctor` folds local adapter state such as `healthy / restart-pending / needs-fix` into the release gate, while `eval` carries current-project `local edit retention`, holdout `failed command rate`, and `first successful change latency` alongside signal precision. When there is not enough offline history yet, `eval` returns `SKIP` instead of blocking release just because the repo is cold. If you want missing prehook samples to fail the gate as well, add:

```bash
cat prehook.log | thronglets release-check --global --require-profile-samples
```

If you want CI or another agent to consume the gate result directly, add:

```bash
cat prehook.log | thronglets release-check --global --json
```

If you also want the same gate to answer “did this release regress relative to the previous baseline?”, add:

```bash
cat prehook.log | thronglets release-check --global --compare-baseline baseline.json
```

That reuses the existing `eval-signals --json` baseline comparison inside `release-check`, and only enforces regression checks on the 3 core outcome metrics:
- `local edit retention`
- `failed command rate`
- `first successful change latency`

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
