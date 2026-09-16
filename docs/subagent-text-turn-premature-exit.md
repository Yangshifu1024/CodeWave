# 缺陷修复：子代理「无工具调用回合」被当作最终汇报提前退出

> 类型：缺陷修复（P0 分类：缺陷）· 影响层：`core/agent/drive.rs`（主循环收尾判定）+ `tools/subagent.rs`（纪律与事件）+ 前端子代理卡展示 · 契约影响：零破坏（事件键名不变，`sub:done` 增可选字段）
> 分支：`fix/subagent-early-exit`（基线 `main`，commit `63a8147`）

## 1. 问题现象

用户在同一会话（`661269b4-fdb1-436a-9e08-bcfeaa1306b5`）连续观察到多个子代理「成功」收尾，但汇报内容是**过程旁白**、任务明显未完成：

| 子代理 | 卡片 | 返回的汇报 |
|---|---|---|
| `sub_9c82d42d` | ✓ 22/80 | `Now I have a complete picture. Let me make the AppShell edits.` |
| `sub_9196c679` | ✓（预算 70） | `反向验证 ① 成功（旧顺序下新用例失败）。现在还原修复并做 ② 的反向验证。` |

两例都是「话说到一半就结束」：子代理明确表示接下来还要动手，却已以成功态终止，剩余 50+ 步预算被浪费。

## 2. 根因

主循环 `drive_agent` 的收尾判定（`drive.rs`，基线 489 行）：

```rust
// ⑨ 组装 assistant 消息
let (assistant_msg, calls, synth_results) = build_assistant_message(&assembled);
let joined = assembled.joined_text();
if !joined.is_empty() {
    final_text = joined;          // ← 只在本回合有文本时覆盖
}
rt.history.lock().unwrap().push(assistant_msg.stamped());

let last_step = params.force_report && step + 1 == params.max_steps;
if calls.is_empty() {
    break 'steps;                  // ← 不写 outcome：成功态收尾，final_text 即「最终汇报」
}
```

该分支为**主会话 / 子代理 / 计划任务三类 run 共用**。对主会话而言「无工具调用的 assistant 回合 = 回答完毕」是正确语义；但对子代理/任务而言，模型**只是打了个旁白**（打算下一回合再动手）也会命中此分支 → 以成功态结束，`final_text` 被当成最终汇报。穷举 `drive_agent` 内全部 `break 'steps` 与终止路径，其余均写入 `Err`（前端表现为 `sub:error`）或需要 step 跑到预算上限，只有这一处能产生「✓ + 旁白文本」形态。

### 2.1 两例的具体入口

**例一（`sub_9c82d42d`）**：某回合为纯文本（无 tool_call）。

**例二（`sub_9196c679`）**：过程历史最后三步为 `assistant: text[38字] + tool_use(read)` → `tool: tool_result(ok)` → `assistant: (空 content)`。即该回合除一个**参数 JSON 无法解析被拒**的调用外没有任何有效文本：

- `stream.rs::build_assistant_message` 对参数不可修复的调用**拒绝执行**（不伪造 args），只把它记进 `synth_results` 且**不为它生成 `tool_use` 块**；
- 于是 `assistant_msg.content` 为空、`calls` 为空 → 命中 `calls.is_empty()` → 成功态收尾；
- 本回合 `joined_text` 为空 → `final_text` 保留**上一回合**的旁白文本，成为「最终汇报」。

日志佐证：`logs/codewave.log.2026-09-15:326`（`sub_9196c679 tool edit 失败 [E_ARGS]：missing field 'path'`）与 `:332`（`工具 edit 的参数 JSON 无法解析（长度 896），调用被拒绝`）。

### 2.2 连带缺陷：被拒调用的反馈被静默丢弃

`calls.is_empty()` 的 break 发生在 `run_tool_batch` **之前**，而 `synth_results`（被拒调用的错误结果）只在 `run_tool_batch` 内部写回历史（`drive.rs` 的 `run_tool_batch`）。因此该回合的拒绝反馈**既不进历史也不发事件**：模型不知道该调用被拒（无从修正参数重发），用户也无从察觉——run 却报告成功。

### 2.3 已排除的误解：不是步数计算错误

用户初始判断为「步数计算错误」，经核对**不成立**：

- `rt.step_count.store(step + 1)`（`drive.rs` 循环顶）语义为「本 run 已启动步数」，在发起该步 LLM 请求前写入，采样只会滞后、不会虚高；
- 卡片的分子由 `subagent.rs` 的 800ms 轮询采样写 `sub:step` 而来，22 即真实值；
- 「1678.0k tok」= 本 run `usage.input + usage.output` 的累加和（`run_llm_turn` 每步 `run_usage += usage`），而 `input` 是**每步重发全量上下文**的口径，`1678k / 22 ≈ 76k tokens/步`，与长 context 子代理的量级吻合——数字与「22 步」互相印证，而非矛盾。

真正的问题是**收尾判定**，步数口径与展示仅在「`sub:done` 不带最终步数、✓ 无法区分跑完与提前结束」这一处存在独立缺陷（见 §3.3）。

## 3. 修复内容

### 3.1 收尾判定参数化 + 纯函数化（`core/agent/drive.rs`）

1. **`DriveParams` 新增 `finish_on_text: bool`**（`Default` = `true`）：
   - `true`：无工具调用回合即视为本 run 完成 —— **主会话语义逐字节不变**；
   - `false`：子代理与计划任务 run —— 无工具调用回合不再结束 run，除非汇报已按约定标记。
2. **新增纯函数 `text_turn_action(text, finish_on_text, text_turns)`**（与既有 `budget_notice_step` 并列，便于矩阵单测）：

   | 判定顺序 | 条件 | 动作 |
   |---|---|---|
   | ① | `finish_on_text`（主会话） | `Finish` |
   | ② | 文本含 `<report>` 标记 | `Finish`（显式最终汇报） |
   | ③ | `text_turns >= MAX_TEXT_TURNS`（=3） | `StopWithLimit`（**显式失败**，绝不静默当成功） |
   | ④ | 其余 | `Continue`（注入提示后继续下一步） |

3. **主循环接入**：
   - 空 assistant 消息（`content` 全被滤空）**不再压入历史**（与后续请求的 `Invalid assistant message` 400 互斥）；
   - `calls.is_empty()` 且存在被拒调用 → 注入 **user 角色**提示 `<tool-args-rejected>`：告知「上一回合有 N 个工具调用因参数 JSON 无法解析被拒绝、未执行，请修正参数重发，不要就此结束任务」；
   - `Continue` → 注入 `<continue-notice>`（未完成则立即继续调用工具推进；全部完成则以 `<report>…</report>` 汇报）并 `continue 'steps`（**续跑消耗步数预算**）；
   - `Finish` → 保持原有 `break 'steps`；
   - `StopWithLimit` → `outcome = Err(Protocol(...))`，走既有失败路径（前端 `sub:error`、主代理可 `ask` 询问是否重派），**不伪装成功**；
   - `text_turns` 在「有工具调用的回合」与「自动压缩成功」时复位。

   为何用 user 角色提示而非 `tool_result`：被拒调用**没有** `tool_use` 块，孤立的 `tool_result` 对 API 非法，且会被 `repair::sanitize` 当作孤儿结果删除。user 角色消息永远合法，与既有 `<budget-notice>` / `<supervision-notice>` 同机制（wire 层合并相邻 user 消息）。

4. **`<report>` 标记解析**：新增 `split_report(raw) -> (正文, 是否带标记)`，供子代理/任务剥离标记后再下发干净汇报。

### 3.2 子代理与计划任务（`tools/subagent.rs`、`run_task_agent`）

- 子代理 `DriveParams.finish_on_text = false`；纪律块（`build_system_extra`）增补：最终汇报必须以 `<report>…</report>` 包裹，**不得以过程旁白充当汇报**；
- 计划任务（`run_task_agent`，`TASK_BUDGET_STEPS = 30`）同样 `finish_on_text = false`，`<task-run>` 提示要求以 `<report>` 标记收尾（任务运行无人在场，中途旁白的「成功任务」更隐蔽）；
- 汇报下发前剥离 `<report>` 标记（`sub:report` / tool_result 内容干净）。

### 3.3 前端：区分「正常汇报收尾」与「提前结束」（`ui/src/*`）

`sub:done` 此前只带 `usage`，卡片分子是最后一次 800ms 轮询采样值，收尾时不刷新，且 ✓ 无法区分跑完与提前结束。现：

- `sub:done` payload 增 **`steps_used`**（收尾真实步数）与 **`ended`**（`"report"` | `"budget"` | `"no_report"`）；
- `SubView` 增 `stepsUsed?` / `ended?`；`sub:done` handler 用 `steps_used` 刷新最终步数并记录 `ended`；
- `SubagentItemCard`：`ended === "report"`（或缺省，向后兼容旧会话）→ 绿色 ✓；`budget` → 橙色警示 +「预算耗尽」；其余 → 橙色警示 +「提前结束」。橙=需注意，符合本项目色彩语义（中性墨色强调，不引入彩色 accent）；
- i18n 中英对称新增 `subagent.endedEarly` / `subagent.endedBudget`。

## 4. 测试

| 测试 | 落点 | 覆盖 |
|---|---|---|
| `text_turn_action_matrix` | `core/agent/tests.rs` | ①主会话语义 ②`<report>` 标记（计数已满仍 Finish）③ `Continue`（旁白 / 空文本）④触上限 `StopWithLimit` |
| `split_report_strips_tag` | `core/agent/tests.rs` | 无标记 / 标准包裹 / 标记外话术 / 未闭合 / 多标记（只取第一个）/ 尾随正文丢弃 / 仅闭合标记 |
| `subagent_text_only_turn_does_not_end_run` | `core/agent/tests.rs` | 端到端：非主会话纯文本回合不结束 run（连接数 2，修复前为 1）+ 注入 `<continue-notice>` |
| `main_session_text_only_turn_ends_run` | `core/agent/tests.rs` | 主会话对照：纯文本回合即收尾（连接数 1）、无续跑提示 |
| `rejected_call_turn_injects_hint_and_continues` | `core/agent/tests.rs` | 被拒调用 → `<tool-args-rejected>` 提示 + 继续 + 空 assistant 消息不入历史 |
| `consecutive_text_turns_stop_at_limit` | `core/agent/tests.rs` | 4 连纯文本 → `Err` 显式失败（连接数 4）+ `<text-turn-limit>` 终止引导 |
| `sub:done` 新字段 | `ui/src/__tests__/run.subagent.test.ts` | `steps_used` 刷新 `step`、`ended` 落 store、旧 payload 兼容 |
| 卡片提前结束态 | `ui/src/__tests__/subagent.card.test.tsx` | `no_report` / `budget` → 警示图标 + 文案；`report` / 缺省 → ✓（向后兼容） |

端到端用例用脚本化 SSE（`spawn_scripted_sse` + `sse_body`，与 `provider/tests_integration.rs` 的 mock 同构）驱动 `drive_agent`，以**连接计数**断言「第 N 个无工具调用回合是否结束 run」。

**反向验证**：临时把 `text_turn_action` 改回「无条件 `Finish`」（= 修复前行为）→ `text_turn_action_matrix`、`subagent_text_only_turn_does_not_end_run`、`rejected_call_turn_injects_hint_and_continues` 三例失败，`main_session_text_only_turn_ends_run` 仍通过（证明断言精准捕获缺陷、主会话语义独立），随后还原。

## 5. 影响面与不变式

- 主会话的「无工具调用回合 = 回答完毕」语义零变化（`finish_on_text` 默认 `true`，`main_drive_params` 走 `DriveParams::default`）。
- 两处对**主会话也生效**的行为修正（与本次缺陷同源，均为「静默」问题）：① 被拒调用（参数 JSON 不可修复）的反馈改以 user 提示注入——此前该反馈被静默丢弃；② 内容全空的 assistant 消息不再入历史——此前会长期随请求发出并被判 400（`Invalid assistant message`）。二者不改变事件键名与数量。
- 事件键名不变；`sub:done` 仅新增可选字段（`steps_used` / `ended`），旧前端忽略即可。
- 子代理权限继承、取消级联、监督（重复失败 / 空转）判定顺序不变；`StopWithLimit` 复用既有 `Err` 通道（子代理 → `sub:error` + `E_SUBAGENT`，主代理可用 ask 询问是否重派）。
- config schema / 会话存储格式零变化。
- 续跑有双重上限（`MAX_TEXT_TURNS = 3` + step 预算），不存在无限续跑。

## 6. 验证

- `cargo test`（`src-tauri/`）：**579 passed / 0 failed / 2 ignored / 0 warning**（基线 main 573 passed；新增 6 例全绿）。
- `pnpm --dir ui test`：**47 files / 336 tests 全绿**。
- `pnpm --dir ui build`：通过（type check + vite build）。
- 反向验证：见 §4 末段（修复前行为下 3 例失败、主会话对照通过、已还原）。
- code-reviewer 跨层审查：**零 🔴**，结论「可合并」。已落实的 🟡：`ended` 判定 off-by-one（`steps_used + 1 >= max_steps` → `steps_used >= max_steps`）、补齐方案承诺的 StopWithLimit 端到端用例、`split_report` 边界断言与语义注释、`continue 'steps` 跳过 checkpoint / 空转看门狗的注释说明。其余记入 §7。
- 界面改动不做 GUI 自动点验，手动验证清单见 §8。

## 7. 已知取舍与遗留

- **🟡 混合批次（部分调用被拒 + 部分成功）仍产出孤儿 `tool_result`**（既存问题，本次未加剧）：`run_tool_batch` 把合成结果与成功结果一同写入 tool_results 消息，而被拒调用没有对应 `tool_use` 块。本次仅在「整回合全被拒」这条路径绕开（改走 user 提示）；彻底修需在批次层过滤并拆分反馈，建议独立立项（三协议对「tool_result 无匹配 tool_use」的容忍度需真实端点取证）。
- **🟡 模型在正文复述 `<report>` 指令**（如「我会用 `<report>` 包裹汇报」）会命中 `Finish` 而提前收尾（`text_turn_action` 按「包含」判定）。接受面：需模型明确写出该字面标记；如需更严可改为「标记须独占行 / 位于文本末尾」。
- **🟡 计划任务的续跑成本**：`run_task_agent`（30 步预算）在模型不遵从新格式时最多消耗 3 次续跑；预算耗尽仍无标记时 `ended = "budget"`（任务本身仍算成功，仅卡片标识提示）。存量任务定义无需改动（`<task-run>` 提示为运行时拼装）。
- **🟢 `SubView.stepsUsed` 未被卡片消费**：`sub:done` 直接刷新 `step`，该字段保留作语义化展示（保留/删除属偏好）。
- **🟢 仅思考（thinking-only）回合**计为一次续跑：3 连即显式失败（保守取舍，已注释）。
- **🟢 日志级别**：续跑提示走 `session_log::warn`（便于排查），高并发子代理下日志量会放大，后续可降为 `info`。
- **未覆盖**：`finish_on_text = false` 且 `<report>` 出现在**同时带工具调用**回合（语义 = 不结束、继续执行工具，无断言）；`emit_events = false` 时压缩成功复位 `text_turns` 的分支未被端到端触达。
- **需人工观察**：`<report>` 纪律块在真实长上下文模型上的遵从率，以及「不遵从 → 3 次续跑 → 显式失败」的实际频率（按 §8 手动清单观察）。

## 8. 手动验证清单

1. 主会话发一条普通提问（无工具需求）→ 模型答完即结束、不出现「继续推进」提示（确认主会话语义未变）。
2. 派一个多文件子代理任务（如「改 A 文件并跑测试」）→ 观察卡片：步数应持续增长直到任务真正完成；若中途出现旁白回合，应继续而非立即 ✓。
3. 子代理撞上步数上限 → 卡片显示「预算耗尽」橙色警示而非绿 ✓。
4. 子代理未按 `<report>` 收尾即结束（可人为构造：派一个只做只读调研、末回合不写汇报的子代理）→ 卡片显示「提前结束」橙色警示，主代理的 tool_result 里可据此判断是否需要重派。
5. 打开历史会话（旧数据无 `ended` 字段）→ 子代理卡仍显示原来的绿 ✓（向后兼容）。

## 9. 提交建议

```
fix(agent): don't treat a text-only turn as a subagent's final report

- DriveParams.finish_on_text (main session keeps current semantics)
- text_turn_action(): <report> tag ends the run, otherwise bounded continue
  (MAX_TEXT_TURNS) or an explicit failure — never a silent success
- return rejected tool calls (unparsable args) to the model as a user-role
  notice instead of dropping them, and keep empty assistant messages out of
  history
- subagent/task runs mark reports with <report>; sub:done carries steps_used
  and ended so the card can show 提前结束/预算耗尽 instead of a green check
```
