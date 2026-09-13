# 缺陷修复：macOS 首次通知弹「Choose Application / Where is use_default?」系统对话框

> 缺陷来源：用户实测报告——ask 需要用户确认（窗口失焦触发 ask:opened 失焦通知，[docs/session-nav-row-states](./session-nav-row-states.md)）时，macOS 弹出系统级「Choose Application」文件选择对话框（标题「Where is use_default?」），与 ask 提问卡同时出现。

## 一、现象与触发链

1. ask 弹出且窗口失焦 → 前端 `systemNotify()`（`ui/src/stores/run.ts`）→ IPC `notify_system` → 后端 `src-tauri/src/host/notify.rs` macOS 分支 `mac_notification_sys::send_notification`。
2. 该库 0.6.15 首次发通知时 `ensure_application_set()` → `get_bundle_identifier_or_default("use_default")`（`lib.rs:136`）——`"use_default"` 是**库里写死的假应用名**，用作「查不到就用默认」的哨兵值。
3. 其 ObjC 实现（`objc/notify.m:8-13`）把应用名拼进 AppleScript：`get id of application "use_default"` → 该应用不存在 → **AppleScript 弹出系统模态「Choose Application」对话框并阻塞**，用户 Cancel 后才返回错误，Rust 侧 `unwrap_or` 兜底为 `com.apple.Finder`。
4. 库内部 `Once`（`INIT_APPLICATION_SET`）保证该查询只执行一次，故只在**首次通知**出现；[docs/session-nav-row-states](./session-nav-row-states.md) 后 ask:opened 失焦通知成为常规路径，首次就撞上用户等待确认的时刻（本缺陷的触发时机）。

## 二、修复方案

利用依赖库公开 API `set_application(bundle_id)`：在每次 macOS `send_notification` 之前主动调用，传入应用自身 identifier（`xyz.yangshifu.codewave`，来自 `app.config().identifier`）。

- **打包版**：set 成功 → 通知以 CodeWave 自身身份投递（图标/归属更正确）。
- **dev 版**（bundle 未向 LaunchServices 注册）：set 失败，但 `Once` **已被消费**（库实现 `INIT_APPLICATION_SET.call_once` 无条件完成），后续永不再触发 `use_default` AppleScript 查询 → 弹框同样根除，投递退化为库原有 Finder 兜底（与缺陷现状一致，无回归）。

改动点（仅 1 文件）：

- `src-tauri/src/host/notify.rs` macOS `native_notify`：`spawn_blocking` 闭包内、`send_notification` 前，`let bundle_id = app.config().identifier.clone(); let _ = mac_notification_sys::set_application(&bundle_id);`（错误有意忽略——dev 未注册时失败属预期，投递仍走 Finder 兜底）。
- 同文件新增 `#[ignore]` macOS 手动探针测试 `probe_macos_notification_no_choose_application`：断言 `set_application("xyz.yangshifu.codewave")` 成功 + `send_notification` 全流程跑通（[docs/oss-prep-batch](./oss-prep-batch.md) 探针风格；需真机 GUI 观察，不进 CI 断言）。

契约影响面：**零**——事件面 27 键、`notify_system` IPC 签名、前端 `systemNotify` 回退链均未动；Windows/Linux 分支零变化。

## 三、验证

- `cargo test`（src-tauri/）：**367 passed / 0 failed / 5 ignored**（其中含本批新增探针），全绿。
- `cargo build`：0 warning；`cargo clippy`：notify.rs 零命中（仓库存量 32 条与本次无关，未触碰）。
- 手动探针（可选，真机）：`cargo test --lib probe_macos -- --ignored --nocapture` → 应看到一条 CodeWave 通知且**无**「Choose Application」弹框。

## 四、手动验证清单（GUI）

1. 确认无 `tauri dev` 实例在跑（单实例互斥），重新 `pnpm tauri dev` 或安装新打包版。
2. 临时会话发任意任务，在任务运行中把 CodeWave 窗口**切到失焦**（如点桌面），等模型触发 ask/审批 → 应只弹应用内提问卡 + 一条系统通知，**不得**出现「Choose Application」系统对话框。
3. run 结束通知（窗口失焦时）→ 同样无系统弹框；点击通知「查看」→ 会话回跳正常（[docs/notification-click-reveal](./notification-click-reveal.md) 链路回归）。
4. （回归）打包版通知图标应显示 CodeWave 图标（set 成功路径的附带收益）。
