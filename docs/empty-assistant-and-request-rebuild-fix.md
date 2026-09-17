# 缺陷修复：空 assistant 消息与「修复后重试」不重建请求体

> 类型：缺陷修复（多文件/跨层，已过 code-reviewer）｜基线：`main @ 63a8147`（v0.3.6）
> 现场：会话 `d9941c4b-a305-44ea-8894-794e10e29121` 实测两条 provider 400

## 1. 现象

同一会话两条 400，均以「sanitize/repair 历史后重试」失败告终：

| 时刻 | 报文 |
|---|---|
| 01:37:58 step 38 | `An assistant message with 'tool_calls' must be followed by tool messages responding to each 'tool_call_id' … call_01_kxEEEVa9ISaSiqxMfhc68983` |
| 01:47:47 step 0 | `Invalid assistant message: content or tool_calls must be set` |

关键证据：两条 400 报文**相隔 2.45 秒、逐字节相同**——修复动作执行了，但修复结果从未发出去。

## 2. 根因

### H1（🔴 放大器）：修复后的重试是死代码

`core/agent/drive.rs` 每步 ⑦ 调一次 `build_stream_request`，把历史一次性 clone 进 `req`；`run_llm_turn` 以 `req: &StreamRequest` 接收，BadRequest 分支只对 `rt.history` 做 `sanitize/repair` 后 `continue`——**从不重建 `req`**，每次尝试 clone 的都是同一份陈旧 body。任何「修复后重试」逻辑都因此无效。

> 复核结论：其余会改历史的路径（②注入队列、③④自动压缩、⑤预算提醒、⑥强制汇报、⑨批次结果/计划批准/监督纠偏）都发生在下一轮 `'steps` 迭代开头，天然被 ⑦ 重建覆盖，无遗漏点。

### H2（🔴）：空 assistant 消息能进历史并直发 wire

`sanitize_inner` 会丢弃 args 无法打捞的 `ToolUse`（字符串中部截断时 `parse_or_salvage` 判定内容不可信）→ 该 assistant 消息 content 变空数组 → `repair()` 不清理空消息 → `openai_chat` 的 `convert_message` 无条件发出 → `content: null` 且无 `tool_calls` → 400。且这条消息会随之后**每次**请求反复带上（磁盘历史里那一条 `{"role":"assistant","content":[]}`）。

### H3（🟡，未闭环）：悬空 `tool_call_id` 的来源证据不足

该 id 在磁盘历史出现 0 次、tool_use/tool_result 各 55 配对平衡，说明它已被 sanitize 丢弃分支 + 孤儿结果清理链处理掉，不会再触发。**确切产生路径仍未定位**，可疑项（未证实）：

- 被安全围栏 / 审批拦截的调用可能不产生 tool 消息，从而留下无应答的 `tool_call_id`（本会话实测复现一次被 `E_PLAN_READONLY` 拦截的调用后即出现同类 400）
- 前端历史截断类命令、同 id 重复 Begin

### H4（🟡）：`synth_results` 在提前 break 时被丢弃

`calls.is_empty()` 时 `break 'steps` 早于 `run_tool_batch`，参数不可解析的合成错误结果永远不进历史——模型拿不到反馈。**本批有意维持现状**（给不可解析的调用伪造 `args: {}` 的 ToolUse 会伪造历史里并未发生的工具调用，风险更高），改为留会话日志 + 模型侧无反馈。

## 3. 修复（三层纵深防御）

| 层 | 位置 | 做法 |
|---|---|---|
| 历史层 | `core/sessions/repair.rs` | `repair()` 末尾丢弃「`role == Assistant` 且 `content` 全空」的消息。**严格限定**：不碰 `Role::Tool`（误删 ToolResult 会造出新的悬空 tool_use）、不碰含任何块的消息 |
| 出网副本层 | `core/agent/stream.rs` | 抽出 `messages_for_request`（clone 历史 + 计划瞬态快照 + **末尾跑一次 `repair`**）与 `refresh_request_messages`；`build_stream_request` 改用前者。**只修副本，`rt.history` 不被改写** |
| wire 层 | `provider/openai_chat.rs` | 文本空且无 `tool_calls` 的 assistant 消息不上 wire（杠绝 `content: null`） |

配套：`run_llm_turn` 的 `req` 改 `&mut`，BadRequest 分支在 sanitize/repair 后调 `refresh_request_messages` 重建请求体（**H1 的唯一直击**）；`drive.rs` step ⑨ 加守卫——组装出的 assistant 消息为空时不入历史，按既有「空响应」语义重试一次，预算耗尽则终止本步。

**取舍**：`refresh_request_messages` 保留本 run 首轮已注入的计划瞬态快照（否则重试那一轮模型会莫名失去计划视图）；不重算 `cache_gen_index`（修复会缩短历史，最坏结果是本次重试少一个代际缓存断点，属缓存效率问题）。

## 4. 回归测试（6 条，`cargo test` 606 → 609）

| 用例 | 守护的不变量 |
|---|---|
| `repair_drops_empty_assistant_but_keeps_empty_tool_message` | 空 assistant 被丢；内容被清空的 Tool 消息**不**被误删 |
| `unparseable_args_leaves_no_empty_assistant` | sanitize 丢块后不留空 assistant；保存→加载往返同样不留（**重启自愈**） |
| `repair_before_send_sanitizes_dirty_history` | 出网副本自我修复三级脏结构 |
| `dirty_history_messages_stay_byte_stable` | 脏历史修复**幂等**（连续两次构建 messages 逐字节相等），否则每步打穿 prompt cache 前缀 |
| `refresh_request_messages_rewrites_body_from_repaired_history` | 重建后 `req.messages` 取自当前已修复历史，不再是陈旧快照（H1 直击） |
| `skips_assistant_without_text_or_tool_calls` / `tool_calls_are_answered_on_wire` | 不上 `content: null`；每个 tool_call_id 必有后续应答 |

另为 `anthropic` / `openai_responses` 各补一条守护用例（两者当前天然跳过空消息，但「天然」是隐式的，重构可能无声破坏）；`stream_request_bytes_stable_across_calls` 补消息数组逐字节稳定断言。

## 5. 现有损坏数据

**无需手工修复**。加载期 `prepare_on_load → repair` 会剔除末尾空 assistant 消息，该会话下次打开即可继续对话（前提：本修复已编译进运行中的二进制——修复只改工作区代码时，跑着的旧实例仍会复现）。

## 6. 遗留

- H3 的确切来源未闭环：需开 verbose 抓 wire 请求全文复现，并排查「被围栏/审批拦截的调用是否遗漏 tool 应答」
- H4 的模型侧反馈缺失（模型可能原样重发同一个坏调用直到预算耗尽）——可考虑后续把拒绝原因作为下一条 user 消息注入，而非伪造 ToolResult
- **H4 部分闭环（2026-09-16 更新）**：①「唯一调用被拒的回合」已由 `<tool-args-rejected>` user 提示闭环（[subagent-text-turn-premature-exit](./subagent-text-turn-premature-exit.md)）；②「正文非空 + 全部调用被拒」的**主会话**静默成功（run 报成功而提示永不被模型看到）已由 [rejected-call-silent-finish](./rejected-call-silent-finish.md) 修复（`text_turn_action` 新增 `rejected` 维度）。**仍未闭环**：混合批次（部分调用被拒 + 部分成功）的合成 `ToolResult` 仍会被出网前 `repair` 当孤儿结果删除 ⇒ 模型看不到拒绝原因（该路径本次已补会话日志取证）。
