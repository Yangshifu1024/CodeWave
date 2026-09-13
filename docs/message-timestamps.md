# 消息时间戳持久化（重开旧会话时间全变「现在」缺陷修复）

## 缺陷

用户重开历史会话，所有消息（user/assistant）时间都显示为打开时刻。现场对话时时间恰好 ≈ 发送时刻，缺陷只在重开旧会话时暴露。

## 根因

两层叠加：

1. **前端伪造时间**：`ChatMessages.tsx` 的 `ts()` 在拿不到 iso 时回退 `Date.now()`，而 `renderItem` 从未向 `UserMessage` 传入时间、`AssistantMessage` 直接写死 `ts(undefined)`——任何消息实际都渲染「渲染时刻」。违反 [docs/session-nav-new-top](./session-nav-new-top.md) 确立的「未活跃留空不冒充」原则。
2. **转录从未持久化时间**：后端 `core/types.rs` 的 `Message` 只有 `role + content`，会话历史文件里根本没有每条消息的时间戳，重开后无从恢复。

## 修复

### 后端（根因）

- `Message` 新增 `#[serde(default, skip_serializing_if = "Option::is_none")] pub created_at: Option<String>`（UTC RFC3339）：
  - `default` → 旧档案缺字段读入为 None（配置结构兼容约定同款）；
  - `skip_serializing_if` → None 不序列化，provider 请求体形状零变化（openai_chat/openai_responses 请求体形状测试原样通过）。
- 新增 `Message::stamped()`：created_at 为空时落当前 UTC 时间。**只在消息首次进入转录时盖章**（agent.rs 的 9 个 `rt.history` push 点：用户消息、注入消息、预算/汇报提示、工具结果、方案批准指令、任务指令、取消标记、assistant 装配消息）——`save_history` 是全量重写，绝不能在保存时补章，否则重开旧会话跑新一轮会把全部 None 历史补成当前时刻。
- 全部 `Message` 构造点补 `created_at: None`（agent/repair/sessions 测试、provider 测试、subagent）；请求体专用克隆（`<current-plan-transient>` 瞬态快照）不入转录，不盖章。

### 前端（停用伪装 + 透传）

- `ipc/types.ts` `Message` 补 `created_at?`；`run.ts` `UiItem` user/assistant 补 `createdAt?`。
- `restoreFromMessages` 透传 `m.created_at`；本地回显与流式 assistant 条目创建时以本地时间盖章（重开后以持久化值为准）。
- `ChatMessages.tsx` `ts()`：无时间/解析失败一律返回 **空串**（留空不冒充）；user/assistant 均渲染真实时间。

## 行为变化

- **新会话**：每条消息时间真实持久化，重开不变。
- **存量旧会话**：时间行留空（诚实显示「无记录」），不再显示伪时间；一旦在该会话继续新 run，旧消息仍留空、新消息开始有真实时间。

## 验证

- `cargo test`：**265/265 全绿**（含 serde roundtrip、请求体形状、repair/trim、save/load 图片用例；此前因角色字段笔误连带失败的 `midstream_disconnect` 同步恢复）。
- `pnpm --dir ui test` **162/162** + `build` 通过；新增 2 用例：restore 无 `created_at` 时间行留空、有 `created_at` 显示真实时间。
