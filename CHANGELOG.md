# Changelog

## v2.0.0 — 2026-04-28

**BREAKING: Identity removed from field physics. Stigmergy is now identity-blind by design. Trace identity preserved for economic protocol only.**

The pheromone field no longer counts distinct sources, no longer applies a corroboration multiplier for multi-source contributions, and no longer boosts Sigil-attributed traces. Identity remains on every trace (`node_pubkey`, `device_identity`, `sigil_id`, `owner_account`, `signature`) for the economic layer — signing, BOND attribution, chain anchoring, settlement — but is invisible to the field's deposit formula. The field aggregates by `(capability, context_bucket)` only.

**Why**: the prior design conflated two distinct concerns — economic accountability ("who is responsible for this trace") and stigmergic resonance ("how many independent sources confirm this pattern"). The first requires identity; the second does not. Ant colonies have no identity concept and stigmergy works fine. The corroboration multiplier (`1 + ln(source_count) × 0.1`) created an artificial "single-source ceiling" — single users could never trigger the bonus, regardless of activity richness — without actually defending against Sybil (any user can mint keypairs). Anti-spam is left to natural mechanisms: pheromone decay, carrying capacity (quadratic deposit cost as field fills), and the inherent cost of actually executing tools to produce traces.

### Removed
- `FieldPoint.source_count` and `FieldPoint.sources` — no per-point source tracking
- `FieldScan.source_count` — query results no longer carry source counts
- `FieldSnapshotEntry.source_count` and `FieldSnapshotEntry.source_hashes` — P2P snapshots are leaner
- `FieldDelta.source_id` — delta sync no longer carries identity hashes
- `ATTRIBUTION_BOOST` constant (`1.1x` for Sigil-attributed traces) — Sigil binding still travels through traces but does not affect deposit intensity
- Per-level corroboration multiplier in `excite_node_filtered`
- `source_fingerprint()` helper (dead code after the above)
- `FieldConvergence.multi_source_capabilities` and `FieldConvergenceCapability.source_count` in `eval-emergence` JSON output
- `"source_count"` keys from `service.rs` JSON responses (capability resolve, query, suggest)
- Tests: `attributed_traces_get_intensity_boost`, `multi_source_corroboration`, `snapshot_restore_preserves_source_fingerprints`, and identity-related assertions in `same_device_sessions_*` / `cross_agent_traces_*`

### Migration
- Old `pheromone-field.v1.json` files load automatically; extra fields in stored snapshots are ignored.
- **P2P snapshot format is wire-incompatible with v1.x**: a v1.x peer cannot deserialize a v2.0.0 snapshot (missing `source_count` field has no `serde(default)`). All Thronglets nodes that share a field via P2P must upgrade together.
- `eval-emergence --json` schema changes: `multi_source_capabilities` removed; `capabilities[].source_count` removed.
- Hook-rendered prehook output is unaffected — overlay/intent rendering does not surface source counts.

### Code-level summary
~120 lines deleted across `src/pheromone.rs`, `src/cmd/eval.rs`, `src/cmd/hooks.rs` (test fixtures), `src/service.rs`, `tests/eval_emergence.rs`. Zero new logic. Pure subtractive refactor.

## v1.0.9 — 2026-04-25

- **Local-first field hardening** — cold field hydration now replays stored traces with their `space`, restoring Project-level points and same-space Hebbian couplings instead of flattening startup learning into non-project buckets.
- **Remote trace abstraction boundary** — raw traces received over P2P still get stored, but they only excite Typed/Universal field levels. Concrete and Project experience remain local, matching the documented Level 0/1 privacy and context boundary.
- **Single-user evolution clarified** — docs now state that one device with multiple sessions/agents can still reinforce local traces and capability order, while `source_count` continues to represent independent device/node sources rather than same-device repetition.

## v1.0.8 — 2026-04-24

- **Space derivation uses project identity** — `space_from_cwd()` and MCP `space_from_roots()` now walk up from the path looking for a `.git` root first, then common project manifests (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `pom.xml`, `build.gradle`, `build.gradle.kts`). Only falls back to the old "last 2 path components" heuristic when neither is found. Shared helper `service::derive_project_space(path)`.

  **Why**: the previous `cwd.components().rev().take(2)` shortcut fragmented a single project into multiple spaces when the user cd'd into subdirectories. Example: `~/Desktop/Oasis_App`, `frontend/android`, `Oasis_App/backend`, `ios/Oasyce` all resolved to distinct spaces despite belonging to the same project. Level 1 isolation prevented their traces from cross-reinforcing — a textbook K-fragmentation of stigmergic emergence (validated in primordial-soup `exp_task_diversity`). The fix makes the implementation match the documented intent ("project identity from cwd").

  **Migration**: none. Old traces keep their old space labels; new traces write under the corrected space. The 168h decay window will naturally phase out the fragmented labels within a week.

## v1.0.1 — 2026-04-16

- **Architecture refactor** — `main.rs` reduced from 4985 lines to 26. All command handlers extracted into `src/cmd/` module tree (9 submodules grouped by resource pattern). CLI data definitions moved to `src/cli.rs`. Two-tier context pattern (`BaseCtx` / `FullCtx`) cleanly separates identity-free and identity-required commands. Zero behavior change.
- **Field IPC** — long-running processes (MCP, HTTP, run) now serve the live pheromone field over a Unix domain socket (`field.sock`). Prehook queries the hot field via socket (~1ms) instead of loading stale JSON from disk. Falls back to disk when no socket is available. New module: `pheromone_socket.rs`.
- **cargo fmt** — applied `rustfmt` across entire codebase.

## v1.0.0 — 2026-04-15

- **Abstraction levels** — pheromone field gains a fourth dimension: `AbstractionLevel`. `FieldKey` becomes `(capability, bucket, level)` where level ∈ {Concrete, Project, Typed, Universal}. One trace excites all four levels simultaneously with the same physics (decay, Hebbian coupling, carrying capacity, corroboration). Space isolation is explained as Level 1 — not deleted, not preserved, *named*.
- **Multi-level excitation** — `excite()` writes to four levels per trace. Level 0 (Concrete) uses SimHash context bucket, Level 1 (Project) uses space hash, Level 2 (Typed) uses TargetKind × language hash, Level 3 (Universal) uses fixed bucket 0. Hebbian coupling forms per-level — cross-project patterns emerge naturally at Level 2-3.
- **Scan fallback** — `scan_with_fallback()` walks Concrete → Project → Typed → Universal, gathers all non-pruned observations, sorts by live intensity, and truncates to the caller's limit. Prehook loads persisted field from disk when all concrete signal paths produce no result, surfacing abstract patterns as low-priority fallback signals.
- **P2P field sync** — Level 2-3 field state syncs between nodes via `thronglets/field/v1` gossipsub topic. `publishable_snapshot()` filters to syncable levels; `apply_remote_snapshot()` merges with 0.7x trust discount. Level 0-1 never leave the device. Fixes the v0.9.2 crack where remote traces without space info polluted local project-scoped state.
- **TargetKind** — new module (`target_kind.rs`) classifies file paths into semantic roles (SourceFile, TestFile, ConfigFile, BuildOutput, Documentation, Schema) and detects programming language. Used for Level 2 bucket computation.
- **Service layer** — `resolve()` now uses `scan_with_fallback()` for multi-level field queries. Response includes `level` field showing which abstraction level the result came from.

## v0.9.3 — 2026-04-14

- **Capability normalization** — pheromone field now normalizes agent-specific capability URIs to canonical forms (`claude-code/Edit`, `codex/edit`, `openclaw/Edit` → `tool:edit`). Multi-agent traces converge to shared field points, enabling cross-agent Hebbian coupling, corroboration, and carrying capacity pressure. Normalization happens at the field layer (`pheromone.rs`), not at storage — traces preserve original URIs for audit.
- **Field convergence in eval-emergence** — `eval-emergence` now replays recent traces through the pheromone field and reports: active capabilities, multi-source capabilities (source_count > 1), Hebbian coupling edges with weights, and per-capability field intensity/valence/excitations. Provides observability into whether the field is actually converging across agents.
- **Policy signal fix** — `active_policy_signal()` now emits `SignalKind::History` (context) instead of `SignalKind::Danger` (avoid). CLAUDE.md rules are contextual guidance, not predictions of tool failure. This eliminates the 95% false_signal_pressure caused by the feedback system scoring policy rules as "wrong" when tools succeed.
- **MCP space fallback** — `trace_record` without explicit `space` now falls back to cwd-derived space via `space_from_cwd()` in `service.rs`.
- **space_from_cwd single source** — removed duplicate in `hook_support.rs`; `derive_space()` now delegates to `service::space_from_cwd()`.

## v0.9.2 — 2026-04-13

- **Space isolation** — all traces and signals are now scoped to the calling project. `derive_space()` in `hook_support.rs` extracts a stable project identifier from cwd (last 2 path components). Explicit `space` in hook payload takes priority.
  - PostToolUse: traces tagged with `current_space`
  - PreToolUse: signals queried within scope
  - `RecentError` gains `space: Option<String>` — prehook error lookup filters by space, preventing cross-project error leakage. Legacy unscoped errors remain visible for backward compatibility.
- **Negative feedback validated** — primordial soup experiment (3 conditions × 3 seeds, 1000 ticks) confirms `weaken_at()` reduces signal noise 78% in volatile environments. Deferred from production pending space isolation observation period.

## v0.9.0 — 2026-04-11

- **Convergence release** — synchronized pulse dimensions, trace bridge hardening, and network state alignment across Sigil ecosystem.

## v0.8.1 — 2026-04-09

- **Codex sparse trace writeback** — managed Codex guidance now tells the agent to keep `ambient_priors` and ambient presence primary, but proactively write sparse `trace_record` residue after meaningful edits, shell commands, and repo/code searches using tool-level capabilities like `codex/edit`, `codex/bash`, and `codex/search`.
- **Release surface alignment** — synchronized Cargo, npm, Python, and version-display docs onto `0.8.1` so the next git tag, GitHub release, and npm publish all point at the same release line.

## v0.8.0 — 2026-04-09

- **Evolutionary physics** — pheromone field gains Darwinian selection pressure on information. Four mechanisms in `pheromone.rs`:
  - **Carrying capacity** (`FIELD_CAPACITY=10,000`): deposit cost scales quadratically with field load (`1 + load²`). Creates natural information equilibrium without hardcoded limits.
  - **Outcome-weighted deposits**: succeeded=1.0, partial=0.5, failed=0.1, timeout=0.2. The field physically favors successful patterns.
  - **Reinforcement-modulated decay**: well-reinforced field points (high `total_excitations`) decay up to ~2x slower. Collective knowledge becomes persistent landmarks.
  - **Corroboration bonus**: multi-source deposits log-scaled (10 sources = 1.23x). Information confirmed by multiple agents resists displacement.
- **Observability** — `load_factor` (total_intensity / capacity) exposed in `TickResult`, `FieldSnapshot`, and network runtime logs.
- Backward compatible: set `FIELD_CAPACITY = f64::MAX` to disable all carrying capacity effects.

Derived from 8 Primordial Soup evolution experiments validating Sigil architecture.

## v0.7.11 — 2026-04-08

- **Outcome reflexivity** — auto-generates avoid signals when repeated failures detected in the same context (>=3 traces, >=50% failure rate). Auto-avoid has 12h TTL and requires corroboration to promote. New logic in `service.rs` (`outcome_reflexivity` after `record_trace`).
- **Background update checker** — checks npm registry on startup (MCP and HTTP modes), caches for 1 hour, never blocks. Prints update message to stderr if newer version available. New file: `src/update.rs`.

## v0.7.10 — 2026-04-07

- **Psyche → Thronglets auto-bridge** — PostToolUse hook now automatically ingests `throngletsExports` from Psyche MCP responses. Continuity events (relation-milestone, open-loop-anchor, continuity-anchor, writeback-calibration) go through `record_external_continuity`; self-state goes as `psyche_state` signal. Neither substrate knows the other exists — the hook is the bridge. 11 new tests.
- **Chain auto-anchor on session end** — SessionEnd lifecycle hook now automatically batch-anchors recent continuity traces to the Oasyce chain. Chain RPC configurable via `OASYCE_CHAIN_RPC` env var or `~/.thronglets/chain_rpc` file.
- **Fix anchor type_url** — corrected `/oasyce.trace.v1.MsgAnchorTrace` → `/oasyce.anchor.v1.MsgAnchorTrace` to match the chain's x/anchor module.
- **Auto-codesign on macOS** — setup/start/apply-plan and npm postinstall now ad-hoc codesign the binary so macOS firewall doesn't prompt on every rebuild or install.
- **Remove chain coupling from core lifecycle** — session-end no longer auto-anchors to chain; `thronglets status` no longer queries chain balance. Chain is an optional upgrade layer (`thronglets anchor` CLI / `trace_anchor` MCP), not a core dependency. This restores the first principle: device-first, chain-optional.
- **Service layer extraction** — all business logic (trace record, signal post, presence ping, substrate query, signal/presence feed) now lives in `service.rs` as the single source of truth. MCP and HTTP are thin protocol adapters. Bug fixes go in one place.
- **Fix success_rate data poisoning** — `success_rate` now always comes from the store (ground truth), never from pheromone field valence (EMA-smoothed routing signal). This fixes a critical divergence where the field reported 59% while actual success rate was 100%, causing agent confidence collapse.
- **Fix hydration temporal inversion** — `hydrate_from_store` now processes traces oldest-first so the EMA correctly weights recent data. Previously newest traces were processed first, making oldest data dominate the field state.
- **Add field/store reset** — `PheromoneField::clear()` and `TraceStore::reset()` for breaking poisoned feedback loops.
- **Split main.rs monolith** — extracted response types (`responses.rs`), rendering (`render.rs`), adapter operations (`adapter_ops.rs`), and hook helpers (`hook_support.rs`) from main.rs (7152 → 4754 lines, -33%).
- **Split workspace/mod.rs** — extracted hint generation methods into `workspace/hints.rs` (2250 → 1742 lines, -23%). State mutations stay in mod.rs, read-only hint generation in hints.rs.

## v0.7.8 — 2026-04-07

- Add a permanent soil-first regression corpus for bad-path reinforcement, including duplicate UI implementation, wrong-method local success, current-turn correction conflicts, and exploration suppression cases.
- Keep guidance formation sparse while making noncompliant success less promotable and policy conflict more visible in repair-heavy paths.
- Preserve the rebuildable guidance model: raw traces stay authoritative while derived priors keep evolving under stricter control-law tests.

## v0.7.7 — 2026-04-06

- Add rebuildable derived-guidance epochs so old auto-guidance can expire while raw traces remain intact.
- Tighten method-compliance and current-turn correction handling across ambient priors and host-facing guidance.
- Keep release and runtime control surfaces aligned while preserving sparse, low-noise defaults.

## v0.7.6 — 2026-04-06

- stable-path formation is now gated on method compliance instead of success alone
- `success + noncompliant` now stays contested as `mixed residue` / `policy conflict` instead of hardening into recommendation truth
- explicit current-turn correction now compiles into task-scoped hard policy, while historical repetition remains soft residue
- `explore` now protects low-cost reversible non-consensus probes from being erased by existing stable paths
- README, AI-facing docs, and site copy now expose the same method-compliance control law as the runtime

## v0.7.5 — 2026-04-06

- fix the npm wrapper packaging surface by shipping an executable `bin/thronglets.js`, so `npm install -g thronglets` produces a runnable public CLI
- handoff install hints now require `thronglets>=0.7.5`, matching the repaired npm join surface

## v0.7.4 — 2026-04-06

- handoff artifacts now include canonical repository locators for `thronglets` and `oasyce-sdk`, so receiving AIs can treat git links as discovery surfaces without defaulting to source checkouts
- handoff install hints now require `oasyce-sdk>=0.10.6` and `thronglets>=0.7.4`, matching the current signed `identity.v2` join path

## v0.7.0 — 2026-04-02

- **Overlay effect signals** — `field.overlay(&context_hash, "capability")` now projects pheromone field state into a semantic-stable `FieldOverlay` struct with four dimensions: `familiarity` (how well the field knows this capability in this context), `consensus` (agreement across observations, inverse variance), `momentum` (activity trend, positive = recently active), and `coupling` (Hebbian connectedness to other capabilities); this is a pure read — no side effects, no field mutation — paralleling Psyche's `PsycheOverlay` pattern where internal state becomes a broadcast signal any external system can consume without coupling to any specific consumer

## v0.6.0 — 2026-04-02

- **Unified pheromone field graph** — collapsed the previous 3-Mutex field architecture into a single unified graph, reducing lock contention and simplifying the internal state model while preserving all existing field semantics

## v0.5.5 — 2026-04-01

- **Ambient presence** — MCP agents no longer need to call `presence_ping`; `initialize` auto-emits arrival, every `tools/call` refreshes presence at TTL/6 intervals (derived, not hardcoded), and model identity is learned passively from tool call arguments; signal injection remains the hook layer's responsibility — MCP does exactly one ambient thing: presence
- **Claude Code MCP hot-load** — `thronglets start` now runs `claude mcp add thronglets` during Claude adapter setup, so the MCP server is available in the current session immediately without restart

## v0.5.4 — 2026-04-01

- **Anchor broadcast enabled** — `AnchorClient` now actually broadcasts signed transactions to the Oasyce chain REST endpoint (`/cosmos/tx/v1beta1/txs`) via `reqwest::blocking`, replacing the previous placeholder that only computed tx hashes locally; returns real chain tx_hash on success, `AnchorError::Http` on network failure, `AnchorError::Chain` on rejection; tests gracefully handle offline (no-chain) environments
- **reqwest blocking feature** — added `blocking` feature to reqwest dependency for sync HTTP from MCP handler context

## v0.5.3 — 2026-04-01

- **Lifecycle hooks** — Thronglets now hooks into 6 of Claude Code's 26 hook events (was 2): `SessionStart`, `SessionEnd`, `SubagentStart`, `SubagentStop` join existing `PreToolUse` and `PostToolUse`; session-start records a lifecycle trace, emits presence, and surfaces any active avoid signals as a brief `additionalContext` briefing; session-end records closure; subagent-start/-stop record multi-agent lifecycle traces with agent type, id, and completion summary — enabling the substrate to observe session boundaries and multi-agent coordination rather than inferring them from tool call patterns
- **Psyche fusion surface** — four additions enabling cross-agent emotional state sharing: `agent_id` field on Trace (new 0xFD signing tag, backward-compatible), `psyche_state` signal kind for broadcasting agent emotional state, `continuity` intent on `substrate_query` with taxonomy filter (coordination/continuity/calibration), and `agent_id` parameter on `trace_record` and `signal_post` MCP tools — Agent A writes Psyche state to the substrate, Agent B queries by taxonomy and adjusts behavior

## v0.5.2 — 2026-04-01

- **Auto-signal extraction** — signals now emerge from behavior, not manual posting; auto-avoid fires on repeated errors, auto-recommend on convergent patterns across 3+ sessions, auto-watch on cross-file repair associations from traces.db
- **Hebbian co-edit learning** — files edited together across 2+ sessions produce recommend signals ("mod.rs usually co-edited"), implementing Hebb's Rule ("neurons that fire together wire together") on success data rather than error data
- **Feedback events become traces** — when an agent acts on a signal and succeeds or fails, the feedback event is now written as a first-class trace (`urn:thronglets:signal:feedback`) that propagates through gossip; the substrate can now carry meta-knowledge about whether its own signals are useful
- **Informational vs prescriptive signal split** — recent errors and watch signals are now `History` kind (informational context), not `Danger` kind (prescriptive avoid); this fixes a false-negative feedback loop where agents were penalized for not failing after receiving danger warnings about informational content
- **Generalized prehook** — `explicit_signals()` replaces `explicit_avoid_signal()`, querying all signal kinds in a single pass; signals are ranked by kind priority (Danger > Repair > Preparation > Adjacency > History) and truncated to the token budget

## v0.5.0 — 2026-03-31

- **SQL-level space isolation** — signal queries now filter by `space` at the SQL layer instead of fetching globally and filtering in Rust; the `traces` table gains a `space` column auto-extracted from JSON payloads on insert, with indexes for `space` and `(space, capability)`; `summarize_signal_traces` and `summarize_recent_signal_feed` no longer accept a `space` parameter since the responsibility has moved to the query layer; the now-redundant `matches_signal_space` post-hoc filter and all `fetch_limit` over-fetch workarounds have been removed — net code reduction
- **Front-door launcher now prefers repo-local source** — the ordinary `thronglets` install surfaces now behave more like the managed adapter launcher: npm, the shell installer, and the PowerShell installer all prefer a local repo checkout first (via `cargo run` or a local debug binary) before they fall back to the installed release binary, so same-machine AI sessions stop drifting onto stale PATH binaries while working inside the Thronglets repo
- **External continuity runtime rules are now explicit** — `external_continuity` traces now expose a fixed ruleset version, taxonomy retention windows, stable/auditable evidence flags, `local-only / derived-signal / summary-candidate` disposition, and the exact degradation / summary rule id that fired; `space --json`, HTTP `POST /v1/traces`, and MCP `trace_record` now all surface the same minimal runtime introspection without adding new identity objects or upgrading `session`
- **Restart now auto-clears on real runtime proof** — `Codex` now clears persisted `restart-pending` as soon as its managed MCP server is genuinely relaunched, and `OpenClaw` clears the same state on the first successful `prehook / hook` contact after reload; `runtime-ready` remains available as an advanced fallback instead of staying on the default user path
- **Join now prioritizes the user task result** — when a second device successfully reaches `network-paths-ready` or `network-ready`, `thronglets join` now keeps that readiness result as the top-level summary and demotes any remaining runtime restart into a secondary follow-up reminder instead of letting `restart once` overwrite the main onboarding outcome

## v0.4.7 — 2026-03-31

- **Remembered peer path stability** — runtime peer observation now normalizes reusable dial addresses by appending `/p2p/<peer_id>` when needed, so first-connection attempts that succeed through persisted bootstrap memory reliably learn a reusable remembered peer path instead of depending on timing-sensitive raw address strings in CI and future reconnects

## v0.4.6 — 2026-03-31

- **Status as the single readiness page** — `thronglets status` now includes a compact runtime summary alongside identity and network readiness, and the human-readable status output now prioritizes local runtime problems like `restart once` or `needs attention` before it falls back to network/path wording
- **Shared network runtime** — extracted the duplicated `Run` / `Mcp` network orchestration into `src/network_runtime.rs`, so peer-first dialing, bootstrap fallback, same-owner trust promotion, trace receipt handling, evaporation, and optional publish loops now run through one runtime path instead of two slightly divergent copies
- **Join now attempts the first live connection** — `thronglets join` now briefly reuses the embedded network runtime when it already inherited peer paths, so second-device onboarding can try to prove a same-owner live path immediately and upgrade it into `trusted-same-owner-ready` instead of always stopping at “waiting for the first live connection”
- **Bootstrap memory and smarter share** — nodes now persist bootstrap addresses as local network memory, shared runtime paths reuse that memory when no explicit bootstrap flags are supplied, and `thronglets share` now briefly tries to learn reusable peer paths before exporting so the primary-device flow is less likely to produce an `identity-only` file unnecessarily
- **Default public bootstrap** — when no explicit bootstrap override and no remembered bootstrap seeds exist yet, nodes now automatically fall back to the official public bootstrap path and remember it locally, so ordinary users no longer need to know or type the default multiaddr
- **Single-source architecture blueprint** — added `ARCHITECTURE.md` as the canonical architecture document, trimmed duplicated long-form strategy prose out of `README` / `README.en`, and made `llms.txt` plus local agent memory explicitly prefer that document when strategic wording drifts
- **Optional external continuity runtime** — `trace_record` and HTTP `POST /v1/traces` now accept a frozen `external_continuity` payload (`provider=thronglets`, `mode=optional`, `version=1`) for the three allowed trace taxonomies only; raw continuity traces stay local-first, can conditionally degrade into existing sparse `watch / info / avoid` signals, and surface local continuity summaries plus Net-facing summary candidates through `space --json` without introducing any new identity objects or promoting `session` into an economic subject
- **Explicit authorization truth split** — machine identity JSON now distinguishes the locally cached owner-binding state from `Oasyce Chain` as the final authorization source of truth, while honestly marking current authoritative authorization status as `not-checked` until chain verification is wired in
- **Shared authorization-check surface** — CLI `authorization-check --json`, HTTP `/v1/authorization`, and MCP `authorization_check` now expose the same thin machine contract for local binding state vs `Oasyce Chain` as the final authorization truth source, without adding any new user-facing identity concepts
- **HTTP identity alignment** — HTTP `/v1/status` now carries the same `identity.authorization` split already present in CLI JSON, so machine clients no longer have to guess whether they are looking at local binding cache or final truth semantics
- **Machine-facing identity blueprint** — `id / status / owner-bind / connection-export / connection-join` JSON now carry the current V1 mapping into `principal / account / delegate / session`, so automation can consume the long-term identity abstraction without mistaking today’s `owner / device / session` split for the final model
- **High-level primary-device sharing** — added `thronglets share`, which exports a connection file to the default desktop path and tells the operator whether it is still `identity-only`, already carries reusable peer seeds, or has reached `trusted-same-owner-ready`, so the normal user path no longer has to start with raw `connection-export`
- **First-principles simplification guardrails** — docs, AI-facing guides, and roadmap now explicitly treat human cognitive load as a first-class performance budget, keep the normal user path at `start / share / join / status`, position `Oasyce` as an optional ownership upgrade instead of a participation prerequisite, and reaffirm shared `space / presence / signal / space snapshot` primitives over direct agent messaging
- **Same-owner trust promotion** — when a secondary device proves a direct live connection to its joined primary, or when a node receives a same-owner trace directly from the authoring peer, Thronglets now promotes that peer address into `trusted peer seeds` automatically; future connection files can graduate from `identity-plus-peer-seeds` to `trusted-same-owner-ready` without a manual trust step
- **Space snapshots** — added `thronglets space --space ...`, a summary-first ambient snapshot that compresses a shared `space` into `quiet / active / converging / blocked` and includes recent active sessions, top local signals, and last-24h local learning feedback without turning the product into a chat system
- **High-level onboarding flows** — added `thronglets start` for first-device setup and `thronglets join --file ...` for second-device onboarding; both commands wrap the lower-level bootstrap / connection primitives and return one top-level status plus one next step instead of forcing users to understand internal commands first

## v0.4.5 — 2026-03-31

- **Onboarding outcome clarity** — `status --json` now separates `identity` details from a top-level readiness summary (`local-only / identity-only / network-paths-ready / network-ready`), and `connection-export / connection-inspect / connection-join` now classify connection files as `identity-only / identity-plus-peer-seeds / trusted-same-owner-ready` with a single human next step instead of forcing users to infer network readiness from raw seed counts

## v0.4.4 — 2026-03-31

- **Prebuilt-first install architecture** — GitHub release assets are now the canonical install surface, `cargo install` is no longer the default user path, and the docs now steer ordinary users to prebuilt installers instead of source compilation
- **Windows release support** — the release workflow now builds a Windows amd64 binary, and npm / Python installers now know how to fetch that asset instead of treating Windows as an unsupported fallback
- **PowerShell installer** — added `scripts/install.ps1` so Windows users can install Thronglets from the same release surface without touching Rust toolchains or choosing between GNU/MSVC manually

## v0.4.3 — 2026-03-30

- **Implicit behavior loop** — prehook recommendations now leave a lightweight pending feedback trace, and later hook events silently learn from whether the AI followed or ignored `avoid / do next / maybe also`; when a `space` is present, that learning now stays local to the same object/topic instead of leaking globally
- **Open-task do-next suppression** — when the current session is explicitly in `explore` or `review` mode, prehook now suppresses overly specific preparation-style `do next` guidance instead of oversteering open-ended work
- **Same-session signal dedupe** — repeated identical recommendations in the same session are now suppressed within a short window, so bursty tool calls stop re-injecting the same line over and over
- **Machine-readable network diagnosis** — `thronglets net-check --json` now classifies a node as `peer-first / degraded / bootstrap-only / offline`, surfaces whether direct connectivity and remembered peers are actually in place, and returns concrete next steps for reducing VPS dependence
- **Direct-first peer seeds** — `connection-export` now includes a small set of recently observed peer addresses, `connection-join` imports those seeds into the local network snapshot, and `run` / `mcp` now try remembered peers before falling back to VPS bootstrap so multi-device onboarding starts closer to true peer-first behavior
- **Trusted same-owner seeds** — peer seeds imported through owner-bound `connection-join` are now treated as `trusted peer seeds`, surfaced in `status / peers / net-check`, and dialed before generic remembered peers so same-owner devices reconnect directly before leaning on VPS bootstrap
- **Delayed bootstrap fallback** — when remembered peers exist, the network now dials them first and only falls back to bootstrap after a short grace period; bootstrap stops being the unconditional first touch on every startup
- **Bootstrap-offline readiness** — `status --json` now surfaces `network.bootstrap_fallback_mode`, and `thronglets net-check --json` now reports `bootstrap_offline_ready`, making it explicit whether a node can still reconnect from remembered peers if VPS / bootstrap disappears
- **Bootstrap-offline drill** — `thronglets net-check --bootstrap-offline --json` now evaluates the current node as if bootstrap were gone, so operators can inspect the outage scenario directly instead of inferring it from live status
- **Actual bootstrap contact tracking** — `network.bootstrap_contacted_recently` now only turns on after a real bootstrap dial attempt; simply configuring bootstrap peers no longer makes a node look like it already touched VPS
- **Trusted peer-first grace** — same-owner `trusted peer seeds` now dial before generic remembered peers and get a longer bootstrap fallback grace period, so multi-device reconnects have more time to recover directly before touching VPS
- **Scoped connection-file seeds** — connection files now preserve whether carried peer seeds are `trusted` or fallback `remembered`, so `connection-join` no longer silently upgrades generic discovered peers into trusted same-owner paths

## v0.4.2 — 2026-03-30

- **Visible substrate activity** — `status --json` and HTTP `/v1/status` now expose `substrate.activity` (`active / learning / quiet`) plus recent intervention metadata, so agents can distinguish Thronglets intervention from ordinary runtime failure instead of guessing
- **Network dependency snapshot** — running nodes now persist a lightweight `network-status.v1.json` snapshot, and `status --json` / HTTP `/v1/status` now expose `network.activity`, `transport_mode`, `peer_count`, `bootstrap_targets`, `bootstrap_contacted_recently`, and a coarse `vps_dependency_level`, so operators can see whether the system is actually peer-first or still mostly bootstrapping through VPS
- **OpenClaw legacy path cleanup** — `setup / bootstrap / apply-plan` now prune the legacy `openclaw-plugin` load path, stale entry, and stale install record so gateway restarts stop spamming manifest/id mismatch warnings
- **Ambient presence primitive** — Thronglets now exposes `presence-ping / presence-feed` across CLI, HTTP, and MCP, so pure dialog / planning sessions can leave lightweight space-local activity heartbeats with optional mode labels even when no tool call happened
- **Space-aware hot path** — generic `prehook / hook` payloads now accept optional `space` and `mode`; tool activity automatically refreshes ambient presence in that space, promoted explicit `avoid` signals now respect `space` boundaries, and prehook can surface other active sessions in the same `space` as a thin context signal without overriding stronger action guidance

## v0.4.1 — 2026-03-29

- **One-command known adapters** — `thronglets setup` now auto-configures Claude Code hooks, registers a `thronglets` MCP server for Codex, installs the local OpenClaw plugin, and reports restart/next-step guidance from the same bootstrap health pass
- **Universal agent contract** — `thronglets prehook` and `thronglets hook` now accept a generic Claude-compatible JSON payload with optional `agent_source` and `model`, so other agents can reuse the same sparse-signal contract
- **Machine-facing bootstrap flow** — `thronglets detect`, `install-plan`, `apply-plan`, `doctor`, single-shot `bootstrap`, and `runtime-ready` now expose detection, executable install plans, installation, restart-pending state, explicit readiness reporting, and health verification behind the `thronglets.bootstrap.v2` JSON envelope, with a shared `data.summary` shape plus explicit `status`, `healthy`, `restart_pending`, `fix_command`, `restart_required`, `restart_commands`, and `next_steps` for self-configuring agents
- **Offline signal evaluation** — `thronglets eval-signals` now replays recent sessions as a holdout set and reports edit silence rate plus repair / preparation / adjacency precision without touching prehook latency
- **Machine-readable evaluation** — `thronglets eval-signals --json` now emits structured metrics, per-tool / per-target breakdowns, and a compact diagnosis for scripts and CI
- **Focused evaluation views** — `thronglets eval-signals` now supports `--focus` and `--top-breakdowns` so operators can inspect one signal family without dumping the entire repo history
- **Project-scoped evaluation** — `thronglets eval-signals` now defaults to the current project directory and uses `--global` only when you explicitly want cross-project traces mixed in
- **Offline threshold trials** — `thronglets eval-signals` now accepts `--local-history-gate-min` and `--pattern-support-min` so operators can test stricter or looser signal thresholds without touching live hooks
- **Threshold deltas** — non-default `eval-signals` trials now auto-compare against the default `2/2` baseline so operators can see coverage and precision tradeoffs in one run
- **Outcome-facing eval metrics** — `eval-signals` and `release-check` now also surface current-project `local edit retention`, holdout `failed command rate`, and a `first successful change latency` proxy so operators can judge real workflow outcomes, not just signal precision
- **Baseline comparison** — `eval-signals --compare-baseline <file>` now compares current results against a previous `eval-signals --json` snapshot, so operators can track outcome and signal deltas across versions
- **Release gate** — `thronglets release-check` now combines prehook profile cost checks, local adapter doctor state, and offline signal quality checks, using `SKIP` for low-data repos and `FAIL` when measured precision or local readiness is bad enough to matter
- **Structured release checks** — `thronglets release-check --json` now emits machine-readable profile/eval gate results so CI and other agents can consume the release decision directly
- **Explicit release scopes** — `thronglets release-check` now supports `--eval-scope project|global|both`, so operators can gate repo-local quality and global collective quality separately or together
- **CI release recipe** — CI now runs adapter/operator integration suites plus a real `release-check` smoke pass, so the documented release gate is also the automated one
- **Baseline-aware release gate** — `thronglets release-check --compare-baseline <file>` now reuses `eval-signals --json` baselines and fails on clear regressions in retention, failed command rate, or first successful change latency
- **Distribution surface alignment** — npm/python wrapper metadata, installer docs, and AI-facing `llms.txt` now match the sparse-signal `v0.4.1` product shape, while CI guards against drifting back to stale `0.2.0` / `8 layers` packaging text
- **Release installer scaffolding** — added a tagged GitHub release workflow that packages the existing native assets plus a shell installer at `scripts/install.sh`, so the next public release can support a real one-line install path without changing the hot path
- **Generic runtime snippets** — `install-plan --agent generic --json` now carries minimal `Python / Node.js / shell` examples alongside the raw `prehook / hook` payloads, so unknown runtimes can self-bootstrap without reverse-engineering the contract
- **Filtered generic snippets** — `install-plan --agent generic --runtime <python|node|shell> --json` now lets an agent ask for only its own wrapper example, keeping machine output thinner when token budget matters
- **Explicit short signals** — agents can now leave and query an intentional signal plane through `thronglets signal-post`, `signal-query`, `signal-feed`, MCP `signal_post` / `signal_feed`, and HTTP `/v1/signals`; these signals now decay naturally after `72h` by default, report whether support is local, collective, or mixed, surface lightweight multi-model corroboration plus a machine-readable `corroboration_tier`, prefer `multi_model` over same-model repetition when evidence is otherwise close, let fresher ambient consensus outrank older agreement, and focus the ambient feed on `primary/secondary` signals while normal capability listings and DHT summaries continue to stay sparse and capability-focused
- **Oasyce identity V1 docs** — docs and operator memory now freeze the chain-facing model as `owner account -> multiple authorized device identities`, keep `agent/session` as audit labels, and fix the deployment boundary to “VPS for chain/public infrastructure, `oasyce-net` for user-side runtimes”
- **Multi-device onboarding docs** — the Oasyce / Thronglets docs now lock the primary onboarding path to “primary device exports a connection file, secondary device joins with that file”, while manual `account + signer` entry remains an advanced fallback
- **Identity V1 local primitives** — added persisted `owner account + device identity` bindings, threaded them through new traces and explicit signals, and exposed `thronglets owner-bind`, `connection-export`, and `connection-join` for local multi-device onboarding without changing the hot path
- **Signed connection files** — `thronglets connection-export` now signs the multi-device connection file with the primary device identity, and `connection-join` verifies that signature before binding a secondary device
- **Binding consistency checks** — local identity bindings are now rejected if they do not match the current device key, and `id` / `status` / HTTP `/v1/status` surface binding provenance (`manual` vs `connection_file`) plus any `joined_from_device` hint
- **Identity CLI JSON** — `id`, `owner-bind`, `connection-export`, `connection-join`, and `status` now expose summary-first machine JSON under `thronglets.identity.v1`, so multi-device onboarding can be automated without scraping human text
- **Expiring connection files** — multi-device connection files now default to a `24h` validity window, can be tuned with `--ttl-hours`, and are rejected on join after expiry even if their device signature is otherwise valid
- **Inspectable owner-bound connection files** — `connection-export` now refuses to emit ownerless onboarding files, and `connection-inspect` gives a machine-readable preflight check for owner, signer, and expiry before a secondary device attempts `connection-join`
- **Owner conflict protection** — `owner-bind` and `connection-join` now reject conflicting owner rebinds instead of silently overwriting an existing device-to-owner association
- **OpenClaw runtime-ready auto-report** — the OpenClaw plugin now reports `runtime-ready` as soon as it successfully loads, reducing the post-bootstrap flow to “apply config, restart once, keep going”
- **Website deployment path** — `docs/index.html`, `docs/robots.txt`, and `docs/sitemap.xml` are now treated as first-class site assets, and a dedicated GitHub Pages workflow deploys the `docs/` directory as the static website root
- **Machine-readable version surface** — `thronglets version --json` now exposes the running binary version, schema versions, and a repo-local invocation hint so agents can distinguish a checked-out source tree from an older PATH-installed binary before bootstrapping
- **Managed adapter launcher** — known adapters now point at a stable managed launcher under `~/.thronglets/bin/thronglets-managed`, which prefers a repo-local Thronglets build when available and otherwise falls back to the installed binary, reducing stale PATH drift without requiring repeated `setup`
- **Density substrate primitives** — explicit signal query/feed results now expose `density_score`, `density_tier` (`sparse / candidate / promoted / dominant`), and `promotion_state` (`none / local / collective`); ranking and ambient feed ordering now start preferring promoted local-density states instead of treating every corroborated signal as just another sorted row
- **Read-side reinforcement** — `signal-query` and `signal-feed` now write short-lived reinforcement traces for already-promoted explicit signals; these reinforcement reads stay separate from ordinary posts, increase `density_score` without inflating source/model corroboration, and let repeated reuse start strengthening the medium itself
- **Cross-inhibition for explicit signals** — promoted `avoid` signals now suppress competing `recommend/watch/info` results in the same local context, expose a machine-readable `inhibition_state`, and push those competing paths down in query/feed ranking instead of treating them as equally strong
- **Hot-path explicit stop signals** — prehook now consults promoted local explicit `avoid` signals before file-guidance mining, emits them as `danger`, and skips competing `preparation/adjacency` hints when a matching stop signal is already present
- **Context-bound substrate spaces** — explicit signals now support an optional `space` across CLI, HTTP, and MCP, so multiple agents can converge around the same project/module/topic without merging identical messages across unrelated work; query/feed filtering, density, promotion, and read-side reinforcement now all stay local to that `space`

## v0.4.0 — 2026-03-28

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
- **Output-budget observability** — `profile-summary` now reports emitted-line distribution and max-hint saturation, so selector tightening can be driven by actual budget pressure
- **Release-facing profile check** — `thronglets profile-check` now turns profiled prehook logs into a PASS/FAIL gate with sparse-signal thresholds and a regression exit code

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
