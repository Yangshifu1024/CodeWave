# 主会话在计划未完成时静默成功收尾（缺陷修复）

> 日期：2026-10-10 · 分支：`fix/pending-todo-finish` · 基线：`c814b73`
> 相关：[subagent-text-turn-premature-exit](./subagent-text-turn-premature-exit.md)（非主会话同类问题的首次修复）、[rejected-call-silent-finish](./rejected-call-silent-finish.md)（主会话被拒调用维度）、[empty-assistant-and-request-rebuild-fix](./empty-assistant-and-request-rebuild-fix.md)、[git-bash-probe-nonstandard-path](./git-bash-probe-nonstandard-path.md)（同分支另一主题）
>
> 本文件含两批：§1–7 是主缺陷修复，§8 是同分支收口的 4 项遗留（code-review 2026-10-10 提出）。

## 1. 现象与取证

会话 `f19c3890-43d3-45a4-90e8-f01d7b8262c2`（「检查并合并未合并PR」），2026-10-09 23:48。

会话日志 `.codewave/logs/f19c3890-43d3-45a4-90e8-f01d7b8262c2.log` 尾部：

```
568| step 10 llm 完成 … tokens in=1116/out=762
569| step 10 响应形态 tools_sent=23 tool_calls=0 names=[] text≈50字 recovered=false
570| run 60d24d9e 完成 input 10945 / output 3173 tokens 耗时 125s
```

第 50 字正文即历史末条 assistant 消息（`~/.codewave/histories/f19c3890-…/0002.jsonl`）：

> 找到插入点了。在 `shell_invocation_per_variant` 之前插入纯函数单测。

一句「打算做什么」的旁白，没有工具调用。**紧接着 run 正常成功收尾**——无 `run:error`、全局日志无 ERROR、索引 `interrupted: null`。此刻计划表（`sessions/f19c3890-….todos.json`）第 4 项 `in_progress`、第 5–7 项 `pending`：**run 带着半成品计划结束**。

用户侧表现链条：

| 现象 | 成因 |
|---|---|
| 会话进入「已结束」态 | `run:done` 已发，前端 `t.running = false`（`ui/src/stores/runHandlers.ts:134-145`），后端索引同样 `running=false` |
| 发消息直接进聊天窗口、不进队列 | 队列仅在 `running` 为真时生效；已结束即走普通 `send()` |
| 再发一次才继续推进 | 新 run 从历史续上，step 0 即 `edit` 补单测，todos 随即把「实现/补单测」标 completed |

## 2. 根因

> 下文行号除标注「基线」者外，均指本分支（`fix/pending-todo-finish`）现状。

`src-tauri/src/core/agent/drive.rs`（基线 `c814b73`）：

```
1009:  let action = text_turn_action(&joined, params.finish_on_text, rejected, text_turns);
1012:      TextTurnAction::Finish => { break 'steps; }
```

`text_turn_action` 的判定顺序（修复前）：

| 序 | 条件 | 结果 |
|---|---|---|
| ① | `rejected`（调用参数不可解析被拒） | Continue / StopWithLimit |
| ② | `finish_on_text` | **Finish** |
| ③ | 正文含 `<report>` | Finish |
| ④ | `text_turns >= MAX_TEXT_TURNS(3)` | StopWithLimit |
| ⑤ | 其余 | Continue |

主会话 `finish_on_text = true`（`main_drive_params`，`DriveParams::default` 同为 true），因此**任何「只输出文字、无工具调用」的回合都在 ② 直接 break，完全不检查 todos 状态**。

子代理/任务运行 `finish_on_text = false`，靠 `<report>` 收尾，早有护栏；主会话的这个语义是 [rejected-call-silent-finish](./rejected-call-silent-finish.md) 第 96 行明确「逐字节不变」保留的遗留缺口——两次修复都只补了 `rejected` 维度，没人动 ② 的计划状态盲区。

**已排除的收尾分支**（tester 复核）：

- `force_report` / `last_step`（基线 `drive.rs:987`、`1676`）——主会话 `force_report` 恒 false；
- 压缩分支——只 `reset text_turns` / `reset_idle`，不收尾；
- 监督 Escalate（重复失败 / 空转）——产出 `Err`，用户看得见错误，与「静默成功」不符；
- `MAX_STEPS = 9999`（`runtime.rs:18`）——本次仅 11 步，未触顶。

## 3. 方案

### 3.1 `src-tauri/src/tools/plan.rs`

新增纯函数：

```rust
pub fn has_pending(todos: &[Todo]) -> bool {
    todos.iter().any(|t| t.status != TodoStatus::Completed)
}
```

口径与 `tools::batch::plan_gate_verdict` 的三态判定同源：**空列表 = 无计划**（不算未完成，否则每次普通提问都会被拦下）、非空且全 `Completed` = 已收尾、其余 = 未完成。

### 3.2 `src-tauri/src/core/agent/drive.rs`

`text_turn_action` 增第 5 参 `todos_pending: bool`，主会话分支改为：

```rust
if finish_on_text {
    return if todos_pending && text_turns < MAX_TEXT_TURNS {
        TextTurnAction::Continue
    } else {
        TextTurnAction::Finish
    };
}
```

调用点从 `rt.todos` 取快照（`has_pending` 判定与文案渲染共用同一快照，保证「提示里点名的项」与「判定依据」严格同源）。

`rejected` 仍先于本分支判定，非主会话路径逐字节不变。

### 3.3 `<continue-notice>` 文案分变体

仍用同一标签（既有断言与知识沉淀不散），但主会话 + 计划未收尾时换文案（`continue_notice_pending_todos`）：

```
<continue-notice>你在上一回合只输出了文字、没有发起工具调用，而当前计划仍有未完成项：补单测；验证
请立即调用工具推进；若这些待办其实已完成或不再需要，先用 plan 更新计划（把已完成项标为 completed、删除无关项）。
再次只输出文字将被视为收尾。</continue-notice>
```

**为什么不能复用子代理那句**（该句以「全部完成时以 `<report>` 包裹」收尾）：`rt.todos` 跨 run 持久化、且 run 开头从磁盘装载（`run_chat` 装载段），「上个 run 留下的半成品计划」也会命中本门。复用那句等于把模型推向一个它**无法自行验证**的断言；点名未完成项 + 显式给出 `plan` 出口，模型才有办法自己解开。

末句「再次只输出文字将被视为收尾」不是同义反复而是**硬约束**：主会话的 `text_turns` 在纯文本回合不复位（见 §4），第 4 个连续纯文本回合会静默回落 `Finish`——不说破，模型容易拿到同一句提示后原地复读。

## 4. 上限回落：为什么是 `Finish` 而不是 `StopWithLimit`

连续纯文本仍不收敛时，**回落 `Finish`**（维持修复前行为）而非 `StopWithLimit`。

**实际触发点是第 4 个纯文本回合，不是第 3 个**：`text_turns` 的复位点在「有工具调用的回合」末尾（`drive.rs` `text_turns = 0`），而纯文本回合走 `continue 'steps` 跳过它，故计数为 `0→1→2→3`，只有第 4 个才满足 `text_turns >= MAX_TEXT_TURNS`。即计划未收尾时**最多强制续跑 3 轮**后回落收尾，绝不无限续跑。

子代理可以「宁可显式失败让主代理重派」，主会话不行：那里「显式失败」= 对一次普通提问弹 `run:error`，比多几轮对话更糟；计划表在右栏可见，信息不丢。这是有意的分档取舍。

## 5. 测试

| 用例 | 位置 | 作用 |
|---|---|---|
| `has_pending_distinguishes_unfinished_plan` | `tools/plan.rs` | 空列表/全完成/含 in_progress/含 pending 四态 |
| `text_turn_action_matrix` | `core/agent/tests.rs` | 既有 13 处补 `todos_pending` 参数（原断言不变） |
| `text_turn_action_matrix_pending_todos`（新） | 同上 | 新维度矩阵：未收尾→Continue；达上限→**Finish**（非 StopWithLimit）；`rejected` 仍优先；已收尾→Finish |
| `continue_notice_pending_todos_lists_only_unfinished_and_gives_escape`（新） | 同上 | 文案纯函数单测：**已完成项不得被点名**（防止 `has_pending` 与文案 filter 漂移）、含 `plan` 出口、含「再空转即收尾」、**不含** `<report>`（主会话语义）、空标题不 panic |
| `main_session_text_only_turn_with_pending_todos_continues`（新） | 同上 | 端到端复刻 f19c3890：预置 1×in_progress + 1×pending，三回合脚本（旁白 → `plan` 标全完成 → 收尾），断言连接数 ≥3、提示仅注入 1 次、最终 `has_pending` 为 false |
| `main_session_text_only_turn_ends_run` | 同上 | 对照组：todos 为空的主会话纯文本回合仍 1 连接结束（逐字节不变） |
| `main_session_text_with_rejected_call_does_not_silently_finish` | 同上 | 被拒路径仍走 `<tool-args-rejected>`、不注入 `<continue-notice>`（`rejected` 优先级守护） |

### 反向验证（判别力）

临时把判定改为忽略 `todos_pending`（= 修复前行为）后：

```
test core::agent::tests::main_session_text_only_turn_with_pending_todos_continues ... FAILED
assertion `left == right` failed: 计划未收尾的纯文本回合不得静默收尾
  left: 1        ← 第 1 个纯文本回合即 Finish（正是事故形态）
 right: 3
```

`text_turn_action_matrix_pending_todos` 与端到端用例亦转红。还原后全量绿。

## 6. 验证结果

> **计数口径**：本分支工作区还携带**上一会话遗留的未提交改动**（`tools/command/tool.rs` + `tests.rs`，Windows Git Bash 非标准安装探测，已向用户报备并归入另一主题）。下方总数含那 3 个 `command` 用例，**本批自身新增 5 个后端测试**（`has_pending_distinguishes_unfinished_plan`、`text_turn_action_matrix_pending_todos`、`continue_notice_pending_todos_lists_only_unfinished_and_gives_escape`、`main_session_text_only_turn_with_pending_todos_continues`、`stale_pending_todos_do_not_gate_plain_qa`）。
>
> 下表是**本分支开发期间**的快照；合并 steer 后与清完 clippy 的**当前 main** 数字以 [AGENTS.md](../AGENTS.md) 为准（`cargo test --workspace` 1177 passed / 3 ignored、`pnpm --dir ui test` 1229 passed / 102 文件）。差异来自 main 侧 steer 的用例与文件，不是本批回归。

| 检查 | 结果（开发期快照） |
|---|---|
| `cargo fmt --all -- --check` | 干净（exit 0） |
| `cargo clippy --all-targets` | 无新增警告（当时存量：`tools/postcheck.rs` unused import、`core/openers/mod.rs` unneeded return——二者已在 `8e73d7d` 清零，**当前为 0 warning**） |
| `cargo test --workspace` | **1171 passed / 0 failed / 3 ignored**（含上一会话遗留的 3 个用例） |
| `pnpm --dir ui test` | **1226 passed / 102 文件**（新增 `internal-hint-render.test.ts` 5 例） |
| `pnpm --dir ui build` | 通过 |

本分支新增用例清单：`has_pending_distinguishes_unfinished_plan`、`text_turn_action_matrix_pending_todos`、`continue_notice_pending_todos_lists_only_unfinished_and_gives_escape`、`main_session_text_only_turn_with_pending_todos_continues`、`stale_pending_todos_do_not_gate_plain_qa`（后端 5） + `internal-hint-render.test.ts` 5 例（前端）；`tools/command/tests.rs` 的 3 个属另一主题（见 [git-bash-probe-nonstandard-path](./git-bash-probe-nonstandard-path.md)）。合计 **8 个后端 + 5 个前端**。

## 7. 明确不做（非目标）

- **不改 `finish_on_text` 默认值**，不动子代理/任务运行的 `<report>` 收尾语义。
- **不改事件面**（`run:done` / `run:error` 契约零变化），不新增 IPC。
- **不把 `core → tools::plan` 记为新依赖边**：该方向本仓早已存在（`core/agent/runtime.rs:103` 的 `rt.todos`、`core/agent/stream.rs:49` 的 `render_todos`、`core/sessions/store.rs:1291` 的 save/load），本批只是多一个消费点，`core` 依然不依赖 tauri（分层硬约束未破）。

## 8. 遗留项（已于 2026-10-10 同一分支收口）

本节的 4 项遗留已全部实现，见 [git-bash-probe-nonstandard-path](./git-bash-probe-nonstandard-path.md)（同批另一文档）。

### 8.1 suggest 收尾路径 gate（原 🟡）

**问题**：suggest 是模型显式的「我做完了」信号，经 `BatchOutcome.suggest_items` 让 `drive_agent` 置 `batch_done` 正常成功收尾——**完全绕过** `calls.is_empty()` 分支里的收尾判定。只修 `text_turn_action` 的话，模型改用 suggest 就能绕开未完成计划。

**实现**：拦截点**前移到工具层**（`tools/suggest.rs` 的 `plan_pending_blocks_suggest`），因为 `run()` 在 emit `run:suggestions` **之前**就返回 ok——到 `drive.rs` 的 `batch_done` 判定时，前端 chips 已出现，为时已晚。

主会话 + 本 run 碰过计划 + `has_pending` → 返回 `E_PLAN_PENDING`（终结性提示，含 `plan` 出口），建议不发出、`suggest_items` 为 `None`、`batch_done` 自然不置位。`drive.rs` 的 suggest 分支旁加了「勿新增旁路」的依赖注释。

**事件面零改动**：`run:suggestions` 被拦截后不再 emit，前端自然不触发，`events.contract.test.ts` 不受影响。

### 8.2 陈旧计划劫持普通提问（原 🟡 → 根治）

**实现**：新增 `SessionRuntime.plan_called_this_run`（照抄 `plan_hint_emitted` 的三段式：`runtime.rs` 字段+构造、`drive.rs` 的 `run_chat` 起点复位、`tools/plan.rs` 写 `rt.todos` 成功后置位）。

收尾门（`text_turn_action` 的 `todos_pending` 与 suggest 的 `E_PLAN_PENDING`）**只看本标记**：本 run 没调 plan 工具就不拦。依据：生产路径里 `rt.todos` 的唯一写者是 plan 工具（已核实——`batch.rs` 的 `todos.push` 在测试夹具 `g3_scope_fixture` 里，非生产路径），故该标记能唯一区分「本 run 的计划」与「上个 run 的遗留」。

**语义边界（有意取舍）**：用户发新消息续跑陈旧计划时，若模型本 run 没有再调 `plan`，门不生效——即「续跑」得靠模型主动更新计划。这是刻意的：模型若真在推进陈旧计划，它必然会调 `plan` 更新状态；不调就说明它在纯问答，不该拦。

### 8.3 `ContinueWithReminder` 死变体（原 🟢 → 已删）

全仓零构造点，目标模式删除时的存量遗留。删除：枚举变体+注释、`let _reminder` 整行、`match` 臂合并写法。枚举不实现 Serialize、不出 wire、`ui/src` 零引用 → 纯删减。

### 8.4 内部提示泄漏到历史展示（原 🟢 → 已修）

**实现**：前端 `buildTranscript`（`ui/src/stores/run.ts`）新增 `isInternalHint`，命中七个标签（`<continue-notice>` / `<tool-args-rejected>` / `<text-turn-limit>` / `<budget-notice>` / `<final-report>` / `<supervision-notice>` / `<supervision-escalated>`）时渲染为 `kind: "notice"` 灰色行，文案走 i18n 新增的 `notice.internalHint`，不展示 XML 原文。

**为什么只处理「整条恰好是标签」**（不做 `includes` 宽松匹配）：后者会把用户自己在正文里提到 `<continue-notice>` 的提问也误判成内部提示。压缩摘要（`handoff-summary`，无标签、含正文）沿用既有的 `includes` 分支，不受影响。

**为什么过滤只放前端**：后端 `load_history_wire`（出网给 `rt.history`）与 `first_page`（展示给前端）是两个不同函数、两份不同数据。若在 wire 侧过滤，模型就看不到续跑提示 → **重演「提示注入却永不被模型看到」的静默成功缺陷**（正是本批修复的同类）。

**只改展示**：落盘数据、恢复数据、wire 三者保持一致，模型仍能在历史里看到这些提示。

**已知缺口（有意不收）**：`drive.rs` 另有两处注入 **无标签**的 user 消息——`[system] 权限模式已变更为 …`（子代理档位变更）与 `[system] 方案已获批准，会话已切换到…`（方案批准）。它们恢复后仍会以 user 气泡显示原文。本批**不纳入**，因为它们没有成对标签，只能改用 `startsWith("[system] ")` 这类**宽松前缀**判据——与「整条恰好是一个标签」不同质，且会把用户自己以 `[system] ` 开头的消息也吞掉。若日后要收，应单独评估该前缀的假阳性风险。

### 8.5 同批附带：补上一会话遗留改动的文档

工作区还携带上一会话（`f19c3890`）的未提交改动：`tools/command/tool.rs` + `tests.rs` 的 Windows Git Bash 非标准安装路径探测（184 行），此前无文档。已补 [git-bash-probe-nonstandard-path](./git-bash-probe-nonstandard-path.md) 并登记 0-README。**只补文档，未改那 184 行代码。**

## 9. 手动验证清单（GUI，不做自动点验）

1. 主会话建一条 3 项计划（1×`in_progress` + 2×`pending`）；
2. 让模型在计划未完成时只说不做（例如「先说一句你接下来打算做什么」）；
3. **预期**：不静默结束——会话继续跑，且转录里出现点名未完成项的续跑提示；
4. 让模型调用 `plan` 把全部标 `completed` 后再收尾 → 正常结束；
5. 另开一个**无计划**的会话问一句普通问题 → 仍是「回答即结束」（对照组，行为不变）；
6. 计划未完成时让模型调 `suggest` 收尾 → 应拿到 `E_PLAN_PENDING` 错误、建议 chip **不出现**、会话继续跑（验证 §8.1）；
7. 在留有半成品计划的会话里问一句与计划无关的普通问题 → **应直接答完**（§8.2 的陈旧计划豁免，不再被拉长）；
8. 重开一个发生过续跑的历史会话 → 内部提示应显示为灰色「（系统内部续跑提示）」行，**不出现 XML 标签**（验证 §8.4）。