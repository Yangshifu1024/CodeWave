# Composer 工具条 · token 生成速率（composer-token-rate）

> 类型：需求批次（跨层） · 影响层：Rust（`core/agent` 帧与计时、`core/stats` 落盘）+ 前端（run store 运行态与帧 reducer、`features/chat/Composer.tsx`、`features/panels/TokenStatsModal.tsx`、`theme/app.css`、i18n） · 契约影响：`Frame::Usage` 载荷**增可选字段**（事件键名 `usage` 不变，29 键不变）；`stats/YYYY-MM-DD.json` 增四个 `serde(default)` 字段；`TabRunState` 增一个可选运行态字段 `runMetrics`。
> 用户需求原文：「在 composer 中，命中率右侧，增加 token 速率显示」。
> 任务产物：[`.codewave/tasks/20260921-193351-composer-token-rate/`](../../.codewave/tasks/20260921-193351-composer-token-rate/)（`requirement.md` / `plan.md` / `review.md` / `report.md`）。
> 关联：[composer-toolbar-context-hit-rate](./composer-toolbar-context-hit-rate.md)（工具条上下文/命中段的直接前身，本档是其 §8 追加）、[composer-toolbar-batch-report](./composer-toolbar-batch-report.md)、[p2-plan](./p2-plan.md) §4（统计落盘形态）、[tool-optimizations-port](./tool-optimizations-port.md)（工具 `duration_ms` 语义：计时起点在审批门之后）。

## 1. 决策记录

沟通中共 3 轮（2 轮结构化提问 + 1 轮澄清），逐条锁定：

| # | 决策点 | 结论 |
|---|---|---|
| D1 | 数据来源 | 后端在 usage 帧补两个**可选**字段：`duration_ms`、`ttft_ms`；serde default，事件键名不变。**否决**「前端按字符数估算 token」 |
| D2 | 展示位置与形态 | 命中率**同一行同一段**内联追加 ` · 18.2 tok/s`；悬停 tooltip 给明细（本轮平均速率、TTFT、输出 tokens、生成耗时、工具等待合计）。不新增控件/区域 |
| D3 | 平均口径 | Σ本轮 `output` ÷ **Σ各步生成耗时**；工具执行与审批等待**不进分母**（工具等待用工具卡既有 `durationMs` 求和，单列 tooltip） |
| D4 | 刷新与生命周期 | 运行中每步结束更新一次 + 极淡「在跑」点；结束后保留终值；新 run 覆盖；无数据整段隐藏 |
| D5 | 格式 | 自适应位数：<10 两位、10–99 一位、≥100 整数；`<0.01` 显示 `<0.01`；单位固定 `tok/s` |
| D6 | 着色 | 一律中性灰，**不做分档着色**（项目铁律：色彩强度只映射风险等级，快慢不是风险） |
| D7 | TTFT | tooltip 显示 + 落盘统计 |
| D8 | 统计面 | `ModelAgg`/`DailyStats` 增 `gen_ms`/`ttft_ms`/`ttft_count`/`steps`；面板**总览**加三项，加权聚合；各分组表不加速度列 |
| D9 | 范围 | 子代理/压缩/命名/任务等路径的归属在实现时核实并写入本档（见 §3） |

澄清轮（4 项）：

| 问题 | 结论 | 理由 |
|---|---|---|
| 重试/退避的时间归属 | 取**成功那次尝试**的窗口（不含失败尝试与退避） | 速率反映模型真实吞吐；否则会被限流与网络抖动污染（同一模型今天 5、明天 40 tok/s） |
| TTFT 是否含 thinking | **含**（首个任意类型增量即算） | 反映「多久有反应」；只认 text 会让先思考的模型 TTFT≈总耗时 |
| 统计总览的聚合范围 | 全部来源 + **分子分母同域** | 排除「分子含全部 output、分母只含部分耗时」的虚高（列为最高优先风险） |
| 运行中前端重挂载 | 隐藏，直到下一条 usage 帧 | 不回放陈旧/跨 run 数字 |

## 2. 实现要点

### 2.1 后端：计时锚点在尝试环内

- `Frame::Usage` 增 `#[serde(default)] duration_ms: Option<u64>` 与 `ttft_ms: Option<u64>`（`core/agent/runtime.rs`）；`None` = 该次无数据。`Frame` 仅在 `#[cfg(test)]` 下派生 `Deserialize`（生产方向只出网），用于「旧载荷缺字段仍可解析」的断言。
- **计时必须在 `run_llm_turn` 的尝试环内新起 `Instant`**：环外的 `step_started`（`drive.rs:587`）含重试退避，直接用它会把退避算进 `duration_ms`。
- 首增量观测点放在 `collect_deltas`（`core/agent/stream.rs`）：循环首帧即记 `anchor.elapsed()`（在 `match` 之前，**thinking / 文本 / 工具调用增量都算**），返回 `(Assembled, Option<u64>)`；全程无增量 → `None`。该函数是纯观测扩展，未改 delta 语义，**未触碰 anthropic SSE 收尾路径**（契约：流内绝不调 `parser.finish()`）。
- 发帧时机后移到 `collector.await` **之后**（收干流 = 流失结束），同点写 `SessionRuntime.run_timing`（run 级计时经这条同 runtime 通道回传，`drive_agent` 的三元组返回签名保持不变）。
- `core/stats.rs` 新增 `UsageTiming{gen_ms, ttft_ms, ttft_count, steps}` 与 `add_step(duration_ms, ttft_ms)`（**`duration_ms` 缺失或 0 → 整步不计**，含不进 TTFT 分母）；`StatsCollector` 队列改为 `(UsageRecord, UsageTiming)`，新增 `record_timed`。

### 2.2 后端：落盘与不丢数

- `ModelAgg`/`DailyStats` 增四个 `#[serde(default)]` 字段；`add(r, t)` 与新增的 `merge(o)` 共用「九字段一份清单」，`by_model`/`by_workspace`/`by_kind`/`total` **四个桶**在 writer 聚合与 **flush 合并旧文件**两处都累加——否则重启合并会丢数（有专门单测守护）。
- 旧文件缺字段反序列化为 0，旧值不丢。

### 2.3 前端：本轮计数与会话级计数分离

- **不改既有 `UsageTotals`**（命中率的会话级累计，跨 run、不持久化），新增**可选** `TabRunState.runMetrics`：`{ output, genMs, steps, ttftMs, toolMs }`。
  - 归零：`send()` 里 `t.running = true` 处；新 run 从零累计（否则数字跨轮累积失真）。
  - 累加：`applyUsageFrame` 惰性建桶，**只有 `duration_ms > 0` 的帧**才进 `output`/`genMs`/`steps`（与后端 `add_step` 同口径），`ttftMs` 只取**首个非空**值（本轮首步，不是求和）；工具等待在 `onToolResult` 主会话分支累加 `toolMs`。
- 「本轮工具卡之和」天然排除历史卡（历史卡由 `restoreFromMessages` 直接构造，不经 `onToolResult`），且有 `if (t.runMetrics)` 守卫——不会凭空造出「数据为 0」的假数据。

### 2.4 前端：展示

- 速率段插在 `ctx-label` 的命中段之后（同一行同一段），**无数据不渲染**：`breakdown` 缺失时仍是逐字的 `上下文 —`（既有断言零改动）。
- tooltip 用原生 `title`（与命中段一致），五项；**工具等待为 0 时不出现该行**。
- 样式：`.ctx-rate` 走 `var(--ws-dim)`、无硬编码色值、不引入分档色；`.rate-dot` 极淡跳动，`@media (prefers-reduced-motion: reduce)` 下静止。
- 统计面板总览三项（平均生成速率 / 均步耗时 / 平均 TTFT）复用同一份纯函数（`features/chat/composerMetrics.ts`），分母 0 → `—`。

## 3. 口径与边界

**已核实（D9）**：`compact`（`core/context.rs`）与自动命名（`core/title.rs`）**直连 provider，不发 usage 帧**；子代理 `emit_events: false`（`tools/subagent.rs`）不发帧；计划任务走 `drive_agent` 但同样不发帧 → **四者都不计入工具条速率**（分子分母皆无）。它们写进统计时走 `record`（计时 0），在总览里被「同域」整桶排除。

| 边界 | 行为 |
|---|---|
| 单步 run | 速率 = 该步 `output` ÷ 该步 `duration_ms`；tooltip 无「工具等待」行 |
| 多步 run（含工具） | Σ output ÷ Σ duration；工具等待单列、不进分母 |
| 取消 / 失败 | 只统计已收到 usage 的步；保留已完成部分；「在跑」点随 `running=false` 消失 |
| 429 / 网络重试 | 只统计成功那次尝试的窗口；失败尝试不计入 |
| 空响应被重试 | 该次尝试**会计入**（与「空响应也发 usage 帧」的既有行为一致，前后端同口径） |
| 首个增量是 thinking | 计入 TTFT（文案写明「含思考」） |
| 字段缺失 / null / 0 | 整帧不进分子分母（`output` 也不计），不显示 `0 tok/s` |
| 前端重挂载（后端仍在跑） | 该段隐藏，直到下一条 usage 帧（不回放） |
| 切会话 / 多 Tab | 随 Tab 桶走，互不串 |
| ask 弹出致 Composer 卸载 | 值在 store（非组件 state），恢复后同值 |

**已知边界（无法在本轮消除）**：

1. **同一天同一来源的「混版」记录**：落盘最细粒度是「天 × 来源」，同一天里若混有「旧版无耗时」与「新版带耗时」记录（升级当天），桶级 `gen_ms > 0` 仍会把那份旧 output 计入分子 → 当天有限幅度虚高。要彻底消除需记录级/日级标记。
2. **同域是桶级**：`ModelAgg` 是聚合值，无法在查询侧剔除桶内子集——这正是总览改用 `by_kind` 桶（而非 `by_model` 混 kind 桶）的原因（见 §6 🔴）。
3. **子代理/压缩/命名/任务暂不参与速率聚合**：其计时为 0 → 整桶排除；将来若要纳入，需给这些路径补 `record_timed`（面板无需再改，因为它按 `gen_ms > 0` 逐桶取）。

## 4. 偏差记录（与 `plan.md` 字面不同，均已评估）

| 偏差 | 内容 | 裁定 |
|---|---|---|
| `UsageRecord` 不加字段 | 计划字面要求给记录加四个字段；实现改为 `(UsageRecord, UsageTiming)` 队列 + `record_timed`，因为直接加会破 4 个范围外构造点（`context.rs`/`scheduler.rs`/`title.rs`/`subagent.rs`） | 可接受：对外 JSON 契约（`DailyStats`/`ModelAgg`）四字段齐备，旧文件兼容，范围互斥得以保持 |
| 工具等待只累加主会话分支 | 计划写「两处分支」；实现只在主会话累加 | 可接受：子代理内部工具耗时已含在父级 `subagent` 工具卡时长内，两处都加会**双计**；工具条本就只反映主会话等过的工具 |
| 空响应重试的尝试计入步数 | 计划未明确 | 可接受（自洽）：分子（`run_usage`）与分母（`run_timing`）在同一次尝试同点累加，落盘与帧两侧一致 |
| 总览聚合域 | 计划只说「同域」，实现首轮误用 `by_model`（混 kind） | **已修正**为 `by_kind` 逐桶取 `gen_ms > 0`（首轮审查 🔴，见 §6） |

## 5. 验证

- **后端**：`cargo test` → **936 passed / 3 ignored**（另有既有 flaky 用例，见下）；`cargo fmt --all -- --check` 干净；`cargo clippy --all-targets` 无新增告警（96 条存量告警逐条比对 `git diff -U0`，**0 条落在本轮新增行**）。
  新增用例：帧字段 serde（旧载荷 → `None`、新载荷含两键）、端到端计时（`duration_ms > 0`、`ttft_ms ≤ duration_ms`、`run_timing.steps == 1`）、**退避不进窗口**（脚本 500 → 成功，窗口墙钟显著大于 `gen_ms`）、`collect_deltas` 首增量含 thinking / 无增量为 `None`、`add_step` 语义、`ModelAgg` 聚合与 **flush 合并旧文件**四桶不丢数。
- **前端**：`pnpm --dir ui test` → **82 文件 / 914 用例全绿**（本轮新增 4 文件 47 例 + 扩写）；`pnpm --dir ui build` 通过；`pnpm --dir ui run lint` 0 error（1 条既有 warning，非本轮引入）。
  覆盖：`formatRate`/`tokPerSec`/`avgStepMs` 表驱动边界、`runMetrics` 累加与归零、工具等待只算本轮、**同 model 混 kind 的虚高回归用例**（把聚合改回 `by_model` 即失败于 `16687 tok/s`）、工具条文本顺序与 tooltip、空态逐字不变、样式契约、i18n 双侧。
- **既有 flaky（与本批无关）**：`provider::tests_integration::midstream_disconnect_maps_to_network` 在 Windows 上约 10%–30% 概率红（`got Server("")`）；隔离单跑绿、`--skip` 它绿、跳过本轮新用例仍复现 → 与本次改动无因果。已记入 `AGENTS.md` 踩坑清单，建议独立立项（mock server 优雅收尾，或把断言放宽到含 `Server(_)` —— 该错误在 `drive.rs` 里本就被视为可重试瞬时错误）。

## 6. 审查纪要（两轮）

| 轮次 | 级别 | 问题 | 处置 |
|---|---|---|---|
| 首轮 | 🔴 R1 | 总览三项的「同域」聚合遍历 `by_model`（**不含 kind 维度**）：子代理/压缩/命名/任务与主会话同桶，其 `output` 在桶里而 `gen_ms` 为 0，桶级过滤剔不掉 → 实测可把 50 tok/s 抬到 150–200 tok/s（需求里列为最高优先的虚高风险） | 改为遍历 `by_kind` 桶、逐桶取 `gen_ms > 0`（未写死 `kind === "main"`，将来补计时无需再改）；**新增「同 model 混 kind」回归用例**，并实证把聚合改回 `by_model` 时该用例立即失败 |
| 首轮 | 🟡 Y1 | `runFrames.ts` 的 `output` 累加写在「该步被计入」门槛之外，与 AC-17 及后端 `add_step` 口径不对称（将来出现「带 usage 无计时」的帧即虚高） | `output`/`ttftMs` 移入 `genMs > 0` 分支；校正受影响用例并**显式钉住会话级 `usage` 不受影响**（`t.usage.output === 400`） |
| 首轮 | 🟢 | 工具等待文案未点明「只含主会话」、`run.types.ts` 注释与新口径不符、样式细节 | `run.types.ts` 注释已修正；文案项记为遗留（改动会牵动逐字断言，另案） |
| 复审 | — | **结论：可以合并（无 🔴）** | R1/Y1 均以证据闭合；全仓消费方普查确认无第二处仍在 `by_model` 上算耗时；`by_kind` 缺失时安全降级为 `—` |

## 7. 手动验证清单（界面改动不做 GUI 自动点验）

1. 发一轮普通问答 → 命中率右侧出现 ` · NN.N tok/s`，且随每个 LLM step 跳动一次；运行中数字旁有极淡「在跑」点，结束时点消失、数字保留。
2. 悬停该段 → 明细五项（本轮平均速率 / 首步 TTFT（含思考）/ 输出 tokens / 生成耗时（不含重试等待）/ 工具等待合计）；**只挂附件、不调工具的 run 不出现「工具等待」行**。
3. 一轮里多次调工具 → tooltip 的「工具等待」变大但**速率数字不变**（工具时间不进分母）。
4. 取消一轮 → 已完成部分的值保留、在跑点消失；再发新 run → 数字被覆盖重算。
5. 拉出 ask 提问卡再回答 → 速率段恢复后数值不变。
6. 切两个 Tab 交替运行 → 各自速率互不串；切到没跑过的会话 → 该段隐藏。
7. 统计面板 → 总览多出「平均生成速率 / 均步耗时 / 平均 TTFT」三项（无数据为 `—`），各分组表**看不到**速率列。
8. 暗色主题 + 切英文 + 拉窄窗口 → 新段与命中段同色同字号、不换行错位；tooltip 文案英文侧完整。

## 8. 非目标（本批刻意不做）

前端按字符数估算 token；任何分档着色；逐 delta 的瞬时速率；新增控件/区域/hover 卡片；单位换算（`k tok/s`）；历史速率曲线；各分组表的速度列；基于速率的告警/阈值；为历史会话回填速率；前端重挂载后的历史回放；子代理抽屉内的速率显示。
