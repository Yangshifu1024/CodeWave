# 运行总览：会话列表与代理层级

> 2026-10-11 · 需求与实施方案草案。用户已选择「会话总览为主，吸收协作树的展开方式」。
> 布局方向已确认；实施方案待批准。拟定分支：`feat/runtime-dashboard`。本文件不代表功能已实施。

## 需求与首版范围

用户希望跨会话了解谁在运行、在做什么、谁需要自己处理。页面名称为「运行总览」，范围是当前应用进程中的项目会话和临时会话，不受 Tab 是否打开影响。

用户故事与验收条件：

| 用户故事 | 验收条件 |
|---|---|
| 同时运行多个会话时一次巡视全部工作 | 关闭仍在运行的 Tab 后，会话和子代理仍出现在总览；统计与列表来自同一份快照 |
| 及时发现等待自己的事项 | 审批与回答分开标识；点击定位正确会话和仍有效的询问；等待工具或子代理不算待用户处理 |
| 看清子代理的归属和当前活动 | 会话展开显示主代理与其子代理；角色、任务摘要、最近活动、预算都有明确来源 |
| 查看收尾结果并返回现场 | 主 run 的正常结束、失败、用户停止分开；子代理部分报告、预算耗尽、未汇报分别显示 |

首版包含：入口、摘要计数、待处理、会话与代理层级、详情抽屉、最近结束、快照刷新与错误状态。

首版聚焦会话与子代理。后台服务、计划任务放到后续扩展；运行会话列表本来不包含计划任务 runtime，不能在首版沿用其计数冒充全应用任务数。费用、ETA、完成百分比、批量停止不列入本轮。

## 页面与交互

1. 左侧固定「运行总览」入口；主内容区展示总览，保留左侧导航与顶部会话 Tab。
2. 顶部三个摘要：活跃会话、需要你回应、运行中子代理。活跃会话包括等待用户的 run；计数不相加。
3. 「需要你回应」置顶，展示所属会话、项目、等待原因与「前往审批 / 前往回答」。正文「正在推进」排除已列出的等待会话，避免重复。
4. 会话行显示标题、项目/临时会话、活动摘要、本轮耗时和子代理数量。默认折叠；用户主动展开后，显示主代理节点和直属子代理行。演示预览展开首条仅为了展示结构。
5. 子代理行显示规范角色名、任务摘要、运行/结束状态、已用步骤预算、最近活动；点击「查看过程」打开右侧详情。
6. 详情抽屉首版显示任务、预算、最近活动及可用的最终报告，并提供「打开所属会话」。完整逐帧转录继续由会话内已有过程抽屉承担；不能把一个活动摘录称为完整过程。
7. 「前往审批 / 回答」进入原会话 AskPanel；总览没有第二套答题或批准逻辑。
8. 最近结束按 run/sub 身份保留本进程近期结果，默认折叠，展开回看；列表更新不抢焦点、不自动展开，也不按每次活动 tick 重排会话。
9. 宽屏显示完整元信息；窄屏优先保留标题、状态、任务与动作，次要字段换行。长标题省略时提供完整文本访问方式。

### 2026-10-11 追加：现有子代理抽屉的小范围调整

用户追加要求：宽度为应用窗口宽度的 50%，点击抽屉外部关闭。这两项独立调整现有 `SubagentDrawer`，不等待仪表盘实施：`size="50vw"` 取消原 360–560px 限制并随窗口缩放；`mask={{ closable: true }}` 通过 antd 遮罩关闭，外部点击仅用于关闭、不透传给下层按钮。关闭只影响可见性，子代理继续运行、过程数据保留。未来总览详情抽屉采用同一宽度与外点关闭约定。

验证：前端全量测试 112 文件 / 1437 用例通过，`pnpm --dir ui build` 与 `pnpm --dir ui run lint` 通过；code-reviewer 七维度审查无严重或一般问题。首次测试在执行用例前遇到沙箱临时文件 ENOENT，改用仓库内 `.codewave/temps/codex-vitest` 后全量通过。界面交付手动验证：打开期间缩放窗口；内部点击不关闭；外点关闭且不透传；运行中关闭后重开仍能看到连续过程。仪表盘主体仍为待批准方案。

状态与视觉口径：

| 状态 | 含义 | 视觉 |
|---|---|---|
| 运行中 | 本轮未收尾，正在生成、执行工具、等待子代理或压缩 | 中性 |
| 待审批 / 待回答 | 存在仍有效、需要用户回应的询问 | 橙色 |
| 正常结束 | 当前 run 已结束；不承诺用户需求全部完成 | 中性 |
| 用户停止 | 主 run 被取消 | 中性，明确文字 |
| 失败 | 主 run / 子代理返回错误 | 红色 |
| 部分报告 / 预算耗尽 / 未汇报 | 按子代理 `ended` 区分；不覆盖其已有成果 | 橙色 |
| 同步失败 | 无法确认最新状态 | 保留旧数据，显示上次同步时间 |

步骤 `18 / 90` 是预算消耗；耗时从 run 实际启动时记录，不能取会话创建时间；主代理与子代理之间仅显示实际归属，不推断依赖图。当前子代理工具禁止派生子代理，首版无需多层依赖图。

## 现有实现依据

- [ui/src/ipc/client.ts](../ui/src/ipc/client.ts) 与 [host/commands/ui_state.rs](../src-tauri/src/host/commands/ui_state.rs)：`list_running_sessions` 已列全应用运行主会话 id，明确不含独立子代理与计划任务。
- [stores/run.ts](../ui/src/stores/run.ts)、[stores/runHandlers.ts](../ui/src/stores/runHandlers.ts)、[stores/sessions.ts](../ui/src/stores/sessions.ts)：详细状态按 Tab 分桶；关闭 Tab 会 dispose，缺桶事件不建立完整后台状态。
- [core/agent/runtime.rs](../src-tauri/src/core/agent/runtime.rs)：已有会话/子代理表、主 run 原子快照、真实步数、项目快照；`asks` 只保存 responder，不能重建询问正文。当前无完整全局活动摘要。
- [tools/subagent.rs](../src-tauri/src/tools/subagent.rs)：spawn 有角色、任务、预算，step 有活动摘录；结束后子代理从 `core.subs` 移除，近期收尾必须另留有界摘要。
- [SubagentDrawer.tsx](../ui/src/features/subagent/SubagentDrawer.tsx)：当前读取活跃 Tab 的子代理和流；不能从总览传一个 sub id 就保证命中其父会话。
- [host/commands/session.rs](../src-tauri/src/host/commands/session.rs)：`load_subagent_history` 当前读磁盘；对运行中、关闭过 Tab 的子代理，不保证存在完整实时过程。
- [AppShell.tsx](../ui/src/features/shell/AppShell.tsx)：设置/任务页已有覆盖显示且保持工作区挂载的范式；总览保留导航，覆盖范围需单独适配。

改前契约：[technical-design](./technical-design.md)、[run-terminal-event-fallback](./run-terminal-event-fallback.md)、[subagent-terminal-event-loss](./subagent-terminal-event-loss.md)、[subagent-budget-and-ended](./subagent-budget-and-ended.md)、[subagent-interaction-drawer](./subagent-interaction-drawer.md)、[session-restore-fidelity](./session-restore-fidelity.md)。

## 技术方案与所有权

### 后端：独立、有界的活动摘要

新增纯 Rust `core/activity.rs`，由 `AgentCore` 持有。职责只限运行观察，不作为审批、取消、编排或计费的事实源。

摘要在 run、ask/审批、工具、subagent 的实际生命周期节点更新，先更新摘要再发既有 UI 事件；失败、取消、panic 清理都收敛摘要。不得依赖 WebView 是否收到事件。活动更新使用短临界区，持锁不 await、不发事件、不读磁盘，避免与历史/索引锁形成反向锁序。

主 run 以 `(session_id, run_id)` 隔离，子代理以所属 root/run/sub 身份隔离。迟到的旧 run 更新、结束后的 sub tick 不得复活条目。读取主 run 真值复用现有 `RunSnapshot` 语义，避免拼接不同时刻的 running 与终态。

返回给总览的是一份派生快照：revision、采样时间、会话摘要、有效等待事项、子代理摘要、近期终态。前端计数从本次返回列表计算；不宣称跨所有 runtime 的线性一致读取。单个 run 的终态与活动必须自洽。

运行条目不淘汰；近期终态拟最多保留 50 条且仅当前进程有效。摘要文本限长、不保存 base64、完整转录或密钥；页面刷新不扫描全部历史。询问完整 payload 只在等待期间保留，按需用于回跳原 AskPanel，回答/取消/run 收尾时清理。

IPC 拟新增 `get_activity_snapshot`，由 `host/commands/activity.rs` 校验并转调 core；必要时新增按 session/run 读取待应答原 payload 的命令。`ipc/client.ts` 是唯一前端入口，DTO 集中在 `ipc/types.ts`。不新增事件键，29 键契约保持不变；现有运行/询问事件可触发节流刷新。

### 前端：独立总览 store 与页面

新增 `stores/activity.ts`，单一快照、loading/error/lastSyncedAt、展开集合、详情目标。它与 `useRun.tabs` 分工明确：总览不为全部后台会话恢复转录，也不改草稿和队列。

页面显示时建议每 2 秒获取摘要，事件触发的刷新合并；焦点恢复立即刷新，隐藏时停止页面轮询。在途响应按请求代际/revision 拒绝过期返回。错误保留旧数据；首次加载、空列表与同步失败分别呈现。

新增 `features/dashboard/RuntimeDashboard.tsx` 与详情组件。Card、Collapse/Tree、Tag、Drawer、Empty 等由 antd 负责；字体与颜色使用现有 token。切换到总览保持 ChatMessages/Composer 挂载，避免中断流或破坏测量与草稿。

「前往处理」必须等待会话实际打开/恢复完成，再以 session/run/ask 身份校验并回填原询问；`revealSession()` 当前只返回布尔、内部异步 fire-and-forget，不能据此认定恢复完成。询问已结束则提示并刷新，不重新批准。

总览详情明确使用目标 session/sub，不依赖 active Tab。首版抽屉的摘要与报告来自活动快照；已有完整过程入口只在可用时复用，并处理正在运行但过程历史不可恢复的情况，不能用空数组伪装完整过程。

### 预计文件范围

| 层 | 文件范围 |
|---|---|
| 运行摘要 | 新增 `core/activity.rs`；接入 `core/agent/runtime.rs`、run 生命周期、ask/approval、工具与 `tools/subagent.rs` 的必要生产/清理节点 |
| IPC | 新增 `host/commands/activity.rs`；注册命令；补 `ui/src/ipc/client.ts` / `types.ts` |
| 页面与状态 | 新增 `stores/activity.ts`、`features/dashboard/`；修改 `AppShell.tsx`、导航入口及 `stores/ui.ts`；补中英文文案 |
| 验证与文档 | 摘要与页面行为回归测试；本主题报告、目录与必要的 AGENTS 职责说明 |

## 实施顺序与验证

- [ ] P1：后端摘要与 DTO；验证关闭 Tab、重复/迟到事件、多 run 隔离、ask 失效、sub 移除后保留收尾、panic/取消收敛、容量上限。
- [ ] P2：总览 store 与快照刷新；验证旧响应拒绝、失败保留数据、计数同源、页面隐藏停止轮询、无历史全量加载。
- [ ] P3：页面、层级展开、详情与回跳；验证目标会话尚未打开时的异步恢复、已失效询问不提交、展开不被刷新重置、无关 Tab 草稿保留。
- [ ] P4：跨层完成后调用 code-reviewer 审查，修复严重问题；运行 `cargo test`、`cargo fmt --check`、`pnpm --dir ui test`、`build` 与 `lint`。若开 PR，再完整跑 `pnpm prepr`。
- [ ] P5：更新本文件实施与测试结果，交付手动验证清单；界面不做 GUI 自动点验。

建议手动验证：两个项目加临时会话并行；主会话启动多个子代理；分别触发审批与回答；关闭其中运行 Tab；从总览进入处理；模拟子代理预算用尽与用户取消；确认零运行空态、暗色、窄窗、长标题、展开与抽屉返回。

方案批准后创建并切换 `feat/runtime-dashboard`；commit / push / merge 仍由用户执行。设计方向选择不视为实施方案或 Git 写操作授权。
