# 子代理

子代理是主会话工具批次内的独立 runtime，负责限定任务并把报告返回主代理。它不是新的用户会话，也不从父批次脱离。本文定义选角、预算、权限继承、文件隔离、终态与过程恢复；主 run/steer/监督见[运行与调度](./runtime-and-scheduling.md)，分析批准门见[计划与批准](./plan-and-approval.md)。

## 1. 角色、任务包与模型

运行时角色的唯一事实源是 [agents/mod.rs](../src-tauri/src/agents/mod.rs)。已注册可委派角色：

| 角色 | 适用任务 | 角色 readonly 标记 |
|---|---|---|
| explore | 多文件只读调研、定位、结构与实现事实 | 是 |
| backend-dev | API、数据模型、事务、并发等后端实现 | 否 |
| frontend-dev | Web 框架、组件、类型与状态管理 | 否 |
| app-dev | 移动端与桌面端、平台能力和桥接契约 | 否 |
| reviewer / code-reviewer | 限定范围的代码审查 | 是 |
| product-manager | 需求分析、用户故事、验收与开放问题 | 否 |
| tester | 复现、根因、测试设计与验证 | 否 |

`title` 是内部自动命名角色，工具入口硬拒绝经 subagent 委派；它由 title 模块单次零工具调用使用。旧 `dev` 不保留别名，未知角色按通用子代理运行，不能把未知字符串说成已注册专家。规范化支持大小写、空白、下划线；`pm/test` 别名通过注册表同源解析，不另外维护角色判定表。

工具入参为自包含 `task`、`role`、`maxSteps`，另有短标签 description 与 cleanContext。schema 要求预算，执行层对缺失仍有默认 25 步并夹取 1–1000；兼容 `max_steps/clean_context` 键名。任务/角色为空拒绝，不悄悄执行空任务。

- cleanContext 默认 true；false 仅带父历史最后 6 条作背景，不等于复制全部会话。
- 开发任务按域/层拆包，混合栈可同批混派；文件范围互斥，共用配置/锁文件/公共类型由主代理处理。
- 通用工程纪律包括先读后改、沿用仓库分层、小步实现、自测与明确汇报未验证平台，不自动执行 git 写操作。
- 标准工作流的建议预算是 explore 40、product-manager 30、开发角色 90、reviewer 45、tester 60；这些是派发建议，不取代工具硬上限。改工作流文字时同时守 `WORKFLOW_SECTION` 字符预算，不为新增说明删除机制锚点。
- `effective_subagent_model` 先读有效的角色级模型映射，再回落父会话有效模型（会话 override → 全局 active）。模型与思考力度在 spawn 固化；运行中根会话换模型不会中途替换子代理模型。

主会话单次 read 累计请求窗口超过 2000 行会返回 `E_READ_TOO_BROAD`，应派 explore 只回传结论；为编辑读取小窗口仍能取得版本令牌。子代理/任务不受主会话这个机械门槛限制，不能用它推导所有读工具都无限制。

## 2. 权限继承与边界

[tools/subagent.rs](../src-tauri/src/tools/subagent.rs) 的基座排除集固定包含 `ask/subagent/plan/skill/scheduled_task/suggest/wait`。子代理不能直接向用户提问、再派生代理或新增定时任务；需要决策时报告给主代理。根身份支持传播，但当前工具限制不开放嵌套派发。

`SubBase` 保存角色、预算、根会话、spawn 模式、基础工具排除、纪律块和 idle 策略。每个 LLM step 边界通过 `subagent_drive_params` 从基座重新构建：

1. 基础排除集。
2. 根会话当前权限档派生的工具/MCP 排除与 `<plan-mode>` 提示。
3. 角色派生限制和提示，最后去重。
4. 同时更新子 runtime 的 `prefs.approval_mode`，使 fence 和文件写审批与工具可见集一致。

**必须重建，不能增量追加。** Plan → AutoEdit 后旧 `<plan-mode>` 或写工具排除若残留，会形成“工具能写、提示说不能写”的矛盾。spawn 和下一步共用装配函数；模式未变化不注入新历史，变化才记 `[system]` 与日志。根会话已删除/查不到时保持原档，不 panic。只同步权限模式，model/effort 不变；已进行的工具调用或已打开审批不追溯撤销。

当前 `WRITE_TOOLS` 是 `edit/create/delete/write_document/edit_document` 五项，不能照早期文档漏掉文档写工具：

- Plan 档额外排除上述写工具、service、scheduled_task 和 MCP，command 仍经 plan 只读 fence 限制。
- explore/reviewer/code-reviewer 在 ConfirmEach、AutoEdit、Plan 下额外排除 WRITE_TOOLS。
- **FullAccess 下只读角色解除这些写工具排除**，纪律块及角色正文附加明确覆盖声明。角色名不等于不可写的安全边界；空转策略仍保持 NudgeOnly。

只读角色并非密闭沙箱：非 Plan 下 command、service、网络工具仍可能可用并产生外部副作用，实际权限还受 fence 与审批控制。权限工具集过滤必须同时发生在模型暴露前和执行时硬拒，不能只依赖角色提示词。更多安全范围见[工具与安全](./tools-and-safety.md)。

## 3. 并发、写认领与取消

全局并发上限 4，`acquire_slot` 用 CAS 检查并占位，满员时最多等待 30 秒、每 500ms 检查且可取消；不能用 load/check/fetch_add 造成瞬时超限。`GuardGuard` 在所有退出路径释放计数。`E_ARGS` 修正参数后重发，`E_SUBAGENT_BUSY` 可稍后重试；运行失败不机械重跑整个任务，避免重复写入，先读已有结果。

### 文件认领

[claims.rs](../src-tauri/src/tools/claims.rs) 锁定整个子任务周期的文件范围，与单次写锁分工不同：

- key 通过 canonical_best_effort 与写锁同源归一。
- 同路径或互为祖先的路径视为冲突，覆盖“删目录 vs 写其内部文件”。一次多路径 claim 在单锁内先全查后全插，自身幂等，不能部分认领。
- 非主 runtime 的受管文件写在写锁前检查认领；冲突为 `E_FILE_CLAIMED`，不得重试等待，应从任务包剔除并在未完成清单列明。
- 多文件 edit 任一冲突整体拒绝；主会话豁免，待所有子任务返回后处理共享文件。
- `ReleaseGuard` 正常结束或 panic Drop 释放认领，顺序派发可复用同一文件。

这是工具内文件写认领，不是操作系统隔离。shell、MCP 等不能静态归因的写入不受同一表完整覆盖，文档写工具当前 claims 分支使用 `root_session_id.is_none()`，与普通写工具的非主 runtime 判定不一致，不能保证子代理文档写入同样受认领隔离；本轮仅静态核实，未运行并发写复现。不能仅凭 role/任务描述声称没有交叉写风险。

### 取消

子 run 令牌由 `DriveParams.parent_cancel.child_token()` 派生，主 run 停止会中断子 LLM 流、审批等待和工具批次；用户单独停子代理则不停止父 run。

- 单独停止：工具返回 `E_SUBAGENT_STOPPED`，明确要求主代理先询问是否重新派发，不自动重启。
- 随父取消：报告“随主会话停止而取消”，不诱导继续询问或重派。
- 关闭过程抽屉、关闭 Tab、steer 均不是停止指令；各自保持已有取消语义。

当前 stop_subagent IPC 验证主会话存在后按全局 sub_id 查询，并未额外核实该 sub 的父会话匹配；不能将 UI 提供正确 session_id 写成完整服务端属主校验保证。

## 4. 步数与汇报终态

步数是 `step_count` 的“已启动 LLM 步数”，在每步开头写 `step+1`；不能拿 `history.len()` 当步数，一步可产生多条消息。800ms 的 sub:step 是活动抽样，后端最终 done/error 都携带真实计数；done 卡片回填已有实现，总览也读真实计数，但聊天 error handler 尚未同步 steps_used，失败卡可能保留最后采样。

子代理使用 `finish_on_text=false`：过程旁白不构成汇报，最终报告须以 `<report>…</report>` 包裹。连续无有效工具回合最多续跑三轮，第四轮显式失败；这与空转看门狗是两道独立限制。被拒调用优先反馈修正，不能因正文非空而静默成功。低预算约剩余 20% 提醒，最后一步强制要求汇报。

`split_report` 剥离首个开始标记与对应正文；未闭合时取后续全部文本，闭合后的尾文不进入报告。当前收尾判据是 contains，模型复述标记也可能误触，这是边界而非严格格式验证。

### ended 四象限

`ended_of(steps_used,max_steps,tagged)` 是唯一判据；预算触顶用 `>=`。

| 触顶 | 有报告标记 | ended | 等待目标状态 | 可置 analysis_done |
|---|---|---|---|---|
| 否 | 是 | report | Succeeded | 仅分析角色可 |
| 是 | 是 | partial | Succeeded | 否 |
| 是 | 否 | budget | Failed | 否 |
| 否 | 否 | no_report | Failed | 否 |

`partial` 承认已有成果，但不保证完整；也可能恰好最后一步做完，主代理须读报告缺口再决定补派，不能把整个任务从头重跑。完整分析批准门使用 `is_delivered()`，只接受 report；pm/tester 仅工具返回 Ok 不足以解锁分析门。条件等待的成功则允许 partial，两种“成功”判据刻意分开。

ended 同时写日志、`sub:done` 和 `ToolOutcome.data`。改值名要同步 `ipc/types.ts`、`run.types.ts`、恢复校验与中英文文案；事件键测试不会自动发现载荷枚举漂移。卡片除 report/旧数据缺省外均显示警示，避免新增值静默变成干净完成；后端以 error 的 cancelled 字段区分用户停止，总览呈现独立停止状态；现有聊天子卡的 error handler 仍将两者写为 error，不能承诺所有展示入口都已区分。两者都不应与 no_report 混同。

## 5. 生命周期与终态兜底

spawn 固化所属根 run 身份，创建独立 runtime 并注册 core.subs、活动摘要和 wait 目标。父工具批次等待子代理返回，必须得到完整工具结果，不能遗留悬空 tool_use。

正常返回先尽力保存过程历史，再发布 report → usage → done；失败/取消发布 error（取消载荷有 cancelled）。`SubCleanupGuard` 覆盖 ok/error/cancel/panic：

1. 停止进度轮询。
2. 在 subs.remove **之前**把 wait 目标置终态，避免轮询看到“从不存在”。
3. 从运行表移除并解绑子通道，不能保留旧版“绑定永久常驻”的描述。
4. panic 时补发 error 收口观察摘要，并尽力保存已有过程历史。

失败工具返回目前未携带 sub_id，因此前端不能假定所有 tool:result 都能关联子代理。收敛使用多道防线：

- `sub:done/sub:error`：正常终态，保留真实报告、ended、最终预算。
- 成功 `tool:result(subagent)` 且带 sub_id：若卡片还在 running，就地补 step/report/ended 并收口该子流工具，不误伤主流；已终态不覆盖。
- 主 run 结束：残留 running 子卡收敛为 done + `no_report`，保守表示未收到完整终态，不能伪装完整报告。
- 主 run 终态事件本身丢失：通过 RunSnapshot 对账触发同一收口，详见[运行与调度](./runtime-and-scheduling.md#1-运行事实与界面事件)。

子代理 step/frame 在终态后必须忽略，不能用迟到抽样覆盖最终步数。当前 sub:spawn 仍无条件 push，而读侧多用 find；重复投递可能使收敛与计数落在不同条目，upsert 去重仍是待补边界。

实时 tool 结果中的 ended 接受后端有效字符串形状以保留新枚举；恢复历史则对白名单 `SUB_ENDED_VALUES` 校验，未知历史不能 `as any` 强行放行。这两条处理的数据可信度不同，不应强制合并为同一宽松路径。

## 6. 实时过程、抽屉与恢复

子代理逐字帧经 `Frame::Sub{sub_id,frame}` 借父 Channel 传递。子帧与 sub:spawn 属两条通路，没有到达顺序保证，前端需惰性建子流。子工具结果的 session 为 sub_id，不在顶层 tabs，必须从 subStreams 找归属；文档/文件写成功同时刷新根会话文件视图。

- 聊天保留每轮子代理卡，下一次 send 不清空过去 subs；卡片点击可回看。
- Composer 运行指示器：0 隐藏、1 直达、多个弹菜单；运行数 0→1 可自动打开，但总览/覆盖页期间须让位。
- [SubagentDrawer](../ui/src/features/subagent/SubagentDrawer.tsx) 从右侧打开，宽 `50vw`、随窗口缩放；外部遮罩点击只关闭不透传，内部点击不关闭，`destroyOnHidden=false` 保留内容。关闭不销毁、不停止，可再次打开。
- drawer 状态 per-Tab，使用与主聊天共用的 timeline/ThinkingBlock/工具卡渲染。自动贴底仅在用户本来位于底部时生效，上翻暂停；过程流与简短活动摘录不可混称。
- 子历史只在独立过程路径保存，不写主索引；当前为 `histories/subs/<parent>/<sub>/` 的 append-only 段，旧 gzip 仅回读兼容。
- 恢复主历史识别 subagent 工具调用与工具结果，回填 ended/steps_used/tokens/report；无真实 sub_id 暂用 `restored:<callKey>`。大结果通过根会话 `.toolres` 回填后改名，再能加载真实过程。
- 归档首次打开抽屉才按需 `load_subagent_history`；缺文件/失败显示任务与最终报告并明确缺过程，不能以空数组声称完整恢复。运行中通常使用当前内存流，关闭过 Tab 后不保证所有在途帧可恢复。
- 过程历史图片 owner 必须 `<parent>__<sub>`；工具结果 owner 是根会话。二者身份不同，不要复用同一目录公式。详细存储和 GC 见[会话与项目](./sessions-and-projects.md#4-图片与完整工具结果)。

## 7. 故障排查与测试入口

先区分后端真挂起和前端没有收口：比较父会话 spawn/返回日志、子日志、真实运行表、主 run 快照与活动摘要。卡片 token 有更新只证明 usage 到达，不能证明 done 已注册或投递。临时诊断探针可分别记录后端 emit 结果、前端 handler enter、命中桶、提交完成，从发射/监听/状态/渲染四层定位；不要把静态推断写成已复现的唯一根因。

既有现场的复现候选是 **Plan 档、同一个 step 并发三个 explore**；应与单发 explore 对照，区分并发投递和通用终态处理问题。该形状来自历史现场，当前仍需真机重验；不据此宣称唯一根因或本轮已经复现。

| 问题 | 优先核查 |
|---|---|
| 部分成果却触发重新整包派发 | ended/报告缺口是否保留，partial 是否被误改 no_report/Failed |
| 主会话放宽后子代理仍被锁 | SubBase 重建、旧 plan 块、子 prefs 同步是否三处一致 |
| Plan 下子代理越权 | 模型工具过滤、批次硬拒、MCP 排除及 fence 读取同一档位 |
| 只读调研被当空转 | batch_digest 真实 files[] 参数、角色 readonly、NudgeOnly 仅限空转层 |
| 文件互相覆盖 | canonical 路径/祖先冲突、claims RAII、是否走 shell/MCP 未覆盖路径 |
| 子卡转圈但日志已返回 | sub终态 → tool结果 → run收尾 → 快照对账各防线、run身份与迟到帧 |
| 抽屉只有报告无过程 | 真实 sub_id、父目录、sidecar回填改名、过程文件是否实际存在 |

后端重点：`tools::subagent`（ended 四象限、is_delivered/wait_state、角色策略/基座、并发槽位）、`tools::claims`、`core::agent::tests`（脚本化 SSE、被拒/旁白/预算/档位）、`core::agent::supervise`、`core::sessions` 子历史/GC。前端重点：`run.subagent`、`subagent.card`、`subagent.drawer`（含滚动）、`run.restore-toolresults`、`run.terminal-fallback`、`activity.navigation`、`activity.dashboard`。命令见[测试指南](./testing.md)，修改共享 DriveParams 后还需跑主会话与任务对照组。

手动回归应包括：单/多代理真实长任务、全权限切换方向、单独停止与父取消、含审批的并发、终态后迟到 tick、关抽屉重开、关 Tab 回跳、跨重启归档过程、大报告回填、窄窗/暗色/缩放与上翻滚动。运行总览 GUI 检查尚未确认，不能以子抽屉自动测试代替。

## 8. 仍开放的边界

- 终态投递丢失的实机根因未最终定位。当前 `bindEvents` 串行订阅，单键 reject 会影响后续键；兜底改善可恢复性，不等于根因已消失。
- 失败/用户停止的 ToolOutcome 没有 sub_id，第二道工具结果兜底只覆盖成功路径；失败主要依靠 sub:error 和主 run 收尾/对账。
- `<report>` 包含匹配会把复述误当汇报；混合批次无对应 tool_use 的被拒结果可能被 repair 丢弃，模型看不到那部分反馈。
- 无工具执行期统一墙钟看门狗；当前 web_fetch 仍存在无界等待风险。LLM 流停滞检测不覆盖整个工具批次。
- NudgeOnly 不豁免失败重复、MAX_TEXT_TURNS、步数预算；“只读永不终止”是错误承诺。grep 命中新文件尚不算独立进展，调用名驱动的摘要也不能可靠证明实际副作用。
- 没有运行中动态扩预算、extend 续跑或从暂停点恢复子 run 的协议。失败、预算不足和 partial 应按已完成/未完成范围决定下一步。
- 中断或监督失败时没有统一生成部分报告的额外收尾轮，已有过程历史不等于已返回结构化成果。子过程通常在结束保存，不能保证每个在途帧耐久保存。
