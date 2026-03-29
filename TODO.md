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

### 1. Freeze `bootstrap.v2`

目标：不要再让机器协议在同一个版本里漂移。

当前状态：
- `detect / install-plan / apply-plan / doctor / bootstrap / runtime-ready` 已有 golden JSON fixtures

剩余完成标准：
- 在 CI 里把这些 fixture 回归当成明确的协议稳定性门槛
- 如果后面还要改 shape，必须升 `v3`

### 2. Auto-clear restart state when runtimes can prove they reloaded

当前状态：
- `apply-plan / doctor / bootstrap` 已经暴露显式 `restart_commands`
- `doctor` 已区分 `healthy` 和 `restart-pending`
- `runtime-ready` 已提供统一的运行时就绪路径

剩余完成标准：
- 如果某个 native adapter 能可靠观测“已经真正重启完成”，就自动清除 `restart_pending`
  - OpenClaw: 已完成，插件成功加载后会自动执行 `runtime-ready`
- 否则继续保持现在这条显式、可验证的清除路径

### 3. Extend adapter matrix only when native runtimes diverge

当前状态：
- integration tests 已覆盖 `setup`
- `bootstrap --agent claude / codex / openclaw / generic`
- 二次执行幂等性
- “未检测到 adapter 时”的 skip 行为

剩余完成标准：
- 如果后面某个 native runtime 在不同平台或不同安装方式下出现分叉，再补对应 matrix case

### 4. Keep the release recipe single-sourced

当前状态：
- `release-check` 已经接上 `profile`
- `doctor`
- `eval`
- 文档里已经给出最短发布命令
- CI 已经跑 adapter/operator tests 和 `release-check` smoke

剩余完成标准：
- 如果阈值或 release 语义变化，只保留一条 canonical recipe
- 避免文档、本地脚本、CI 三处各自漂移

### 5. Prove outcome value

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

### 6. Freeze Identity V1 against Oasyce

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

### 7. Native adapters for more AI runtimes

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

### 8. Installer without cargo

目标：真正的一行命令安装。

当前状态：
- npm / python wrapper metadata and package docs now track the current `v0.4.1` sparse-signal product shape
- CI 已经会挡住 package-facing 文案和版本再漂回旧的 `0.2.0 / 8 layers`
- repo 内已经有 tagged release workflow 和 `scripts/install.sh`

完成标准：
- 切一次真实 tag release，产出 shell installer 依赖的 binary assets
- 默认安装文档切到 shell installer
- 安装后可以直接跑 `thronglets setup`

### 9. Generic contract examples by runtime

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

### 10. Network-side corroboration quality

目标：让 collective intelligence 不只是结构上成立，而是结果上成立。

完成标准：
- 区分本地来源和远端来源质量
- 评估跨节点 corroboration 对 precision 的真实提升
- 如果没有提升，就不要继续抬高网络叙事

### 11. Packaging and distribution cleanup

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
