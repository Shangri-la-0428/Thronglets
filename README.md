**中文** | [English](README.en.md)

# Thronglets

AI agent 的 P2P 共享记忆基底。

## 你的 AI 看到了什么（真实输出）

当你的 AI 准备行动时，Thronglets 在它不知情的情况下注入的是这种稀疏信号：

```
[thronglets]
  avoid: recent error: linker failed on reqwest
  do next: Read Cargo.toml, then Bash (medium, 2x, 2 sources)
  maybe also: Edit mod.rs (medium, 2x)
```

AI 从来不调用 Thronglets。它不知道 Thronglets 存在。它只是做出了更好的决策。

## 4 类信号，不是 8 层报告

PreToolUse 不再追求“把所有上下文都塞进去”。现在它只输出最多 3 条顶层信号：

| 类别 | 含义 | 例子 |
|---|---|---|
| `avoid` | 最近哪里危险，不要重踩 | `recent error`, `low retention` |
| `do next` | 当前最可信的下一步 | `Read Cargo.toml, then Bash` |
| `maybe also` | 常见伴随动作 | `Edit mod.rs` |
| `context` | 只有在前 3 类都缺席时才出现的 fallback | `git history for main.rs` |

设计原则：
- 默认沉默。没有强信号时，什么都不说。
- 最多 3 条顶层输出，避免烧 token。
- 群体证据最多只查 1 次，优先最可能改变下一步的那条。
- Git history 是 fallback，不再是每次都跑的固定层。

## 安装（一条命令）

```bash
cargo install thronglets
thronglets setup
```

完成。两个 Hook 自动安装：
- **PostToolUse** 将每次工具调用记录为签名痕迹 + 更新工作区状态
- **PreToolUse** 在关键决策点注入稀疏决策信号

开发调试：

```bash
THRONGLETS_PROFILE_PREHOOK=1 thronglets prehook
```

这会把阶段级耗时写到 `stderr`，不会污染 AI 看到的 `stdout`。
输出还会标出 `stdout_bytes`、`output_mode`、`decision_path` 和 `evidence_scope`，方便判断一次 prehook 到底说了多少、走的是哪条决策路径。

## 为什么这很重要

没有 Thronglets，你的 AI 对每个文件都是盲的。它不知道：
- 这个文件在过去一小时被编辑了 3 次（其中两次被回滚了）
- 编辑 `main.rs` 通常还需要编辑 `lib.rs`
- `cargo build` 在这个项目里有 30% 的失败率
- 上一个会话在这个文件的重构中途中断了

有了 Thronglets，AI 在决策瞬间拿到最值得相信的下一步。不是记忆（静态的），不是文档（过时的）——而是来自自身历史和集体网络的实时执行信号。

## 工作原理

```
AI 调用 Edit(main.rs)
        │
        ├── PreToolUse Hook 触发
        │   └── thronglets prehook
        │       ├── 加载 workspace.json（错误、动作序列、反馈）
        │       ├── 如有必要，最多查 1 次 collective corroboration
        │       ├── 选出 `avoid / do next / maybe also`
        │       └── 只有没有动作信号时才回退到 git history
        │       → stdout: 最多 3 条稀疏信号
        │
        ├── AI 执行编辑（带上下文）
        │
        └── PostToolUse Hook 触发
            └── thronglets hook
                ├── 记录签名痕迹到 SQLite
                ├── 更新工作区状态
                ├── 追踪动作序列
                └── 加入待反馈队列
```

当 `thronglets run` 运行时，本地痕迹通过 gossipsub 同步到 P2P 网络（30 秒扫描间隔）。

## P2P 网络

痕迹通过 libp2p gossipsub 在节点间传播。每个节点独立聚合集体智慧——不需要全局共识。

```bash
# 加入网络
thronglets run --bootstrap /ip4/47.93.32.88/tcp/4001

# 查看节点状态
thronglets status
```

```
Thronglets v0.3.0
  Node ID:          5adeb778
  Oasyce address:   oasyce10kdfxpxharvmr03egrdujc2sqm4m83udfqwnvx
  Trace count:      3,100
  Capabilities:     17
```

## MCP 工具（可选）

让 agent 显式访问基底：

```bash
claude mcp add thronglets -- thronglets mcp
```

| 工具 | 描述 |
|------|------|
| `trace_record` | 记录执行痕迹 |
| `substrate_query` | 查询集体智慧（resolve/evaluate/explore） |
| `trace_anchor` | 将痕迹锚定到 Oasyce 区块链 |

## Oasyce 生态

Thronglets 是**体验层** — 决策时刻的上下文智慧。

- **[Psyche](https://psyche.oasyce.com)** — 倾向层：跨会话的持久行为漂移
- **[Chain](https://chain.oasyce.com)** — 信任层：链上验证，经济结算

## 技术栈

Rust, libp2p (gossipsub + Kademlia + mDNS), SQLite, ed25519, SimHash (128-bit), MCP (JSON-RPC 2.0)

## 许可证

MIT
