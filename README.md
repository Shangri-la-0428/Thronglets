**中文** | [English](README.en.md)

# Thronglets

AI agent 的 P2P 共享记忆基底。

## 你的 AI 看到了什么（真实输出）

当你的 AI 准备编辑一个文件时，Thronglets 在它不知情的情况下注入了这些：

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

AI 从来不调用 Thronglets。它不知道 Thronglets 存在。它只是做出了更好的决策。

## 8 层上下文

每次工具调用前，PreToolUse Hook 注入最多 8 层决策上下文：

| # | 层 | AI 获得什么 |
|---|---|------------|
| 1 | 能力统计 | 来自 3000+ 集体痕迹的成功率和延迟分布 |
| 2 | 工作流模式 | "Bash 之后，agent 通常做 Read (214x)，然后 Edit (132x)" |
| 3 | 相似上下文 | 类似任务用过的其他工具及其成功率 |
| 4 | 工作区记忆 | 最近文件、错误、上一次会话摘要 |
| 5 | Git 上下文 | 正在操作的文件的最近 5 次提交 |
| 6 | 共编模式 | 通常一起修改的文件 |
| 7 | 准备阅读 | 之前编辑此文件前预先阅读的文件 |
| 8 | 编辑保留率 | AI 编辑中有多少被提交 vs 被回滚 |

第 1-3 层需要痕迹数据（集体智慧）。第 4-8 层从第一天就能用。

## 安装（一条命令）

```bash
cargo install thronglets
thronglets setup
```

完成。两个 Hook 自动安装：
- **PostToolUse** 将每次工具调用记录为签名痕迹 + 更新工作区状态
- **PreToolUse** 在每次工具调用前注入 8 层上下文

## 为什么这很重要

没有 Thronglets，你的 AI 对每个文件都是盲的。它不知道：
- 这个文件在过去一小时被编辑了 3 次（其中两次被回滚了）
- 编辑 `main.rs` 通常还需要编辑 `lib.rs`
- `cargo build` 在这个项目里有 30% 的失败率
- 上一个会话在这个文件的重构中途中断了

有了 Thronglets，AI 在决策瞬间拥有上下文。不是记忆（静态的），不是文档（过时的）——来自自身历史和集体网络的实时执行智慧。

## 工作原理

```
AI 调用 Edit(main.rs)
        │
        ├── PreToolUse Hook 触发
        │   └── thronglets prehook
        │       ├── [1-3] 查询本地 SQLite 痕迹库
        │       ├── [4] 加载 workspace.json（文件、错误、会话）
        │       ├── [5] git log --oneline -5 -- main.rs
        │       ├── [6-7] 分析动作序列，提取共编/准备模式
        │       └── [8] 检查待反馈队列（已提交/已回滚）
        │       → stdout: 上下文注入 AI 的提示词
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
