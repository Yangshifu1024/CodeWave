# 35 · 通知点击回跳对应会话

> 缺陷修复 + 跨层增强批次：站内通知栈与系统通知（Windows / macOS）点击后均回到对应会话。

## 1. 背景与根因

用户报障：弹出的系统消息，点击后无法回到对应会话。tester 复核确认三条链式根因：

1. **点击只聚焦窗口**：`AppShell` 通知栈 `onClick={focusWin}` 仅 `setFocus()`，从不切换 `activeKey`；
2. **通知无会话信息**：`ui.ts` 的 `notifications` 元素仅 `{ id, title, body }`，点击处无从得知目标会话；
3. **事件源未传 session**：`run.ts` 的 `run:done` 失焦分支 `notify(title, "任务完成")` 未携带 `p.session`。

## 2. 方案要点

- **站内通知**：通知携带可选 `sessionId`；点击按条处理——已开 Tab 直切 `activeKey`，未开从会话列表 `openSession` 重开（自带去重与 running 态恢复），会话已删除则兜底仅聚焦；点击后移除该条（顺带修复点击后悬挂 6 秒的缺口）。
- **系统通知（Windows / macOS）**：tauri-plugin-notification 桌面端 JS API 不暴露点击回调（action 仅移动端），后端 `host/notify.rs` 按平台直驱原生通知并注册回调，点击后 emit `notify:activate`（payload `{ session_id }`），前端复用同一跳转函数。tauri-winrt-notification / mac-notification-sys 均为该插件桌面端底层同款，Cargo.lock 内已有传递依赖，**零新增包版本**。
- **回退链**：项目会话先走后端 `notify_system`，失败（Linux / 权限 / API 错误）回退现有插件路径；临时会话（无 session）直接插件路径。两条路径互斥，不会双重系统通知。

## 3. 改动清单

**后端（src-tauri/）**

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | `[target.'cfg(windows)']` 增 `tauri-winrt-notification = "0.7"`；`[target.'cfg(target_os = "macos")']` 增 `mac-notification-sys = "0.6"` |
| `src/host/notify.rs`（新） | `notify_system` 命令：Windows `Toast::new(POWERSHELL_APP_ID)` + `on_activated` → `emit_to("main", "notify:activate")`；macOS `spawn_blocking` + `send_notification(MainButton::SingleAction)`，`Click/ActionButton` 时 emit 同事件；其他平台 `Err("unsupported platform")` 触发前端回退 |
| `src/host/mod.rs` · `lib.rs` | 模块声明 + invoke_handler 注册 |

**前端（ui/src/）**

| 文件 | 改动 |
|---|---|
| `ipc/client.ts` | `notifySystem(sessionId, title, body)` |
| `stores/ui.ts` | `notifications` 增可选 `sessionId`；`notify` 三参；新增 `dismiss(id)` / `dismissBySession(sessionId)` |
| `stores/run.ts` | `systemNotify(sessionId, …)` 后端优先、失败回退插件；`run:done` 失焦通知传 `p.session` |
| `stores/sessions.ts` | 新增 `revealSession(sessionId): boolean`（已开直切 / 未开重开 / 找不到返回 false） |
| `features/shell/AppShell.tsx` | `handleNotifyActivate(sessionId?)` 共用跳转（setFocus + reveal + dismiss）；通知栈点击接入；新增 `listen("notify:activate")`；删除 `focusWin` |
| `__tests__/events.contract.test.ts` | emit 扫描正则扩展 `emit_to`；新增「直连 listen」校验通道（扫描范围仅 `ui/src`，取 listen 参数事件名字面量精确比对） |
| `__tests__/notify-reveal.test.tsx`（新） | 5 用例（见 §4） |

契约影响：`notify:activate` 走 AppShell 直连 `listen`（有意设计，不进 run.ts 27 键 handler 面），契约测试 1/2 双向覆盖；`notify_system` 为新增 IPC 命令，无既有契约变更。

## 4. 验证

- 后端 `cargo test`：**253 passed / 0 failed / 0 warning**（Windows 实测）；`cargo check` 0 warning；`cargo fmt` 干净。
- 前端 `pnpm --dir ui test`：**120 passed**（含新增 5 用例：已开跳转 / 未开重开 / 已删兜底 / toast 不跳 / run:done 通知携带 sessionId）。
- 前端 `pnpm --dir ui build`：type check + vite build 通过。
- code-reviewer 七维审查：无 🔴；🟡 采纳修复——Windows AppID 改回 `POWERSHELL_APP_ID`（与插件现状一致，显示级别不回退，规避未安装 AUMID toast 不显示的回归）；macOS 发送失败补 `tracing::warn`（排队后命令已返回，失败无法传导回退链）；契约测试扫描范围收窄 `ui/src` + 精确字面量比对；测试 afterEach 补面板开关重置；`cargo fmt`。

### 手动验证清单（GUI 不做自动点验）

**Windows（dev 或打包版）**

1. 打开两个项目会话 Tab，在会话 A 发起一次长任务，Alt+Tab 切出应用；
2. 任务完成 → 右下角站内通知 + 系统 toast 同时出现；点击**站内通知** → 窗口聚焦且切到会话 A，通知消失；
3. 关闭会话 A 的 Tab（后端 run 继续跑的会话需在 6 秒通知窗口内操作，或直接重发任务），再次触发完成通知 → 点击站内通知 → 会话 A 以新 Tab 重开并落位；
4. 点击**系统 toast 通知本体** → 窗口聚焦 + 跳到会话 A（dev 形态 toast 显示为 PowerShell 图标/名称，属既有已知限制，本次无回退）；
5. 触发一条 toast 类通知（如未选会话点发送）→ 点击 → 仅聚焦窗口，不跳转。

**macOS**

1. 同场景触发完成通知 → 系统通知带「查看」按钮；点击通知本体或「查看」→ 应用聚焦并跳到对应会话；
2. 通知自然超时（不点击）→ 不跳转（符合尽力而为语义）；通知中心历史里的旧通知点击**不**保证回跳（UNUserNotification 回调仅存活于展示期，属平台行为）。

**通用**

6. 会话在通知存活期内被删除 → 点击兜底仅聚焦窗口，无报错；
7. 契约守卫：`pnpm --dir ui test` 中 `events.contract.test.ts` 双向通过。

## 5. 已知限制与非目标

- **Linux**：系统通知点击跳转不支持（后端返回 unsupported，自动回退插件路径，通知本身照常弹出）。
- **Windows toast 显示为 PowerShell**：沿用插件现状（`POWERSHELL_APP_ID`），打包安装形态后续可切自定义 AUMID（需处理未安装 AUMID 不显示的回归，另行批次）。
- **macOS 通知历史点击**：平台回调存活期限制，仅展示期内点击可回跳。
- **scheduled:*（计划任务）通知跳转**：后端事件 payload 无 session id，需契约变更，另行立项。
- `openSession` 的 `loading` 防抖窗口内点击通知，极端时序下可能被静默吞掉（低概率，审查 🟡#4 记录在案，暂不处理）。
