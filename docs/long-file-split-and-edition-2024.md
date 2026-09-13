# 超长文件拆分 + Rust edition 2024 升级批次报告

> 缘起：[docs/shell-threat-analysis-survey](./shell-threat-analysis-survey.md) 期间的全仓行数普查发现 11 个非测试源文件超过 ~600 行，结构分析确认三类问题——
> 测试与实现混居（ask 54% / fence 47% / edit 44%）、单文件多职责混杂（agent.rs 五类概念、Composer 六个关注点零抽象）、
> 单函数过长（drive_agent ~420 行 / bindGlobalHandlers ~314 行）。本批次按「纯搬移优先、行为零变化」原则分五个批次收敛，
> 并顺带完成 Rust edition 2024 升级。

## 0 · 基线与结论

- 基线：`cargo test` **393 passed / 0 failed / 3 ignored / 0 warning**；`pnpm --dir ui test` **247 passed** + build 绿
  （注：会话期间用户并行提交了 [docs/shell-threat-analysis-survey](./shell-threat-analysis-survey.md)–60，基线高于 AGENTS.md 所载 243/240）。
- 终态：`cargo test` **393 passed / 0 warning**（edition 2024，rustc 1.98.0）；前端 **254 passed**（+7 新增 runFrames 单测）+ build 绿。
- 契约红线全程未破：27 事件键名逐字不变（`events.contract.test.ts` 双向守护全绿）；lib.rs 53 条 `host::commands::xxx`
  注册路径逐字不变（仅 commands/mod.rs 增加 glob re-export）；`crate::core::agent::test_support::xxx` 路径不变（18 文件 30+ 调用点零改动）；
  core/tools 层零 `use tauri`。

## 1 · 批次 1a：Rust 测试外移（目录化，mod.rs 纯枢纽）

| 原文件 | 拆分后 | 行数变化 |
|---|---|---|
| tools/ask.rs（1089） | ask/{mod.rs 9, tool.rs 499, tests.rs 584} | 实现 1089 → 508 |
| safety/fence.rs（2024*） | fence/{mod.rs 11, check.rs 1133, tests.rs 883} | 实现 2024 → 1144 |
| tools/edit.rs（885） | edit/{mod.rs 9, tool.rs 490, tests.rs 389} | 实现 885 → 499 |
| tools/command.rs（907） | command/{mod.rs 8, tool.rs 650, tests.rs 252} | 实现 907 → 658 |

\* fence.rs 在会话期间被用户并行提交（[docs/shell-threat-analysis-survey](./shell-threat-analysis-survey.md)–60 加固）从 1797 增至 2024 行，拆分以实测边界为准；原 ps_probe/ps_probe2 探测模块已随并行提交合并消失。

- 布局：`mod.rs` 只做 `mod` 声明 + `pub use tool::*;`（glob 导出保住 `crate::tools::ask::AskTool` 等外部路径）+ `#[cfg(test)] mod tests;`。
- 测试代码零逻辑修改；仅两类机械适配：① 原先经 `use super::*` 间接可见的导入（serde_json json!/Value、Tool/ToolCtx 等）在 tests.rs 头部显式补行；
  ② 实现文件内部 `super::pathutil` 等相对路径改 `crate::tools::pathutil` 绝对路径（edit/tests.rs:149 一处 `super::pathutil` → `super::super::pathutil`）。
- 可见性提升（受众恒等原则：拆分前 private 受众 = 本模块子树，提升 pub(super) 后受众不变）：
  - ask/tool.rs：6 个计划批准门函数（arch_gate_shape / is_approve_option / approval_shape / wants_mode_switch / has_valid_answer / plan_approval_gate / approve_option_id / save_plan_file / plan_text）+ Args/Question/Option2 全部字段；
  - edit/tool.rs：min_indent / strip_indent + Change/FileEdit 字段；
  - command/tool.rs：无（测试全走 pub 面与 serde 构造）。

## 2 · 批次 1b：host/commands.rs 拆 10 域

原 commands.rs（1266 行，52 个 `#[tauri::command]`）→ `host/commands/` 目录：

| 文件 | 行数 | 内容 |
|---|---:|---|
| mod.rs | 26 | 纯枢纽：`mod` 声明 + 10 个 glob re-export |
| util.rs | 28 | `err()` / `type Core<'a>` / 跨域辅助 `open_dir_in_file_manager`（logs+system 两处调用），pub(super) |
| session.rs | 322 | 15 个会话/聊天/ask 命令 |
| workspace.rs | 318 | 7 个文件命令 + session_write_roots/session_files_payload/read_file_base64 私有辅助 + 顶部异常测试块随迁 |
| project.rs | 103 | 项目/配置 5 命令 |
| git.rs | 116 | 4 git 命令 + session_roots 辅助 |
| mcp.rs | 76 | 4 MCP 命令 + mcp_config_path |
| scheduler.rs | 71 | stop_service + 3 定时任务命令 |
| skills.rs | 42 | 2 命令 |
| stats.rs | 26 | 2 统计命令 |
| logs.rs | 42 | 4 日志命令 |
| system.rs | 150 | ping/activate_and_show 等系统命令 + validate_open_url + about_commands_tests |

- 关键机制：tauri 的 `generate_handler!` 会在**注册路径所在模块**解析 `#[tauri::command]` 宏生成的隐藏项
  （`__tauri_command_name_*`），逐名 re-export 不携带它们 → mod.rs 采用 **glob re-export**（`pub use session::*;`），
  隐藏项随 glob 自动可见，lib.rs 注册清单零改动（`cargo check` 专项验证宏解析通过）。

## 3 · 批次 2a：run.ts 拆三文件 + 批次 2b：Composer.tsx 拆四 hook

| 原文件 | 拆分后 | 行数变化 |
|---|---|---|
| stores/run.ts（1073） | run.ts 481 + run.types.ts 113 + runFrames.ts 189 + runHandlers.ts 397 | 门面 481，四层职责各归其位 |
| features/chat/Composer.tsx（864） | Composer.tsx 666 + useComposerAttachments.ts 107 + useComposerHistory.ts 59 + useComposerMentions.ts 78 + useComposerEvents.ts 38 | 组件 864 → 666 |

- run.ts 保留 store 本体 + 全部 action + `bindGlobalHandlers` 唯一注册方法（内部聚合 runHandlers.ts 六个事件族工厂：
  run 生命周期 10 键 / 压缩流 5 / tool 2 / ask 2 / sub 6 / 其他 2 = 27 键逐字不变）+ useActiveRun/useContextPct；
  类型经 `export type {...} from "./run.types"` 门面再导出，segments/ChatMessages/ToolCallCard 等消费方 import 路径零改动。
- runFrames.ts 为纯 reducer（blank/appendDelta/applyFrameToTab/applyFrameToSub/messagesToSubStream 等），
  新增独立单测 `__tests__/runFrames.test.ts`（7 用例：穿插合并、C1 幽灵帧守卫、C2 代际守卫、工具锚点、子流懒建与迟帧丢弃、
  历史重建回填）。
- runHandlers.ts 的 `systemNotify`/`markUnreadIfAway` 随事件处理整体迁入；handler 函数体逐字保留。
- Composer 四 hook 均按仓库惯例置于 `features/chat/`（先例：files/useSessionFiles.ts、shell/useTitlebar.ts）；
  attachments→history 单向依赖（recalledImages 复用校验链）；`send`/`onKeydown`/`onInputChange` 及全部 JSX 留在组件
  （横跨多关注点，强拆需大量 context 传递，收益低）。
- 顺手修复：run.ts 334 行 `}interface RunStore {` 同行格式异常（RunStore 转正为导出接口）。

## 4 · 批次 3：agent.rs 拆七文件

原 core/agent.rs（2050 行）→ `core/agent/` 目录：

| 文件 | 行数 | 内容 |
|---|---:|---|
| mod.rs | 25 | 纯枢纽 + 外部路径 re-export（AgentCore/EventSink/Frame/SessionRuntime/DriveParams/NormalizedCall/StartChatBody/drive_agent/main_drive_params/run_chat/run_task_agent/model_side_result/stream_flush_loop + pub(crate) CompactingGuard/lock_ok + #[cfg(test)] pub mod test_support） |
| runtime.rs | 389 | 4 个 pub 常量（pub(super) 化）+ Frame + EventSink + SessionRuntime/impl + AgentCore/impl |
| drive.rs | 805 | NormalizedCall + DriveParams + run_chat + drive_agent（主循环）+ run_task_agent + retry/checkpoint 辅助 + StartChatBody |
| stream.rs | 290 | 流式拼装（build_stream_request/collect_deltas/build_assistant_message/flush_segments）+ stream_flush_loop + 2 个日志裁剪常量 |
| guards.rs | 45 | DriveUnwindGuard（字段 pub(super)，drive.rs 结构体字面量构造）+ CompactingGuard + lock_ok |
| test_support.rs | 43 | pub 测试基建（路径不变） |
| tests.rs | 500 | 测试本体 |

- 执行要点：跨文件访问统一 `pub(super)`（受众 = agent 子树 = 拆分前 inline 可见范围，crate 公开面零扩大）；
  SessionRuntime::new 由 private 提升pub(super)（test_support 触达）。
- 提取事故与恢复：首轮 sed 区间笔误漏掉 AgentCore 块（原 272–411 行），编译即时暴露，经 `git show HEAD:` 完整恢复，无内容损失。
- **未实施项（后续建议）**：drive_agent（~420 行）内部按语义段（重试/backoff / 工具批次 / checkpoint 节奏）提函数——
  行为敏感的逻辑重构需精读主循环逐段验证，不宜与搬移混批；主循环现已隔离于 drive.rs，具备独立执行条件。

## 5 · 批次 4：Rust edition 2024 升级

- **`gen` 改名 → `generation`**（edition 2024 保留字）：util/throttle.rs（字段/方法/测试 15 处，纯内部）、
  core/agent（Frame 字段 2 处 + 构造/模式匹配/测试）。
- **wire 契约保持**：Frame 枚举字段改名后加 `#[serde(rename = "gen")]`，序列化键仍是 `"gen"`——前端 `frame.gen`（runFrames）、
  `p.gen`（run:retry payload）、测试断言 `v["frame"]["gen"]` 全部零改动；emit_retry 的 json! 字面量键 `"gen"` 为字符串不受影响。
- Cargo.toml：`edition = "2024"` + 补 `rust-version = "1.85"`（2024 默认启用 MSRV-aware resolver；项目此前从未声明 MSRV）。
- `cargo fix --edition` 预迁移后剩余 13 处 `tail_expr_drop_order` 复核警告（MutexGuard/JoinSet 等临时值 drop 顺序），
  属「提示人工复核」性质、非可自动修复项；切 2024 后 rustc 0 error / 0 warning，行为由 393 测试全绿兜底。
- 风险排除记录（评估结论归档）：desktop-only，lib.rs `mobile_entry_point` 被 cfg_attr(mobile) 门控从不展开（宏产物按调用方
  edition 编译、可能发 `#[no_mangle]` 的唯一链路已排除）；@tauri-apps/cli ^2.11.0 / tauri-build 2.6.3 的 cargo_toml 已支持
  解析 edition 2024（tauri#11829 为 2024-11 tauri 2.1.1 时代问题）；`static mut`/`unsafe fn`/`extern`/`no_mangle`/`-> impl`
  全仓 0 处，RPIT 捕获与 unsafe 语义变化免疫。

## 6 · 验证记录

| 时点 | cargo test | pnpm test | build |
|---|---|---|---|
| 批次 0 基线 | 393 passed / 0 warning | 247 passed | 绿 |
| 批次 1a 后 | 393 passed / 0 warning | — | — |
| 批次 1b 后 | 393 passed / 0 warning（cargo check 宏解析专项 ✓） | — | — |
| 批次 2a 后 | — | 254 passed（+7 runFrames 单测） | 绿 |
| 批次 2b 后 | — | 254 passed | 绿 |
| 批次 3 后 | 393 passed / 0 warning | — | — |
| 批次 4 后 | 393 passed / 0 warning（edition 2024） | （前端零改动） | — |

## 7 · 备案与后续建议

1. **clippy 存量**：`cargo clippy --all-targets` 有 56 warning + 1 个去重后 error（`never_loop`，位于本次**完全未触碰**的
   core/sessions/repair.rs:206），系存量问题（CI lint.yml 对 clippy 为 continue-on-error 非阻断）；其余告警均落在原样搬移的代码上，
   无新引入逻辑。建议后续独立批次做 clippy 清零。
2. **drive_agent 提函数**（见 §4）。
3. **存量 mod.rs 带逻辑**：`tools/mod.rs`（ToolKind/ToolOutcome/trait Tool + 内联测试 194–271 行）、`core/sessions/mod.rs`
   （声明与实现混合）不符合「mod.rs 只做导出/测试声明」原则，本次未动，可追加收敛批次。
4. **防回潮**：可考虑 CI 增加非测试源文件行数上限检查（如 >800 行告警）。
5. 前端 `applyFrameToTab` 的 3 处测试导入改指 runFrames（app.smoke / run.subagent / run.interleave），属既定方案。

## 8 · 提交信息草案（git 由用户执行）

```
refactor(tools,safety): extract unit tests to sibling modules (ask/fence/edit/command)
refactor(host): split commands.rs into 10 domain submodules, mod.rs as pure hub
refactor(ui): extract run store types/reducers/event-handler factories (run.ts → 4 files)
refactor(ui): split Composer into focused hooks (mentions/history/attachments/events)
refactor(agent): split agent.rs into runtime/drive/stream/guards + test modules, mod.rs as pure hub
chore: migrate to Rust edition 2024 and declare MSRV 1.85
docs: add [docs/long-file-split-and-edition-2024](./long-file-split-and-edition-2024.md) split-and-edition batch report
```
