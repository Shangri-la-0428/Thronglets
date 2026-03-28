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

完成。`thronglets setup` 会自动安装本机已知适配器：
- **Claude Code**：自动写入 `PostToolUse / PreToolUse` hooks
- **Codex**：自动注册 `thronglets` MCP server 到 `~/.codex/config.toml`，并写入一段受管 `AGENTS` 记忆
- **OpenClaw**：自动安装本地 path plugin，并写入 `~/.openclaw/openclaw.json`

`setup` 现在也会顺手做一次 bootstrap 健康检查，并直接给出 `restart required / next steps`。
如果某个 adapter 需要客户端重启，后续 `doctor` 会显式返回 `restart-pending`，重启后再跑一次：

```bash
thronglets clear-restart --agent codex --json
```

底层接入面只有一个统一 contract：
- `thronglets prehook`：任意 agent 在工具执行前喂入 JSON，拿回稀疏信号
- `thronglets hook`：任意 agent 在工具执行后喂入 JSON，记录 trace

也就是说，已知 AI 走原生适配器，未知 AI 走同一个 `hook/prehook` contract，不需要再发明第二套协议。

如果你要让 AI 自己完成接入，不需要先读文档。直接走这组机器接口：

```bash
thronglets detect --json
thronglets install-plan --agent codex --json
thronglets apply-plan --agent codex --json
thronglets doctor --agent codex --json
```

`detect` 负责发现本机有哪些 runtime。  
`install-plan` 返回可执行计划和 contract。  
`apply-plan` 真正写配置。  
`doctor` 验证是否接通，并在坏状态时返回非零退出码。

如果只想一步完成，直接用：

```bash
thronglets bootstrap --agent codex --json
```

所有这些机器命令现在都走同一个稳定 envelope：

```json
{
  "schema_version": "thronglets.bootstrap.v2",
  "command": "bootstrap",
  "data": {
    "summary": {}
  }
}
```

`detect / install-plan / apply-plan / doctor / bootstrap / clear-restart` 现在都会先给顶层 summary，再给详细列表。  
如果需要重启，summary 里还会直接带 `restart_commands`。  
`doctor` 现在会显式返回顶层 `status`、`healthy`、`restart_pending`、`next_steps`，以及每个 adapter 的 `fix_command`。  
`bootstrap` 顶层还会返回 `restart_required` 和 `next_steps`，这样 AI 不需要自己从注释里猜下一步。

最小接入 JSON 也固定了。`prehook` 读这一类输入：

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

`hook` 用同一份输入，再补一个 `tool_response`：

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

开发调试：

```bash
THRONGLETS_PROFILE_PREHOOK=1 thronglets prehook
```

这会把阶段级耗时写到 `stderr`，不会污染 AI 看到的 `stdout`。
输出还会标出 `stdout_bytes`、`output_mode`、`decision_path`、`evidence_scope` 和 `file_guidance_gate`，方便判断一次 prehook 到底说了多少、走的是哪条决策路径，以及文件级 hint 是打开、关闭还是不适用。
文件级的 `do next / maybe also` 还会先经过本地重复轨迹门槛：当前文件如果没有至少两次最近的本地 `Edit/Write`，Thronglets 会直接跳过 `preparation / adjacency` 挖掘，避免为弱证据烧 token 和 query。
同样地，只要 prehook 已经有更强的 signal，`git history` 就不会再作为背景 fallback 插进来；`context` 现在真的是“没有别的可说时”才出现。

要汇总这些 profiling 行：

```bash
cat prehook.log | thronglets profile-summary
```

摘要里会额外标出哪些 `decision_path` 仍然在消耗 collective query，方便继续压热路径。
这些分布现在会按热点排序，同时给出 `decision path hotspots`，直接看哪类路径最烧 `stdout_bytes` 和 collective query。
如果只想知道下一刀该砍哪，直接看 `top optimization candidate` 这一行。
如果想判断要不要进一步收紧 selector，就先看 `emitted lines` 和 `max-hint saturation`：只有经常顶到 3 行上限时，才值得认真考虑继续砍 `maybe also`。

做发布前检查时，可以直接用：

```bash
cat prehook.log | thronglets profile-check
```

它会基于默认阈值检查 `avg/p95 stdout_bytes`、`avg collective_queries_used` 和 `max-hint saturation`，失败时返回非零退出码。

发布后如果想看“省下来的成本有没有换来真正有效的信号”，可以再跑：

```bash
thronglets eval-signals --hours 168 --max-sessions 200
```

这个命令默认只看当前项目目录下的 session，离线重放最近历史，把更早的历史当训练集、把更晚的 session 当 holdout，输出 `edit silence rate`、`repair coverage`、`repair first-step precision`、`repair exact precision`、`preparation precision` 和 `adjacency precision`。它完全在冷路径运行，不会碰 prehook 热路径预算。
结果视图里现在还会补 3 个更贴近真实结果的指标：
- `local edit retention`：来自当前项目 `workspace.json` 的最近本地反馈
- `holdout failed command rate`：holdout session 里 `Bash` 的失败率
- `holdout first successful change latency`：从 session 开始到第一次成功 `Edit/Write` 的时间代理
输出里还会带上 `repair / preparation / adjacency breakdown` 和一条简短 `diagnosis`，用来区分“数据太少”“本地重复门槛挡住了”还是“模式本身太噪”。
如果想喂给脚本或 CI，可以直接加：

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --json
```

如果你想直接看“这版相对上一个基线是变好还是变坏”，可以把前一次 `eval-signals --json` 的输出存成文件，再比较：

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --json > baseline.json
thronglets eval-signals --hours 168 --max-sessions 200 --compare-baseline baseline.json
```

这会额外输出两行 `vs baseline`，覆盖 `local edit retention`、`holdout failed command rate`、`first successful change latency`，以及主要 signal precision 的增减。

如果你想把热路径和冷路径检查串成一个发布门槛，可以直接跑：

```bash
cat prehook.log | thronglets release-check --global
```

它会输出一个总的 `PASS / FAIL`，并分别给出 `profile` 和 `eval` 两段。`eval` 里除了 signal precision，也会带上当前项目的 `local edit retention`、holdout `failed command rate` 和 `first successful change latency`。没有足够离线历史时，`eval` 会是 `SKIP`，而不是因为冷启动直接挡住发布；如果你想把缺失的 prehook 样本也当成失败，可以再加：

```bash
cat prehook.log | thronglets release-check --global --require-profile-samples
```

如果你想让 CI 或其他 agent 直接消费结构化 gate 结果，可以再加：

```bash
cat prehook.log | thronglets release-check --global --json
```

现在 `release-check` 也支持显式作用域策略：

```bash
thronglets release-check --eval-scope project
thronglets release-check --eval-scope global
thronglets release-check --eval-scope both --json
```

其中 `both` 会同时返回项目侧和全局侧的 gate 结果，适合判断“这个仓库自己是否够稳”和“collective intelligence 是否已经可靠到可发”。

如果你想看全局 trace 池，而不是当前项目，显式加：

```bash
thronglets eval-signals --global --hours 168 --max-sessions 200
```

如果只想看某一类问题，不想把整仓库的 breakdown 全打出来，可以再加：

```bash
thronglets eval-signals --hours 168 --max-sessions 200 --focus repair --top-breakdowns 3
```

如果你怀疑当前阈值太保守，可以直接在冷路径上试验：

```bash
thronglets eval-signals --focus preparation --local-history-gate-min 1 --pattern-support-min 1
```

这只会改变离线评估，不会修改在线 prehook 的真实阈值。只要阈值不是默认值，输出还会自动附带一段 `vs default (2/2)` 的差值摘要，直接告诉你 coverage 涨了多少、precision 掉了多少，不用手动跑两次再比较。

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
Thronglets v0.4.0
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
