# CodeWave · 11 思考过程穿插显示 实施报告

> 日期：2026-08-31
> 需求：思考过程（thinking/reasoning）从「顶部整块输出」改为「按时间顺序穿插在工具调用之间」。
> 范围：后端流式管线（节流 → 帧 → 装配）+ 前端转录模型（store → 渲染 → 历史恢复）。
> 约束遵守：零 git 操作；界面改动不自动化点验，交付手动验证清单（§7）。

---

## 1. 根因

顺序信息在数据流的三处被压扁，导致 UI 只能按固定布局渲染（思考 → 全部工具卡 → 正文）：

| 层 | 位置 | 压扁方式 |
|---|---|---|
| 后端节流 | `util/throttle.rs` StreamBuffer | `text`/`reasoning` 两个独立字符串缓冲，64ms 窗口内交错顺序丢失 |
| 后端帧 | `Frame::Delta{text, reasoning}` | 一个帧同时携带两种增量，无序信息 |
| 后端装配 | `Assembled{text, reasoning}` | 整个 run 的 reasoning 拼成一个字符串 → 历史消息里单个 Thinking 块 |
| 前端 store | `run.ts` AssistantItem | `{text, reasoning, tools[]}` 平行字段，整个 run 一条 UI 项 |
| 前端渲染 | `ChatMessages.tsx` | 固定顺序布局，非时间序 |

## 2. 方案（三层贯通：帧到达序 = 展示序）

product-manager 评审结论：不采纳「保留单 Delta 帧 + 前端启发式排序」——64ms 窗口内时序已丢失，不解决根因。

1. **节流段序列化**：`StreamBuffer{text, reasoning}` → `segments: Vec<Segment>`（`Text/Reasoning` 两变体）。push 时与末段同 kind 合并、异 kind 新开段，控制高频下的段数；`try_take/take_final/reset` 接口不变，flush 产物天然保序。
2. **帧协议拆分**：`Frame::Delta` 拆为 `DeltaText{text}` 与 `DeltaThinking{text}`；flush 循环按段序逐段发帧（`flush_segments`）。Channel 帧不在 events.ts 24 键契约内，事件契约测试不受影响。
3. **装配块序列化**：`Assembled{text, reasoning}` → `blocks: Vec<AsmBlock>`（`Text/Thinking`），`build_assistant_message` 按 blocks 顺序生成 `Content::Text / Thinking` 交替序列 + ToolUse 后置 → 历史消息按真实顺序存块。
4. **前端 timeline 化**：AssistantItem 改为 `{timeline: TimelineSeg[], toolsMap: Record<callKey, ToolView>, streaming}`；`TimelineSeg = text | thinking | tool锚点`。流式帧、`tool:result`、历史恢复、`run:retry` 全部走同一 timeline 结构。
5. **交互形态**（用户选定）：思考块用 antd `Collapse` ghost 折叠面板，**默认收起**，点击展开查看。

## 3. 改动清单

| 文件 | 改动 |
|---|---|
| `src-tauri/src/util/throttle.rs` | Segment 枚举 + segments 缓冲；新增交错顺序单测 |
| `src-tauri/src/provider/dto.rs` | `AsmBlock` + `Assembled.blocks`（push_text/push_thinking/is_empty/joined_text）；新增合并顺序单测 |
| `src-tauri/src/core/agent.rs` | Frame 拆 `DeltaText/DeltaThinking`；`flush_segments` 按段发帧；`collect_deltas` 写 blocks；`build_assistant_message` 按序成块；空响应判定改 `Assembled::is_empty()`；`final_text` 取 `joined_text()`；测试更新 + 新增穿插断言 |
| `ui/src/ipc/types.ts` | Frame 联合拆 `delta_text` / `delta_thinking` |
| `ui/src/stores/run.ts` | `TimelineSeg`/`toolsMap` 模型；`applyFrameToTab`（导出，send 的 onmessage 与测试共用）；`ensureToolAnchor` 工具卡锚点穿插；`restoreFromMessages` 不再丢弃 thinking 按 content 序恢复；`run:retry` 清 timeline；`service:update` 遍历 toolsMap |
| `ui/src/features/chat/ChatMessages.tsx` | `ThinkingBlock`（antd Collapse ghost，默认收起）；timeline 顺序渲染（thinking 折叠 / 工具卡 / markdown 正文分段）；流式光标跟最后一个未定稿 text 段；自动滚动依赖改为 segments 总量 |
| `ui/src/theme/app.css` | `.reasoning` → `.thinking-block`（Collapse 头部与正文体样式，颜色走 `--ws-*` token） |
| `ui/src/i18n/{zh-CN,en-US}.ts` | 新增 `chat.thinking`（思考过程 / Thinking） |
| `ui/src/__tests__/run.interleave.test.ts` | 新增 5 用例（§5） |
| [docs/thinking-interleave-report](./thinking-interleave-report.md) | 本文档 |

## 4. 兼容性

- **存储格式零迁移**：`Message.content` 本就支持混合序列；新版按真实顺序写块，旧记录（thinking 块至多一个且在首位）解析结果与旧版渲染等价。
- **出站 sanitize 行为不变**：`repair::sanitize` 照旧丢弃 Thinking 块（anthropic 签名约束），回放路径无影响。**注意**：保存路径 `prepare_for_save` 同样经 sanitize，因此磁盘历史中实际不含 thinking 块；「历史恢复展示思考」仅对内存中的会话（本轮运行内重开 Tab）成立，跨重启恢复时思考块不出现（sanitize 已丢弃），属预期行为（审查 C3 修正：此前「已落盘的思考块可恢复展示」表述不成立）。
- **Frame 仅 IPC 内存传输**，不落盘、不涉及配置 serde 兼容。
- 前后端帧协议同批次变更：旧后端 + 新前端（或反向）时 delta 帧被前端静默忽略（onmessage 只处理认识的 type），不崩溃、只是无增量，属可接受降级。

## 5. 验证

| 项 | 结果 |
|---|---|
| `cargo test`（src-tauri/） | ✅ **150 passed / 0 failed / 0 warning**（147 基线 → 150：throttle 段序、Assembled 合并、assistant 装配穿插各 +1） |
| `pnpm --dir ui test` | ✅ **21/21**（16 基线 + 5 新增穿插用例：帧序穿插 / tool 锚点穿插 / restore 同构 / retry 无残留 / 多步相邻） |
| `pnpm --dir ui build` | ✅ tsc --noEmit + vite build |

## 6. 已知边界

- 帧粒度的穿插精度 = 64ms 节流窗口（与流式渲染节奏一致；窗口内同类增量合并，跨类保序）。
- openai 系协议的 reasoning 增量若与正文同窗到达，按到达序穿插（协议本身不保证块边界，属预期）。
- `ThinkingBlock` 的展开状态不持久化（默认收起，与 antd Collapse 受控语义一致）。

## 7. 手动验证清单（GUI 不做自动点验）

前置：`pnpm tauri dev`，选择一个已配置 reasoning 模型（设置中模型标注 · reasoning）的会话。

1. **流式穿插**：发送一个会触发多步工具调用的任务 → 观察：每个「思考过程」折叠面板出现在其对应步骤的工具卡**之前**，正文段穿插其间；点击折叠面板可展开查看思考文本。
2. **默认收起**：思考块默认只显示「思考过程」标题行，不展开。
3. **历史恢复（限内存内）**：完成一轮后关闭 Tab → 从左侧导航重开会话 → 思考/工具卡/正文顺序与流式期间一致。注意：跨应用重启后思考块不再出现——保存路径经 sanitize 丢弃 thinking（anthropic 签名约束），属预期（见 §4）。
4. **自动滚动**：流式期间停在底部 → 内容增长持续吸底；向上翻阅时不被强行拉回。
5. **取消与重试**：流式中点停止 → 「已取消」出现，无半截思考残留；（可选）模拟请求失败重试 → notice 提示后 timeline 从空重建。
6. **多 Tab**：两个 Tab 各跑一个任务 → 各自 timeline 独立穿插，互不串扰。
7. **回归**：无 reasoning 模型（关闭思考）跑任务 → 界面无思考块，工具卡与正文渲染正常。

## 8. 审查修复记录（同批次）

code-reviewer 七维度审查结论：无 🔴；3 项 🟠 + 若干 🟡，全部修复：

| 编号 | 发现 | 修复 |
|---|---|---|
| C1 | Channel 帧与事件总线无跨路径序保证：run:done 先处理时，迟到尾帧重建流式条目 → 幽灵光标 | `applyFrameToTab` 加 `t.running` 守卫，run 结束后帧直接丢弃；补回归测试 |
| C2 | retry 竞态：`run:retry` 清空 timeline 后，旧 attempt 在途帧又落入 → 错序残留 | 节流批次引入代次 `gen`（`reset()` 递增、随帧携带、随 run:retry 下发）；前端丢弃 `gen < t.streamGen` 的帧；补回归测试 |
| C3 | 报告声称「已落盘思考块可恢复展示」与实现矛盾（保存路径 sanitize 必丢 thinking） | 修正 §4/§7.3 措辞，明确恢复仅限内存内 |
| C4 | 收尾时 ticker 与 take_final 双 flusher 微竞序 | 收尾前 `flush_stop.cancel()` 后等待一个节流窗口再 take_final |
| M1 | run:retry 用 `items[length-2]` 位置耦合定位 assistant 条目 | 改为从尾就近查找 streaming assistant |
| M2 | StreamBuffer 字段 pub 外泄内部结构 | 收紧为 `pub(crate)` |
| T1/T2 | 缺帧级契约与 C1/C2 回归测试 | 新增 `flush_segments_emits_frames_in_segment_order`（CaptureSink）与前端两个用例；后端 152 / 前端 23 全绿 |
