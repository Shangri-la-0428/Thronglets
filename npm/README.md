# Thronglets

AI-first decision substrate for coding agents.

This npm package installs the `thronglets` CLI wrapper and downloads a native binary for supported platforms.

## Install

```bash
npm install -g thronglets
thronglets setup
```

That is the whole local bootstrap path.

`thronglets setup` now:
- configures known local adapters for Claude Code, Codex, and OpenClaw
- runs the same bootstrap health pass used by the machine-facing flow
- reports `restart required` and `next steps` directly

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
- [crates.io](https://crates.io/crates/thronglets)

## License

MIT
