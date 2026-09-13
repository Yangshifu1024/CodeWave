# plan 纪律宿主强制 · 实施报告

> 日期：2026-09-07 · 状态：已完成（cargo test 412 passed / 0 warning）
> 来源需求：把 system prompt `<plan-protocol>` 层「实现类工作先建 plan、随推进保持更新」的软约束落地为宿主确定性强制。需求分析与技术方案经 plan 档批准（两硬一软三门 + 双豁免），dev 子代理实施、code-reviewer 审查两轮。

## 1. 背景

`<plan-protocol>`（`core/prompt.rs`）要求模型实现类请求先调 plan 工具并保持状态更新，但纯提示词无宿主兜底：模型跳过计划直接写文件时，用户失去进度可视性，「计划外顺手改」无法防范。本批次把该约束收敛为批次层硬门，与既有 `E_TOOL_BLOCKED`（exclude_tools / exclude_mcp）硬门同构。

## 2. 实现语义

### 两道硬门（批次层，spawn 前拒绝不执行）

| 错误码 | 触发条件（同时满足） | 解除路径 |
|---|---|---|
| `E_PLAN_REQUIRED` | 主会话 + `ToolKind::FileWrite` 工具 + `todos` 为空 | 调 plan 建计划（一条 todo 亦可） |
| `E_PLAN_STALE` | 主会话 + 写工具 + `todos` 非空且全部 completed | 调 plan 新增待办（继续写 = 计划外工作） |

- 判定收敛于纯函数 `plan_gate_verdict(todos, is_write)`（`tools/batch.rs`），三态可单测；
- 门位于 exclude 门 / mcp 门**之后**：plan 档写工具先被 exclude 拦为 `E_TOOL_BLOCKED`，新门天然惰性，plan 档行为不变；
- 双豁免：`main_session=false` 整门跳过（子代理被排除 plan 工具，dev 必须能写文件）；本批含 `plan` 调用时写放行（同批乐观豁免——模型同批建计划说明意图存在，拦则必假阳性；豁免不看 plan 调用成败，有测试钉死）；
- 判定基于**批次入口快照**（循环外一次 `lock().clone()`，JoinSet 并发共享同一判定）；
- 文案为终结性提示：明示重试 / 改参无效，给出解除路径（与 `E_TOOL_BLOCKED` 同构）。

### 软提醒（不阻断）

写工具成功 + todos 非空且无 InProgress 时，向该 ToolResult content 尾部追加「计划提醒：当前计划没有进行中条目，完成后请用 plan 工具标记状态。」；`SessionRuntime.plan_hint_emitted`（AtomicBool CAS）保证每 run 至多一次，`run_chat` 起点复位。

### 范围边界（有意不做）

- 只读工具 / command（含 shell 重定向写，归 fence 与 G3 范围门兜底）/ MCP 工具不拦；
- 无用户配置开关、无「直接改」意图豁免（第一版）、无计划-改动相关性校验；
- 前端零改动，事件面 27 键与 `plan:update` 协议未动。

## 3. Wire 层关键发现（R1）

初版软提醒走 `extra_model_content`（Tool role 消息中的独立 Text 块）。code-reviewer 发现三协议 wire 转换（`anthropic.rs` / `openai_chat.rs` / `openai_responses.rs` 的 `Role::Tool` 分支）只保留 ToolResult 块、丢弃其余——**提醒在请求体中不可达，且 CAS 已烧掉唯一机会；批次层测试全绿造成虚假信心**。

修复：提醒并入 ToolResult content 尾部（三协议均原样透传），并以 6 个 provider 层测试双向钉死：

- `tool_result_content_reaches_wire_verbatim` ×3：含提醒的 ToolResult 在三协议请求体中可检出；
- `tool_role_non_toolresult_blocks_dropped` ×3：Tool 消息中的独立 Text/Image 块在任何协议都到不了请求体（协议语义固化，防止将来误用该通道）。

**遗留独立缺陷（本批次未修）**：`tools/read.rs` 图片注入走同一 `extra_model_content` 通道（Image 块进 Tool 消息），同因被三协议丢弃，模型实际收不到图片。修复思路与本批次相同（并入 ToolResult content 或 transient 通道），需另立任务。

## 4. 改动清单

| 文件 | 内容 |
|---|---|
| `src-tauri/src/tools/batch.rs` | `plan_gate_verdict` 纯函数 + 两道硬门 + 双豁免 + 快照显式化 + `maybe_emit_plan_hint`（并入 ToolResult）+ 12 个测试 |
| `src-tauri/src/core/agent/runtime.rs` | `plan_hint_emitted: AtomicBool` |
| `src-tauri/src/core/agent/drive.rs` | `execute_batch` 调用点透传 `main_session`；run 起点复位提醒标志；冗余赋值清理 |
| `src-tauri/src/provider/{anthropic,openai_chat,openai_responses}.rs` | wire 层守护测试 ×6 |

## 5. 验证

- `cargo test`（src-tauri/）：**412 passed / 0 failed / 1 ignored，exit 0，0 warning**（基线 404，净增 8：批次层 7→9、wire 层 +6、f 系改造）；主会话独立复测确认（非仅采信子代理汇报）；
- 集成覆盖：空计划拦 / 陈旧拦 / 活跃计划放行 / 子代理豁免 / 同批 plan+edit 放行 / plan 失败仍豁免 / 并发批次共享快照双双拦截 / 软提醒恰一次 / 含 InProgress 无提醒 / wire 可达 ×3 / wire 丢弃 ×3；
- code-reviewer 七维度审查两轮：R1（🔴）已修复；🟡 快照显式化、测试缺口、docs 登记均落定；🟢 注释自文档化已采纳。
