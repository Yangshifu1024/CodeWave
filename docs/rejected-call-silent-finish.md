# 缺陷修复：主会话「正文非空 + 全部工具调用被拒」回合静默成功收尾

> 类型：缺陷修复（P0 分类：缺陷）· 影响层：`core/agent/drive.rs`（收尾判定 + 取证）+ `core/sessions/repair.rs`（诊断摘要，行为零变化）· 契约影响：零（事件 28 键不变 / config schema 不变 / 前端零改动）
> 分支：`fix/rejected-call-not-silent`（基线 `main`，commit `ee81603` / v0.3.7）
> 现场：会话 `5100ea0c-0f27-4e4a-acd6-b913306a179d`（项目 CommandWave，Plan 档，`deepseek-v4.1-flash`）

## 1. 问题现象

用户描述该会话「又卡死了」「没有继续输出方案」。run 停在 step 9 后再无输出。

会话日志（`<主目录>/.codewave/logs/5100ea0c-….log`，行号取现场文件）：

```
[16:49:34.250] tool plan ok 耗时 4ms args={...}
[16:49:34.251] step 9 llm 请求开始 model=deepseek-v4.1-flash in≈0ms
[16:49:54.626] step 9 llm 完成 model=deepseek-v4.1-flash 耗时 20374ms tokens in=56502/out=3804
[16:49:54.627] [WARN] step 9 1 个工具调用参数不可解析被拒绝，已反馈给模型
[16:49:54.712] [INFO] run 9d086f07-… 完成 input 577249 / output 37780 tokens 耗时 540s
```

**没有 step 10 的「llm 请求开始」行，也没有 run 失败/取消行** —— run 以「成功」收尾（`run:done`）。

磁盘历史末两条（`~/.codewave/histories/5100ea0c-….json.gz`，macOS 注意 `zcat -f`）：

```
assistant: [Text] 方案已登记为 todos。下面是完整方案（含分支名 `feat/cursor-height-inset`）。
user:      <tool-args-rejected>你上一回合有 1 个工具调用因参数 JSON 无法解析而被拒绝、未执行。…
```

模型那句「下面是完整方案」之后本该由 `ask`（参数 8531 字符，含方案全文）弹出的方案卡**从未产出**；被拒调用既不进历史、也不上 wire，方案全文无处可寻（唯一残留是 step 8 `plan` 工具已落盘的 todos）。

## 2. 根因

`drive_agent` 主循环的 `calls.is_empty()` 分支（`drive.rs`）：

1. 组装结果 `content` 非空（历史里确有该 assistant 文本）→ 入历史 → 命中 `calls.is_empty()`；
2. `synth_results` 非空（存在被拒调用）→ 注入 `<tool-args-rejected>` 提示；
3. 紧接着 `text_turn_action(&joined, params.finish_on_text, text_turns)`，主会话 `finish_on_text = true`（`main_drive_params` / `DriveParams::default`）→ **`Finish`** → `break 'steps`；
4. 该 `break` **不写 `outcome`**（保持 `Ok(())`）→ `run_chat` 打印「run 完成」并发 `run:done`。

即：提示写进了历史，却永远没有下一次请求去消费它；run 还报成功。

**排除法**（穷举 `'steps` 全部退出点）：能产生 `Ok(())` 的 break 只有「`Finish`」与「`batch_done`（需 `calls` 非空 + `force_report`）」两处；分支一（空 `content`）会 `continue 'steps`，必然留下 step 10 的请求行（日志无此行）；步数预算（`MAX_STEPS = 9999`）、用户取消、panic `catch_unwind`、空响应重试各自都会留下对应日志行（均不存在）。

**错位变体**：回合只有 thinking（`content` 非空、`joined_text()` 为空）同样被 `Finish`，`final_text` 保留空串/上一回合文本当最终答复——同一缺陷的另一种形态。

## 3. 修复内容

### 3.1 收尾判定：新增 `rejected` 维度（`core/agent/drive.rs`）

`text_turn_action(text, finish_on_text, rejected, text_turns)` 判定顺序：

| 顺序 | 条件 | 动作 |
|---|---|---|
| ① | `rejected` 且 `text_turns < MAX_TEXT_TURNS` | `Continue` |
| ① | `rejected` 且 `text_turns >= MAX_TEXT_TURNS` | `StopWithLimit`（显式失败） |
| ② | `finish_on_text`（主会话） | `Finish` —— **仅限无被拒调用的回合** |
| ③ | 文本含 `<report>` | `Finish` |
| ④ | `text_turns >= MAX_TEXT_TURNS` | `StopWithLimit` |
| ⑤ | 其余 | `Continue` |

- 被拒判定**必须先于** `finish_on_text`：否则主会话被拒回合仍会静默成功（本缺陷）。
- 上限复用 `MAX_TEXT_TURNS`（=3）+ 步数预算双兜底 → **不无限续跑**；超限走 `Err(Protocol)` → `run:error`（用户可见的显式失败，而非静默成功）。
- `Continue` 时若 `rejected`：**不注入** `<continue-notice>`（其文案「你只输出了文字、没有发起工具调用」在被拒场景不实），只 `text_turns += 1; continue 'steps`。
- `StopWithLimit` 错误文案按成因分流（被拒 →「未发起任何有效工具调用（调用参数反复不可解析）」）；`<text-turn-limit>` 引导语同步覆盖两种成因。
- 分支一（空 `content` 的 `continue 'steps`）**零改动**。

### 3.2 取证增强（`core/sessions/repair.rs` + `core/agent/drive.rs`）

被拒调用的 args 既不进历史也不上 wire，此前日志只有「工具名 + 长度」，事后无法回答「8531 字符为何不可解析」。新增：

- `repair::diagnose_unparsable(s) -> String`（**不参与任何解析决策**、行为零变化）：`len` + 首尾各 200 字符（`\n`/`\r` 转义）+ serde 解析错误原文；
- `drive.rs::log_rejected_calls(rt, step, &assembled)`：在被拒的**三条**路径输出——① 空 `content` 分支；② 正文非空且调用全被拒分支；③ **混合批次**（部分被拒 + 部分被执行：被拒项的合成 `ToolResult` 会在出网前被 `repair` 当孤儿删除，会话日志是唯一留痕点）。

### 3.3 测试（`core/agent/tests.rs`）

| 用例 | 守护的不变量 |
|---|---|
| `text_turn_action_matrix`（扩） | ① 主会话纯文本仍 `Finish`；①’ 被拒 → `Continue`；①’’ 被拒超限 → `StopWithLimit`；①’’’ 被拒优先于 `<report>` |
| `main_session_text_with_rejected_call_does_not_silently_finish` | 端到端：`hits == 2`（修复前 1）、`final_text` 不停留在被拒回合、注入 `<tool-args-rejected>`、**不**注入 `<continue-notice>` |
| `main_session_thinking_only_rejected_call_turn_continues` | 错位变体（仅思考 + 被拒）同样继续 |
| `sub_rejected_call_shares_text_turn_budget` | 非主会话 4 连被拒 → `Err` + `<text-turn-limit>` + 无 `<continue-notice>` |
| `diagnose_unparsable_reports_length_and_edges` | len / 首尾 / 错误原文 / 换行转义；且 `parse_or_salvage` 的判定不变 |

端到端用例复用既有脚本化 SSE mock（`spawn_scripted_sse` + `sse_body`）驱动 `drive_agent`，以**连接计数**断言「被拒回合是否结束 run」。

## 4. 验证

- `cargo test`（`src-tauri/`）：**625 passed / 0 failed / 2 ignored**；`cargo check --tests` **0 warning**。
- **反向验证**：临时把调用点改为 `let rejected = false;`（＝修复前行为）→ 3 个新端到端用例必红（`hits 1 != 2`；上限用例因 `<continue-notice>` 被注入而红），`rejected_call_turn_injects_hint_and_continues`（分支一）与矩阵测试仍绿；随后还原（工作区无残留）。
- code-reviewer 跨层审查：**零 🔴**，结论「可合并」；两条 🟡 已落实（混合批次补 `log_rejected_calls`、`<report>`+被拒补矩阵断言）。
- 界面改动为零 → 不做 GUI 自动点验（手动清单见 §7）。

## 5. 影响面与不变式

- 主会话「无被拒调用的纯文本回合 = 回答完毕」语义**逐字节不变**（`finish_on_text` 的默认值与判定位置未动，`rejected` 仅在其之前拦截被拒场景）。
- 事件面 28 键零变化；config schema 零变化；前端与 `ipc/types.ts` 零改动。
- 历史合法性不变式：任何新终止路径都不产生「`content` 空的 assistant」或孤立 `tool_result`。
- 被拒回合的历史形态：`assistant(正文非空)` + `user(<tool-args-rejected>)`，两者皆合法（wire 层合并相邻 user 消息）。

## 6. 已知取舍与遗留

- **🟡 `<report>` 与被拒同回合**：`rejected` 优先 ⇒ 子代理/任务会多走最多 3 步（此前直接 `Finish`）。有意取舍：被拒调用尚未被模型知晓，先让它修正重发。
- **🟡 混合批次语义未修**：被拒项的合成 `ToolResult` 仍会被出网前 `repair` 当孤儿结果删除 ⇒ 模型看不到拒绝原因（run 照常推进）。本批只补日志取证；彻底修需在批次层过滤并改走 user 提示（与 [empty-assistant-and-request-rebuild-fix](./empty-assistant-and-request-rebuild-fix.md) §7 同源，建议独立立项——三协议对「`tool_result` 无匹配 `tool_use`」的容忍度需真实端点取证）。
- **🟡 8531 字符 JSON 为何不可解析仍未闭环**：本批只让下次可取证。已排除 `max_tokens` 截断（截断会注入 `[注意：回复因 max_tokens 被截断]`，历史中无此字样；`out=3804` 亦与之量级不符）。可疑项：字符串内未转义控制字符（多行计划文本）/ 引号配对错位 / args 分片拼接异常（`stream.rs` 的 `ToolCallArgsDelta` 无校验）。
- **🟡 `ask` 参数过大**：本次是模型把整份方案塞进 `ask` 的 `plan` 字段（8531 字符）。即便参数合法，超大参数也会抬高被拒概率与 token 成本；是否限制 `ask.plan` 长度、或改为「先落盘文档再 ask 引用」属独立议题。
- **🟢 内部提示泄漏**：`<tool-args-rejected>` / `<continue-notice>` / `<text-turn-limit>` 等内部提示在重开历史时以 user 气泡展示（无过滤白名单）——既存问题，未在本批处理。

## 7. 手动验证清单

1. 打开会话 `5100ea0c`（历史已健全，无需手工修复）→ 直接追问一句，确认能继续对话、不再停在原处。
2. 构造被拒场景（让模型发起一个参数超长/非法的 `ask`）→ 确认 run 不会静默结束：要么继续输出，要么以 `run:error` 显式失败；会话日志出现 `step N 被拒调用 <工具名>：len=… err=… head="…" tail="…"`。
3. 主会话普通提问（无被拒调用）→ 仍一次答复即结束，且不出现续跑提示。
4. 连续 3 次被拒 → 确认以显式错误终止（不是成功卡住）。

## 8. 提交建议

```
fix(agent): don't let a rejected tool call end a main-session run silently

- text_turn_action(): new `rejected` dimension judged before finish_on_text, so a
  turn with text plus only-unparsable tool calls continues (bounded by
  MAX_TEXT_TURNS) instead of finishing with Ok — the notice was pushed to history
  but never reached the model while the run reported success (session 5100ea0c:
  an 8531-char `ask` argument vanished and the plan card was never produced)
- suppress <continue-notice> on rejected turns (its wording is false there)
- StopWithLimit message and <text-turn-limit> guideline now cover both causes
- repair::diagnose_unparsable + drive::log_rejected_calls: record len, head/tail
  excerpt and the serde error for rejected calls (also on mixed batches), so the
  next occurrence is diagnosable — previously those args left no trace at all
```
