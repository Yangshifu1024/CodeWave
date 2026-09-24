# 目标模式（Goal Mode）

> 2026-09-24 · 新增第五档会话管控档位 `ApprovalMode::Goal`：**设定目标 → 澄清 → 一次批准 → 自主推进到达成**，执行期零提问、零弹窗。
> 分支 `feat/goal-mode`，基线 `0b9ec35`；验证：本地门禁 `pnpm prepr` **8/8 全绿**（`cargo test` **1111 passed / 0 failed / 3 ignored**、`pnpm --dir ui test` **1109 passed / 95 文件**、`ui build` 通过、`fmt` 通过、`scripts` 24 通过、`clippy` 软步骤退出码 0）。

## 1. 要解决的问题

现有四档（逐项确认 / 自动编辑 / 计划档 / 完全访问）都在回答「允许模型做多少」，没有一档回答「模型该做到什么、做完了没有」：计划档止步于方案获批，完全访问档放行一切但仍以「模型输出纯文本 = 回答完毕」结束（`finish_on_text`）。用户若想「给个目标、别烦我、做完再叫我」，只能人工反复催问。

目标模式把「一次澄清成本」换成「整段无人值守执行」，核心价值是**确定性**（达成标准与改动面事前写死）与**不被打断**。

## 2. 用户可见行为

### 2.1 入口（无触发词）
档位下拉菜单与 `Shift+Tab` 循环里新增第五档「目标模式」（⚡ 图标、橙色 = 需注意级；沿用 `approval-*` 类名约定，未引入新色系）。**刻意不做 `/goal`、`$goal` 触发词**：`/` 与 `$` 在仓库里是「技能点名」与「子代理点名」的纯文本信号（后端无硬路由），塞进第三种语义会让「以为点名了其实没点名」的概率上升；档位切换是确定性的。

切入后 Composer 出现提示条，且**下一条消息即目标陈述**（切档本身不启动 run，必须有一条消息）。

### 2.2 澄清阶段（只读）
- 工具集 = 只读工具 + `ask` + 只读子代理；**排除全部文件写工具（含 `write_document`/`edit_document`）、`command`/`service`/`scheduled_task`、`http_request` 与 MCP**——「澄清不动任何东西」由工具排除和批次硬门共同保证。
- 澄清方法论内置在档位提示块里（`<goal-mode>` 澄清期文案），**不依赖用户侧是否装了 grilling 技能**：逐轮结构化提问（每问附推荐答案）、设计树前沿为空才停、不设轮次上限。
- 三份产物：目标陈述 / 可验证的达成标准清单 / 预期改动面与运行时账本，由 `goal` 工具登记（`status: clarify`）。

### 2.3 批准点（全程唯一一次被问）
澄清结束时 `ask` 单题询问（选项「开始推进」/「补充意见」），汇总全文走 ask 的 `plan` 字段落盘为计划文件 + 计划卡可查看（复用现成链路，零契约改动）。批准即：
- `goal.status: clarify → executing`（批准动作本身驱动，不依赖模型再调一次工具）；
- 记录**进入目标档前的档位快照**（`SessionRuntime::goal_prev_mode`）；
- **不**冻结 `approved_plan` 基线 → 计划外步骤确认门（G3）在目标档下恒不成立；
- 未登记目标或达成标准为空时**拒绝批准**（不允许「没有合同的执行」）。

### 2.4 执行阶段（零提问）
- **`ask` 从工具集里被物理移除**——「执行期不提问」不是承诺而是能力缺失。
- 纯文本收尾改为**继续推进**：注入一条瞬态推进指令（第 N 轮 / 目标 / 未达成标准 / 账本摘要 / 三条硬约束：不得提问、遇歧义按「最小惊讶 + 可回滚」自决并记入 `decisions`、未达成前不要停）。推进指令**只进本次请求、不写入会话历史**（复用 `<current-plan-transient>` 的瞬态机制），界面只留轮次感。
- 文本收尾上限：目标档 8 轮（先提醒、超限收尾出报告）；**非目标档仍是原有的 3 轮硬终止**（回归红线，有测试钉死）。
- 停滞检测（双信号两段式）：进展信号 = 有非只读工具调用 ∨ 达成标准勾选状态变化；连续 5 步无进展 → 提醒，连续 10 步 → 自停出报告并置「已暂停」。
- 空转看门狗（`supervise.rs`）在目标档两阶段都改用 `IdlePolicy::NudgeOnly`（只提醒不终止）——默认的「14 批空转即终止」会误杀长只读调研；失败重复层与步数门照常。

### 2.5 账本（安全模型）
澄清阶段把「预期改动面」编译成运行时账本：**路径白名单 + 程序白名单**。执行期：
- **文件写入**：目标路径必须落在账本路径内，否则工具直接返回 `E_GOAL_OUTSIDE_LEDGER`（不弹窗、不问人，模型自行换方案或写进收尾报告）。
- **命令**：只接受单条简单命令，拒绝 shell 组合、重定向和嵌套 shell；程序名过账本。fence 对可静态识别的工作区内写 / 工作区外新建目标采用写目标探针，目标路径也过账本。`http_request` 与 MCP 在执行期同样排除，因为它们尚无账本适配。
- **L1/L2 硬拦与 L3 灾难/高危**：硬拦 + 记入 `blocked` + 请求硬停 → 本轮收尾出报告，等用户回来。
- 越界计数 `ledger_denials` 累计到 3 → 判为账本漂移，自停出报告（收尾成因 `LedgerDrift`，优先于通用 `Blocked` 呈现）。
- 白名单本身的护栏：根路径 / 家目录本身 / 系统路径（含 `C:\Windows`、`/etc`、`Program Files`、Windows 8.3 短名形态）一律不可作为账本条目；脏条目不生效（≠ 整盘授权）。
- 账本**宁宽勿窄**的引导写进了澄清期提示块（执行期无法再问用户，漏列程序的代价是自停）。
- **子代理的写入同样过账本**：闸门判定作用域取**根会话**的目标（子代理 runtime 本身不持目标），并行开发包绕不过账本；子代理越界也**记在根会话**的 `ledger_denials` 上，3 次同样触发父会话的漂移自停。
- 每次越界被拒的目标会追加进 `blocked`，所以收尾报告里能看到「到底越了什么」——这是「漂移 → 自停 → 用户回来补授权」这条闭环的最后一环（复用既有 `blocked`，不新增字段）。

### 2.6 暂停 / 续跑 / 收尾
- 用户点停止 = **暂停**：状态置 `paused`、目标与进度全保留；「继续推进」按钮（Composer 提示条 + 右栏目标段）调 `resume_goal` 起一轮新 run（`status` 回 `executing`；起跑失败自动回滚为 `paused`）。普通发消息会被 `E_GOAL_PAUSED` 拒绝，防止绕过显式续跑。该命令是**可重放**的，故 host 与 core 两层都校验「当前档位必须是目标模式」（`E_GOAL_MODE_REQUIRED`）——否则会出现「档位已切走、目标却回到执行中而闸门不生效」的静默空窗。
- **切档 = 账本失效**（刻意取舍）：闸门与档位严格绑定，因此完全访问档不会在目标暂停后被静默降级成「只能写账本内」；代价见 §7。
- 用户切走档位 = 目标自动进入「已暂停」（不锁档）；切回目标档不自动续跑，需点「继续推进」。
- 执行期用户主动发消息：作为补充上下文注入当前 run（不等回答、不打断目标）；明确说改目标 = 重开澄清。
- 达成（模型用 `goal` 工具把最后一条标准勾完并置 `status: done`）→ 输出**收尾报告**（达成标准逐条结果 / 实际改动 vs 账本 / 自行决策 / 待批项 / 被拦项）→ 发 `goal:update` → **自动回落到进入目标档前的档位**（快照为空则回落全局默认）→ 目标存档保留可回看。

### 2.7 清单分工
- `goal` 的达成标准清单 = **验收合同**：澄清期由用户批准，执行期条目文本与账本锁定（改则 `E_GOAL_CONTRACT_LOCKED`），模型只能改勾选状态与追加 `decisions`/`pending`。
- `plan` 的 todos = **执行工作台**：模型自管，照旧。右栏两段并列。

## 3. 实现地图

### 后端（`src-tauri/src/`）
| 文件 | 内容 |
|---|---|
| `core/prefs.rs` | `ApprovalMode::Goal`（wire `goal`，`Plan` 仍是默认档）；Goal 的 `confirm_inside_writes()`/`plan_readonly()` 均为 false（澄清期只读由工具集实现）；**前档快照刻意不放这里**（prefs 是前端整体替换写的事实源，放了会被补丁冲掉），有测试钉死 wire 字段集 |
| `core/agent/goal.rs`（新） | `GoalState`（text/criteria/ledger/status/decisions/pending/blocked/rounds/stall_streak/ledger_denials）、`goal_phase`、`render_goal_block`（阶段化提示块）、`render_goal_summary`、`ledger_allows`（含禁用路径与**宽松归一化**）、`stall_verdict`、常量 `STALL_NUDGE_AT=5`/`STALL_STOP_AT=10`/`GOAL_TEXT_TURN_LIMIT=8`/`LEDGER_DENIAL_RETRY_LIMIT=3`；**运行时闸门** `goal_execute_phase` / `ledger_gate` / `goal_hard_block` / `ledger_denial_message` / `prev_mode_to_record` / `program_of_command` |
| `core/agent/runtime.rs` | `goal: Mutex<Option<GoalState>>`、`goal_abort`、`goal_prev_mode` 及访问器；`transition_prefs`（进档记快照 / 离档暂停目标） |
| `core/agent/drive.rs` | `apply_goal_mode`（两阶段工具集与 `system_extra`，从基座重建、每步重算）；`text_turn_action` 加 `goal_execute` 维度；瞬态推进指令；`goal_bookkeep`/`goal_close_out`/`goal_account_step`/`bump_goal_round`/`render_goal_advance`/`render_goal_report`；`mark_cancelled` 置暂停；`pause_goal_if_mode_left`（每步兜底：档位已切走则暂停执行中的目标）；`cancel_session_subagents`（收尾时取消本会话在跑子代理）；会话恢复装载 `store.load_goal` |
| `core/agent/stream.rs` | 请求组装支持尾部瞬态（计划快照 + 推进指令），**绝不写入历史** |
| `core/prompt.rs` | 新增 `GOAL_MODE_SECTION`（`<goal-mode-protocol>`，逐出 `WORKFLOW_SECTION` 的 2000 字预算：后者实测已 1995 字） |
| `tools/goal.rs`（新） | `goal` 工具（Meta）：澄清期登记/修订，执行期合同锁定与 `done` 门 |
| `tools/ask/tool.rs` | 目标批准分支（置 executing、不冻基线、补记前档）；`wants_mode_switch`/`mode_label` 新臂；schema `mode` 枚举补 `goal` |
| `tools/batch.rs` / `tools/command/tool.rs` / `tools/service.rs` | 账本闸门接线（写入路径 / 程序名 / 需确认级携带的写目标），判定作用域统一取根会话（`goal_gate_rt`）；G3 显式排除目标档；**目标档执行期豁免 plan 纪律门**（`E_PLAN_REQUIRED`/`E_PLAN_STALE`，范围控制改由账本承担）；目标档执行期命令免确认（合法性由账本承担） |
| `core/sessions/store.rs` / `cleanup.rs` | 边车 `sessions/<id>.goal.json`（原子写）+ 级联删除 + 孤儿清理 |
| `host/commands/session.rs` / `lib.rs` | `resume_goal(session_id, on_event) -> run_id`、`get_session_goal`；`set_session_prefs` 改走 `transition_prefs` |
| 事件面 | **29 → 30 键**：新增 `goal:update`（载荷 `{session, goal}`，`goal: null` = 清除） |

### 前端（`ui/src/`）
`ipc/types.ts`（类型 + `ApprovalMode` 加 `goal`）、`ipc/client.ts`（`resumeGoal` 带 `Channel` 返回 run_id、`getSessionGoal`）、`stores/run.ts`（`goal` 运行态、`resumeGoal` 含本地用户气泡回显、`syncGoal` 代际守卫）、`stores/runHandlers.ts`（`goal:update` + 达标/`run:done` 回读 prefs）、`stores/sessions.ts`（`syncPrefs` + 序列号守卫）、`features/chat/Composer.tsx`（五档菜单 + 提示条 + placeholder）、`features/shell/RightBar.tsx`（「目标」段：状态徽标/轮次/正文/标准勾选/账本摘要/继续推进）、`utils/goal.ts`、`theme/app.css`、`i18n`（中英 16 键）、`utils/rightbarPrefs.ts`、`features/subagent/SubagentDrawer.tsx`。

### 与既有机制的边界
- **标准工作流**：分档路由保留；S1–S5 的确认部分合并进澄清阶段（澄清末尾那一次批准即 S5），S6–S9（并行开发 / 审查 / 收尾产物）照旧。
- **G2 保留**（`E_PLAN_ANALYSIS_REQUIRED` 只是错误码、不打扰人）；**G3 关闭**（它会弹窗要人批，与零提问直接冲突）。
- **子代理**：档位跟随根会话（允许写），收到只读「目标 + 本包任务」摘要；**显式排除 `goal` 工具**（子代理 runtime 不继承目标，调用会写出脏边车）。
- **临时会话 / 计划任务**：不受影响（任务 runtime 仍显式 `FullAccess`；档位不持久化，重启回落全局默认，与原四档一致）。

## 4. 关键设计取舍（含执行中的修正）

1. **前档快照放 `SessionRuntime` 而非 `SessionPrefs`**：prefs 是全量覆盖写的事实源（`updatePrefs` 走 `{...prev, ...patch}`），纯后端运行时状态放进去会被前端补丁冲掉；顺带消掉 19 处结构体字面量改动。
2. **澄清期连 `command` 一起排除**：决策原文是「只读工具 + ask，不含写工具」，而 `command` 能通过 shell 写入——排除它才让「澄清只读」成为机制而非自律。
3. **账本路径比较用「宽松归一化」**（`normalize_existing`）：账本条目与工具目标路径可能来自不同解析链，实测 Windows 上 `tempfile` 给出 8.3 短名 `C:\Users\YANGZH~1\…` 而 `canonicalize()` 得到长名 `C:\Users\Yangzhenbiao\…`，纯文本比较会把**同一条路径**判成越界（三次即自停）。做法：解析「已存在的最近祖先」后拼回不存在的尾巴；同时剥 `\\?\` verbatim 前缀、UNC 还原、补 8.3 短名形态的系统路径兜底。
4. **收尾成因顺序**：`LedgerDrift` 判定提到通用 `take_goal_abort` 之前（越界达上限时两者都成立，报告取更具体的那个）并消费标记避免重复收尾。
5. **`resume_goal` 的本地用户气泡在起跑前 push、失败回滚**（而非 `await` 之后）：`run:start` 的 handler 会在 items 末尾建流式助手项，若 await 之后才 push，用户气泡会落到助手项下方并破坏「流式助手项恒为末项」的不变量。
6. **文案 `继续推进` 刻意不做 i18n**：它由后端写进持久化历史（`RESUME_GOAL_TEXT`），国际化会让实时视图与重开会话后的历史文案漂移。
7. **目标执行期豁免 plan 纪律门**：批次层的 `E_PLAN_REQUIRED`/`E_PLAN_STALE` 原本**不看档位**，会让目标执行期第一次 `edit` 就被拒（目标档提示块通篇讲账本与验收标准、只字未提「必须先建计划」），与「零提问 + 自主推进」直接冲突；现改为目标档执行期豁免，范围控制由账本承担（非目标档逐字不变，有**反向测试**钉死仍会被拒）。
8. **子代理账本作用域取根会话，而非给子 runtime 继承快照**：快照会与「工具集按父档每步实时重建」脱节（父在澄清期 spawn、随后进执行期时，子拿到写工具但快照仍是澄清期 → 缺口只是变窄没消失）；改为判定点直接解析根会话目标。同时新增 `SessionRuntime::mutate_goal`（读-改-写全程持锁），否则父会话与并行子代理改同一份目标会丢计数、自停阈值永不触发。
9. **停止 / 收尾时取消本会话在跑的子代理**：`cancel_run` 只取消主会话 token，子代理的 `parent_cancel` 只在 spawn 内派生 → 原先会出现「UI 显示已停止、子代理还在写文件」（目标档下更糟：目标已暂停而子代理仍按执行期账本动工作区）。现 `mark_cancelled` 与 `goal_close_out`（覆盖全部 5 种收尾原因，含达成）都会**按根会话过滤**后取消（`core.subs` 是全进程注册表，不过滤会误杀其它会话）。
10. **`goal_approval` 只让「最终档位是目标档」的批准推进状态**：否则「打开时是目标档、用户却选了自动编辑档」会留下「档位非目标档 + 目标 executing」的不一致（判据保留宽松以免模型漏声明 `mode` 时批准失效，迁移收紧）。

## 5. 验证

- 后端：`cargo test --workspace` → **1111 passed / 0 failed / 3 ignored**（基线 1025，新增 86 例）。覆盖：档位 serde 与语义矩阵、阶段化工具集、纯文本改继续 + **非目标档仍 3 轮**、停滞两段式、合同锁定、账本匹配与禁用路径、越界计数与硬停、越界项可见化、命令免确认、G3 关闭、**plan 纪律门在目标档豁免（含反向）**、暂停/续跑/档位守卫/回滚、前档快照、边车持久化与级联清理、瞬态指令不入历史、子代理排除与摘要注入、**子代理账本作用域**、**收尾取消子代理（含不误杀其它会话）**、`service` 账本接线。
- 前端：`pnpm --dir ui test` → **1109 passed / 95 文件**（基线 1076 / 93）；`pnpm --dir ui build` 通过；`events.contract.test.ts` 键数锚点 29 → 30 + `goal:update` 载荷形状断言（前端 handler 链被真实走过一遍）；`ui lint` 仅 1 条**存量** warning（`Composer.tsx:240`）。
- 本地门禁：`pnpm prepr` → **8/8 全绿**（lockfile / rust-fmt / rust-clippy（软，exit 0）/ rust-test / ui-lint / ui-test / ui-build / scripts-test）。
- 界面改动不做 GUI 自动点验，手动清单见 §6。

## 6. 分步手动验证清单

1. `pnpm tauri dev`（仓库根；先确认没有 dev / 打包实例占用 bundle id）。
2. 任一项目会话 → 输入区左下权限胶囊 → 菜单应有**五档**，第五项「目标模式」为**橙色 + ⚡**，说明文案「设定目标后自主推进；澄清完毕就不再打扰你，直到目标达成。」
3. 选中后：胶囊变「目标模式」（橙），输入框 placeholder 变「描述你想达成的目标」，输入卡上方出现澄清期提示条。
4. 输入框内按 `Shift+Tab` 循环：变更前确认 → 自动编辑 → 计划模式 → **目标模式** → 完全访问 → 回环。
5. 发一条目标消息 → 进入澄清阶段（模型用结构化提问、每问带推荐答案；此阶段不修改任何文件、不跑写命令）。
6. 澄清末尾出现唯一一次批准卡（可「查看完整计划」看到目标/达成标准/账本全文）→ 点「开始推进」。
7. 执行期：提示条变「目标推进中（第 N 轮）」；**期间不应出现任何审批弹窗或模型提问**；模型在账本内写文件、跑命令。
8. 右栏「信息」页出现「目标」段（与「当前计划」段并列）：状态徽标、已推进轮次、目标正文、达成标准勾选、账本「路径 N 项 / 程序 N 项」。
9. 点停止 → 目标变「已暂停」，提示条与右栏出现「继续推进」按钮；点击后应继续（且聊天流出现一条「继续推进」用户气泡）。
10. 执行中把档位切走（Shift+Tab）→ 目标自动变「已暂停」且状态保留；切回目标模式后点「继续推进」可接上。
11. 让模型声明达成 → 收尾报告输出、状态变绿「已达成」、**权限胶囊自动回落切档前的档位**、提示条消失。
12. 重开应用 / 切换会话：右栏「目标」段能恢复显示（`get_session_goal` 回读 + `goal:update` 推送）。

## 7. 已知边界与后续可做

- 澄清期**不能跑只读命令**（`command` 被整体排除，只用 `read`/`grep`/`list_files` 等）。若希望澄清期允许只读 shell，需把「只读命令」判定下沉到 fence 策略并按阶段透传。
- 账本执行期不可扩展：漏列程序/目录会导致越界拒绝（累计三次自停）。缓解：澄清期提示块要求「写实+宁宽勿窄」（列会真正调用的**可执行程序名**与每个会改动的目录）；被拒项会进 `blocked` 供用户回来修订；后续可考虑「账本扩展申请 → 用户一次性补批后继续」。
- `resume_goal` 起跑失败会回滚为「已暂停」，但**不会**自动重试；用户需再点一次。
- 命令账本只能约束程序名与 fence 可静态识别的写目标，不能沙箱化已获准程序内部的文件写入。例如允许 `cargo` 后，它的内部写盘行为不逐文件受账本约束；需要严格限制改动范围时，应只批准文件工具操作，不在程序白名单中放入可能写文件的程序。复合 shell、重定向、嵌套 shell 均被拒绝，模型需拆成独立调用。
- 目标档下的上下文压缩会把推进指令挤掉（瞬态内容不参与持久化，压缩按历史计算），超长目标依赖目标块每步重新注入。
- **切档 = 账本失效**（刻意取舍）：闸门与档位严格绑定（`goal_execute_phase` 含档位门），这样完全访问档不会在目标暂停后被静默降级成「只能写账本内」；代价是用户切走档位后，账本对该档位不再有约束力（目标此时会显示「已暂停」）。若将来需要「切档但保持账本」，正确做法是新增显式的「归档」状态并告知用户，而不是让闸门在别的档位上生效。
- **子代理越界计入根会话**：并行子代理越界 3 次会让父会话走 `LedgerDrift` 收尾（并取消在跑子代理，见 §4.9）。这是刻意选择（账本是执行期的唯一范围约束，子代理越界同样是漂移）；若要「子代理只拒不自停」，需为子代理单独计数——会新增状态，当前合同已冻结故不做。
- `resume_goal` 与 `set_session_prefs` 在**同一会话**上的并发（TOCTOU）目前只有档位/状态守卫，没有会话级互斥锁；两处都基于 `running` 与目标状态判断，窗口极小但存在。
- `host` 层 `resume_goal` 与 `core::AgentCore::resume_goal` 的档位守卫是**两层同样文案**（共享常量 `GOAL_RESUME_MODE_REQUIRED`），改动时须同步。
- 暂未做：多目标队列、目标模板、跨会话共享目标、账本可视化编辑、`/goal` 触发词。
