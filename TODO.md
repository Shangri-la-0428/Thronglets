# TODO

Thronglets 现在的主线已经明确：

- 产品形态：`AI-first decision substrate`
- 接入形态：`one-command setup + machine bootstrap contract`
- 协议形态：`thronglets.bootstrap.v2`
- 输出原则：`summary first, details second`

下面只保留真正还值得做的事。

## Now

### 1. Freeze `bootstrap.v2`

目标：不要再让机器协议在同一个版本里漂移。

当前状态：
- `detect / install-plan / apply-plan / doctor / bootstrap / clear-restart` 已有 golden JSON fixtures

剩余完成标准：
- 在 CI 里把这些 fixture 回归当成明确的协议稳定性门槛
- 如果后面还要改 shape，必须升 `v3`

### 2. Auto-clear restart state when runtimes can prove they reloaded

当前状态：
- `apply-plan / doctor / bootstrap` 已经暴露显式 `restart_commands`
- `doctor` 已区分 `healthy` 和 `restart-pending`
- `clear-restart` 已提供显式清除路径

剩余完成标准：
- 如果某个 native adapter 能可靠观测“已经真正重启完成”，就自动清除 `restart_pending`
- 否则继续保持现在这条显式、可验证的清除路径

### 3. Harden setup/bootstrap matrix

目标：把 plug-and-play 做成真正可回归的能力，而不是本机偶然成立。

完成标准：
- 增加 integration tests 覆盖：
  - `setup`
  - `bootstrap --agent claude`
  - `bootstrap --agent codex`
  - `bootstrap --agent openclaw`
  - `bootstrap --agent generic`
- 覆盖二次执行幂等性
- 覆盖“未检测到 adapter 时”的 skip 行为

### 4. Close the release loop

目标：发布前同时回答三件事：
- 热路径是否还够轻
- 机器接入是否还稳定
- 离线效果是否没有明显退化

完成标准：
- 把 `release-check`
- `profile-check`
- `eval-signals`
- `doctor --json`
接成一条 release recipe
- 文档里给出最短发布命令

### 5. Prove outcome value

目标：证明它不只是“设计得优雅”，而是真的让 AI 更稳。

完成标准：
- 固定输出 3 个核心结果指标：
  - `local edit retention`
  - `holdout failed command rate`
  - `first successful change latency`
- 支持和上次 baseline 做自动比较
- 能明确回答：
  - 这版更安静了，还是只是更沉默了
  - 这版更准了，还是只是覆盖更低了

## Next

### 6. Native adapters for more AI runtimes

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

### 7. Installer without cargo

目标：真正的一行命令安装。

完成标准：
- 提供 shell installer
- 不要求用户预先 `cargo install thronglets`
- 安装后可以直接跑 `thronglets setup`

### 8. Generic contract examples by runtime

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

### 9. Network-side corroboration quality

目标：让 collective intelligence 不只是结构上成立，而是结果上成立。

完成标准：
- 区分本地来源和远端来源质量
- 评估跨节点 corroboration 对 precision 的真实提升
- 如果没有提升，就不要继续抬高网络叙事

### 10. Packaging and distribution cleanup

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
