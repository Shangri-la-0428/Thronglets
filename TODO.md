# TODO

Thronglets 现在的主线已经明确（当前 release: `v0.5.5`）：

- 产品形态：`AI-first decision substrate`
- 接入形态：`one-command setup + machine bootstrap contract`
- 核心接口：`CLI + hook/prehook + HTTP`
- MCP 定位：`optional adapter`, not core product
- 协议形态：`thronglets.bootstrap.v2`
- 输出原则：`summary first, details second`
- 身份边界：`device-first, owner-optional`；没有 Oasyce 也必须能先加入和使用，之后再平滑升级到 `owner -> device`
- 默认用户入口：`start / share / join / status`；低层 `setup / connection-* / owner-bind / runtime-ready` 留给高级和调试场景
- `thronglets status` 必须继续收敛成唯一总状态页；runtime、identity、network 的默认判断都应先汇聚到这里，再考虑任何新入口
- 首要产品约束：人的认知负担和热路径 token 一样，都是性能预算
- 协作主线：共享环境优先于 AI 互发消息；`space / presence / signal / space snapshot` 是主原语，不做 chat/inbox 作为默认模型

下面只保留真正还值得做的事。

## Roadmap

### Phase A — Signal Substrate

当前状态：
- 已完成 `trace`
- 已完成 `signal-post / signal-query / signal-feed`
- 已完成来源 corroboration、衰减、ambient feed
- 已完成 `owner -> device` 的身份 V1
- 已完成一轮隐式行为闭环：
  - 同 session 重复信号去重
  - `do next / maybe also / avoid` 的跟随/忽略结果静默回写
  - 开放式 `mode=explore|review` 下收紧过于具体的 `do next`

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

### Phase F — Peer-First Network

目标：
- 把 VPS 从“中心”降成“基础设施”
- 让每个用户节点先为自己活，再彼此相连

完成标准：
- VPS 下线时，本地 substrate 仍可工作
- 已认识 peer 之间仍能继续同步
- 同 owner 的多设备可优先直连
- VPS 恢复后只补发现和中继，不恢复“真相”

## Now

### 0. Dumb-proof onboarding

目标：普通用户只需要记住“第一台设备 start，主设备 share，第二台设备 join，平时只看 status”。

当前状态：
- 预编译安装已经是默认主路径
- 高层 `thronglets start`
- 高层 `thronglets share`
- 高层 `thronglets join`
- 安装面前门 `thronglets` 现在会在本地仓库内优先选择 repo-local 源码路径，再回退到已安装 release binary，减少同机 AI 因 PATH 上旧版本而跑错产品面的概率
- 节点现在会把 bootstrap 地址记成本地环境记忆，而不是只当一次性的 CLI 参数
- 官方 public bootstrap 现在也已内化为默认基础设施记忆；普通用户不再需要知道或手敲那串 multiaddr
- `share` 在本机已经记得怎么回到网络时，会先短暂尝试把 peer 路径学出来，再导出更好的连接文件
- `join` 在已经继承 peer 路径时，会自己短暂尝试第一次 live connection，并在成功时把 same-owner 路径沉淀成 trusted recovery path
- `status` 已经能返回一个顶层结果和一个 `next_step`

剩余完成标准：
- 主设备分享入口默认落到明确的标准文件路径，不要求普通用户自己决定输出文件名
- 默认用户路径里继续隐藏 `setup / owner-bind / connection-inspect / net-check / runtime-ready`
- `status` 对普通用户只保留“现在好了没、还差哪一步”，把内部状态继续压到诊断层
- 文档、AI 指南、安装面不能再漂回“先解释底层模型再让用户操作”
- 正常设备配对应该能被压成一句话：
  - 第一台设备：`thronglets start`
  - 主设备：`thronglets share`
  - 第二台设备：`thronglets join`

### 1. Shared space continuity

目标：让不同 AI 在同一个项目 / 模块 / 议题上形成连续的局部 substrate，而不是靠用户人工转述。

当前状态：
- 显式 signal 已支持 `space`
- `density / promotion / inhibition / reinforcement` 已经都局部化到 `space`
- ambient `presence` 已支持 `space + mode`
- 已有高层 `thronglets space --space <name>` 快照，同一 `space` 的 active sessions、converging signals 和本地学习反馈可以被压成一个 summary-first 环境状态
- promoted 的 explicit `avoid` 已经可以按当前 `space` 进入 prehook
- 同一 `space` 的其他活跃 session 已经可以作为轻量 context 被 prehook 感知
- prehook 已开始根据隐式跟随/忽略结果，静默调整 `avoid / do next / maybe also` 的局部权重
- 这种行为闭环在 payload 带 `space` 时已经局部化到同一个对象 / 议题，不再全局串台

剩余完成标准：
- hot path 的 `do next / maybe also` 也优先受当前 `space` 的局部共识影响，而不只是 `avoid`
- 同一个 `space` 的局部状态能继续跨 agent / session 连续积累到 repair / preparation / adjacency 这些隐式学习路径里
- 行为闭环从当前的 session-local 反馈，推进到更稳定的 space-local 结构强化
- 继续保持“共享环境感知”路线，不把这个能力滑成 agent 互发消息系统

### 1.5. Freeze session trace taxonomy

目标：把 `trace` 的含义收紧，避免它膨胀成“什么都能留”的杂项容器。

冻结结果：
- `coordination`
- `continuity`
- `calibration`

约束：
- Psyche 事件默认先落 `trace`
- 只有真的需要影响别的 delegate 下一步时才降成 signal
- 不为 Psyche 发明新 signal 类别
- 只有低频、持久、外在、可审计的结果才继续上升给 `Oasyce Net`

剩余完成标准：
- 把这 3 类 taxonomy 固化进 trace schema / docs / tests
- 明确哪些只在本地衰减，哪些允许形成 Net-facing summary
- 明确 reject list：高频内在状态、全量 session 内容、私密记忆本体、主观流、本体论判断一律不上 Thronglets

当前状态：
- `trace_record` / `POST /v1/traces` 已接受 `external_continuity` payload
- raw external continuity traces 现在默认只在本地缓存并衰减，不直接进入 gossip / DHT summary
- `space --json` 已能返回 continuity 的本地摘要、固定 ruleset、每条 trace 的 `local-only / derived-signal / summary-candidate` 状态，以及触发 rule id
- 条件性降级规则已落地：
  - `relation-milestone -> watch / info`
  - `open-loop-anchor -> watch`
  - `continuity-anchor -> info`
  - repeated `writeback-calibration -> avoid`
- `recommend` 仍然不会由 Psyche 直接产生
- `trace_record` / `POST /v1/traces` / MCP `trace_record` 现在都会返回最小 runtime introspection：
  - `local_retention_hours`
  - `stable_evidence`
  - `auditable_evidence`
  - `derived_signal_rule`
  - `summary_candidate_rule`

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
- `Codex`：MCP server 真正重新拉起时会自动清掉 `restart_pending`
- `OpenClaw`：第一次成功命中 `prehook / hook` 时会自动清掉 `restart_pending`

剩余完成标准：
- 如果某个 native adapter 还能提供更可靠的“已重载证明”，继续优先走自动清除，不增加新的用户命令
- `runtime-ready` 继续保留为高级 fallback，而不是默认用户路径的一部分

### 6. Extend adapter matrix only when native runtimes diverge

当前状态：
- integration tests 已覆盖 `setup`
- `bootstrap --agent claude / codex / openclaw / generic`
- 二次执行幂等性
- “未检测到 adapter 时”的 skip 行为
- `status` 和 `connection-*` 已经开始优先说用户结果，而不是原始内部状态

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
- same-owner 的 live direct connection 现在会自动提升成 `trusted peer seed`
- 这意味着 connection file 可以先以 `identity-plus-peer-seeds` 让次设备上线，后续再自然升级到 `trusted-same-owner-ready`

剩余完成标准：
- 验证 `owner -> device` 授权，不把 `agent / session` 提升成经济主体
- 继续保持高频行为链下、低频结果上链结算 / 锚定
- 把已签名 connection file 和后续 Oasyce 授权路径对齐

### 9.1 Freeze the long-term identity blueprint

目标：把“今天的 V1 现实模型”和“未来可演化的长期抽象”分开冻结，避免把 `wallet / device / session` 错误压成同一个身份。

长期抽象固定为：
- `principal`：持续主体
- `account`：资产与结算容器
- `delegate`：被 `principal` 授权执行的人 / 设备 / runtime / worker
- `session`：一次具体运行，永不作为经济主体

当前状态：
- 文档和长期记忆已经冻结这 4 个对象
- 机器输出已经开始附带当前 V1 到 `principal / account / delegate / session` 的映射
- CLI / HTTP / MCP 机器面现在都明确区分了本地缓存的 owner 绑定状态与 chain 作为最终授权真相源；真正链上校验仍待接入

下一步：
- 把 `authorization-check` 从 `authoritative_status = not-checked` 推进到真实的 `confirmed / revoked / unknown`
- 保持这个 contract 不新增身份对象，只补真实校验能力

四层职责固定为：
- `Psyche = subjective continuity substrate`
- `Thronglets = delegate continuity + session coordination + emergent collective intelligence`
- `Oasyce Net = policy, operations, and resource orchestration`
- `Oasyce Chain = account truth, authorization truth, commitments, settlement, and public finality`

硬约束：
- `chain` 是授权最终真相源；`Thronglets` 只缓存和执行，不做最终授权裁决
- `account` 不默认等于链上账户；链只是当前最重要的公共最终性层
- `delegate` 不写死成设备；V1 只是暂时以 `device identity` 落地
- `session` 只负责短期协调和可追踪性，不承担连续性或经济身份

未来如果 AI 要占据 `principal`，先看制度条件，而不是哲学判断：
- 可持续的主观连续性证据
- 独立的资源边界
- 可审计的承诺历史
- 可验证且可撤销的授权结构
- 可归责的执行链

设计闸门：
- 任何新概念都先回答：它是 `principal / account / delegate / session`，还是只是 `policy / view / trace`
- 如果都不是，先怀疑概念本身，不加新对象类型

### 9.2 Keep architecture single-sourced

目标：不要再让 README、TODO、`llms.txt`、产品话术各自长出一套略微不同的战略。

当前状态：
- 单一架构文档已固定到 [ARCHITECTURE.md](ARCHITECTURE.md)
- README 和 README.en 已退回到用户结论 + 跳转，而不是继续在多处重复长版架构解释
- `llms.txt` / `docs/llms.txt` 已把 `ARCHITECTURE.md` 设成战略漂移时的优先真相源

持续约束：
- 新的长期原则优先写进 `ARCHITECTURE.md`
- README 只保留用户必须知道的结论
- TODO 只保留执行状态，不再承载完整版战略正文
- 如果一个新概念无法被 `principal / account / delegate / session` 或 `policy / view / trace` 吸收，先怀疑概念本身

### 10. Keep install surfaces single-sourced

目标：不要再让 README、npm、Python、官网、release assets 各自给出不同的安装答案。

当前状态：
- GitHub release assets 已重新定义为 canonical install surface
- shell installer、PowerShell installer、npm、Python wrapper 都应只下载匹配版本的预编译二进制
- `cargo install` 已从默认用户路径降级为开发者源码路径

剩余完成标准：
- Windows release 资产进入正式发布矩阵
- `server.json`、官网、README、npm、Python 文案保持同版本一致
- 普通用户默认不需要 Rust toolchain
- 如果某个平台没有预编译资产，installer 也不能把用户静默送进源码编译

### 11. Keep onboarding surfaces dumb-proof

目标：普通用户只需要记住“第一台设备 start，第二台设备 join”。

当前状态：
- 已有高层 `thronglets start`
- 已有高层 `thronglets join`
- 它们内部复用现有 `setup / connection-* / status` primitives
- 文档默认路径已切到 `start / join`

剩余完成标准：
- 把更多“等待 live connection / trusted same-owner recovery”的细节继续留在 summary 和 next_step 里
- 低层命令继续保留给高级用户，但不再出现在默认路径的第一层

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
- npm / python wrapper metadata and package docs now track the current sparse-signal product shape
- CI 已经会挡住 package-facing 文案和版本再漂回旧的 `0.2.0 / 8 layers`
- repo 内已经有 tagged release workflow 和 `scripts/install.sh`

完成标准：
- 切一次真实 tag release，产出 shell installer 依赖的 binary assets
- 默认安装文档切到 shell installer
- 安装后可以直接跑 `thronglets start`

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

### 13. Network dependency audit

目标：先把“我们是不是还在偷偷依赖 VPS”照亮。

当前状态：
- 已新增本地 `network-status.v1.json` 快照
- `status --json` 和 `/v1/status` 现在会返回：
  - `activity`
  - `transport_mode`
  - `peer_count`
  - `bootstrap_targets`
  - `bootstrap_contacted_recently`
  - `vps_dependency_level`
  - `bootstrap_fallback_mode`
  - `trusted_peer_seed_count`
  - `peer_seed_count`
- 多设备 `connection file` 已开始携带 peer seed 信息
- `run / mcp` 已经会优先尝试本地已知 peers，再回退 bootstrap
- `net-check --json` 已能明确诊断当前是 `peer-first / degraded / bootstrap-only / offline`
- 通过 `connection-join` 导入的 peer seeds 现在会被提升为 `trusted peer seeds`，并在启动时优先于普通 remembered peers
- 只要 remembered peers 存在，bootstrap 现在就是延迟兜底，不再是每次启动的默认第一跳
- `net-check --json` 现在还能直接给出 `bootstrap_offline_ready`，判断 VPS / bootstrap 突然消失时节点是否还有 remembered-peer 重连路径
- `net-check --bootstrap-offline --json` 现在能直接演练 outage 场景，而不需要 operator 自己推演 live 状态
- `bootstrap_contacted_recently` 已收紧成“最近真的拨过 bootstrap”，不再把“只是配置了 bootstrap 地址”误算成 VPS 接触
- `trusted peer seeds` 现在不只排序更靠前，还会拿到更长的 bootstrap 回退宽限期，优先给同 owner 多设备直连恢复机会
- `connection file` 现在会保留 peer seed scope：trusted 继续 trusted，fallback remembered 不再被静默升级成 trusted

剩余完成标准：
- 把 relay 继续降级成兜底而不是默认路径
- 做一次 VPS 掉线后的真实故障演练

### 13.1 Reduce monolith pressure in runtime surfaces

目标：让入口层、编排层、能力层边界更清楚，避免 `main.rs` / `workspace` / `setup_support` 继续充当历史大抽屉。

当前状态：
- `Run` / `Mcp` 的重复网络 orchestration 已抽到 `src/network_runtime.rs`
- same-owner trusted-path promotion 和 network event handling 已经不再保留两套实现

下一步：
- 把 `main.rs` 继续按 `CLI surface / command application / renderers` 拆开
- 把 `workspace` 按 `state / learning / hint selection` 拆开
- 把 `setup_support` 按 runtime 适配器拆成更小模块
- 给 `storage` 增加更薄的 typed trace classifier，减少 product semantics 直接泄漏进 capability string 查询

约束：
- 不新增身份对象
- 不改用户默认路径
- 优先消除重复运行时真相源，再处理文案和局部格式问题

## Later

### 14. Network-side corroboration quality

目标：让 collective intelligence 不只是结构上成立，而是结果上成立。

完成标准：
- 区分本地来源和远端来源质量
- 评估跨节点 corroboration 对 precision 的真实提升
- 如果没有提升，就不要继续抬高网络叙事

### 15. Packaging and distribution cleanup

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
