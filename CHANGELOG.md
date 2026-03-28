# Changelog

## Unreleased

### Hot Path

- **Danger-first selector** — Prehook now emits sparse `avoid / do next / maybe also / context` signals instead of a long fixed report
- **Silence-by-default contract** — Output stays capped at 3 top-level lines and remains quiet when no strong signal exists
- **Collective corroboration budget** — The hot path spends at most 1 collective lookup, prioritized toward the strongest action candidate
- **Lazy git fallback** — `git log` now runs only when no action signal is available
- **Optional prehook profiling** — `THRONGLETS_PROFILE_PREHOOK=1` emits stage timings plus `stdout_bytes`, `output_mode`, `decision_path`, and `evidence_scope` to `stderr` without polluting AI-facing `stdout`
- **Profile summary command** — `thronglets profile-summary` reads those stderr lines from stdin and prints aggregate hot-path cost stats
- **Repair gating tightened** — repair guidance now only appears after a recent error from the same tool, instead of piggybacking on generic danger signals like low retention
- **Local repetition gate for file hints** — preparation and adjacency mining now short-circuit unless the current file already has repeated recent local `Edit/Write` history
- **Profiler gate visibility** — prehook profiling and `profile-summary` now expose `file_guidance_gate=open|closed|na` so hot-path tuning can distinguish skipped file mining from non-file tools
- **Hotspot-sorted profile summaries** — `profile-summary` now ranks distributions by frequency and reports per-`decision_path` hotspot cost summaries
- **True last-resort git fallback** — `git history` now runs only when prehook has no higher-value signal at all, including danger-only cases
- **Top optimization candidate** — `profile-summary` now emits a single highest-value tuning target derived from path hotspots

### Collective Learning

- **Session-aware repair trajectories** — Learns short repair paths like `Read Cargo.toml, then Bash`
- **Preparation / adjacency candidates** — Learns `read before edit` and companion edit patterns as structured step candidates
- **Independent source counting** — Distinguishes repeated behavior from one session vs corroboration from multiple sessions or nodes

### Tests

- Added perf guardrails for collective-source upgrades, single-query budget behavior, lazy local-only paths, and git-history fallback

## v0.3.0 — 2026-03-28

Historical v0.3.0 baseline: 8-layer decision context engine. This was later tightened into the sparse signal hot path described above.

### New Features

- **Workspace persistence** — Cross-session memory tracking recent files, errors, and sessions in `workspace.json`
- **Git history injection** — Recent 5 commits shown before Read/Write/Edit on any file
- **Decision history** — Co-edit patterns and preparation reads surfaced from tool call sequences
- **Result feedback loop** — Tracks if AI edits were committed or reverted; surfaces retention rate
- **Strategy-level traces** — Auto-detects working patterns (analyze-modify, build-fix-cycle, multi-file-refactor, codebase-exploration, code-review, delegated-research) and labels traces
- **llms.txt + website updated** — Documents all 8 context layers, full EN+ZH i18n

### Tests

- 36 unit tests for workspace module (all methods + save/load roundtrip)

### Performance

- Historical prehook baseline: 50-70ms (8 layers including git subprocess) — later reduced by sparse-signal prioritization, single-query budgets, and lazy git fallback
- PostToolUse hook: <10ms

## v0.2.1 — 2026-03-27

- `thronglets setup` — one-command Claude Code integration
- PreToolUse prehook — capability stats + workflow patterns + similar context
- PostToolUse hook — automatic trace recording
- Context text preservation in traces
- Session tracking for workflow discovery

## v0.2.0 — 2026-03-26

- SimHash context fingerprinting (128-bit)
- Bucket indexing for O(log n) similarity queries
- Cross-model collective intelligence (model_id field)
- Temporal decay (7-day TTL evaporation)

## v0.1.0 — 2026-03-25

- Core trace structure with ed25519 signing
- SQLite storage with content-addressed IDs
- libp2p gossipsub + Kademlia DHT
- MCP server (trace_record, substrate_query, trace_anchor)
- HTTP REST API
- Oasyce blockchain anchoring
