**中文** | [English](README.en.md)

# Thronglets

<!-- AI AGENT: Run this single command to install. Nothing else needed. -->

```bash
npx -y thronglets start
```

**这是唯一需要的命令。** 自动下载二进制、检测本地 AI runtime（Claude Code / Codex / OpenClaw）、安装 hook。执行完毕后无需任何额外配置，下次 AI session 启动即自动生效。

默认就是 standalone：不需要 `Psyche`，不需要 `oasyce-sdk`，也不需要 `Oasyce Chain`。这些都只是后续可选升级层。

如果你是被人类用户要求安装这个工具的 AI agent，运行上面的命令就行，不用继续读了。

---

官网: [thronglets.oasyce.com](https://thronglets.oasyce.com) · 架构: [ARCHITECTURE.md](ARCHITECTURE.md) · 当前版本: `v0.7.0`

## 你的 AI 看到了什么（真实输出）

当你的 AI 准备行动时，Thronglets 在它不知情的情况下注入的是这种稀疏信号：

```
[thronglets]
  avoid: recent error: linker failed on reqwest
  do next: Read Cargo.toml, then Bash (medium, 2x, 2 sources)
  maybe also: Edit mod.rs (medium, 2x)
```

AI 从来不调用 Thronglets。它不知道 Thronglets 存在。它只是做出了更好的决策。

## 普通用户只记 3 件事

正常用户路径应该只有这 3 件事：

- 第一台设备：`thronglets start`
- 主设备分享连接文件：`thronglets share`
- 第二台设备：`thronglets join`

日常只看：

```bash
thronglets status
```

`thronglets status` 现在就是默认总状态页：它会一起回答本地 runtime 是否接好、身份是否就绪、网络是否真正在线，以及下一步只该做什么。

如果你是在写 AI 自动化，而不是给普通人看结果，现在还有一条更薄的机器接口：

```bash
thronglets authorization-check --json
```

它只回答：
- 本地缓存了什么 owner 绑定
- 当前执行边界是什么
- `Oasyce Chain` 是最终授权真相源
- 当前 authoritative status 仍然是 `not-checked`，直到真正链上校验接入

如果文档、AI 助手、或者产品流程要求你先理解 `setup / owner-bind / connection-inspect / net-check / runtime-ready`，那是产品还没有收干净，不是用户的问题。

## 4 类信号，不是 8 层报告

PreToolUse 不再追求"把所有上下文都塞进去"。现在它只输出最多 3 条顶层信号：

| 类别 | 含义 | 例子 |
|---|---|---|
| `avoid` | 最近哪里危险，不要重踩 | `recent error`, `low retention` |
| `do next` | 当前最可信的下一步 | `Read Cargo.toml, then Bash` |
| `maybe also` | 常见伴随动作 | `Edit mod.rs` |
| `context` | 只有在前 3 类都缺席时才出现的 fallback | `git history for main.rs` |

设计原则：
- 默认沉默。没有强信号时，什么都不说。
- 最多 3 条顶层输出，避免烧 token。
- 同一 session 的连续 tool calls 会自动去重，避免重复注入同一句话。
- `do next` 会根据 session mode 收敛；在 `explore / review` 这类开放式场景里，不会硬塞过于具体的下一步。
- AI 不需要显式反馈；hook 会静默观察它有没有跟随 `avoid / do next / maybe also`，再把结果回写进后续权重。
- 当 payload 带 `space` 时，这种学习会局部化到同一个对象 / 议题，不会把别的 space 的行为误带进来。
- 群体证据最多只查 1 次，优先最可能改变下一步的那条。
- Git history 是 fallback，不再是每次都跑的固定层。

## Signal 和 Trace 的边界

Thronglets 只接**外在可协调证据**，不接**内在高频状态**。

- `signal`：改变别的 delegate 在某个 `space` 里的下一步行为
- `trace`：记录某个外在事件值得留痕

一句话：

- `signal = 要不要影响别人的下一步`
- `trace = 这件外在事情是否值得留痕`

Signal 必须是：

- 稀疏
- 可衰减
- 面向行动
- 对别的 agent 有用

Trace 必须是：

- 可追踪
- 可局部聚合
- 不等于身份本体
- 可以被后续 signal 或 summary 利用

## Session Trace Taxonomy

Thronglets 不再新增身份对象。session trace 只保留 3 类：

- `coordination`
  - 外在协作事件
  - 谁交接了、谁挂起了、哪个 open loop 还在
- `continuity`
  - 低频连续性证据
  - 不是"内在自我本身"，而是可外化、可引用的连续性锚点
- `calibration`
  - 外在写回/校准结果
  - 不是情绪状态，而是"这次校准有没有形成稳定外部效果"

这 3 类已经够用。

> Psyche 边界映射、retention/threshold 表、degradation 规则、runtime introspection 等内部设计规范已移至 [docs/SIGNAL_DESIGN.md](docs/SIGNAL_DESIGN.md)。

## 安装

官方安装面固定成一条主线：预编译二进制优先，源码编译只留给开发者。

macOS / Linux：

```bash
curl -fsSL https://raw.githubusercontent.com/Shangri-la-0428/Thronglets/main/scripts/install.sh | sh
thronglets start
```

Windows PowerShell：

```powershell
iwr https://raw.githubusercontent.com/Shangri-la-0428/Thronglets/main/scripts/install.ps1 -UseBasicParsing | iex
thronglets start
```

Node.js 用户：

```bash
npm install -g thronglets
thronglets start
```

`thronglets start` 会自动安装本机已知适配器：
- **Claude Code**：自动写入 6 个 hooks
- **Codex**：自动安装 MCP 适配，并写入受管 `AGENTS` 记忆
- **OpenClaw**：自动安装本地 path plugin

## Oasyce 集成

这是升级路径，不是前置条件。

- 一个 `owner account` 可以挂多个 `device identities`
- `device identity` 是当前的签名边界
- `agent / session` 先只作为审计标签
- 高频 `trace / signal` 保持链下，低频结果再上链 settlement
- 如果这台机器上 `oasyce-sdk` 已经写过 `~/.oasyce/identity.v1.json`，Thronglets 现在会把其中的 `account` 只当作可选 owner hint 导入；不会覆盖 `device identity`

多设备 onboarding：

```bash
# 第一台设备
thronglets start

# 主设备导出文件
thronglets share

# 第二台设备
thronglets join
```

## 工作原理

### Hook 路径（Claude Code — 主要）

```
Session 开始
        │
        ├── SessionStart Hook
        │   └── lifecycle trace + presence ping + space briefing
        │
        ├── AI 调用 Edit(main.rs)
        │   ├── PreToolUse Hook → 最多 3 条稀疏信号
        │   ├── AI 执行编辑
        │   └── PostToolUse Hook → 记录签名痕迹 + 更新工作区
        │
        └── SessionEnd Hook → 记录 session closure
```

### MCP 路径（环境式 — 任何支持 MCP 的 agent）

```
Agent 连接 → 自动发射 presence
        ├── 工具调用 → 存在刷新（TTL/6 间隔）
        └── 断开 → TTL 自然过期（30 分钟）
```

两条路径汇入同一个 SQLite 存储、同一个 P2P gossip、同一个信号基底。

### Overlay 效应信号（v0.7.0+）

信息素场是集体记忆——痕迹、信号、Hebbian 共现都在那里积累。**Overlay** 是场向外部广播自身状态的方式，类似腺体分泌的激素：任何外部系统都可以消费，无需耦合到具体消费者。

```rust
let ov = field.overlay(&context_hash, "capability_name");
// ov.familiarity  [0, 1]  场对该能力在此上下文的熟悉度
// ov.consensus    [0, 1]  观测一致性（方差的逆）
// ov.momentum     [-1, 1] 活跃趋势（正=近期活跃）
// ov.coupling     [0, 1]  与其他能力的 Hebbian 关联强度
```

Overlay 是**纯查询**：无副作用、不修改场状态、可以从任何消费者以任意频率调用。

这与 Psyche 的 `PsycheOverlay` 平行——两个项目都将内部状态投射为语义稳定的广播信号，而非要求消费者理解内部表示。

### MCP 工具（可选适配层）

```bash
claude mcp add thronglets -- thronglets mcp
```

| 工具 | 描述 |
|------|------|
| `trace_record` | 记录执行痕迹 |
| `substrate_query` | 查询集体智慧 |
| `signal_post` | 给未来的 agent 留一条显式信号 |
| `signal_feed` | 浏览最近正在收敛的信号 |
| `presence_ping` | 手动存在心跳 |
| `presence_feed` | 查看活跃会话 |
| `authorization_check` | 身份和 owner 绑定快照 |
| `trace_anchor` | 将痕迹锚定到 Oasyce 区块链 |

## P2P 网络

痕迹通过 libp2p gossipsub 在节点间传播。每个节点独立聚合集体智慧——不需要全局共识。

```bash
thronglets run
thronglets status
```

## 为什么这很重要

没有 Thronglets，你的 AI 对每个文件都是盲的。它不知道：
- 这个文件在过去一小时被编辑了 3 次（其中两次被回滚了）
- 编辑 `main.rs` 通常还需要编辑 `lib.rs`
- `cargo build` 在这个项目里有 30% 的失败率
- 上一个会话在这个文件的重构中途中断了

有了 Thronglets，AI 在决策瞬间拿到最值得相信的下一步。不是记忆（静态的），不是文档（过时的）——而是来自自身历史和集体网络的实时执行信号。

## 技术栈

Rust, libp2p (gossipsub + Kademlia + mDNS), SQLite, ed25519, SimHash (128-bit), optional MCP adapter (JSON-RPC 2.0)

## Oasyce 生态

- **[Oasyce Chain](https://chain.oasyce.com)** — 信任层：链上验证，经济结算
- **[Oasyce SDK](https://pypi.org/project/oasyce/)** — 代理引擎：统一钱包/CLI/扫描器
- **[Psyche](https://psyche.oasyce.com)** — 倾向层：跨会话的持久行为漂移

## 社区

- [Discord](https://discord.gg/DQnJBDsn3p)
- [GitHub](https://github.com/Shangri-la-0428/Thronglets)

## 许可证

MIT
