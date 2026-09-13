# — ask 忽略显式失败修复（E_ASK_NOT_ANSWERED）

> 缺陷：用户在询问窗口点「忽略」后，模型把空应答误读为「已获回应/默许」而继续执行（实际案例：批准形询问被忽略后模型直接开工实施未批准的方案）。
> 改动面：仅 `src-tauri/src/tools/ask.rs`（后端单文件）；前端交互、事件面 27 键、IPC 契约零改动。
> 验证基线：`cargo test` 277 passed / 0 failed / 2 ignored（真实 GLM/MCP E2E 显式忽略项）。

## 1. 现象与根因链

用户点「忽略」的既有产品语义是「未回答/拒绝此刻作答」：前端 `AskPanel.ignoreCurrent()`（AskPanel.tsx）末页将当前题清空后**直提空载荷**（所有题 `selections: []` 且 `note: ""`）——该语义本身正确，本批次不改动。

误判发生在后端工具层：

1. `ask.rs` 收到 `Some(answer)`（空载荷也是有效送达，不走取消分支）；
2. 组装模型可读文本时，空题仅追加尾注 `→ （未回答）`；
3. 以 `ToolOutcome::ok` **成功形态**返回——与正常回答的结构完全一致，只差一行尾注；
4. `has_valid_answer=false` 虽然正确阻止了 ConfirmEach 档的 Light 切档（[docs/notification-click-reveal](./notification-click-reveal.md)）与 Plan 档批准协议，但对**模型行为**没有硬约束：模型收到 ok 结果后倾向于解读为「用户已回应」而继续推进。依赖模型自觉读取尾注不可靠。

## 2. 修复

`ask.rs` 在 `ask:closed` 事件发出之后、组装答案文本之前增加**全空判定**：

- `!has_valid_answer(&args.questions, &answer)`（所有题 selections 空且 note trim 后空）→ 返回
  `ToolOutcome::err("E_ASK_NOT_ANSWERED", ...)`，错误消息显式告知模型：
  「用户忽略了本次询问（未作答）。这不是批准或确认：不要继续执行、不要重发同一询问；请简要说明当前状态或调整方案，然后停下等待用户的进一步指示。」
- 会话日志 `warn` 区分两类路径：`E_ASK_CANCELLED`（run 中止/应答通道关闭）与「用户忽略（全空应答）」，便于黑匣子日志（[docs/session-logging-report](./session-logging-report.md)）判别。

检测点位于 `ask:closed` 之后：前端询问卡已正常关闭、交互不受任何影响；失败信号只作用于模型侧工具结果。

## 3. 行为矩阵

| 场景 | 修复前 | 修复后 |
|---|---|---|
| 单题询问点「忽略」 | ok + 尾注「（未回答）」（模型可误读为默许） | `err E_ASK_NOT_ANSWERED`（硬信号：停下等待） |
| 多题末页忽略（全空直提） | 同上 | 同上 |
| 部分应答（≥1 题有效） | ok，未答题保留尾注 | **不变** |
| note 仅空白字符 | ok（按 has_valid_answer trim 语义本就不算有效） | `err E_ASK_NOT_ANSWERED`（与有效应答判定对齐） |
| run 取消 / 通道关闭 | `err E_ASK_CANCELLED` | **不变**（日志文案改为「用户取消」，与忽略区分） |
| 批准形询问选「执行方案」 | ok + plan_approved + 切档 + 注入 | **不变** |
| 忽略时权限档 | 不切档 | **不切档**（新增回归测试守护） |

## 4. 测试

新增 5 例（`ask.rs` tests，直驱完整 `run()` 流程——挂起后从 `rt.asks` 注册表取回 ask_id，按前端 `resolveAsk` 同形载荷回注）：

- `ignored_single_question_returns_err`：单题忽略 → err，错误码与消息文案断言（含「忽略」「不要继续执行」）；
- `ignored_all_empty_multi_question_returns_err`：多题全空 → 同路径 err；
- `partial_answer_still_ok_with_unanswered_tail`：部分应答行为不变（ok + 「选定：」+「（未回答）」尾注）；
- `whitespace_note_counts_as_ignored`：空白 note 按忽略失败；
- `ignored_answer_keeps_confirm_each_mode`：忽略绝不切档（ConfirmEach 档保持不变）。

测试基建：新增 `ask_ctx_mode(mode)` 工厂（按权限档构造 ToolCtx，`plan_ctx` 复用）与 `drive_ask_with_answer` 驱动器（spawn 侧重建 ToolCtx 共享 Arc 字段规避借用生命周期；轮询 `rt.asks` 键拿 ask_id）。

## 5. 影响面与契约

- 事件面 27 键 / IPC 契约 / 前端组件：零改动；
- 新错误码 `E_ASK_NOT_ANSWERED` 仅存在于模型侧工具结果文本（ToolOutcome），无前端消费点；
- 「忽略 = 清空直提」前端语义保持不变（[docs/ask-unified-plan-card-and-answer-switch](./ask-unified-plan-card-and-answer-switch.md) 既有约定）。

## 6. 手动验证清单（用户执行）

1. `pnpm tauri dev`，让模型发起任意批准形询问 → 点「忽略」；
2. 预期：模型停止推进，简短说明当前状态并等待指示；**不再继续执行**；
3. 右侧栏「日志」页签检索 `ask`：应出现「用户忽略（全空应答）→ 按 E_ASK_NOT_ANSWERED 返回」warn；
4. 对照：询问选择「补充意见」并填写内容提交 → 模型按意见修订（部分应答路径不受影响）。

## 7. 提交信息草案（AI 不执行 git，由用户提交）

```text
fix(ask): 显式失败被忽略的询问，防止模型把空应答误读为默许
```
