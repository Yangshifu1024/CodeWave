# 子智能体交互批次：聊天卡 + Composer 运行指示器 + 过程抽屉 + 会话恢复

> 需求：① 聊天框简单显示「子智能体 <名称> · <分配的任务>」；② Composer 左下角机器人头像 + 运行中数量，数量=1 自动弹抽屉完整显示执行过程（与主聊天同构），数量>1 向上 dropdown 选择后弹框；③ 抽屉默认常显、左上角关闭、关闭不销毁可再进；④ 全部结束后图标消失，聊天列表项仍可点击回看完整过程；⑤ 恢复会话时一并恢复。
> 抽屉位置经用户确认：**右侧向左滑出**（需求文字「左侧向右」与截图冲突，AskUserQuestion 定案）。

## 一、现状与根因（调研结论）

- 聊天已有子代理卡 `SubagentItemCard`（`sub:spawn` 落 timeline 锚点），但不可点击、无「子智能体」前缀，且 `send()` 执行 `t.subs = []`（原 L-5）导致上一轮的卡直接消失。
- 子代理内部**没有逐字流**：`stream_flush_loop` 的帧 flush 不受 `emit_events` 门控（agent.rs），但 sub_id 从未注册 Channel → `TauriSink::channel_frame` 查不到 → 帧静默丢弃。前端只有 800ms 采样的 `sub:step` 摘录 + 最终 `sub:report`。
- 全库无 Drawer 先例；`restoreFromMessages` 不识别子代理条目（恢复后只有一张通用工具卡）；子代理内部历史从不持久化（[docs/session-logging-report](./session-logging-report.md) 的「子代理全轨迹」是文本日志，非结构化转录）。
- `tool:result`/`tool:error` 事件对子代理驱动是**无条件发射**的（batch.rs `emit_result` 无 emit_events 门控），payload `session = sub_id`，此前因前端 `s.tabs[sub_id]` 查不到 Tab 而被丢弃——正好可作为子代理流内工具卡的回填通道。

## 二、总体设计

**零新增事件键**（27 键契约不动）。子代理的流式帧经 **`Frame::Sub` 信封**借父会话 Channel 下发，前端按 `sub_id` 解包路由进每子代理独立消息流；子代理完整历史在结束时落盘 sidecar，会话恢复后按需重建。

```
spawn → sink.bind_sub_channel(parent, sub)
  ├─ sub 帧经 TauriSink 包装 Frame::Sub{sub_id, frame} → 父 Channel → 前端解包 → applyFrameToSub
  ├─ tool:result/tool:error（session=sub_id）→ onToolResult 按 subStreams 归属回填子代理工具卡
  └─ 结束 → store.save_sub_history(parent, sub, history) → histories/subs/<parent>/<sub>.json.gz
恢复 → restoreFromMessages 识别 tool_use(subagent)+tool_result(sub_id) → 归档卡 → 开抽屉时 load_subagent_history 重建流
```

## 三、后端改动（src-tauri）

| 文件 | 改动 |
|---|---|
| `core/agent.rs` | `Frame` 加变体 `Sub { sub_id, frame: Box<Frame> }`（serde tag="sub"）；`EventSink` 加 `bind_sub_channel` 默认空实现（测试 sink 零改动） |
| `host/events.rs` | `ChannelRegistry` 加 `subs: DashMap<sub, parent>`；`bind_sub_channel` 记录绑定；`channel_frame` 命中 sub 绑定时包装 `Frame::Sub` 借父 channel 下发（绑定常驻，父 run 结束前子代理必已收尾，无父通道重注册错投窗口） |
| `tools/subagent.rs` | spawn 后调 `sink.bind_sub_channel`；`sub:spawn` payload 增加 `name`（注册表规范名）与 `task`（**全量**，抽屉首块；2026-09-18 前为 `trunc(2000)`，会使实时面板显示「…(truncated)」而落盘历史是全文，实时/归档不一致）；`ToolOutcome::ok` data 首位加 `sub_id`（compact head 截断后仍保留，恢复时关联过程文件）；drive 返回后（ok/err）+ `SubCleanupGuard` panic 路三处调 `store.save_sub_history` |
| `core/sessions/mod.rs` | 新增 `save_sub_history`/`load_sub_history` → `histories/subs/<parent>/<sub>.json.gz`（复用 repair/trim/gzip 管线，**不 upsert 会话索引**，子代理不得出现在 list_sessions）；`remove`（delete_session）级联删除 `histories/subs/<parent>/` 整目录 |
| `host/commands.rs` + `lib.rs` | 新命令 `load_subagent_history(session_id, sub_id) -> Vec<Message>`；文件缺失返回空（旧会话降级） |

**兼容性**：`Frame::Sub` 为新增变体（主会话帧永不包装）；`sub:spawn` payload 与 outcome data 均为附加字段；配置零变化；serde default 语义不受影响。

## 四、前端改动（ui/src）

| 文件 | 改动 |
|---|---|
| `ipc/types.ts` | `Frame` 联合加 `{ type:"sub"; sub_id; frame }`；`SubagentEvent` 加 `name?/task?` |
| `ipc/client.ts` | `loadSubagentHistory(sessionId, subId)` |
| `stores/run.ts` | ① `SubView` 加 `name/task`；新增 `SubStream`（timeline/toolsMap/status/gen/loaded）与 `TabRunState.subStreams`、`subDrawer`（**per-Tab**）；② `applyFrameToTab` 拦截信封帧 → 新导出 `applyFrameToSub`（镜像主归约：delta 追加、tool_progress 落锚，惰性建流——channel 帧与 sub:spawn 事件分属两链路无到达序保证；流按 status 收口丢弃迟到帧）；③ `onToolResult`：session 无对应 Tab 时按 `subStreams` 反查归属 Tab 回填子代理工具卡，create/edit 同步 bump `writeTick`（[docs/session-artifacts-and-files-tab](./session-artifacts-and-files-tab.md) 归属语义顺带补全——此前子代理写文件不刷新 Files 页签）；④ `send()` **移除 `t.subs = []`**（归档化）；⑤ `sub:spawn` 初始化流 + **运行数 0→1 自动弹抽屉**；`sub:done/error` 同步收口流；⑥ `restoreFromMessages` 预扫 tool_result，`tool_use(name="subagent")` → `{kind:"sub"}` 锚点 + 归档 SubView（outcome 解析 `sub_id/role/report`，旧会话无 sub_id 合成 `restored:<callId>` 降级）+ 空流注册；⑦ `openSubDrawer`（归档子代理首次打开按需拉历史 → `messagesToSubStream` 重建；运行中不拉）/ `closeSubDrawer`（只关不销毁） |
| `features/chat/segments.tsx`（新） | 自 ChatMessages 抽取 `renderCached`/`ThinkingBlock`/`TimelineSegsView`（thinking/工具卡/子代理卡/markdown 正文按 timeline 序渲染），主聊天与过程抽屉共用同一套视觉；ChatMessages.AssistantMessage 改为消费 `TimelineSegsView`（渲染逻辑零变化纯搬移） |
| `features/subagent/SubagentDrawer.tsx`（新） | antd Drawer：`placement="right"`、`mask={false}`（常显监控不挡操作）、`destroyOnHidden={false}`（关闭不销毁）、自定义宽度 `min(560px, 45vw)` 走 `styles.wrapper`（antd 6 `width` prop 弃用告警）；自绘头部**左上角关闭** + 机器人 + 「子智能体 <规范名> · <任务>」+ 状态 Tag；正文 = 任务首块（user 气泡）+ `TimelineSegsView` 实时/重建消息流（流式光标/自动贴底跟随、滚轮上滚暂停）；降级分支（无过程文件）显示任务 + 最终汇报 + 说明行 |
| `features/subagent/SubagentItemCard.tsx` | 重写为单行可点击卡：「子智能体」+ 规范名胶囊 + 「·」+ description（省略）+ 状态图标（运行中 spinner/✓/✕）+ `step/maxSteps · N tok`；点击 → `openSubDrawer`（Enter/Space 可达） |
| `features/chat/Composer.tsx` | `.toolbar-left` 权限胶囊后加指示器：运行数 0 隐藏；=1 机器人+数字、点击直达抽屉；>1 向上 Dropdown（`placement="topLeft"`）列出各子代理（角色/描述/状态着色）选择后打开 |
| `features/shell/AppShell.tsx` | 挂载 `<SubagentDrawer/>`（per-Tab 状态，切换 Tab 各自记忆） |
| `i18n/zh-CN.ts` / `en-US.ts` | 新 `subagent.*` 命名空间（label/close/statusRunning/statusDone/statusError/taskLabel/reportLabel/noProcess/runningCount） |
| `theme/app.css` | `.sub-card` 单行卡、`.sub-drawer*` 抽屉（头部/任务块/流/降级说明）、`.subs-indicator`/`.subs-menu-*` 指示器与菜单，全量走 `--ws-*` token（墨色语言，暗色自动） |

## 五、验证

- 后端：`cargo test` **269 passed / 0 failed / 0 warning**。新增：`sub_history_round_trip_not_indexed_and_cascade_removed`（往返/不进索引/缺失返回空/级联清理）、`frame_sub_envelope_serialization_shape`（信封 JSON 形状锚点，对应前端 Frame 联合）。
- 前端：`pnpm --dir ui test` **179 passed**（新增 `run.subagent.test.ts` 8 例：信封路由/自动弹抽屉/迟到帧不入流/子代理工具结果回填+writeTick/归档跨 run/恢复识别/合成 key 降级/按需拉取与只关不销毁；`subagent.drawer.test.tsx` 7 例：卡片单行与点击/归档可点击/抽屉头部左上角关闭与流渲染/关闭保留/指示器 0 隐藏·1 直达·2 菜单选择）；`pnpm --dir ui build` 通过。
- 事件契约测试（27 键双向扫描）未动即绿——本批次零新增事件键。

## 六、已知边界

- 运行中状态不跨进程重启：恢复语义 = 聊天卡 + 完整过程回看（运行数徽标天然为 0）。
- 旧会话（本批次前）无过程落盘：抽屉降级显示任务 + 最终汇报（outcome 截断到连 report 都缺失时显示说明行）。
- 抽屉 overlay 覆盖 RightBar 与聊天区右侧；`mask=false` 不阻塞主区交互。
- 子代理 Channel 绑定进程内常驻（数量 ≤ 并发上限 4 的累计值，无害）；窗口关闭时 channel 发送失败仅 debug 日志。

## 七、手动验证清单（GUI 不做自动点验）

1. **单子代理实时抽屉**：让主代理委派一个子代理（如 `$explore` 调研任务）→ 聊天出现「Ⓐ 子智能体 explore · <任务>」单行卡；Composer 左下出现 🤖1；右侧自动滑出抽屉，逐字显示子代理的思考/正文/工具卡（工具卡可展开看结果），流式光标 + 贴底跟随。
2. **多子代理**：让主代理并行委派两个子代理 → 指示器变 🤖2，点击弹向上菜单列出两者（运行中橙点）；选择另一个 → 抽屉切换；聊天中两张卡各就各位。
3. **关闭不销毁**：抽屉左上角 ✕ 关闭 → 主界面恢复；点 🤖（或聊天卡）→ 重新打开且过程内容完整未丢失。
4. **收口**：子代理完成 → 卡变 ✓ 与步数/token 定格、抽屉状态 Tag 变「已完成」；全部结束后 🤖 指示器消失；抽屉仍可停留在已完成视图。
5. **回看**：同会话发起新任务（多轮后）→ 上一轮子代理卡仍在原位，点击仍可打开完整过程（归档未清）。
6. **会话恢复**：切换到别的会话再切回（或关 Tab 重开）→ 历史中子代理调用显示为子代理卡（不再是通用工具卡），点击 → 抽屉从落盘历史重建完整消息流（思考/工具卡/汇报）。
7. **降级**：打开一个本批次之前的旧会话中的子代理卡 → 抽屉显示任务 + 最终汇报与「未记录过程详情」说明，不报错。
8. **暗色/窄窗**：暗色主题下抽屉与卡片配色正常；窄窗口抽屉宽度收窄至 ≤45vw。
