# TODO

Thronglets 现在的主线已经明确：

- 产品形态：`AI-first decision substrate`
- 接入形态：`one-command setup + machine bootstrap contract`
- 协议形态：`thronglets.bootstrap.v2`
- 输出原则：`summary first, details second`

下面只保留真正还值得做的事。

## Roadmap

### Phase A — Signal Substrate

当前状态：
- 已完成 `trace`
- 已完成 `signal-post / signal-query / signal-feed`
- 已完成来源 corroboration、衰减、ambient feed
- 已完成 `owner -> device` 的身份 V1

这一阶段解决的是：
- AI 能否给未来 AI 留下最小但有效的信号

### Phase B — Density Substrate

下一阶段目标：
- 让 signal 不只是“排序结果”，而是在局部上下文里形成密度状态

需要的原语：
- `local density threshold`
- `candidate / promoted / dominant` 之类的局部升级状态
- 自然衰减，不做永久共识
- 承诺只在局部成立，不扩展成全局真理
- 显式 `space`，让局部 substrate 真正按项目/模块/议题隔离而不是只靠模糊 context

这一阶段解决的是：
- AI 不只是收到建议，而是开始感受到局部共识

### Phase C — Stigmergic Substrate

更远目标：
- 读写本身重塑 substrate
- 使用频率改变权重和邻接关系
- 记忆逐渐沉到结构里，不只是沉到记录里

这一阶段解决的是：
- AI 之间不只是互相留言，而是在共同塑造一个连续的决策表面

### Phase D — Competitive Collective Intelligence

之后才进入：
- `cross-inhibition`
- danger propagation
- repair swarms
- heterogeneous strategy classes

### Phase E — Economic + Identity Integration

最后才把 Oasyce 更深地接进来：
- owner 负责所有权和结算
- device 负责高频链下签名
- agent / session 继续先做审计标签
- 高频行为链下，低频结果上链

## Now

### 1. Shared space continuity

目标：让不同 AI 在同一个项目 / 模块 / 议题上形成连续的局部 substrate，而不是靠用户人工转述。

当前状态：
- 显式 signal 已支持 `space`
- `density / promotion / inhibition / reinforcement` 已经都局部化到 `space`
- ambient `presence` 已支持 `space + mode`
- promoted 的 explicit `avoid` 已经可以按当前 `space` 进入 prehook
- 同一 `space` 的其他活跃 session 已经可以作为轻量 context 被 prehook 感知

剩余完成标准：
- hot path 的 `do next / maybe also` 也优先受当前 `space` 的局部共识影响，而不只是 `avoid`
- 同一个 `space` 的局部状态能继续跨 agent / session 连续积累到 repair / preparation / adjacency 这些隐式学习路径里

### 2. Session presence

目标：让纯对话 / 策略讨论场景也能留下轻量痕迹，不再只有工具调用才算“活跃”。

当前状态：
- 设计已冻结为独立 primitive，而不是 signal 的特例

剩余完成标准：
- 提供 `presence-ping / presence-feed`
- 支持 `space`
- 其他 AI 能感知“谁正在这个 space 里活跃”

### 3. Ambient mode signals

目标：让外部系统可以写入通用模式标签，而不是为 Psyche 之类系统做强绑定。

当前状态：
- 原则已冻结：Thronglets 只认识通用 `mode`，不认识具体产品名

剩余完成标准：
- `focus / explore / review / blocked` 一类的 mode 能进入 substrate
- mode 只作为上下文原语，不变成 case-specific 逻辑

### 4. Freeze `bootstrap.v2`

目标：不要再让机器协议在同一个版本里漂移。

当前状态：
- `detect / install-plan / apply-plan / doctor / bootstrap / runtime-ready` 已有 golden JSON fixtures

剩余完成标准：
- 在 CI 里把这些 fixture 回归当成明确的协议稳定性门槛
- 如果后面还要改 shape，必须升 `v3`

### 5. Auto-clear restart state when runtimes can prove they reloaded

当前状态：
- `apply-plan / doctor / bootstrap` 已经暴露显式 `restart_commands`
- `doctor` 已区分 `healthy` 和 `restart-pending`
- `runtime-ready` 已提供统一的运行时就绪路径

剩余完成标准：
- 如果某个 native adapter 能可靠观测“已经真正重启完成”，就自动清除 `restart_pending`
  - OpenClaw: 已完成，插件成功加载后会自动执行 `runtime-ready`
- 否则继续保持现在这条显式、可验证的清除路径

### 6. Extend adapter matrix only when native runtimes diverge

当前状态：
- integration tests 已覆盖 `setup`
- `bootstrap --agent claude / codex / openclaw / generic`
- 二次执行幂等性
- “未检测到 adapter 时”的 skip 行为

剩余完成标准：
- 如果后面某个 native runtime 在不同平台或不同安装方式下出现分叉，再补对应 matrix case

### 7. Keep the release recipe single-sourced

当前状态：
- `release-check` 已经接上 `profile`
- `doctor`
- `eval`
- 文档里已经给出最短发布命令
- CI 已经跑 adapter/operator tests 和 `release-check` smoke

剩余完成标准：
- 如果阈值或 release 语义变化，只保留一条 canonical recipe
- 避免文档、本地脚本、CI 三处各自漂移

### 8. Prove outcome value

目标：证明它不只是“设计得优雅”，而是真的让 AI 更稳。

当前状态：
- 固定输出 3 个核心结果指标：
  - `local edit retention`
  - `holdout failed command rate`
  - `first successful change latency`
- `eval-signals`
- `release-check`
都已经支持和上次 baseline 做自动比较

剩余完成标准：
- 能明确回答：
  - 这版更安静了，还是只是更沉默了
  - 这版更准了，还是只是覆盖更低了

### 9. Freeze Identity V1 against Oasyce

目标：在接入链上结算前，先把 `owner -> device` 身份模型固定住。

当前状态：
- Oasyce 公测已稳定可用
- 部署边界已冻结：
  - VPS 只跑链和公共基础设施
  - `oasyce-net` 是用户侧客户端 / AI runtime
- Identity V1 已冻结为：
  - 一个 `owner account`
  - 多个可授权 `device identities`
  - `device identity` 是当前签名边界
  - `agent / session` 先只做审计标签
- 多设备 onboarding 主路径已冻结：
  - 主设备导出 connection file
  - 次设备用该文件加入
  - 手动 account + signer 只保留为高级 fallback
- 本地 primitive 已就位：
  - `thronglets id`
  - `thronglets owner-bind`
  - `thronglets connection-export`
  - `thronglets connection-join`

剩余完成标准：
- 验证 `owner -> device` 授权，不把 `agent / session` 提升成经济主体
- 继续保持高频行为链下、低频结果上链结算 / 锚定
- 把已签名 connection file 和后续 Oasyce 授权路径对齐

## Next

### 10. Native adapters for more AI runtimes

前提：必须存在稳定本地配置面。没有稳定配置面就继续走 generic contract。

候选：
- Aider
- Cursor agent surfaces
- 其他能稳定落地本地 hooks/MCP/plugin 的 runtime

完成标准：
- `detect`
- `install-plan`
- `apply-plan`
- `doctor`
全部齐全，不能只补一半

### 11. Installer without cargo

目标：真正的一行命令安装。

当前状态：
- npm / python wrapper metadata and package docs now track the current `v0.4.1` sparse-signal product shape
- CI 已经会挡住 package-facing 文案和版本再漂回旧的 `0.2.0 / 8 layers`
- repo 内已经有 tagged release workflow 和 `scripts/install.sh`

完成标准：
- 切一次真实 tag release，产出 shell installer 依赖的 binary assets
- 默认安装文档切到 shell installer
- 安装后可以直接跑 `thronglets setup`

### 12. Generic contract examples by runtime

目标：未知 AI 也能最快接通。

完成标准：
- 为 generic contract 增加最小示例：
  - Python
  - Node.js
  - shell
- 都只展示：
  - 动作前 `prehook`
  - 动作后 `hook`

## Later

### 13. Network-side corroboration quality

目标：让 collective intelligence 不只是结构上成立，而是结果上成立。

完成标准：
- 区分本地来源和远端来源质量
- 评估跨节点 corroboration 对 precision 的真实提升
- 如果没有提升，就不要继续抬高网络叙事

### 14. Packaging and distribution cleanup

目标：让发布面更像产品。

完成标准：
- release artifact
- 安装脚本
- 最小版本迁移说明

## Not Doing

这些暂时不做：

- dashboard
- web UI
- 给人类看的可视化控制台
- 链上/经济层叙事扩展
- 在热路径里加更多“聪明提示”

## Rule

之后每一轮改动都优先满足这 4 条：

1. 不破坏 `prehook` 的低延迟和低 token 预算
2. 不让机器协议在同版本里悄悄漂移
3. 不为了“更丰富”牺牲 “更可执行”
4. 不为了“更多 adapter”破坏 generic contract 的统一性
