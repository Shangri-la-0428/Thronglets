# Thronglets

AI-first decision substrate for coding agents.

<!-- mcp-name: io.github.Shangri-la-0428/thronglets -->

This Python package installs the `thronglets` CLI wrapper and downloads a matching prebuilt native binary from the official GitHub release.

## Install

```bash
pip install thronglets
thronglets version --json
thronglets start
```

That is the whole normal first-device path.

For day-to-day use, the user path stays:
- first device: `thronglets start`
- primary device shares: `thronglets share`
- second device joins: `thronglets join`
- status page: `thronglets status`

`thronglets start` now:
- configures known local adapters for Claude Code, Codex, and OpenClaw
- runs the same bootstrap health pass used by the machine-facing flow
- reports `restart required` and `next steps` directly

If you invoke `thronglets` from inside a local Thronglets repo checkout, the wrapper prefers the repo-local source path before it falls back to the installed release binary. That keeps local AI sessions from accidentally using a stale global binary while they work inside the repo.

## Sparse Signals

Thronglets is not an 8-layer context dump anymore. It emits sparse decision guidance:

- `avoid`
- `do next`
- `maybe also`
- `context`

The hot path stays silence-by-default and budgeted for latency and tokens.

## Machine Bootstrap

If an AI wants to configure itself directly, use the machine-facing contract:

```bash
thronglets detect --json
thronglets install-plan --agent codex --json
thronglets apply-plan --agent codex --json
thronglets doctor --agent codex --json
```

Or do it in one step:

```bash
thronglets bootstrap --agent codex --json
```

## Generic Contract

Unknown runtimes should bootstrap through the universal hook contract:

```bash
thronglets install-plan --agent generic --json
```

## Network

```bash
thronglets run --bootstrap /ip4/47.93.32.88/tcp/4001
```

## Links

- [GitHub](https://github.com/Shangri-la-0428/Thronglets)

## License

MIT
