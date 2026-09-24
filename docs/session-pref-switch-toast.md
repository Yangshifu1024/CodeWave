# 模式/模型切换 toast 与生效时机确认

> 批次内容：切换权限模式 / 会话模型后弹出轻量 toast 确认；并核查确认后端对 session prefs 的消费时机为「下一次 LLM 请求 / 下一次工具调用即生效」。后端零改动，纯前端批次。

## 生效时机核查（后端，零改动）

session prefs（approval_mode / model_id / reasoning_effort）存于 `SessionRuntime.prefs: Mutex<SessionPrefs>`（`core/agent/runtime.rs`，in-memory、前端为 source of truth），`set_session_prefs` 整把替换，三个消费点全部**实时读取、无 run 开始快照**（代码行号易漂，定位见 [docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)）：

| 偏好 | 消费点 | 时机 |
|---|---|---|
| model_id | `core/agent/stream.rs:22-33` `build_stream_request` 每次 LLM 请求现读 `rt.prefs()` → `effective_model`（悬空 id 回落全局 active） | 每轮 turn；run 进行中切换 → 下一轮请求生效（当前正在流式的轮次不受影响，重试沿用本轮已解析模型） |
| approval_mode / fence | `tools/tool.rs:115-128` `ToolCtx::approval_mode()`/`fence_policy()` 直接 lock 现读 | 每次工具调用（batch.rs / service.rs / command 消费点同） |
| reasoning_effort | `stream.rs:36-41` 与 model 同处现读 → 三协议请求体 | 每轮 turn |
| approval_mode（子代理侧） | `tools/subagent.rs` 的 `subagent_drive_params(base, parent_prefs)`：每步从基座（`SubBase`）重建工具集与系统块 + 刷新子 rt 的 `approval_mode`（fence / 写审批门据此判定） | 每个 LLM step 边界——跟随**根会话**当前档位；进行中的工具批次不追溯（[docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)） |

Plan 档工具集排除与系统提示按步重算（main session 每步 `main_drive_params(&rt.prefs())`，ask 批准切档即下一步生效的同一机制；该「按步重算」机制自 [docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md) 起也覆盖在跑子代理）；run 开始处的 `effective_model` 仅用于日志、不影响请求。

**边界（任务运行冻结、子代理实时跟随）**：task run（`run_task_agent` 自持 runtime，不读主会话 prefs）不受中途切换影响，此为既有设计（[docs/long-file-split-and-edition-2024](./long-file-split-and-edition-2024.md) 注释）；**子代理的 spawn 快照语义已作废**——在跑子代理现在于**每个 LLM step 边界**从基座（`SubBase`）重建 `DriveParams`，工具集、系统块（`<plan-mode>` 按当前档位重拼）与子 rt 的 `approval_mode`（fence / 写审批门据此判定）三处同步跟随**根会话**档位；只有 `model_id` / `reasoning_effort` 保持 spawn 快照。同一工具批次内切换仍从下一 step/批次生效（[docs/mode-gate-and-subagent-sync](./mode-gate-and-subagent-sync.md)）。

## 前端实现

`ui/src/features/chat/Composer.tsx`：新增 `switchMode` / `switchModel` 两个 helper（updatePrefs + `message.success` toast，复用组件既有 `App.useApp()` message），收敛三个切换点——权限菜单 onClick、模型菜单 onClick（`__manage` 管理入口跳过）、**Shift+Tab 循环切模式**（[docs/composer-shift-tab-mode-cycle](./composer-shift-tab-mode-cycle.md)，键盘路径同样有确认反馈）。

- toast 文案走 i18n 新键（中英对称）：`composer.modeSwitched` =「已切换权限模式：{{label}}」/ `composer.modelSwitched` =「已切换模型：{{label}}」；label 复用菜单项既有文案（模式名 / 模型 `m.model`，id 查不到时回退 id）。
- 力度（reasoning_effort）菜单未在需求范围，未加 toast（如需对齐可后续补）。

## 验证

- `pnpm --dir ui test`：254/254 全绿；`app.smoke.test.tsx` 三个用例扩展 toast 断言（权限菜单点击、Shift+Tab 切档、模型菜单切换各断言 toast 文案出现）。
- `pnpm --dir ui build`：type check + vite build 通过。
- 手动验证清单：
  1. 会话内切权限模式（含 Shift+Tab 键盘路径）→ 顶部居中出现「已切换权限模式：<档名>」，工具条胶囊同步；
  2. 切模型 → 「已切换模型：<模型名>」；「管理供应商」入口不弹 toast；
  3. 生效时机：发送消息使 run 流式进行中，切换模型/模式 → 下一轮请求用新模型（RightBar 模型信息）、下一工具调用按新档位审批；点「管理供应商」外的其他交互不受影响。
