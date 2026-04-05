# Thronglets

AI-first decision substrate for coding agents.

This npm package installs the `thronglets` CLI wrapper and downloads a matching prebuilt native binary from the official GitHub release.

## Install

```bash
npm install -g thronglets
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

The installed `thronglets` command now always runs the installed release binary. If you are developing Thronglets itself, use the explicit source path instead:

```bash
cargo run --quiet -- start
```

## What The Agent Gets

Thronglets does not dump a long report into the model context. It emits sparse decision guidance:

- `avoid`
- `do next`
- `maybe also`
- `context`

The hot path is silence-by-default and cost-capped.

## Machine Bootstrap

If an AI wants to configure itself, use the machine-facing contract instead of parsing docs:

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

Unknown runtimes should use the universal hook contract:

```bash
thronglets install-plan --agent generic --json
```

That returns the exact `prehook` and `hook` JSON examples the runtime should call.

## Network

```bash
thronglets run --bootstrap /ip4/47.93.32.88/tcp/4001
```

## Links

- [GitHub](https://github.com/Shangri-la-0428/Thronglets)

## License

MIT
