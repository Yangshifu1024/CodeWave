# 计划、询问与批准

> 当前行为指南；源码核对：2026-10-11。面向维护工作流、权限切换与 AskPanel 的开发者。命令/文件边界见[工具与安全](./tools-and-safety.md)，子代理生命周期见[子代理](./subagents.md)，界面布局见[聊天与界面](./chat-and-interface.md)。

## 1. 流程与权限是两套机制

工作流提示规定分析、方案、执行、审查的顺序；宿主只在确定的节点强制工具排除、计划存在、批准前置与范围确认。不能把完整阶段流程描述为一个已经实现的后端状态机。

[ApprovalMode](../src-tauri/src/core/prefs.rs) 有四档：

| 档位 | 内置文件写入 | shell | 说明 |
|---|---|---|---|
| `plan` | 排除 `edit/create/delete/write_document/edit_document`，另排除 `service/scheduled_task` 与全部 MCP | 只读白名单 + fence；所有 Confirm 转 `E_PLAN_READONLY` | 全局审批开启时，新会话默认此档 |
| `confirm_each` | 每次 FileWrite 先确认，能生成 diff 时展示 diff | 已识别的区内写、高危等需确认 | 普通 ask 的有效应答另有轻量切档行为，见下文 |
| `auto_edit` | roots 内可写；不逐项审批 | 高危、区外新建等按 fence 确认 | 不绕过路径、删除、计划与范围门 |
| `full_access` | 跳过逐项写入审批 | 跳过普通 Confirm；灾难级仍拦截 | 不等于关闭所有宿主安全门；MCP 仍按 server 策略审批 |

全局 `approval.enabled=false` 映射为新会话 FullAccess；实际 `ToolCtx::fence_policy()` 的 `approval_enabled` 固定为 true，FullAccess 的跳过在 command/service 消费点处理。不要把旧的“审批关闭使 Confirm 变 Block”底层测试策略当成当前界面的关闭开关。权限档不随会话偏好边车持久化，恢复时回落全局初始档；模型与思考力度另存。

工具列表过滤与执行拒绝必须同源：[main_drive_params](../src-tauri/src/core/agent/drive.rs) 生成排除集，[execute_batch](../src-tauri/src/tools/batch.rs) 在 spawn 前返回 `E_TOOL_BLOCKED`。只从模型 schema 中藏工具不构成硬门。

## 2. 选择工作流

### 计划档 P0–P6

1. P0 声明需求/缺陷/问答/混合分类与依据。纯问答直接回答，不建计划。
2. P1 消除关键歧义，或明确假设。
3. P2 需求先交 product-manager，产出用户故事、AC、边界、非目标与开放问题；缺陷先交 tester，产出复现、根因、影响面与回归要点。分析不另设用户批准门；开放问题回流澄清，通常不超过两轮。
4. P3 主代理写文件级方案、风险/回滚/验证方式与分支名，登记含验证项的 todos，再发三选项批准询问（方案卡提供固定预览按钮）。
5. P4 用户批准后按方案执行，更新 todos；批准后不重新启动完整流水线。
6. P5 多文件/跨层修改须 code-reviewer；严重问题必须解决。轻量路径不强制审查。
7. P6 汇报实际改动、验证结果、偏差与未完成事项。

预计不超过两个文件、无删除、无新依赖、无跨层改动时，可明示“轻量路径”并用内置简析替代 P2 子代理。用户要求跳过分析也不免除 todos 与计划档批准询问。**文件数阈值属于提示协议，宿主 `lightweight=true` 没有文件数或 todo 条数校验。**

### 非计划档的标准研发流水线

[WORKFLOW_SECTION](../src-tauri/src/core/prompt.rs) 是常驻、字节稳定的系统提示；`arch` 已不再是技能或可派生角色。

- 轻量需求直接实现，仍登记 todos，不生成四份任务文档。任一轻量条件超阈进入完整流程；明确要求完整/轻量可改变路由，拿不准默认轻量。
- S1 explore 与 S2 product-manager 同批并行；S3 汇总 requirement.md，关键歧义最多先澄清一轮。
- S4 主代理写 plan.md 与 todos；S5 走下文批准门。
- S6 先建已批准的计划分支，再按技术栈派 backend-dev/frontend-dev/app-dev。任务包文件范围互斥；公共配置、锁文件、导出与公共类型由主代理集中处理。并发额度内尽量并行，真实依赖才串行。角色和写认领的硬约束见[子代理](./subagents.md)。
- S7 reviewer 与 S8 tester 同批并行。测试必须真实执行，返工时注明测试对应的代码版本；严重问题最多一轮返工复审，仍未对齐则交用户裁决，不能改写为“全部完成”。
- S9 汇报各阶段结论、产物与遗留事项。子代理只回报，四文档由主代理落盘。

项目任务产物目录为 `<主目录>/.codewave/tasks/<YYYYMMDD-HHMMSS>-<slug>/`；slug 不超过 24 字符并过滤非法路径字符，冲突加后缀。四文档分别是 requirement.md、plan.md、review.md、report.md，即产即存。临时会话可跑完整流程，但跳过产物落盘并在收尾说明。

### 计划批准与 git 的授权范围

git 仓库方案须提出 `<type>/<slug>`（type 使用 Conventional Commits，slug ≤24 字符），基线为当前 HEAD；题干与完整 plan 都展示分支名。批准仅预授权创建/切换该分支，不授权 commit、push、merge、stash 或清理分支。脏工作区须说明未提交修改会随分支携带；不擅自 stash。非 git 目录注明跳过分支。

动工前执行 `git switch -c <name>`；若重名须提出带后缀的名称并重新让批准询问展示它；创建失败如实报告。**分支条款是提示与用户授权协议，后端 git 模块并不代建分支，fence 也没有完整 git 子命令写分类**，不能将其当成一条已强制的权限边界。

## 3. 批准门 wire 与三条防提权路径

标准批准询问是单题 `id="approve_plan"`，携带精确拼写的 `switchToAutoEdit=true` 与完整 `plan`。三项是：

| 选项 | `mode` | 效果 |
|---|---|---|
| `approve`，以自动编辑档执行（推荐） | `auto_edit` | 批准并按此档执行 |
| `approve_full`，以完全访问档执行 | `full_access` | 批准并按此档执行；界面恒定附加风险说明 |
| `revise`，补充意见 | 不带 | 修订，不切档、不冻结基线 |

[Option2](../src-tauri/src/tools/ask/tool.rs) 的 `mode: Option<ApprovalMode>` 用 serde default 与 `skip_serializing_if`：缺省时 wire 不出现该键，不能输出 `mode:null`。结构化批准类判定为 `mode.is_some()`；目标取用户选中项的 mode，旧批准形态才回落 AutoEdit。旧 `id="approve"`、`switchToAutoEdit` 与宽松 id/label 匹配保留兼容，不要随意删掉。

必须区分：

- `approval_shape` 是宽松的单题批准形**渲染标记，无安全语义**。
- `arch_gate_shape` 是单题且存在精确 `id="approve"`，限定 `switchToAutoEdit` 的完整工作流标记。
- `selected_target_mode` 按选中的 id（兼容 label）读取 mode，当前实现**不要求严格 arch 形状**；不能沿用旧文档“所有 mode 都受 arch_gate_shape 限制”的说法。

### 常驻预览动作与旧选项兼容

方案卡底部「查看方案 / 查看预览」（英文 View plan / View preview）同行；查看方案仍打开已保存的计划文件。宿主下发可选 `can_preview_plan`，条件为当前单题批准形且具有真实非空方案正文，不依赖模型是否提供预览选项或计划文件是否保存成功。普通询问、多题、安全审批和无正文的卡片不新增入口。

固定按钮走当前 `resolve_ask` responder，发送 `{action: "preview_plan", note: "…"}`，不夹带默认批准项、不发送普通聊天或 steer。后端按打开时的方案快照校验，优先于全空回答、批准匹配和切档路径处理；混合/异常动作载荷失败关闭。成功返回 `preview_requested` 与完整 `plan_body` 快照；仅此类成功的 ask 结果豁免模型侧 head/tail 截断（普通 ask 不变），即使计划文件保存失败也能保留长方案正文。要求加载 preview 技能展示该方案并重新发起同一批准询问，不批准、不切档、不冻结或重置 todos 基线。备注只作为预览要求，不能成为批准。实际批准继续走原 G2/G3 与用户选择的档位。

按钮的“常驻”指每次待确认方案卡都有入口；请求后沿用运行界面，重新询问后恢复入口，不给历史方案增加生成操作。请求按原会话和 ask 身份单飞；失败可重试，旧请求不能回写新会话或新询问。按钮键盘操作不冒泡为批准/提交，旧模型预览项在固定入口存在时去重，键盘编号与展示列表一致。

### 旧预览选项必须同时在三处排除

1. **结构化选档**：正常预览项不带 mode，因此目标为 None；前端 `planPath`、`modePath` 还要乘 `!previewOnly`，防模型给预览项误挂 mode。
2. **宽松批准匹配**：后端 `approved_hit`、批准候选与前端同款判断都先剔除预览 id。模型可能自拟 `preview_approve`，不能因其中含 approve 就批准。
3. **轻量切档**：`valid_answer` 与最终 `switch` 都乘 `!preview_only_answer`，不能只挡其中一条；否则 `switchToAutoEdit` 标记仍可触发切档。

预览识别为 id 大小写无关匹配 + label 的“先看预览”/“Preview first”兜底。选中项全为预览且至少一项才是 preview-only；备注不改变该意图。“预览 + 备注”同样不得批准。**不得为安全而收紧 `approval_shape` 去排除预览**：`preview_option_ids` 以它为前提，收紧反会失去预览识别并打开轻量提权通道。

[AskPanel](../ui/src/features/tools/AskPanel.tsx) 先 resolveAsk，再同步 updatePrefs；按实际选择同步，不能硬写 auto_edit，也不能只覆盖从 Plan/ConfirmEach 出发的路径。后续 prefs 是整份写入，任何前后端分叉都可能被下一次偏好保存静默翻回。后端以**询问打开时档位快照**判断，防应答期间前端先改 prefs 干扰基线冻结。

## 4. 宿主硬门与范围状态

### 实现类写入必须有未完成计划

[batch.rs](../src-tauri/src/tools/batch.rs) 对主会话 FileWrite，在工具/MCP 排除门之后、spawn 之前检查批次入口 todos 快照：

- 空计划 → `E_PLAN_REQUIRED`；全部 completed → `E_PLAN_STALE`。
- 非主会话豁免，因子代理没有 plan 工具。
- 同批含 plan 调用则乐观豁免，**即便该 plan 调用最终失败也仍豁免**；这是当前契约，不可写成事务性保证。
- command（包括 shell 写）与 MCP 不经过此门。它不是“全部副作用必须先建计划”的通用拦截器。
- 写入成功、todos 非空且没有 in_progress 时，每 run 最多一次软提醒。提醒必须拼入 ToolResult content，不能只加在 Tool role 的独立 Text 块，三协议会丢弃后者。

[plan 工具](../src-tauri/src/tools/plan.rs) 不传 todos 是读取，传入则全量替换；最多 100 项、标题非空、同时最多一条 in_progress。状态随会话保存并发 `plan:update`。

### 批准前置 G2/G3

完整批准路径（打开时 Plan，或严格工作流标记且真正批准）先跑 `plan_approval_gate`：

1. todos 为空 → `E_PLAN_TODOS_REQUIRED`，任何豁免都不能跨过。
2. `skipAnalysis=true` 且最近用户消息命中口令，或 `lightweight=true`，可跳过分析产物检查。口令包括“不用分析/跳过分析/无需分析/skip analysis”；“直接改”还须与“分析”语义同现。lightweight **不再限制 todo 条数**。
3. 其余情况需要成功的 product-manager/tester 分析标记，否则 `E_PLAN_ANALYSIS_REQUIRED`；角色别名与角色注册表同源。
4. 通过后冻结 todo 标题为 `approved_plan`，重置范围扩张、永久允许及拒绝记录，切档并注入已批准执行指令。

宿主只核对 todos 非空与分析标记，不验证方案质量、是否真的包含验证项或分析内容完备。普通 ConfirmEach 非严格批准闸的有效回答可走轻量路径：只切到 AutoEdit（显式 mode 时按所选值），不跑完整 G2/G3、不冻基线、不注入开工消息。批准闸的 revise/preview 均不能借此路径切档；全空回答也不切。

### 执行范围 G3

计划批准后全量更新 todos 时，`classify_new_todos` 与批次写通道门共用分类口径。归一化仅剥序号、合法任务包标签、标点壳与重复空白；不做语义相似度。细化必须是**新标题为原基线的子串**且至少 8 个字符，避免“单测”“顺手删库”这类短片段被误免。路径符号保留；任务包标签不能含藏在角色括号/包序号之外的正文。

真正新增触发范围询问，覆盖 FileWrite 与 fence 探针判出 Confirm 的 command，Network/MCP 不在该范围门内。`scope_gate_lock` 单飞，锁内重判；锁序 `file_ops → scope_gate_lock`，纯只读命令不排队。

| 决定 | 当前效果 |
|---|---|
| 允许 | 标题保序去重**追加**进基线，不是替换，也不是永久允许 |
| 本会话始终允许 | `scope_allowed=true`，仅此 runtime，下一次方案批准会重置 |
| 拒绝 | 规范化标题加入 `scope_denied_titles`，同一被拒范围的排队调用及后续批次持续返回 `E_SCOPE_DENIED`，不重复弹卡；须收窄范围或重新取得批准 |

旧“拒绝消费本轮标记、同批后续写放行”已被当前实现替换，不得恢复。`approval_kind=scope` 与普通 `once` 的文案和后果分开：范围门不是项目级长期命令白名单。G3 基线是内存态，手动切到执行档也不会凭空生成基线；它不提供重启后的完整范围追踪保证。

## 5. 普通询问、计划卡与预览

ask schema 为 1–5 问，每题最多 6 选项，支持开放题。ask/wait 必须独占批次。全空 selections 且 note trim 后为空时，在 `ask:closed` 后返回 `E_ASK_NOT_ANSWERED`，明确要求停下等待，不把忽略当成功/默许，也不原样重问；run 取消/通道关闭为 `E_ASK_CANCELLED`。部分回答保留未回答题尾注，不把整组当忽略。

- AskPanel 覆盖输入区；回答或忽略后恢复草稿。多题按页回答，非末页忽略只清当前题并翻页，末页清当前题后提交整组。
- 批准形渲染优先；其余由[同一 `inferSingle`](../ui/src/features/tools/askShape.ts) 控制单选展示与 toggle，不能“显示 radio、实际多选”。显式 true/false 优先于启发式。当前 Rust `Question.single` 是 default bool，标准事件会把缺省序列化为 false，因此不要承诺模型省略该字段就一定触发前端启发式；强互斥题应显式 true。
- 键盘环包含选项与补充说明位；输入位 Enter/Space 只聚焦，不提交。完全访问风险提示由界面追加，不得被模型 description 抵消。

计划卡必须有真实正文：优先显式非空 plan；否则取当前 runtime 本轮 assistant Text，要求与归一化题干不同、长度不短于题干、没有 `<ask`/`</ask`。仍无真方案则不落盘、不显示空壳计划卡。`plan_body` 与保存文件同源，复制/显示均不能自己再拼题干。保存失败不阻塞询问；系统保存允许 Plan 档产出方案文件，并不等于开放一般写工具。

preview 技能只用于已有成型对象，界面示意须标“示意稿 · 未实现”，不写业务代码、不落盘；render_html 单次不超过 50,000 字符，自包含且无外链请求。渲染失败回退聊天概览并重新提供批准机会；用户明确点击可重复预览，不再有三次后隐藏入口的限制，不允许模型自行连续生成预览。此改动不开放业务代码写入、服务、网络或新的权限档位。

点击查看预览后，在 resolve_ask 发出前按原会话置位自动弹框信号（IPC 失败仅回滚本次请求）：armed → 本 run 第一张成功且有非空 html 的主会话 widget 锁定 callKey → 打开即消费；错误/空结果不消费，终态清空；新询问清除尚未锁定的旧请求，已锁定 widget 仍只打开一次。历史恢复、子代理 widget、未 armed 的普通 render_html 不自动弹。不能把自动弹框改造成切档副作用。

文本 ask 兜底只救回主会话末尾、独占起始行、结构完整的 ask 块；必须本步无真实工具、ask 未被排除、每 run 最多一次。救回后仍走真实 AskTool 与全部批准门；不能扩展为把正文 command/edit 等当工具执行。显示剥离和晚到分片规则见[聊天与界面](./chat-and-interface.md)。

## 6. 等待与同步

安全审批与普通 ask 共用 `ask:opened`/`ask:closed`，但等待语义不同：主会话安全审批默认无限等；显式开启 auto_confirm 后 300 秒自动允许当次，不自动写“始终允许”。灾难级永不自动允许。子代理安全审批事件挂根会话并附 sub_id，回答仍找真实 responder；无论 auto_confirm 设置如何，600 秒无回答按拒绝收口，不能挂死。

根会话切档在每个 LLM step 边界传播到在跑子代理：工具集、`<plan-mode>` 系统块、子 rt.prefs 都从 `SubBase` **重新构造**，禁止增量追加导致旧 Plan 块残留。只传播 approval_mode，模型/力度维持 spawn 快照；根会话缺失保留当前档，不 panic。FullAccess 下只读角色写工具可解锁，但行为/文件认领仍受角色与调用契约约束。计划任务 runtime 独立且显式 FullAccess；steer 文本只注入主会话，不等于传给子代理或改变档位。

## 7. 回归与仍待核验的接缝

后端重点入口：[ask/tests.rs](../src-tauri/src/tools/ask/tests.rs)、[batch.rs 测试](../src-tauri/src/tools/batch.rs)、[plan.rs](../src-tauri/src/tools/plan.rs)、[approval.rs](../src-tauri/src/safety/approval.rs)。前端入口：[askpanel.test.tsx](../ui/src/__tests__/askpanel.test.tsx)、[runWidgetAutoOpen.test.ts](../ui/src/__tests__/runWidgetAutoOpen.test.ts)、[events.contract.test.ts](../ui/src/__tests__/events.contract.test.ts)。载荷只增可选字段时不改事件键名。

必须覆盖：无模型预览选项仍有固定入口、旧选项去重、计划保存失败仍可预览、重复/键盘点击与跨会话竞态、第四次手动预览；四档起点按所选 mode 同步；preview/preview+备注/自拟 approve 子串 id 均不切；revise 不冻结基线；G2 豁免仍拒空 todos；同批 plan 失败仍豁免的既有取舍；并发范围门只弹一次且拒绝持续拦；忽略不继续；自动预览只开一次且恢复不重开。

还需补齐文本 ask 的双向契约测试：`text_ask.rs` 的 `MAX_QUESTIONS/MAX_OPTIONS` 与 AskTool schema 的 `maxItems` 目前双写 5/6；当前四档 `ApprovalMode` 的 serde 名与 schema enum 也须成套比对。现有单一 `full_access` 解析用例不能替代这两类等价断言。

尚需专门验收：真实多步 provider 驱动下的根档位变化到下一子代理 step 工具集变化、生产计划任务档位入口，以及子 step 的 approval_mode 后端事件断言。普通 ask 有效回答切档与完整批准 gate 的边界、single 缺省的前后端差异也应纳入端到端测试。GUI 按[测试指南](./testing.md)交手动清单，不把历史“测试全绿”数量当成本次验证。
