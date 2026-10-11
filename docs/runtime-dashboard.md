# 运行总览：会话列表与代理层级

> 2026-10-11 · 需求、实施方案与验证报告。用户已选择「会话总览为主，吸收协作树的展开方式」。
> 用户以「提交，然后实施仪表盘任务」批准实施；此前抽屉调整与方案已提交为 `3de736f`，仪表盘在 `feat/runtime-dashboard` 分支完成实现、代码审查与离线验证。实际窗口观感按下方清单由用户手动验收；随后用户以「commit push pr」授权提交、推送及创建 PR。

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

1. 左侧固定「运行总览」入口；主内容区展示总览，保留左侧导航与会话状态。标题栏显示当前页面名称，总览中隐藏后台会话的目录、分支胶囊，返回会话后恢复。
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

验证：前端全量测试 112 文件 / 1437 用例通过，`pnpm --dir ui build` 与 `pnpm --dir ui run lint` 通过；code-reviewer 七维度审查无严重或一般问题。首次测试在执行用例前遇到沙箱临时文件 ENOENT，改用仓库内 `.codewave/temps/codex-vitest` 后全量通过。界面交付手动验证：打开期间缩放窗口；内部点击不关闭；外点关闭且不透传；运行中关闭后重开仍能看到连续过程。以上结果仅覆盖该次独立抽屉调整，仪表盘验证见后续实施报告。

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

`ActivitySink` 包装既有 `EventSink`，在转发事件前更新摘要，完整委托帧与子通道绑定/解绑；即使下游没有 WebView，摘要仍生效。主 run 启动显式绑定 runtime，终态在释放运行槽位前归档；`RunCleanupGuard` 覆盖任务异常退出。runtime 通过 Weak 引用对账，不被总览额外延长生命。主会话与子代理的真实 `asks` 是待处理真值，子代理审批虽然向根会话发事件，仍验证子 runtime 中的 responder。

摘要在 run、ask/审批、工具、subagent 的实际生命周期节点更新，先更新摘要再发既有 UI 事件；失败、取消、panic 清理都收敛摘要。不得依赖 WebView 是否收到事件。活动更新使用短临界区，持锁不 await、不发事件、不读磁盘，避免与历史/索引锁形成反向锁序。

主 run 以 `(session_id, run_id)` 隔离，子代理以所属 root/run/sub 身份隔离。迟到的旧 run 更新、结束后的 sub tick 不得复活条目。读取主 run 真值复用现有 `RunSnapshot` 语义，避免拼接不同时刻的 running 与终态。

返回给总览的是一份派生快照：revision、采样时间、会话摘要、有效等待事项、子代理摘要、近期终态。前端计数从本次返回列表计算；不宣称跨所有 runtime 的线性一致读取。单个 run 的终态与活动必须自洽。

运行条目不淘汰；近期终态最多保留 50 条且仅当前进程有效，每个活跃主 run 内的已结束子代理也保留最近 50 条。任务与活动摘要最多 500 字符，报告摘录最多 2000 字符；不收集完整转录、工具参数与整套出参，不读取密钥配置；页面刷新不扫描历史。任务、活动或报告本身仍属于用户工作内容，摘要没有另做任意文本脱敏。询问完整 payload 只在等待期间保留，按需用于回跳原 AskPanel，回答/取消/run 收尾时清理。

IPC 新增 `get_activity_snapshot` 和 `get_activity_ask(session_id, run_id, ask_id)`，由 `host/commands/activity.rs` 转调 core 并验证原询问仍有 responder。`ipc/client.ts` 是唯一前端入口，DTO 集中在 `ipc/types.ts`。不新增事件键，29 键契约保持不变；子代理生命周期与询问 payload 增加 run 身份字段，用于拦截旧轮事件。子 runtime 固化根 run 身份，审批不用其自身空的 run snapshot 代替。

### 前端：独立总览 store 与页面

新增 `stores/activity.ts`，单一快照、loading/error/lastSyncedAt、展开集合、详情目标。它与 `useRun.tabs` 分工明确：总览不为全部后台会话恢复转录，也不改草稿和队列。

页面显示时每 2 秒获取摘要；焦点恢复立即刷新，页面关闭或文档隐藏时停止轮询。在途刷新单飞，请求代际/revision 拒绝过期返回。错误保留旧数据；首次加载、空列表与同步失败分别呈现。

新增 `features/dashboard/RuntimeDashboard.tsx` 与详情组件。Card、Collapse/Tree、Tag、Drawer、Empty 等由 antd 负责；字体与颜色使用现有 token。切换到总览保持 ChatMessages/Composer 挂载，避免中断流或破坏测量与草稿。

「前往处理」经 `openSessionReady()` 等待会话实际打开、历史恢复与运行对账，再以 session/run/ask 身份校验并回填原询问；`revealSession()` 只返回布尔、内部异步 fire-and-forget，不作为加载完成信号。请求期间已收到询问关闭、run 收尾或换轮信号时，拒绝迟到 payload。询问已结束则提示并刷新，不重新批准；实时事件与恢复共用 `hydrateAsk()` 映射，恢复不重复发系统通知。

总览详情明确使用目标 session/sub，不依赖 active Tab。首版抽屉的摘要与报告来自活动快照；已有完整过程入口只在可用时复用，并处理正在运行但过程历史不可恢复的情况，不能用空数组伪装完整过程。

### 文件范围

| 层 | 文件范围 |
|---|---|
| 运行摘要 | 新增 `core/activity.rs`；接入 `core/agent/runtime.rs`、run 生命周期、ask/approval、工具与 `tools/subagent.rs` 的必要生产/清理节点 |
| IPC | 新增 `host/commands/activity.rs`；注册命令；补 `ui/src/ipc/client.ts` / `types.ts` |
| 页面与状态 | 新增 `stores/activity.ts`、`features/dashboard/`；修改 `AppShell.tsx`、`TopBar.tsx`、导航入口及 `stores/ui.ts`；补中英文文案 |
| 验证与文档 | 摘要与页面行为回归测试；本主题报告、目录与必要的 AGENTS 职责说明 |

### 审查与回归修正

product-manager 完成需求分析，tester 完成静态测试审计，code-reviewer 完成七维度审查。复查后无尚待修复的严重或一般问题；审查不替代全量检查与实际界面验收。

本轮修正的关键边界：

- 总览页 Escape 与后台子代理抽屉让位，避免总览操作取消背景 run 或被自动打开的 Portal 遮挡；三个新建/历史会话入口成功后退出总览。
- 会话加载以关闭代际隔离，关闭立即移除旧载入 Promise；恢复途中关闭或同 id 重开，不接纳旧历史返回。Git 刷新核对既有桶及 run epoch，不再为迟到返回创建桶。
- 回跳 IPC 仅临时观察目标询问的关闭信号，无关询问关闭不误报目标失效；`ask:closed` 按 ask id 清理当前面板。旧 run 或已关闭目标的返回不回填。
- 等待区与推进区合并各自展开状态；详情绑定目标父会话，打开失败消费拒绝并提示。
- 已结束子代理按完成顺序保留最近 50 条；失败、取消、panic 的最终预算读真实 `step_count`，不止取最近轮询值。用户停止有明确中性状态。

### 2026-10-11 回访：标题栏没有跟随总览切换

用户截图中总览已打开，标题栏仍显示后台会话标题与目录/分支。`TopBar` 原来只判断设置页，随后直接渲染 active Tab；总览没有触发会话切换，因此标题保持旧值。

修复为按当前覆盖页显示标题（设置 → 计划任务 → 总览 → 会话/空态），页面标题统一复用既有 i18n 键。覆盖页隐藏会话胶囊，退出后恢复原标题与 Git 信息；导航宽度及会话状态沿用原逻辑。文案借用按原 i18n 契约登记到精确文件范围与理由，没有放宽守门测试。

离线 `TopBar` 回归先复现两种失败（有 Tab 时显示旧标题；无 Tab 时显示 CodeWave），修复后通过，并补总览被设置/任务页覆盖及退出恢复的两个用例。标题与 i18n 专项 2 文件 / 60 用例通过；最终全量结果见下表。

## 实施顺序与验证

- [x] P1：后端摘要与 DTO；验证关闭 Tab、重复/迟到事件、多 run 隔离、ask 失效、sub 移除后保留收尾、panic/取消收敛、容量上限。
- [x] P2：总览 store 与快照刷新；验证旧响应拒绝、失败保留数据、计数同源、页面隐藏停止轮询、无历史全量加载。
- [x] P3：页面、层级展开、详情与回跳；验证目标会话尚未打开时的异步恢复、已失效询问不提交、展开不被刷新重置、无关 Tab 草稿保留。
- [x] P4：跨层完成后调用 code-reviewer 审查，修复问题；运行 `cargo test --workspace`、`cargo fmt --check`、严格 clippy、`pnpm --dir ui test`、`build` 与 `lint`。本次未开 PR；若开 PR，再完整跑 `pnpm prepr`。
- [x] P5：更新本文件实施与测试结果，交付手动验证清单；界面不做 GUI 自动点验。

### 实跑结果

| 检查 | 结果 |
|---|---|
| `pnpm --dir ui test` | 标题栏回访修复后：115 文件 / 1462 用例通过 |
| `pnpm --dir ui build` | 通过；含 TypeScript 检查与 Vite 打包 |
| `pnpm --dir ui run lint` | 通过 |
| `cargo fmt --check` | 通过 |
| `cargo test --workspace` | 1305 通过 / 0 失败 / 3 既有 ignored；main 与 doc-tests 同时通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过，无 warning |

全量前端曾发现新页面 i18n 目录/文件范围、跨段标题和动态键缺登记，已按原契约补齐；没有放松测试正则或白名单规则。最终全量已通过。

用户授权创建 PR 后重新执行完整 `pnpm prepr`，8/8 通过，耗时 133 秒：依赖锁定、Rust fmt/clippy/test、前端 lint/test/build、脚本测试全部通过。Rust 1305 通过 / 3 既有 ignored，前端 115 文件 / 1462 通过，脚本 24 通过；clippy 无 warning。日志保存在忽略目录 `.codewave/temps/codex-vitest/dashboard-prepr.log`。

验证环境：前端 `TEMP` / `TMP` 设为仓库内 `.codewave/temps/codex-vitest`，避免沙箱临时文件 ENOENT。默认临时目录下既有 Rust atomic 文件用例报 Windows `PermissionDenied`，同一已编译用例只调整临时目录后通过；既有 HTTP 回环用例在沙箱内阻塞，允许连接自身 mock server 后通过。仓库内 Rust 夹具还会继承 `.gitignore`，影响两个既有文件枚举用例，因此最终 Rust 全量使用正常系统临时目录及必要的本机测试权限，所有非 ignored 用例通过。设置 `CARGO_INCREMENTAL=0` 避免本环境增量缓存硬链接失败导致的复制告警。前端 build 仍有既有 Tauri `window.js` 同时动态与静态导入的 Vite 提示。

### 手动界面验收

界面按项目约定不做 GUI 自动点验。离线行为测试不替代下列实际窗口验证：

1. 两个项目会话加一个临时会话并行运行；从左侧打开运行总览，确认会话、待处理和运行中子代理计数与列表一致。关闭其中一个运行 Tab 后，条目仍在。
2. 一会话等待回答，另一会话等待审批，确认它们只列在「需要你回应」。分别进入正确 AskPanel；询问已结束时提示失效并更新总览。
3. 同一会话两个子代理同时等待审批，选择其中一个；另一个取消或超时后，当前有效询问保持可处理。
4. 同时展开等待区与推进区，等待数次刷新，确认展开与焦点不被重置、会话不随活动 tick 重排。进入总览时标题栏显示「运行总览」且不显示原会话胶囊；通过左侧新建会话或打开历史会话，确认进入聊天内容并恢复相应标题。打开设置/计划任务页时标题跟随页面，退出后恢复总览。
5. 总览打开期间新派子代理，确认原过程抽屉不自动遮挡总览；按 Escape 不取消后台 run。回到原会话，消息流、草稿、滚动与过程数据保持。
6. 打开子代理详情并缩放窗口，确认宽度保持窗口的一半；内部点击不关闭，外部点击关闭且不透传。关闭后代理仍运行；从详情进入正确所属会话。
7. 检查正常报告、部分报告、预算耗尽、未汇报、主/子代理用户停止及失败的文字和颜色；步骤按实际预算消耗显示，报告明确为摘录。
8. 隐藏应用后恢复焦点，确认立即刷新；切入设置或任务页后总览暂停轮询。同步失败保留数据并显示上次同步时间。
9. 在最小窗口、暗色主题、长标题与长任务内容下，检查标题、状态与操作可访问，详情可以滚动；无活跃会话时显示空态。

用户已批准并创建、切换 `feat/runtime-dashboard`，并以「commit push pr」授权提交本轮仪表盘及标题栏修复、推送该分支、创建 PR。PR 比较目标为远端 `main`，包含尚未进入远端的基础修复 `0eb7b0c`（详见 [release-review-remediation](./release-review-remediation.md)）及抽屉调整 `3de736f`；merge 仍按后续用户指令执行。
