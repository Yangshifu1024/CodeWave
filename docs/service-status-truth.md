# 后台服务状态：以进程存活为准，不再拿「日志非空」当代理

> 分支 `fix/service-status-truth`（基线 `main` · cba9ebf）。修的缺陷：用户报告「后台服务明明在运行，工具卡却显示**已停止**且没有日志」（会话 03dd2bc4-…-5580）。
> 现场：卡片上写着「已使用 后台服务」+「已停止」，下方只有一句「用 read 查看日志；stop 停止（进程树）」——没有任何日志区，也没有停止按钮。

## 一、四条根因（全部独立核实到行）

| # | 根因 | 位置 | 机理 |
|---|---|---|---|
| **R1** | 状态判据用错字段 | `ToolCallCard.tsx` `data.tail ? "运行中" : "已停止"` | `tail` 是**日志内容**，而 `start` 出参**根本不含** `tail`（只有 `id / pid / purpose / owner_root_id / note`）。判据从一开始就读不到东西 |
| **R2** | 空输出服务永不发事件 | `service.rs` ticker + `RingLog::append` | ticker 只在 `tail != last_tail` 时推，而 `last_tail` 初值是空串、`append` 对空文本提前返回 ⇒ 无输出的服务（如 `sleep 300`）`tail` 恒空 ⇒ 条件恒假 ⇒ **一个事件都不发**。这是「无日志」的完整成因 |
| **R3** | 子代理场景永久静默丢弃 | `runHandlers.ts` `s.tabs[p.session]` | 子代理启动的服务其 `session = sub_id`，挂在 `tabs[*].subStreams[sub_id]` 下而**不在 tabs 顶层** ⇒ 事件一律 `hit=false` 后静默 `continue`，无日志无报错 |
| **R4** | 历史恢复后永久误判 | `restoreFromMessages` | 从模型侧文本重建工具卡；`start` 出参本无 `tail` ⇒ 永久「已停止」；且**停止按钮也绑在 `data.tail`** ⇒ 一并消失 |

R1–R4 合起来会形成**僵尸服务**：进程占着端口与内存，界面说它停了，用户连停止按钮都看不到，最终撞上 16 个服务上限且无从排查。

## 二、三条设计决策

### 1. 状态与数据解耦：`running` 是权威字段，`tail` 回归它本来的职责

`running` 承载「进程是否存活」，`tail` 只承载「日志内容」。两者生命周期完全不同，混用必然出错。落点：

- 后端 `start` 出参加 `"running": true`；`exited` / `removed` 事件载荷带 `running: false`
- 前端卡片改三态（见下）
- **反向纪律**：`start` 出参**不得**塞一个空 `tail` 来「修好」问题——那只是把同一个 bug 换个位置。有后端单测钉死（`start_outcome_declares_running_and_omits_tail`）

### 2. 三态渲染：`未知` 是一等公民，不是「已停止」的伪装

| `data.running` | 显示 | 停止按钮 |
|---|---|---|
| `true` | 运行中 | 可见 |
| `false` | 已停止 | 隐藏 |
| `undefined` | **状态未知** | **可见** |

第三态是本设计的关键：既没收到退出信号、又没有权威快照可依（旧历史 / 推送全丢）时，**界面必须承认不知道**。谎报「已停止」会同时藏掉停止按钮，把用户推向僵尸服务。未知态保留按钮，是为了不留没有收手入口的局面。

### 3. 推送是增量通道，**必须**配一条拉取兜底

`service:update` 是增量推送，天生会丢帧：Tab 未挂载、分页未加载、子代理流未就绪、静默期无输出。任何一次丢失都让状态永久停在错的地方。所以另加只读 IPC `list_services` 做对账：

- 后端抽出 `pub fn service_info(h: &ServiceHandle) -> Value`，工具 `list` 与新 IPC **共用同一份投影**——两处序列化不会漂移
- 前端 `reconcileServices(set, get)` 按权威快照覆写所有 service 卡的 `running`；`scheduleServiceReconcile` 以 3s 防抖、**自维持链式重排**（否则「本轮刚收到过推送 → 跳过 → 无人再排」会让兜底永久失效），无 service 卡时靠 `hasServiceCard` 早退自行停住
- `ServiceInfo` 前后端 9 个字段逐一对齐；`purpose` 经 `#[serde(rename_all = "snake_case")]` 确为 `"development" | "preview"`

## 三、落盘快照不可信（B1）

`running: true` 会随 `compact_for_model` 进历史，而它只是**启动当时**的值——进程可能早已退出、被 stop、或随应用重启被回收。直接沿用会得到「**永久误报运行中**」，比误报已停止更坏（用户会相信服务还活着）。

处理：`restoreFromMessages` 与 `openSubDrawer`（归档子代理的过程流是**懒加载**的，service 卡此刻才第一次出现）两处都调 `demoteRestoredServiceState`，把落盘的 `running` 一律降为 `undefined`，随后由 `list_services` 以进程实况覆写。**两处都要做**——只做首屏恢复会让懒加载出的子代理服务卡永远停在过期快照上。

## 四、容器形状：子代理的工具卡**不在** `timeline` 里

`serviceToolsOf` 必须认两种形状，写错任一处都会让子代理启动的服务收不到任何更新：

- 主会话：`items[]` 中的 assistant 项，每项自带 `toolsMap`
- 子代理流 `SubStream`：**平铺**的 `toolsMap`（`timeline` 里只有 tool/text/thinking 等 seg；见 `runFrames.ts` 的 `closeRunningTools` 遍历口径）

把 `SubStream` 当数组遍历并筛 `kind === "assistant"`，属主查找会对子代理**恒返回空集**——B2 表面已修、实则未修。这条正是由 store 层回归测试揭出的。

## 五、明确不采纳的做法

| 做法 | 为什么不做 |
|---|---|
| 用 `tool.status === "running"` 判存活 | 那描述的是**工具调用**的生命周期（`start` 早已返回 ok），与服务进程存活无关，会引入新的误判 |
| 新增 `service:status` 事件键 | `running` 只是**已有** `service:update` 载荷的增量字段。事件面**保持 29 键**，不触发 `events.contract.test.ts` 契约变更 |
| 在 `start` 出参里塞空 `tail` | 把同一个 bug 换个位置；状态必须由 `running` 承载 |
| 用 `owner_root_id` 做清理 | 该字段的注释声称有清理逻辑，实际全仓无消费者（`ServicePurpose::Preview` 亦同）。属另一处独立的模型不完整，本次不动 |

## 六、验证

**自动**：`cargo test` **1213 passed / 0 failed / 3 ignored**（含 2 条新后端用例）、`cargo clippy --all-targets` **0 warning**、`pnpm --dir ui test` **1351 passed / 110 文件**（新增 24 条）、`pnpm --dir ui build` 通过、`pnpm --dir ui run lint` 干净。

新增回归用例：
- 后端 `start_outcome_declares_running_and_omits_tail` / `ticker_emits_first_update_even_while_silent`（后者用捕获 sink 钉死 R2：空输出服务也必须发首拍）
- 前端 `run.service-update.test.ts`（15 例，handler 直驱）+ `toolcard.service-status.test.tsx`（9 例，组件层）

**手工验证清单**（界面改动按 AGENTS.md 约定不做 GUI 自动点验）：

1. 启动**无输出**的服务（如 `sleep 300`）→ 显示**运行中** + 停止按钮可见（钉死 R2）
2. 启动**持续打日志**的服务 → 运行中 + 日志滚动
3. 切 Tab 再切回 → 状态仍正确（拉取兜底）
4. 重开会话 → 在跑的显示运行中（靠 `list_services` 校准，不靠落盘快照）；无法判定显示「状态未知」
5. 点「停止服务」→ 翻「已停止」、按钮消失、日志保留最后一段
6. 子代理启动服务 → 事件不再静默丢弃（R3）
7. 外部 kill 服务 → 事件驱动翻「已停止」，无需切 Tab

## 七、遗留（不在本次范围）

- `ToolCallCard` 的停止按钮在 `data.id` 本身也缺失时（旧历史只剩 `{ restored: true }`）点了会静默无反应——`undefined` 态刻意保留按钮是对的，但「id 缺失」这一子情形缺兜底提示
- `droppedServiceUpdateCount()` 目前只被测试消费，尚无诊断面板读取
- `owner_root_id` 注释与实现不一致、`ServicePurpose::Preview` 无消费者（同源于 service 工具的状态模型不完整）