# 会话保存与恢复优化 · 批1「回到现场」实施报告

> 需求与方案：`.codewave/tasks/20260916-011940-session-restore/`（`requirement.md` 24 条设计共识 / `plan.md` 分批与文件级改动点）。
> 分支：`feat/session-restore-and-storage`｜基线：`main @ 63a8147`（v0.3.6）｜批2「存储与完整性」未开工。

## 1. 目标与验收线

- **验收线**：关闭应用再打开 → 打开的项目与每个会话回到关闭前（Tab 集合/顺序/活跃 Tab/项目定位/滚动锚点/草稿/队列/树态/面板态），内容丝毫不差
- **耐久线**：崩溃/强杀最多丢 1–2 秒；半截回复落盘并标「已中断」
- 持久化边界：会话现场态 → `~/.codewave/ui-state.json`；主题/语言/字体/左右栏开合仍留 localStorage（首屏同步可得、防闪烁），两类不双写

## 2. 后端（新增模块，全部带单测）

| 模块 | 职责 | 要点 |
|---|---|---|
| `core/ui_state.rs` | 全局 `ui-state.json` | schema v1 + `util/atomic.rs::atomic_write`；损坏备份 `.corrupt`、版本不匹配备份 `.v<N>.bak`（**备份而非删除**）；超体积写入拒绝；窗口几何越界回落主屏居中（`resolve_window_geometry`） |
| `host/commands/ui_state.rs` | IPC `get_ui_state` / `set_ui_state` / `resolve_exit_request` / `clear_session_interrupt` | 命令只校验 + 转调 core；`ExitGuard` 单飞、取消时作废看门狗、confirm 幂等 |
| `core/sessions/interrupt.rs` | 进程运行标记与中断语义 | `<data_dir>/running.marker` 存在 = 上次异常退出 → `recover_after_crash` 把 `running` 会话标 `interrupted(crash)`；正常退出走 `finish_normal_exit`（删 marker）；退出前中止标 `quit`；`InterruptWatchSink` 装饰事件汇，run 收尾事件即清 `running` 且不凭空造 `sub_*` 条目 |
| `core/sessions/store.rs` | `SessionMeta` 中断标记 | `running`（按会话 id 排序的列表）与 `interrupted {kind: crash\|quit, at}`，serde default 向前兼容；`upsert_meta` 保留标记，不被列表刷新抹掉 |
| `lib.rs` | 启动钩子与退出拦截 | 启动 `recover_after_crash` + `create_marker`，事件汇包 `InterruptWatchSink`；`RunEvent::ExitRequested` → prevent_exit + `app:exit_requested{running}` 下发 → 无 run：2 秒看门狗；有 run：等前端应答，并带**两道兜底**（见下） |

### 退出拦截的两道兜底（审查 🔴 返工）

后端在 `prevent_exit()` 之后等前端决定，本身就已有一个「卡死用户」的缺口：前端 JS 未启动、启动窗口期事件未绑定、或应答抛错后用户不再交互时，应用会永久退不出去，只能强杀进程。因此 `handle_exit_requested` 补两道兜底（`host/commands/ui_state.rs`）：

| 兜底 | 覆盖场景 | 优次 |
|---|---|---|
| **二次触发放行** | 「用户不耐烦」：前端活着但不应答，用户再点一次托盘退出 / 再按一次 Cmd+Q（`app.exit(0)` 会重新触发 `ExitRequested`） | 高（先到先赢，立即放行） |
| **有 run 时 45s 长看门狗** | 「前端死了」：JS 未启动、启动窗口期、应答抛错后无人交互 | 低（时间维度兜底） |

互不干扰的关键在放行权统一：两条路径都只能经 `ExitGuard::confirm()` 抢「已确认」位，而 `confirm()` 会置 `confirmed` + 清 `awaiting` + `epoch += 1`；看门狗到期时用 `confirm_if_still_awaiting(old_epoch)` 双条件（仍 awaiting **且** epoch 未变）判定，因此**迟到超时不生效**——用户选「等完成」后不会被超时踢出。45s 取值必须 > `EXIT_ABORT_DRAIN_SECS`（30s）的收尾窗口，否则会抢占用户选「中断并保存后退出」时的收尾。

放行前统一走 `force_exit`：在跑会话 `mark_quit_interrupted`（不落标记会在删 marker 后永久残留 `running: true`，界面永远「运行中」）→ `finish_normal_exit` → `exit(0)`。

## 3. 前端（接线是本批的主要工作量）

| 模块 | 职责 |
|---|---|
| `utils/uiState.ts` | 快照构建/校验/hydrate/防抖落盘（1.2s 防抖 + **2s 最长等待**——纯防抖在流式持续输出时每帧被重置，会一次都不落盘）；写失败节流上报不静默；窗口几何读**逻辑像素 + 内尺寸 + 外框位置**（后端用 `set_size(LogicalSize)`/`set_position(LogicalPosition)`，物理像素会导致 macOS 2x、Windows 125–150% 下几何恢复失效） |
| `utils/scrollAnchor.ts` | 纯函数锚点协议（`itemSig` / `computeAnchor` / `pickTarget` / `collectNodes` / `captureAnchor` / `restoreAnchor` / `BOTTOM_EPS`） |
| `features/shell/AppShell.tsx` | **唯一编排点**：**先绑定事件**（`app:exit_requested` 是单次下发，绑定晚了会永久丢失 → 应用退不出去）→ `refresh + loadProjects`（顺序不可倒，见下）→ `loadUiState` → `applyUiStateToStores` → `initUiStatePersistence` → 急切加载活跃 Tab；退出拦截与关 Tab 确认两个弹窗；顶部中断提示条 |
| `features/chat/ChatMessages.tsx` | 移除「切 Tab 贴底硬重置」；滚动记录锚点（200ms 防抖 + **防抖窗口内切 Tab 时给旧会话写 `bottom`**，绝不把新会话的几何写给旧会话）；激活时按锚点还原；懒加载首帧不落位 + 最多 3 帧二次校正；3 帧仍失配则降级贴底并改写锚点，避免反复无效校正 |
| `features/shell/ProjectNav.tsx` | 会话行中断徽标 + 清除入口（`stopPropagation` 防误触切会话）；树展开/折叠态改读 `useUi` store |
| `stores/ui.ts` | `exitRequest` / `closeTabRequest` / `treeExpand` / `treeCollapsed`——事件 handler 与弹窗分处两层，store 是唯一交点 |

### 三个必须写下来的设计决定

1. **事件绑定必须早于挂载链上任何 `await`**：`app:exit_requested` 单次下发不重放，而 `load 配置 → refresh → loadProjects → loadUiState` 每一步都是 await，期间到达的退出请求在订阅到位前会永久丢失。
2. **hydrate 必须在 `refresh()` + `loadProjects()` 之后**：`restoreTabs` 要用会话/项目列表校验引用（已删的一律静默剔除）。顺序倒了就是「重启后 Tab 全没了」——本批最容易踩的坑。
2. **树展开态放 store 而不是组件 `useState` / 模块内存**：hydrate 是异步读盘，晚于 `ProjectNav` 首渲染；挂载时读一次模块内存永远读到空值。放进 store 后 hydrate 的 `setState` 能推给已挂载的订阅者，把「快照何时到货」与「谁在监听」解耦。
3. **滚动锚点存「顶端条目 sig + 段内偏移」而非像素**：消息被裁/被压缩/图片异步撑高时，sig 能唯一定位，像素不能。`scrollAnchor.test.ts` 有专门用例守护「防抖窗口内切 Tab 的会话校验」。
4. **retained（关 Tab 时用户选择保留的内容）写盘前必须按会话列表收敛**：无条件全量写盘会让「已删会话」的草稿留在 `ui-state.json`，下次启动又被搬回驻留表 → 永久复现（E14 孤儿草稿）。删除路径也额外显式清一次，两侧互为保险。

## 4. 前后端契约变化

- **事件面 27 → 28 键**：新增 `app:exit_requested`。`events.contract.test.ts` 双向动态取集合，无需改动；`ui/src/stores/run.ts`、`ui/src/ipc/events.ts`、`AGENTS.md`、`CONTRIBUTING.md` 的「27 键」表述已同步为 28
- **`SessionMeta` 新增 `running` / `interrupted`**（serde default 向前兼容，旧索引可读）
- 新增 IPC：`get_ui_state` / `set_ui_state` / `resolve_exit_request` / `clear_session_interrupt` / `list_running_sessions`

## 5. 验证

| 项 | 结果 |
|---|---|
| `cargo test`（`src-tauri/`） | 615 passed / 0 failed / 0 warning |
| `pnpm --dir ui test` | 51 文件 / 424 tests 全绿（批1 新增 `uiState` / `scrollAnchor` / `projectnav.tree-state` / `interrupt-marker` / `appshell.restore` 五个测试文件） |
| `pnpm --dir ui build` | type check + vite build 通过 |
| 界面验收 | 按 AGENTS.md 约定不做 GUI 自动点验，交付分步手动清单 |

**批1 修复的两个现行回归**：① 有 run 在跑时应用退不出去（退出拦截弹窗前端缺失，后端已在无限等应答）；② 有草稿时关 Tab 静默无响应（`closeTab` 判定已启用、弹窗缺失）。两者均已在 `AppShell.tsx` 接上。

## 6. 已知盲区（只能人工确认）

滚动锚点真实像素与多显示器几何、真实浏览器异步撑高（mermaid/katex/图片）后的二次校正是否够用（当前 3 帧上限）、断电级刷盘、虚拟滚动帧率。批2 引入分页与虚拟滚动后需重新回归锚点。

## 7. 与 plan.md 的偏差（登记）

- **窗口几何越界回落**实现在后端（`core/ui_state.rs::resolve_window_geometry` + `lib.rs`），plan 原写「AppShell 负责」——放后端更合理：几何要在窗口创建时就应用，前端那时还没跑起来。
- **新增三个方案外文件**：`core/sessions/interrupt.rs`、`utils/uiState.ts`、`utils/scrollAnchor.ts`（把职责从 `stores/*` 拆出来，正向拆分）。
- **`stores/run.ts` 仅改注释、`run.types.ts` 零改动**：计划里列了它们，实际快照逻辑落在新建的 `utils/uiState.ts`。
- **`lib.rs` 改用 `.build().run(callback)`** 取代 `.run()`：退出拦截需要 `RunEvent` 回调。

## 8. 未做（批2 范围）

分段 append-only JSONL 存储（**已完成**）、thinking 落盘（**已完成**）与 `display_only`（**未做**：概念不可考——全仓仅本节一处提及且批1 需求原文目录已不存在）、取消 8MB 硬上限（**已完成**，改为软告警 200MB / 硬熔断 1GB）与图片外置（**已完成**，[session-history-limits](./session-history-limits.md)）、压缩归档移出 `tmp/`（**未做**：`~/.codewave/tmp/compacted/` 仍无清理者）、子代理过程历史统一格式（**已完成**，与主路径共用段式模块）、按段分页（**已完成**）+ 虚拟滚动（**未做**）+ 内联折叠回看（**未做**）、后端注入队列持久化（**未做**：32 格 mpsc 纯内存，崩溃即丢）、设置页「清理旧格式历史」入口（**已完成**）。
