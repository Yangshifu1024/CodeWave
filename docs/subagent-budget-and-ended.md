# 子代理步数预算与收尾形态（`ended`）

> 契约文档（[docs/0-README](./0-README.md)）。后端判据在 `src-tauri/src/tools/subagent.rs` 的 `SubEnded` / `ended_of`；前端渲染在 `ui/src/features/subagent/SubagentItemCard.tsx`；预算数字在 `src-tauri/src/core/prompt.rs` 的 `WORKFLOW_SECTION`。

## 1. 步数预算（`maxSteps`）

子代理有独立步数预算（工具级缺省 `DEFAULT_STEPS = 25`，硬上限 `MAX_STEPS = 1000`）。它是**硬上界**，不是节拍器：drive 主循环 `for step in 0..max_steps` 到点即退出，因此预算是子代理「必须能自动收尾并交出成果」的工程前提，没有它一次派发就可能变成永不返回的长跑，而主代理会被挂住。

`maxSteps` 同时扮演四个角色，改动任一处都必须同步另外三处：

| 角色 | 位置 |
|---|---|
| 子代理的规划依据（写进纪律提示，模型据此决定读几个文件、写几个文件） | `build_system_extra`（`subagent.rs`） |
| drive 的硬循环上界 | `core/agent/drive.rs` 的 `if step >= params.max_steps` |
| `force_report` 的触发点（撞顶前一步注入 `<final-report>`） | `drive.rs` 的 `step == max_steps.saturating_sub(1)` |
| 成果是否到手的判据 | `ended_of`（见 §2） |

预算耗尽**不是无声截断**：drive 层有两道递进提醒——剩余 20% 时注入 `<budget-notice>`（`budget_notice_step = max_steps - max_steps / 5`），最后一步注入 `<final-report>` 要求立即交汇报。

## 2. 收尾形态（`ended`）四象限

**旧判据只看「报告有没有 `<report>` 标记」**，于是 `force_report` 逼出来的半成品汇报与真正做完的汇报同判 `report`——主代理无从知道自己被砍断过，只能读报告正文推断缺口再派一次，探路与实现成本付两遍。现在改为四象限：

| 跑满预算 | 带 `<report>` | `ended` | 语义 | `is_delivered` |
|---|---|---|---|---|
| 否 | 是 | `report` | 主动完成后按约定汇报 | ✅ |
| **是** | **是** | **`partial`** | **撞上限才交汇报**：成果在手，未必做完 | ❌ |
| 是 | 否 | `budget` | 撞上限且未交汇报 | ❌ |
| 否 | 否 | `no_report` | 提前退出且未交汇报 | ❌ |

判据单点定义在 `subagent.rs` 的 `ended_of(steps_used, max_steps, tagged)`（抽成纯函数而非内联三元——四象限是跨模块契约，此前内联写法导致该判定**零 Rust 测试覆盖**）。撞顶判定用 `>=`：`step_count` 是「已启动步数」（每步开头写 `step + 1`），真正跑完预算时恰等于 `max_steps`。

`ended` 落地三处（本轮）：会话日志、`sub:done` 事件载荷、`ToolOutcome.data`。`TargetState` 的映射（[`feat/wait-until-targets`](./wait-conditional-wait.md) 分支上的 `wait_targets.finish`）**尚未合入 main**，属待落地项。

### 已知局限：`partial` 无法区分「被砍断」与「恰好在最后一步做完」

`force_report` 在 `step == max_steps - 1` 注入 `<final-report>`。模型在该步**合规**交汇报（恰好做完了）与「只做了一半」在判据上完全一样，都判 `partial`——本机制没有更细的信号可用（模型不会自报完成度）。

后果：主代理看到 `partial` 时**不能假定工作没做完**，只能读报告正文的未完成清单决定是否补派，否则会对已完成的工作重复劳动。这是刻意的取舍——宁可多一次检查，不可再出现「半成品伪装成成功」。

### 为什么 `partial` 不判 Failed

撞顶的子代理**已经拿到部分成果**（落盘的文件是真的）。判 `Failed` 会诱导主代理整体重派同一任务，把已完成的域无视掉——那正是要避免的浪费。判成功保住成果被承认，同时 `partial` 标记 + 报告正文里的未完成清单让主代理**按缺口补派**，而不是从头再来。

### `analysis_done` 必须按 `is_delivered()` 过滤

pm / tester 子代理返回后置位 `analysis_done`（plan 档 G2 分析闸 + arch 批准闸的放行条件）。旧判据只看「结果是否 Ok」，于是**撞顶交出的半成品需求分析照样解锁批准门**——跨机制的静默提权路径。现要求 `ended.is_delivered()`。

### 线协议注意

`ended` 的值名是**线协议**：前端 `run.types.ts` 与 `ipc/types.ts` 两处**各自独立**声明同一联合类型，`events.contract.test.ts` 只守事件键名、**不守载荷字段**——两端失配没有自动守护，必须同 PR 改，禁止 `as any` 绕过。i18n 文案同理需中英对称。

## 3. 前端徽标：判定方向必须反转

`SubagentItemCard.tsx` 的收尾警示判定是**反向**的：只有 `report` 与缺省（旧数据无 `ended` 字段）算绿勾，其余任何值一律橙警示。

```tsx
// 正确：新增枚举值默认算异常收尾
sub.ended != null && sub.ended !== "report"
// 错误：白名单式——后端新增 partial 时静默漏判，落回绿勾
sub.ended === "budget" || sub.ended === "no_report"
```

警示色用 `var(--ws-warn)` 橙（需注意）；红色在本项目语义是「危险」，`partial` 不属此列。

## 4. 恢复路径

会话恢复时子代理卡**曾恒显绿勾**——`ui/src/stores/run.ts` 的恢复分支不读 `ended` / `steps_used` / `tokens`。现从 `tool_result` JSON 回填（`applyRestoredEnded`），并用白名单 `SUB_ENDED_VALUES` 校验 `ended`（恢复路径读到的是任意历史 JSON，筛不到即当缺省，**不得用 `as` 强转放行未知值**），`steps_used` 回填到 `step`、`tokens` 回填到卡片 token 数。字段缺失时保持 `undefined` / 0，缺省仍是绿勾，向后兼容不得破坏。

## 5. 编排层的预算分档

`prompt.rs` 的 `WORKFLOW_SECTION` 按阶段给角色分配预算。数字取自真实会话的落盘实测（`steps_used` vs `steps_budget`）：

| 阶段 / 角色 | 预算 | 实测 | 依据 |
|---|---|---|---|
| S1 explore | 40 | 19–37，从未跑满 | 下调会削弱调研能力，无数据支撑 |
| S1 product-manager | 30 | 25 | 充裕 |
| S6 开发三角色 | **90** | 41 / 49 / 50 / **60 跑满一次** | 撞顶者报告里另一域完全未动，是自证超预算 |
| S7 reviewer | **45** | 33 | 已接近上限 |
| S7 tester | 60 | — | 跑测试天然长 |

**「把预算调大来治跑满」是花更多 token 买同一场事故**——真正的收益来自 §2 的判据修复与 §6 的切包纪律，预算分档只是配套。

### S6 切包纪律

任务包**只覆盖一个域/一层**；跨域任务必须切成多个文件范围互斥的包再并行派发。把三个域的类型收敛塞进一个 backend-dev，正是它 60 步跑满的直接原因——而实测中该子代理做完了一个域、另一个域完全没动。

### 字数护栏

`WORKFLOW_SECTION` 有 2100 字硬护栏（`prompt.rs` 内测试钉死 `chars().count() <= 2100`，演进史 1510 → 1744 → 2000 → 2100，注释写明「勿无理由膨胀」）。**本轮实测 2090（余量 10 字）**——下次再加机制前必须先实测，撞线则**压缩既有冗余措辞**腾空间（不得删机制锚点），仍不够才可调高预算并在注释写明理由。

⚠️ 跨平台测字数的坑：Windows 上 `core.autocrlf` 检出会把常量块变成 CRLF，按原始字节数会数多出「换行数」个字符（如 2116 vs 2090），看似「改动前就已超线」。**以 Rust 侧 `chars().count()` 为准**（Rust 词法分析把 `\r\n` 归一为 `\n`）。

## 6. 非目标

运行中动态调整预算、子代理续跑（extend）、改并发上限（`MAX_CONCURRENT`）、给 `TargetState` 扩第四态、改 `until.state` 协议——本轮均不做。

## 相关文档

- [standard-workflow](./standard-workflow.md) — 标准工作流 S1–S9（本文件的预算数字与切包纪律的来源）
- [plan-mode-workflow](./plan-mode-workflow.md) — plan 档 G2 分析闸（`analysis_done` 的消费方）
- [arch-orchestrator](./arch-orchestrator.md) — arch 批准闸（`analysis_done` 的另一消费方）
- [subagent-text-turn-premature-exit](./subagent-text-turn-premature-exit.md) — `<report>` 标记与 `force_report` 的引入背景
- [budget-notice-step-fix](./budget-notice-step-fix.md) — 低预算提醒触发点从 `max_steps/5` 修到 `max_steps - max_steps/5`
- [subagent-idle-watchdog-misfire](./subagent-idle-watchdog-misfire.md) — 只读子代理空转层放宽的前提正是「预算天花板交给 max_steps」