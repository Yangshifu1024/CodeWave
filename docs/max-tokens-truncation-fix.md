# 缺陷修复：回复因 max_tokens 被截断

> 类型：缺陷修复（P0 分类：缺陷）· 影响层：`provider/`（三协议）+ `core/config.rs` 默认值 + 前端模型表单 · 契约影响：零（事件 27 键 / config schema 零变化）

## 1. 问题现象

用户报告：今天开始模型回复总是被截断，流末尾出现「[注意：回复因 max_tokens 被截断]」（见用户截图）。

用户操作轨迹：

1. 起初设置模型时误将某值设为 128K；
2. 后续把「上下文窗口」改成 100M；
3. 截断依旧。

## 2. 根因

`max_tokens`（单次输出上限）与 `context_window`（上下文窗口）是模型配置里两个**独立字段**（`config.rs` 的 `ModelConfig` / `ProviderModel`）：

- **请求体 `max_tokens` 直接取自模型配置的「最大输出 Token」**（anthropic.rs `build_body`；openai_chat 同名字段；openai_responses 为 `max_output_tokens`）。新模型默认 **8192**。
- **`context_window` 只消费于两处**：compaction 触发阈值（`core/context.rs` 的 `breakdown` ratio）与前端 Composer 用量百分比显示。改成 100M 的实际效果是自动压缩几乎永不触发 + 用量显示趋近 0%——**对请求体 `max_tokens` 零影响**，截断必然依旧。
- **放大因素（用户走 anthropic 协议 + Max 推理档）**：扩展思考 budget = `max_tokens × 0.8`（clamp `[1024, max_tokens-1]`，`core/prefs.rs::anthropic_ratio` Max=0.8），即 8192×0.8 ≈ **6553 tokens 被思考占用，正文只剩约 1600 tokens**——这解释了「思考 114 秒 + 一段正文后戛然而止」的量级。
- 反证排除网络/服务端截断：那类截断走 `!acc.finished → "SSE 流被截断"` 可重试错误路径；出现优雅尾注即证明服务端明确回报 `stop_reason=max_tokens`。

**附带缺陷（tester 复核发现）**：openai 双协议截断时**静默无提示**——

| 协议 | 截断信号 | 截断时行为 |
|---|---|---|
| anthropic | `stop_reason == "max_tokens"` | 注入尾注 ✅ |
| openai_chat | `finish_reason == "length"` | **静默**（`length` 分支不存在，静默结束） |
| openai_responses | `response.incomplete` + `incomplete_details.reason` | **静默**（信号已接收但被丢弃，与 completed 同路只取 usage） |

openai 兼容端点（DeepSeek/GLM/Ollama 等）截断时 agent 主循环完全无感，模型可能自以为回复完整。

## 3. 修复内容

### 后端

1. **新模型默认 `max_tokens` 8192 → 32768**（`config.rs` 的 `ModelConfig::default` 与 `ProviderModel::default` 两处）。仅影响新建模型；存量配置经 serde 显式值读入不受影响。
2. **`dto.rs` 抽公共常量 `MAX_TOKENS_NOTICE`**（`\n[注意：回复因 max_tokens 被截断]`），anthropic.rs 改用常量（文案与行为零变化），防止三协议文案漂移。
3. **openai_chat**：`handle_chunk` 新增 `finish_reason == "length"` 分支——注入尾注 + `finished = true`（对齐 anthropic 语义）。工具调用兼容性经审查确认安全：装配层由流结束后 `build_assistant_message` 统一装配（`ToolCallEnd` 在装配层是 no-op），截断的 args 走既有 salvage 路径。
4. **openai_responses**：`response.incomplete` 且 `incomplete_details.reason == "max_output_tokens"` 时注入尾注；其他 incomplete reason 与 `completed` 不提示（保留扩展空间，不误报）。

### 前端

5. **`ProvidersPanel.tsx`**：`providerModelDefaults()` 默认 32768；模型弹窗 `onChange` fallback 同步；两个 `InputNumber` 的 `Form.Item` 加 help 澄清文案（对症本次误解）：
   - 最大输出 Token：「模型单次回复（含思考过程）的输出上限，超出即被截断；偏小会过早截断长回复。」
   - 上下文窗口：「仅用于自动压缩触发阈值与用量显示，不影响单次回复的输出长度。」
6. **i18n**：`maxTokensHint` / `contextWindowHint` 两键，中英对称。

## 4. 测试

| 测试 | 落点 | 覆盖 |
|---|---|---|
| `finish_reason_length_truncation_notice` | openai_chat.rs tests | length → 尾注 + finished |
| `finish_reason_stop_no_notice` | openai_chat.rs tests | 正常 stop 不误报（反漂移） |
| `length_mid_tool_call_notice_and_args_intact` | openai_chat.rs tests | 工具调用中途截断：尾注为 Text 且已收集的 tool args 不被破坏（code-review 审查焦点钉进测试） |
| `incomplete_max_output_tokens_notice` | openai_responses.rs tests | incomplete(max_output_tokens) → 尾注 + usage 照常记录 |
| `incomplete_other_reason_and_completed_no_notice` | openai_responses.rs tests | 其他 reason / completed 不误报 |
| 默认值 + help 断言用例 | providers.panel.test.tsx | 新模型默认 32768 + 两字段 help 渲染 |
| 7 个测试文件 fixture | `__tests__/` | `max_tokens: 8192 → 32768` 同步 |

## 5. 验证

- `cargo test`：**370 passed / 0 failed / 0 warning**（新增 5 例全过）
- `pnpm --dir ui test`：**235/235 passed**（34 文件）
- `pnpm --dir ui build`：通过（type check + vite build）
- code-reviewer 跨层审查：**零 🔴**，结论「可合并」；🟡 建议项（组合测试、存量配置明示）已落实/本文档即明示。

## 6. 已知取舍与遗留

- **存量模型配置不受益**：serde default 语义决定存量 `max_tokens: 8192` 的模型原样保留。**需要在 设置 → 供应商 → 编辑模型 中手动调大「最大输出 Token」**（建议 32768 或按端点上限）。
- **🟢 遗留（未做，收益有限）**：openai_responses 兼容端点以 `event: message` 透传 + data 内 `type=response.incomplete` 时 `name` 解析为 `"message"` 会漏注尾注（仅少提示不影响正确性）；`handle_event` 中 `name == "response.incomplete"` 判断在 match 收敛后属冗余防御（保留无害）。
- **🟡 thinking budget 与 max_tokens 解耦（暂缓）**：现设计使「想多思考必须同调大输出上限」，且思考挤占正文额度（本次 6553/8192 即例证）；语义上 `budget_tokens` 本是独立参数，建议未来独立配置项，非本缺陷根因，不捆绑修。
- 前端双列 `Form.Item help` 在窄弹窗下英文文案折行可能导致双列高度不齐（antd 默认灰字自适应亮暗，功能无碍）——见 §7 手动验证项。

## 7. 手动验证清单

1. 设置 → 供应商 → 编辑本次出问题的模型 → 「最大输出 Token」调到 32768（或端点上限内更大值）→ 保存。
2. 同一会话继续提问长任务（如「阅读某文件并写完整重构方案」），确认不再出现「[注意：回复因 max_tokens 被截断]」。
3. 打开添加模型弹窗：确认「最大输出 Token」默认 32768、两字段下方有灰色 help 说明（中英两种语言各看一遍，检查窄弹窗折行观感）。
4. （可选，openai 协议用户）用 openai_chat 兼容端点把「最大输出 Token」设为 256，提问长文 → 确认回复末尾出现截断尾注（修复前为静默截断）。

## 8. 提交建议

工作区当前混有本批（max_tokens 修复）与另一批未提交改动（标题栏右段/关于弹窗/docs/titlebar-logo-right-segment 标题栏批次，约 200 行）。**建议分两个 commit**，本批建议提交信息：

```
fix(provider): raise default max_tokens to 32k and surface truncation in openai protocols

- New-model default max_tokens 8192 → 32768 (backend ModelConfig/ProviderModel + frontend form)
- Extract shared MAX_TOKENS_NOTICE constant; anthropic notice now uses it
- openai_chat: inject truncation notice on finish_reason="length" (was silent)
- openai_responses: inject notice on response.incomplete(reason=max_output_tokens)
- Clarify model form copy: max_tokens caps per-reply output, context_window does not
```
