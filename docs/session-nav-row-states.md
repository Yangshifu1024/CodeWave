# 会话行状态批次：等待确认行操作互斥 + 会话结束未读点 + git 头像黑底白字 + 审批等待策略可配置

> 批次报告。四项改动：三项纯前端（左栏会话行状态、头像配色），一项前后端联动（审批等待策略 + 审批系统通知）。

## 1. 等待确认的会话行隐藏重命名/删除按钮（缺陷修复）

**现象**：会话处于等待确认（ask pending）时，左栏行悬停仍显示重命名/删除按钮，且按钮组绝对定位（`right: 8px`）直接叠压在绿色「等待确认」徽标上。

**根因**：`ProjectNav.tsx` 的 `SessionRow` 中 `.row-actions` 无条件渲染在 DOM，靠 CSS 悬停显隐（`.session-nav-row:hover .row-actions`），而其定位槽位与 `askPending` 徽标同为行右侧，二者叠压。

**修复**：`.row-actions` 改为条件渲染 `{!askPending && (...)}`，复用行内已有的行级订阅（`useRun((s) => !!s.tabs[meta.id]?.ask)`）。等待确认期间按钮不可见不可点，`ask:closed` 后恢复。CSS 零改动。

## 2. 会话结束未读点（新功能）

**语义**：会话 run 结束（`run:done` / `run:error`）时若用户不在该会话（`activeKey !== session`），左栏该会话行的运行图标槽位（`.session-run-slot`，12px 恒渲染占位）显示 8px 圆形未读点（`var(--ws-accent)`，`title="有未读回复"`）；进入会话后消失。

**实现**：

- `sessions.ts`：`SessionsState` 新增 `unread: Record<string, boolean>` + `markUnread` / `clearUnread`（内存态，不持久化——store 本无 persist 中间件，重启后无未读概念）。
- **清除收口点 = store 订阅层**：模块底部 `useSessions.subscribe` 监听 `activeKey` 变化即 `clearUnread(newActiveKey)`。不变式「活跃会话永远无未读」由此结构性成立，覆盖 openSession（已开聚焦/未开重开）、revealSession（通知回跳）、cycleTab、navBack/navForward、closeTab 邻位切换全部入口，后续新增进入路径自动覆盖；`set` 幂等（无未读时返回原 state 不触发通知，无循环）。
- `run.ts`：`run:done` / `run:error` handler **顶部、幂等守卫之前**调用 `markUnreadIfAway(p.session)`——置于守卫前是关键：Tab 已关的迟到结束事件（状态桶已删，`!before?.running` 会提前 return）也能标记，行上才能收到「有新结果」信号。`run:cancelled` 不标（取消只能在本会话内发起，用户在场）。
- `ProjectNav.tsx`：槽位三态 `running ? spinner : unread ? dot : null`——`!running` 门槛使队列接续运行期间 spinner 优先，队列全部排空后才显点。
- `app.css`：`.session-nav-row .session-unread-dot { width:8px; height:8px; border-radius:50%; background:var(--ws-accent); }`。

## 3. 左下角 git 身份头像默认生成态改黑底白字

`AppShell.tsx` 的 `SiderFooter`（展开态 26px）与 `SiderRailFoot`（折叠窄轨 22px）两处：已配置 git 身份时生成的首字母头像由 `hsl(hashHue(seed) 45% 45%)` 彩色改为 **`#000` 背景 + `#fff` 字**（显式 `color:"#fff"`）；未配置身份的「?」保留 `var(--ws-border)` 灰底作视觉区分。`hashHue` 函数随之删除（无其余使用处，避免 `noUnusedLocals` 构建失败）。

## 4. 审批等待策略可配置 + 审批系统通知（前后端）

**现状**：ask 提问工具本就无超时（`tools/ask.rs` 只等应答或 run 取消）；唯一超时点是审批门（`safety/approval.rs`）——`APPROVAL_TIMEOUT = 120s` 硬编码，超时自动**拒绝**（`E_APPROVAL_DENIED`），不可配置。用户放置不管的审批 2 分钟后即静默失败。

**改造**：

- `core/config.rs`：`ApprovalSettings` 新增 `auto_confirm: bool`（serde default，默认 false，旧配置兼容）。语义：**勾选 = 审批 5 分钟（`AUTO_CONFIRM_AFTER = 300s`）无响应自动确认推荐选项；未勾选 = 永不超时、无限等待**。
- `safety/approval.rs`：`ApprovalRequest` 新增 `auto_confirm` 字段（`confirm` 只收 `rt/sink`，配置经 4 个调用点从 `core.cfg` 读取传入：`batch.rs` 文件写入确认与计划外步骤确认、`command.rs` 命令审批、`service.rs` 后台服务命令）。等待分支二态：
  - 未勾选：`select! { cancel => None, rx => Some(r) }`——无限等待；
  - 勾选：`cancel` 分支 + `tokio::time::timeout(AUTO_CONFIRM_AFTER, rx)`，超时置标记 → outcome `{approved: true, always: false}`（推荐选项 = 允许，**不做**「始终允许」——该选择爆炸半径更大，不无人值守代做），session_log info「5 分钟未响应，自动确认推荐选项（允许）」。
  - run 取消中断（H2 修复）两态均保留。
- **审批系统通知**：`run.ts` 的 `ask:opened` handler 内，桶存在且 `!document.hasFocus()` 时 `systemNotify(session, 会话标题, kind === "approval" ? "等待你的确认" : "等待你的回答")`——与 `run:done` 通知同门槛（聚焦时应用内已有询问窗，不发）；复用 [docs/notification-click-reveal](./notification-click-reveal.md) 点击回跳链路（`notify_system` → 点击 → `notify:activate` → `revealSession`），点击通知即进入该会话看到审批窗。
- **设置界面**：`SettingsModal` 安全页签新增开关「5 分钟后自动确认推荐选项」+ extra 说明（勾选：审批 5 分钟无响应自动允许；不勾选：始终等待）；i18n 双语 key `settings.autoConfirm` / `settings.autoConfirmHint`；`ipc/types.ts` 类型同步。

**测试**：`approval.rs` 新增两条 `#[tokio::test(start_paused = true)]`（虚拟时钟）：勾选后推进超 300s 自动 approved 且 always=false；未勾选推进 1 小时仍挂起（asks 表条目在）、应答后按应答返回。为此 dev-dependencies 引入 `tokio features=["test-util"]`（仅测试构建）。前端新增 `projectnav.row-states.test.tsx`（行操作互斥 / 未读点标记与清除 / 迟到事件标记 / 失焦通知），`sidebar.style.test.ts` 补 `.session-unread-dot` CSS 契约，app.smoke 安全页签补新开关文案断言。

## 验证

- `cargo test`：267 passed / 0 failed / 0 warning（`midstream_disconnect_maps_to_network` 曾在全量并发下单次偶发失败，隔离复跑与全量复跑均绿，系网络时序敏感的既有用例，与本次改动无关——provider 层零改动）
- `pnpm --dir ui test`：168/168 全绿；`pnpm --dir ui build` 通过

## 手动验证清单

1. 会话等待确认：左栏行悬停 → 无重命名/删除按钮、徽标完整；答复/忽略后悬停 → 按钮恢复。
2. A 会话运行时切到 B：A 结束后 A 行运行槽位出现墨色未读点；点击进入 A → 点消失。
3. A 运行失败结束 → 同样出点；关闭 A 的 Tab 后 run 结束 → 左栏行同样出点。
4. 队列接续运行期间 → 只显 spinner 不显点，队列排空后出点。
5. 系统通知点击回跳进入会话 → 点消失。
6. 左下角头像黑底白字，折叠窄轨同款；未配置 git 身份时灰色「?」不变。
7. 默认（未勾选）：触发审批后放置超 5 分钟 → 询问窗不消失、run 不失败，拒绝/停止才结束。
8. 设置-安全勾选「5 分钟后自动确认推荐选项」：触发审批后放置 5 分钟 → 自动按「允许」继续执行，会话日志出现「自动确认推荐选项」。
9. 失焦时审批/提问弹出 → 收到系统通知；点击通知 → 进入该会话看到询问窗。
