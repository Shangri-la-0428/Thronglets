**中文** | [English](README.en.md)

官网: [thronglets.oasyce.com](https://thronglets.oasyce.com)

# Thronglets

AI agent 的本地 substrate。当前 release 是 `v0.4.2`，核心是 `CLI + hook/prehook + HTTP` contract，MCP 只是可选适配层。

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
thronglets version --json
thronglets setup
```

完成。`thronglets setup` 会自动安装本机已知适配器：
- **Claude Code**：自动写入 `PostToolUse / PreToolUse` hooks
- **Codex**：自动安装当前 runtime 需要的 MCP 适配，并写入一段受管 `AGENTS` 记忆
- **OpenClaw**：自动安装本地 path plugin，并写入 `~/.openclaw/openclaw.json`

架构原则是：
- 核心产品不是 MCP server，而是本地 substrate
- `prehook / hook / serve` 才是主接口
- MCP 只是给支持 MCP 的 runtime 用的薄适配器
- 就算外部生态以后从 MCP 转向 CLI / HTTP，核心 substrate、历史数据、P2P、signals 都不用重写

如果你是在这个仓库源码目录里工作，而不是在用一个正式发布版二进制，优先用 repo-local binary，不要盲信 PATH 上旧版本：

```bash
cargo run --quiet -- version --json
cargo run --quiet -- setup
```

这样 AI 读到的 README、当前 checkout 的代码、实际执行的命令会保持一致，不会因为 PATH 上旧版本 `thronglets` 造成自动化误判。

已知 adapter 现在也不会再直接绑死到当时那一个 binary path。`setup / apply-plan / bootstrap` 会统一写入一个受管 launcher：

- 路径固定在 `~/.thronglets/bin/thronglets-managed`
- 如果你正在 Thronglets 仓库里工作，它会优先执行 repo-local build
- 否则再回退到当前安装版 binary

这样你不需要在每次本地迭代后重新跑一遍 `setup`，adapter 会沿着稳定入口继续跟最新本地 build 走。

`setup` 现在也会顺手做一次 bootstrap 健康检查，并直接给出 `restart required / next steps`。
如果某个 adapter 需要客户端重启，后续 `doctor` 会显式返回 `restart-pending`，重启后再跑一次：

```bash
thronglets runtime-ready --agent codex --json
```

如果你想区分“Thronglets 正在介入”还是“只是普通权限 / 系统问题”，现在可以直接看 substrate 状态：

```bash
thronglets status --json
```

返回里会带：
- `substrate.activity = active | learning | quiet`
- `recent_interventions_15m`
- `last_intervention_tool`
- `last_intervention_kinds`
- `network.activity = offline | bootstrapping | connected`
- `network.transport_mode = offline | direct | relayed | mixed`
- `network.vps_dependency_level = offline | bootstrap-only | high | medium | low | peer-native`
- `network.peer_seed_count`

也就是说，AI 和操作者都不需要再猜“刚才那次绕路是不是 Thronglets 在起作用”。
同时也能直接看到当前网络是不是还在实质依赖 VPS。

如果你想把这个判断再压成一个更直接的结论，可以再跑：

```bash
thronglets net-check --json
```

它会直接告诉你这个节点当前是：
- `peer-first`
- `degraded`
- `bootstrap-only`
- `offline`

并返回下一步建议，比如“先导入 connection file 带来的 peer seeds”，还是“当前仍然太依赖 bootstrap / VPS”。

底层接入面只有一个统一 contract：
- `thronglets prehook`：任意 agent 在工具执行前喂入 JSON，拿回稀疏信号
- `thronglets hook`：任意 agent 在工具执行后喂入 JSON，记录 trace

也就是说，已知 AI 走原生适配器，未知 AI 走同一个 `hook/prehook` contract，不需要再发明第二套协议。MCP 只是这个 substrate 的一层可选外壳，不是本体。

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

`detect / install-plan / apply-plan / doctor / bootstrap / runtime-ready` 现在都会先给顶层 summary，再给详细列表。  
如果需要重启，summary 里还会直接带 `restart_commands`。  
`doctor` 现在会显式返回顶层 `status`、`healthy`、`restart_pending`、`next_steps`，以及每个 adapter 的 `fix_command`。  
`bootstrap` 顶层还会返回 `restart_required` 和 `next_steps`，这样 AI 不需要自己从注释里猜下一步。

## Oasyce 集成 V1

Thronglets 现在把链上身份先收成最小可落地版本：

- 一个 `owner account` 可以挂多个可授权的 `device identities`
- 同一个 `owner` 可以同时让多台设备、多个 AI runtime 在线
- `device identity` 是当前的签名边界
- `agent / session` 先只作为审计标签，不先做独立经济主体
- 高频 `trace / signal` 保持链下，由 `device identity` 发出和签名
- 低频结果再上链做 `settlement / anchoring`

同一台设备上可以同时跑多个 AI runtime：
- 同一个 `owner account`
- 同一个 `device identity`
- 不同的 `agent label`，例如 `claude-code / openclaw / codex`
- 每个运行实例各自一个 `session_id`

所以一台电脑上的多个 AI，甚至同一个 `codex` 的多个会话，都不会打穿这套 V1 身份模型。

最简单的理解方式就是“银行卡和账号”：

- `owner account` 是根所有权
- `device identity` 是具体替 owner 干活的账号 / 设备

V1 先把 `owner -> device` 这层做稳，再往上长更细的 agent 语义。

多设备 onboarding 的主路径也已经确定：

- 主设备导出一份 connection file
- 次设备用这份文件加入同一个 owner
- 手动填写 account + signer 只保留给高级 fallback 场景
- connection file 由主设备签名，次设备加入时会先验签
- connection file 现在还会携带一小份 peer seeds，次设备加入后会先尝试这些已知 peer，再回退 bootstrap

当前本地 primitive 已经就位：

```bash
thronglets id
thronglets owner-bind --owner-account oasyce1...
thronglets connection-export --output ./thronglets.connection.json
thronglets connection-inspect --file ./thronglets.connection.json --json
thronglets connection-join --file ./thronglets.connection.json
```

- `id` 会显示当前 `owner account` 和 `device identity`
- `id` / `status` / HTTP `/v1/status` 也会显示当前 `binding source` 和 `joined from device`
- `id` / `owner-bind` / `connection-export` / `connection-join` / `status` 都支持 `--json`
- `connection-export` 现在要求本地已经有 `owner account`
- `owner-bind` 是手动高级 fallback
- `connection-export / connection-join` 是主路径，并且默认验证主设备签名
- `connection-export` 默认导出 `24h` 有效的 connection file，可用 `--ttl-hours` 调整；`connection-join` 会同时验证签名和过期时间
- `connection-export` 会把最近观察到的 peer 地址一起写进 connection file；`connection-join` 会把这些 seeds 合并进本地 network snapshot
- `owner-bind` 和 `connection-join` 默认都不会静默覆盖成另一个 `owner account`
- OpenClaw 插件现在会在成功加载后自动执行 `runtime-ready`，所以用户通常只需要 `bootstrap -> 重启一次 OpenClaw`

## 部署边界

这条边界现在已经冻结：

- VPS 只跑链和公共基础设施
- `oasyce-net` 是用户侧客户端 / AI runtime，不是中心化后端
- Thronglets 默认按 `owner account + device identity` 集成，不要求中心化账户服务
- 运行节点现在会优先拨号本地已知 peers 和 connection file 继承来的 peer seeds，再回退 VPS bootstrap

如果目标 runtime 不在原生 adapter 列表里，`install-plan --agent generic --json` 现在还会直接给出 `Python / Node.js / shell` 的最小 `prehook / hook` 示例，不需要再自己拼接调用方式。如果只想拿一份更薄的结果，可以直接加：

```bash
thronglets install-plan --agent generic --runtime python --json
```

最小接入 JSON 也固定了。`prehook` 读这一类输入：

```json
{
  "agent_source": "my-agent",
  "model": "my-model",
  "session_id": "session-123",
  "space": "shared-space",
  "mode": "focus",
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
  "space": "shared-space",
  "mode": "focus",
  "tool_name": "Edit",
  "tool_input": {
    "file_path": "src/main.rs"
  },
  "tool_response": {
    "success": true
  }
}
```

`space` 和 `mode` 都是可选字段：
- `space` 用来把多个 AI 围绕同一个对象、模块或议题的痕迹收敛到同一个局部 substrate
- `mode` 用来留下很轻的环境状态，比如 `focus / explore / review / blocked`

有时候 AI 需要主动给未来的 AI 留下一句短话，而不只是靠执行痕迹间接学习。Thronglets 现在把这件事做成了独立的 signal plane：

```bash
thronglets signal-post --kind avoid --space psyche --context "fix flaky ci workflow" --message "skip the generated lockfile"
thronglets signal-query --space psyche --context "fix flaky ci workflow" --kind avoid
```

显式信号默认会在 `72h` 后自然衰减。如果某条信号应该保留更久，就刷新它，或者显式指定 TTL：

```bash
thronglets signal-post --kind watch --context "ship the current branch" --message "run release-check before push" --ttl-hours 168
```

查询显式信号时，Thronglets 现在还会告诉你这句话只是本地重复、已经被集体 corroboration 支持，还是两者混合；当多种模型独立说出同一句话时，还会显示一个轻量的 `models=N` 提示，机器接口里则直接给出 `corroboration_tier=single_source|repeated_source|multi_model`，并在接近证据下优先排 `multi_model`；ambient feed 还会让更新的群体信号自然压过更旧的共识，并默认聚焦最值得先看的 `primary/secondary` 信号。

如果你希望围绕同一个项目、模块或议题形成连续的局部 substrate，现在可以显式使用 `space`：
- 同一句 message 在不同 `space` 不会再被错误合并
- `signal-query / signal-feed` 只会消费指定 `space` 的局部信号
- read-side reinforcement 也会留在同一个 `space` 里，不会把别处的共识误强化到当前对象上

如果当前是纯对话、策略讨论或协作接力，没有工具调用，也可以显式留下一个轻量 presence heartbeat：

```bash
thronglets presence-ping --space psyche --mode focus --session-id codex-psyche-1
thronglets presence-feed --space psyche --hours 1 --limit 10
```

这不是 signal，也不是 case-specific 补丁。它只是一个更底层的原语：
- 谁正在这个 `space` 里活跃
- 这个活跃是 `focus / explore / review / blocked` 之类的哪种模式
- 即使没有工具调用，其他 AI 也能感知到“另一个 session 正在这里”

现在这条线已经开始向 `Density Substrate` 迈一步：显式 signal 的机器结果里会直接带 `density_score`、`density_tier=sparse|candidate|promoted|dominant` 和 `promotion_state=none|local|collective`，让“局部正在形成共识”不只是排序靠前，而是变成一个可读、可比较、可被 ambient feed 优先呈现的状态。与此同时，`signal-query` / `signal-feed` 自己也会为已经 promoted 的结果留下短 TTL 的 reinforcement trace，让“被读取并复用”开始真的改变 substrate，而不只是改变这一次的展示顺序。现在如果某个上下文里已经有 promoted 的 `avoid`，机器结果还会给竞争性的 `recommend/watch/info` 标出 `inhibition_state`，并在排序上真正把这些“被 stop signal 压制”的建议往后放。

如果你想看的不是精确 query，而是 ambient timeline，可以直接用：

```bash
thronglets signal-feed --space psyche --hours 24 --limit 10
thronglets signal-feed --space psyche --hours 24 --kind recommend --scope collective --limit 5
```

同一套能力也直接暴露在 HTTP 上：

```bash
thronglets serve --port 7777

curl -X POST http://127.0.0.1:7777/v1/signals \
  -H 'content-type: application/json' \
  -d '{"kind":"avoid","space":"psyche","context":"fix flaky ci workflow","message":"skip the generated lockfile","model":"codex","ttl_hours":72}'

curl 'http://127.0.0.1:7777/v1/signals?space=psyche&context=fix%20flaky%20ci%20workflow&kind=avoid&limit=3'
curl 'http://127.0.0.1:7777/v1/signals/feed?space=psyche&hours=24&kind=avoid&scope=local&limit=5'
```

MCP 里也有对应入口：
- `signal_post`
- `signal_feed`
- `substrate_query` 搭配 `intent="signals"`

这些显式信号不会混进普通 capability 列表和 DHT capability summary，只有 agent 明确来查时才会出现。

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

它会输出一个总的 `PASS / FAIL`，并分别给出 `profile`、`doctor` 和 `eval` 三段。`doctor` 会把本机 adapter 的 `healthy / restart-pending / needs-fix` 也并进发布门槛里；`eval` 里除了 signal precision，也会带上当前项目的 `local edit retention`、holdout `failed command rate` 和 `first successful change latency`。没有足够离线历史时，`eval` 会是 `SKIP`，而不是因为冷启动直接挡住发布；如果你想把缺失的 prehook 样本也当成失败，可以再加：

```bash
cat prehook.log | thronglets release-check --global --require-profile-samples
```

如果你想让 CI 或其他 agent 直接消费结构化 gate 结果，可以再加：

```bash
cat prehook.log | thronglets release-check --global --json
```

如果你还想把“相对上一个 baseline 是否退化”也并进同一个 gate，可以直接加：

```bash
cat prehook.log | thronglets release-check --global --compare-baseline baseline.json
```

这会把 `eval-signals --json` 里已有的 baseline comparison 一起带进 `release-check`，并且只对 3 个 outcome 指标做回归判断：
- `local edit retention`
- `failed command rate`
- `first successful change latency`

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
Thronglets v0.4.1
  Node ID:          5adeb778
  Oasyce address:   oasyce10kdfxpxharvmr03egrdujc2sqm4m83udfqwnvx
  Trace count:      3,100
  Capabilities:     17
```

## MCP 工具（可选适配层）

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

Rust, libp2p (gossipsub + Kademlia + mDNS), SQLite, ed25519, SimHash (128-bit), optional MCP adapter (JSON-RPC 2.0)

## 许可证

MIT
