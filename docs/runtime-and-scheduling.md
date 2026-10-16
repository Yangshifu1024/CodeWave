# 运行与调度

本文覆盖主 run 终态、steer、运行总览、后台服务、计划任务与条件等待。持久化见[会话与项目](./sessions-and-projects.md)，派发和子代理终态见[子代理](./subagents.md)，批准门见[计划与批准](./plan-and-approval.md)。

## 1. 运行事实与界面事件

主会话 runtime 是运行真值，UI 事件是通知通道，两者不能互相替代。[runtime.rs](../src-tauri/src/core/agent/runtime.rs) 的 `RunSnapshot` 同时包含 `running/run_id/terminal`；终态包括 `done/error/cancelled` 及完整收尾载荷。发布终态和释放运行槽位必须收敛为一致快照，不能分别读取多个原子值拼出“空闲但无终态”的偶然组合。

主 run 的终态事件与快照可携带 `task_run`，作为[整轮过程折叠](./chat-and-interface.md#44-任务完成后的整轮过程折叠)的同一份真实计时与展示边界。`run:start` 提供起始用户消息时间，成功、错误和取消保留各自状态；旧 run 的迟到回执不能覆写新一轮，重复终态不能重复插入任务入口。总耗时使用单调时钟计量，包含等待与重试；它不是 `usage.duration_ms` 的生成耗时总和。持久化与缺失降级规则见[会话与项目](./sessions-and-projects.md#任务轮次展示元数据)。

主会话 Channel 的 `step_start` 帧标记真实 LLM 步骤开始，供界面暂存当前步骤的候选正文并在正常收尾时先折叠、后显示总结。上一 step 的残余片段与新边界串行发送；重试沿用原有代次与 `run:retry`，不发新的步骤边界。它不改变驱动的结束判定，不增加具名事件键；完整展示契约由[聊天与界面](./chat-and-interface.md#44-任务完成后的整轮过程折叠)维护。

[drive.rs](../src-tauri/src/core/agent/drive.rs) 正常/失败/取消路径统一收尾；`RunCleanupGuard` 负责 panic 或任务异常退出时取消子任务、关闭 inject 接纳、清询问 responder、发布错误终态。旧 run 的结束不能覆写新 run。

前端 [runHandlers.ts](../ui/src/stores/runHandlers.ts) 的共享 `settleRun` 清理运行标志、流式正文、运行/等待工具卡、残留子代理及 AskPanel。只有清工具卡而不清 ask，会留下输入区被询问永久覆盖的假卡死。

明确跳过后仍可在同一会话重新发消息：历史、已写文件与 todos 保留，新 run 重新建立取消令牌和运行态；旧询问 responder 不复用。带 `run_id` 的实时 `ask:opened` 必须排除已收尾轮次及与当前轮不符的事件，避免跳过后的迟到询问重新覆盖输入区。start_chat 回执在途时允许新身份的询问先到达；缺 run_id 的旧载荷保持兼容。取消事件与询问关闭事件丢失时仍走下述快照对账收敛，取消本身不自动续队列。

### 丢事件后的自愈

[run.ts](../ui/src/stores/run.ts) 的 `reconcileRun` 通过 `session_run_snapshot` 双向对账：

- 后端在跑：修正本地运行标志和 run 身份。
- 后端已停，且 terminal 与本地 run_id 一致：复用相应 `run:done/error/cancelled` handler。明确正常完成可续队列；失败/取消保持暂停。
- 只知道空闲、没有匹配终态：仅收敛 UI、恢复输入，不臆测成功，不自动续队列。
- IPC 失败：保留原状态，不能反推成“已经结束”。

Tab 生命周期使用 `runEpoch`；启动回执未到的 `startPending` 不接受旧快照，启动期间的终态按回执身份结算。关闭、重开、收尾和新 run 都要隔离旧响应；过期 Git/历史返回不能新建旧桶。`lastDoneRunId` 的去重不能顺手收敛正在跑的新一轮。

普通 ask 等用户回答或 run 取消；审批默认无限等待，只有用户启用 auto_confirm 才在 300 秒后自动允许，且不代选“始终允许”。等待策略与批准门详见[计划与批准](./plan-and-approval.md)。

AppShell 的 5 秒看门狗只查询有未收敛状态的 Tab：判据包括 running、ask、运行中子代理以及主/子流中的 running/waiting 工具，不能仅看 running，否则 `run:start` 丢失时完全失效。切换激活会话也会对账。事件契约保持现有 29 键；扩展载荷仍需分别验证 Rust DTO 与 TypeScript 消费方，键名契约测试不覆盖所有字段。

## 2. 队列与 steer 接纳契约

运行中发送的内容进入前端队列。纯文本条目的“立即”是把新消息注入当前 run，不停止主 run 或子代理；停止按钮/聊天场景 Esc 才是取消，并沿取消令牌级联。子代理仍是父工具批次内执行，不做 detached 后台汇总。

`inject_run_message` 发送“消息 + oneshot 收据”，成功只在驱动实际把消息加入本 run 内存历史并尝试 checkpoint 后返回。写入 mpsc 本身不算成功，也不承诺磁盘写入必然成功；保存问题继续走历史状态提示。

### 后端时序不变量

- 接纳检查、通道入队、空队列关闭和拒绝未消费项共用 `inject_rx` 锁。不能 try_send 后再单独检查原子标记，已被消费的消息可能被误报失败，导致前端重发。
- `drain_inject` 统一步首和收尾前的历史追加、`run:inject`、checkpoint、收据应答；锁不跨 await。
- 纯文本 Finish 与 suggest/batch_done 收尾前，在还有步数和连续文本预算时复查；新消息到达则清掉旧 suggestions、复位 retry 预算并续跑，空队列则原子关闭窗口。
- 空闲、缓冲满、已收尾、未消费即取消/出错/触顶或 panic，均明确拒绝；未消费消息不得泄漏进下个 run。
- 消化消息置一次性续跑标记，在下一次 LLM 结果后无论有无工具调用都消费，避免标记滞留到任务完成后再多跑一轮。已消费消息仍服从显式 `<report>` 收尾与连续文本上限。
- 退避等待可被取消；压缩替换历史前再次检查取消，避免新消息在取消窗口被旧摘要覆盖。

### 前端防重复

`QueueItem.injecting` 必须在 await 前同步占用；占用项禁止再次立即发送、编辑、删除，自动出队也跳过。确认成功才删除条目，失败释放占用并保留。正常 done 早于注入确认时仅凭 `queueResumeAfterInjection` 补续队；取消、错误、启动新 run 必须清该标记，迟到确认不得解除暂停。

关闭 Tab 选择保留内容时，确认仍要结算驻留队列；占用标志不进磁盘快照，快照只保存 id/text/images。带图项不能中途注入，运行中“立即”禁用，等待自然出队时完整发送附件。

`run:inject.texts` 是可选、每条最多 200 字符的 FIFO 回执预览，缺失时回落条数提示。它只用于界面、不重复入模型历史或计费；notice 用纯文本展示并保留滚动锚点属性。重开会话时恢复普通 user 消息，不重建原 notice，预览 tooltip 也不是超过 200 字符的全文。

## 3. 主循环什么时候可以结束

`text_turn_action` 当前判定顺序很重要，不能根据早期修复文档拼回旧顺序：

1. 全部调用参数不可解析被拒：先让模型看到 `<tool-args-rejected>` 并修正；连续超限显式失败，优先级高于 `<report>`。
2. 正文包含 `<report>`：显式汇报收尾。
3. steer 一次性续跑：还有连续文本预算则继续，否则 Finish。
4. 主会话 `finish_on_text=true`：仅当本 run 调过 `plan` 且仍有未完成 todo 时有界续跑；否则回答完即结束。
5. 子代理/计划任务 `finish_on_text=false`：未显式汇报继续推进，超限为 `StopWithLimit`。

`MAX_TEXT_TURNS=3` 表示最多强制续跑三轮，第四个连续无有效工具回合触及上限；工具回合或成功压缩复位计数。主会话未完成计划到上限回落 Finish，子代理及反复被拒路径显式失败。这意味着“正常结束”只表明 run 已结束，不等于全部需求、todos 或验证已经完成。

未完成计划提示只列同一快照内未完成项，并给出 `plan` 更新/删除无关项的出口；不向主会话强加子代理的汇报纪律。`plan_called_this_run` 每轮重置，陈旧待办不能劫持普通问答。suggest 在工具层、发建议事件之前以 `E_PLAN_PENDING` 拦截同样的未完成计划，不能等建议已上屏后再拦。

被拒工具无合法 `tool_use`，不能伪造孤儿 `tool_result` 反馈；整回合被拒改用 user 提示。诊断记录参数长度、解析错误和首尾片段。混合批次的被拒项仍有反馈被 repair 丢弃的缺口，见开放事项。

## 4. 预算、进展与看门狗

低预算提醒按已启动步数计算：`budget_notice_step(max_steps) = max_steps - max_steps/5`，即剩余约 20%，不是消耗 20%。预算少于 5 步时不触发；5–9 步可能与最后一步强制汇报同轮。子代理/任务最后一步注入 `<final-report>`；主会话没有该强制汇报预算策略。

[supervise.rs](../src-tauri/src/core/agent/supervise.rs) 是主/子/任务共用的分层监督：

| 信号 | 纠偏 / 终止 |
|---|---|
| 最近 12 次调用内同工具、同错误、同参数签名失败 | 3 次 / 5 次 |
| 同工具、同错误，参数可不同 | 6 次 / 10 次 |
| 完全相同的成功调用 | 4 次纠偏 |
| 默认 Stop 策略连续无进展 | 8 步 / 14 步 |
| 只读角色 NudgeOnly 的空转层 | 16 步纠偏一次，不在此层终止 |
| 同一文件连续分段读 | 10 次专门纠偏，不直接终止 |

进展摘要 `batch_digest` 必须按 `read/batch_read` 的真实 `files[]` 参数取路径，不能错读顶层 path；历史单 path 只作兜底。首次读新文件或不在只读工具名集合内的调用清零空转计数；压缩成功清空空转状态和已读集合。当前摘要主要看调用名/参数，不等于确认工具成功产生实质进展，拒绝的 command 也可能清零计数。

流停滞看门狗仅监测 LLM 流：配置默认 300 秒，夹取 5–3600 秒；停滞取消本次尝试并走既有重试，用户取消优先。它不是所有工具执行期的墙钟超时，不能宣称任何后台调用都有上限。

## 5. 运行总览

运行总览展示当前进程内项目会话、临时会话及其子代理，不依赖 Tab 是否打开。后台服务和计划任务不在当前范围内，不得把会话数称为全应用任务数；不估算费用、ETA、完成百分比或依赖图。

[activity.rs](../src-tauri/src/core/activity.rs) 是有界观察摘要：

- `ActivitySink` 先更新摘要再转发现有事件，完整委托流帧与子通道绑定；WebView 没收到事件也不影响摘要。
- 用 Weak runtime 对账，不延长 runtime 生命周期；主项按 session/run、子项按 root/run/sub 隔离，旧 run 与已结束 sub 的迟到 tick 不得复活条目。
- 待处理真值是实际 `asks` responder，包含子 runtime 内 responder；等待工具或子代理本身不算需要用户回应。
- 锁内不 await、不 IO、不发事件；读取 runtime 真值前释放摘要锁，避免反向锁序。快照保证单 run 自洽，不宣称全 runtime 的线性一致快照。
- 活跃项不淘汰；最近终态最多 50 条，活跃主 run 内已结束子代理也按完成顺序保留最近 50 条。任务/活动最多 500 字符，报告摘录最多 2000 字符。
- 不收集完整工具参数、转录或密钥配置；任务/报告本身仍是用户工作内容，并无通用文本脱敏。询问完整 payload 只在等待期间保留，回答/取消/run 结束清除。

[activity.ts](../ui/src/stores/activity.ts) 维护独立快照，计数从同一份列表计算。页面可见时每 2 秒刷新，恢复焦点立即刷新，隐藏或覆盖页关闭时停止；请求单飞并按 generation/revision 拒绝旧响应。同步失败保留旧数据和上次同步时间，不能变成“无活跃会话”。

“前往审批/回答”复用原 AskPanel：先 `openSessionReady()` 等待真实会话恢复、对账，再调用 `get_activity_ask(session_id,run_id,ask_id)` 核实 responder。`revealSession()` 的 fire-and-forget 返回值不能当加载完成信号。期间询问已关闭、run 已结束或换轮时拒绝迟到 payload；无关询问关闭不能误伤目标。恢复与实时共用 `hydrateAsk`，不重复发送系统通知，不在总览再造一套批准逻辑。

界面分“需要你回应”“正在推进”“最近结束”，等待项不重复列入推进区；展开与焦点不随刷新重置。步骤是已消耗预算、耗时从本 run 启动计算。详情以明确 session/sub 为目标，展示活动和报告摘录；完整过程仍到会话抽屉查看，空数组不能冒充完整转录。总览保留 ChatMessages/Composer 挂载，标题栏跟随覆盖页并隐藏后台会话胶囊，Esc 不取消背景 run。

**实际 GUI 验收尚未确认。** 现有自动测试和历史门禁不能替代以下手动检查：

1. 两个项目会话和一个临时会话并行，关闭运行 Tab 后仍有正确总览与计数。
2. 主/子代理同时等待多个审批或回答，逐一回跳到正确且仍有效的 AskPanel；目标过期与无关询问关闭均正确处理。
3. 刷新不重置展开、焦点与排序；总览/设置/任务/会话切换的标题、胶囊、草稿和滚动正确。
4. 总览期间新子代理不弹过程抽屉遮挡；Esc 不停止背景 run。
5. 详情宽为窗口一半，缩放跟随；外点只关闭不透传，关闭不停止代理。
6. 报告、partial、budget、no_report、失败和用户停止有准确文字；窄窗/暗色/长标题与同步失败状态可读，恢复焦点及时刷新。

## 6. 后台服务：存活与日志必须分开

[service.rs](../src-tauri/src/tools/service.rs) 的 `service_info()` 是工具 list 与 `list_services` IPC 的共用投影。`running` 表示进程存活，`tail` 只表示日志内容；start 成功声明 `running:true`，不能补空 tail 伪造状态。工具调用已完成不代表其启动的进程已停止。

前端三态：true 为运行中、false 为已停止、缺字段为未知。运行中和未知保留停止入口。无输出服务也发首拍，退出/移除携带最终 tail；退出先发布权威 done 状态，停住 ticker，再发送终态，不能在 exited 后晚到 running:true。

推送外还需每 3 秒 `list_services` 对账：有服务卡才轮询，单飞且拒绝迟到快照；持续日志不能推迟静默服务的对账。遍历既含主 assistant 的 toolsMap，也含平铺的 subStreams.toolsMap。恢复主卡与懒加载子抽屉时都先把历史 `running` 降为未知，再取当前进程快照；服务从快照消失只改存活状态，保留已知日志。

服务表是进程内状态。不要将 owner/purpose 字段、旧日志或历史 start 回执解释成已实现的跨重启清理/恢复承诺。停止应回收进程树；容量失败同样要回收已经创建的进程且不登记永远运行的 wait 目标。

## 7. 计划任务

实现：[scheduler.rs](../src-tauri/src/core/scheduler.rs)、[scheduled_task.rs](../src-tauri/src/tools/scheduled_task.rs)、[任务 store](../ui/src/stores/tasks.ts)。

### 时间、执行与权限

- 调度支持 `cron:`、`every:`、`once:`。五字段 cron 可由周期选择器生成；`every:30 m` 的空白是文法的一部分，`every:30m` 非法。分钟最多 43200、小时 720、天 30。
- cron 按系统本地时区计算 `Schedule::after(&now)`，RFC3339 存具体触发时间；界面本地格式化不是调度修复。夏令时回拨应跳过不晚于 now 的候选。
- 恢复时校正错误的启用 cron next_run；未来值必须是最近下一次触发，不能仅验证“符合表达式”。匹配的过期时间交既有 tick 处理。
- supervisor 每 15 秒 tick，到期执行通过全局 `exec_lock` 串行化；手动“立即运行”抢不到锁直接报忙，不排队，也不修改 next_run。
- 每次执行新建独立 task runtime，工作目录从所属项目解析，项目缺失或目录无效则记 skipped；不借用某个当前会话目录。
- task runtime **显式 FullAccess**，不继承当前主会话权限变更，模型与力度默认跟随全局；灾难级 fence 拦截仍在。工具集排除 ask、subagent、scheduled_task、suggest、wait 和 MCP，30 步预算，要求 `<report>`。创建任务即安排后续自动执行，安全边界见[工具与安全](./tools-and-safety.md)。

暂停任务不产生新的自动认领；已认领的当前 run 仍正常收尾并记录结果。启用/修改周期重新算 next_run。`enabled` 的 serde 缺省必须 true，避免旧任务意外全暂停。`TaskRun` 记录时间、状态、摘要、来源及输出 token，常规记录最多 20 条、新者在前。状态 `ok/skipped` 用中性文字，失败用红；未知状态原样展示。

### 持久化、认领与撤销

项目任务保存、装载、删除与日志统一解析真实注册项目的 `<主目录>/.codewave/tasks/<id>.json`（日志在同一托管目录的 logs）。不扫描旧全局项目布局、不写 fallback 目录、不搬动数据。没有 project_id 的任务仍仅在当前进程有效。

项目读取和任务恢复共用 `projects::load_report`：读取 `directories.json` 及会话快照发现锚点，普通读取绝不忘记暂时离线项目或改写损坏索引。任务恢复只读普通 `.json` 文件，拒绝任务文件/任务目录符号链接；校验 id、文件名、所属项目，重复 id 的所有冲突副本均隔离。损坏文件保留原样，其它健康项目继续装载。任务页展示逐项恢复问题；项目不可读、索引损坏与真正空列表有独立诊断。

所有恢复、创建、编辑、启停、删除、认领及结果提交共用 mutation_lock。固定锁序为 `exec_lock → mutation_lock → state`，编辑从 mutation_lock 开始，永不等待 exec_lock；state 锁不跨文件 IO。首次列表/修改与 supervisor 共用一次初始化门，恢复不能覆盖同时新建或删除的任务。原子替换前失败回传 IPC/工具并保留旧值；文件已替换后目录同步失败不是回滚，创建/编辑发布已提交快照并携带 persistence_error。认领遇到这种持久化不确定性时绝不执行，隔离未执行的原计划，只有重写原快照并确认持久化后才重新接纳。启动恢复仅在需要纠正 cron 或结算中断/错过的 once 时回写，不重写全部文件。

每次 tick 只生成 `{task_id,generation,scheduled_for}` 候选：每任务最多一个 pending，收集阶段不推进 next_run、不发事件、不登记等待目标。拿到全局执行锁后，重新验证任务存在、版本、enabled、计划时间和到期状态；从当前实体生成执行快照，并先持久化 active_run 与 next_run，成功后才允许执行。创建、编辑、启停、删除都会使旧候选失效。重复周期在排队期间错过多次时合并为一次，下一次从实际认领时间计算。

认领提交是撤销边界：暂停/删除先成功，旧候选零执行；认领先提交，当前 run 可以完成，暂停只影响后续定时运行。运行中编辑不修改已经认领的指令，收尾只合并匹配 run_id 的结果，不覆盖新名称/指令/周期。手动运行仍允许暂停任务、忙时直接拒绝、不改 next_run。项目删除与任务写入共用协调锁，先失效候选，迟到结果/日志不得重建已删除托管目录。

### once、崩溃与未保存结果

once 消费调度机会时只清 next_run，保留实体和 enabled 偏好；不把“没有后续触发”说成成功。`active_run {run_id,source,scheduled_for,started_at}` 与执行机会一同保存；`TaskRun.run_id` 保证收尾幂等，状态/摘要、最多 20 条历史和清 active_run 一次提交。

- 认领后异常退出：下一进程记 `error`「上次执行中断，结果未确认」，不自动重放；同一 run_id 多次恢复不重复记账。已经另行编辑的未来时间保留。
- 未认领但离线期间过期的启用 once：保留任务，记 `skipped`「应用离线时已错过计划时间」，不突然补跑完全访问指令。暂停的过期 once 不记历史；启用前须改成未来时间。
- 结果写盘失败：真实结果保留内存，persistence_error 明确显示「结果尚未保存」，避免伪装已保存。supervisor 每拍尝试一次保存当前快照；成功前不再认领该任务，删除/项目删除后不重建。进程在未保存状态再次退出只能恢复磁盘里的未确认状态，不能保证恢复内存里尚未写入的摘要。
- panic/取消收尾只操作本次 run，并释放 pending/运行标记与执行锁。异步兜底清理继续持有执行锁直到结果结算，驱动不作为 detached 子任务逃出锁生命周期。

以上保证是不盲目重放未确认操作，**不是外部副作用 exactly-once**。回退旧二进制可能忽略 active_run 并重新删除 once，必须先停止调度并备份任务文件，优先前向修复。

### 界面对账与人工验收

任务页与左栏共用 `stores/tasks.ts`。列表是运行权威快照，包含 active_run 和未保存状态；页面可见时每 3 秒、恢复焦点时刷新。fired/done 附可选 run_id，迟到的旧 done 不得清掉新 run。列表请求按序号与本地 revision 拒绝旧响应；失败及非法非数组结果保留原列表。创建/编辑/删除的成功结果不被更早列表覆盖。

已消费 once 可展开历史、编辑、删除、手动重跑；开关禁用并提示「修改时间以重新安排」。只改名称/指令保留原始计划字符串（包括秒和时区），不误重排；新计划才计算下一次。运行中暂停显示当前 run 继续；保存失败明确可见。项目归属继续区分未归属、读取失败和可信列表中不存在。任务页保留工作区挂载；Esc 先关闭浮层，再关闭页面，不停止后台会话。

自动回归覆盖真实项目 roundtrip、无会话离线项目与损坏索引、文件名/归属/重复/symlink 隔离、原子写失败、pending 撤销与重建、虚拟时钟下串行假执行器、once 三种终态/重复收尾、崩溃中断与恢复保存失败、手动历史上限及前端乱序对账。此处列出的是测试覆盖，不能替代完整 Tauri 构建或实机验收。

**GUI 与跨平台人工验收仍待用户执行：**

1. 真实项目创建 future once/every，运行后退出重开，核对启停、下一次与历史；完成 once 仍可展开/手动重跑，重命名不重排。
2. 用无外部副作用 fixture A 占锁、B 排队，分别暂停/删除/改期 B，确认 A 结束后 B 不启动；认领后暂停只阻止后续运行。
3. 暂时移走测试项目托管目录，刷新项目与任务页，确认可见诊断、其它项目可用，移回并重启后仍可发现。
4. 模拟认领后退出，重开显示未确认结果且不自动重跑；模拟写失败，核对未保存提示与恢复写入后历史只有一条。

## 8. 条件等待

[wait.rs](../src-tauri/src/tools/wait.rs) 支持 `seconds/reason/until?`，依然是必须独占批次的 Interactive 工具，不启动任务。目标使用已得到的 service/subagent/scheduled_task 回执 id。

- 不带 until：保持原 `{waited_seconds,reason}` 出参，不增加条件字段。
- 带 until：seconds 为最长等待 1–3600 秒，1 秒轮询；条件满足提前返回。
- `state=done` 接受任意终态，`exited` 只接受 Succeeded，`failed` 只接受 Failed。
- `match.text` 大小写不敏感子串；仅带 match 时运行中日志即可命中。显式 state 与 match 是 AND；二者均缺省才默认 done。
- 判据顺序为取消 > 条件 > 超时。命中为 `condition_met`；超时仍 ok，带 `timeout` 与 observed；目标不存在或被移除为 `E_NOT_FOUND`，参数非法 `E_ARGS`，取消 `E_CANCELLED`。
- 判定和 observed 必须来自同一原子快照；锁异常保守不命中并带 probe_error，不能伪造成功。observed 文本按 UTF-8 安全截到约 2000 字节。

[wait_targets.rs](../src-tauri/src/core/wait_targets.rs) 是独立进程内登记表，不反查“结束即移除”的 subs。service 真正启动后登记、确认退出置终态；子代理 Drop 在 subs.remove 前置终态；scheduled task 每次 run 启动重新登记，并原子重置上一轮状态。连续两轮同为 ok/error/skipped 也应完成，baseline 仅供诊断，不是“必须状态变化”的门槛。

service ticker 更新活日志；service 退出当前按 Succeeded 表示进程已结束，不解释退出码成功与否。子代理 report/partial 都是 Succeeded，budget/no_report 是 Failed，分析批准门另用更严格判据。终态单生命周期只前进；lookup 惰性清理超过 1 小时的终态，活跃目标永不淘汰。重启不保留这些等待目标。

## 9. 排查与回归

| 症状 | 判别顺序 |
|---|---|
| 界面转圈但后端已结束 | 会话日志 → RunSnapshot/run_id → 前端 epoch/startPending → settleRun；不要仅猜 provider 挂起 |
| 注入后消息丢失或重复 | 收据是否已确认、所属 run、接纳锁、队列 injecting、迟到 done/确认的顺序 |
| 旁白后提前成功结束 | rejected → report → steer → 本 run plan 标记 → text_turns；检查 suggest 是否另走旁路 |
| 无输出服务被显示停止 | running 三态、首拍、list_services 对账、子流平铺 toolsMap |
| 定时执行晚几个小时 | 实际 next_run 和本地 cron 计算，不能只修显示 |
| wait 不结束 | 本轮登记是否重置、match 是否误带 state、真实终态映射、observed/probe_error |

重点测试：后端 `core::agent::tests` 的收尾矩阵和脚本化 SSE、runtime 的 run_snapshot/activity、`core::scheduler`、`core::wait_targets`、`tools::wait`、`tools::service`；前端 `run.terminal-fallback`、`run.queue`、`run.service-update`、`toolcard.service-status`、`activity.store`、`activity.navigation`、`activity.dashboard`、`tasks.page`、`stores.tasks`、`schedulePreset`。完整运行方式见[测试指南](./testing.md)。

仍开放：事件投递丢失的实机根因尚未最终证实，`bindEvents` 仍逐键串行 await、单次注册失败可中断后续注册；兜底不等于已定位根因。ask 的部分等待路径仍缺独立 Drop 收口。混合批次被拒调用的 user 反馈、`<report>` 只是包含匹配导致复述误收尾、无界 web_fetch/工具执行超时、服务退出 stop-all/owner 清理、缺 service id 的停止按钮提示，均不能标成已修。本指南只记录边界，本轮不修改运行代码。
