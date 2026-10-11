# CodeWave · AGENTS.md

> 项目入口指南。CodeWave 是桌面 AI 编程 Agent：Tauri 2 + 纯 Rust 后端，React 19 + antd 6 前端。
> 核心概念：**项目 = 单个主目录**——项目数据存于主目录下 `.codewave/`；临时会话免目录直接开聊（[docs/sessions-and-projects](./docs/sessions-and-projects.md)）。

## 必读文档

[README](./README.md) 是产品与上手入口；[docs/0-README.md](./docs/0-README.md) 是完整文档索引。当前 `docs/` 共 **15 篇（14 篇长期主题指南 + 索引）**，按本次任务选择必读主题：

| 任务 | 必读主题 |
|---|---|
| 开发、结构与验证 | [开发与工程约定](./docs/development.md)、[架构与决策](./docs/technical-design.md)（D1–D8 与现行分层）、[测试与本地门禁](./docs/testing.md) |
| 会话、执行与委派 | [会话与项目](./docs/sessions-and-projects.md)、[运行与调度](./docs/runtime-and-scheduling.md)、[子代理](./docs/subagents.md) |
| 计划、权限与工具 | [计划与审批](./docs/plan-and-approval.md)、[工具与安全](./docs/tools-and-safety.md)、[文档工具](./docs/documents.md) |
| 模型、扩展与设置 | [供应商与网络](./docs/providers-and-network.md)、[MCP 与技能](./docs/mcp-and-skills.md)、[设置指南](./docs/settings-guide.md) |
| 界面与交付 | [聊天与界面](./docs/chat-and-interface.md)、[发布、签名与自动更新](./docs/release.md) |

本文件[契约锚点](#契约锚点改前必读)保留改动前必须关注的约束；详细行为、理由与回归入口以对应长期主题为主。

### 文档维护

- `docs/` 保持平铺结构，主题指南使用不带编号的英文 slug，索引保留 `0-README.md`。**优先更新已有长期主题，不为每次修复新增报告**。
- 本轮整理以含索引不超过 20 篇为规模目标，当前为 15 篇；篇数不是永久不变的架构约束。确需新增独立主题时说明已有指南为何容纳不了，并同步索引与阅读入口；调整规模目标时一并更新索引规则。
- 当前目录保留有效契约、决策缘由、排障与未完成检查；完成计划、一次性验收和纯视觉微调留在 Git 历史，不另建归档副本。
- 同一契约由一个主题主维护，其余文档保留摘要与链接。合并、改名或删除文档时，同步全仓 Markdown 链接、源码注释引用及读取文档的测试；不得为整理文档修改业务逻辑。
- 引用文档使用 Markdown 链接，按当前文件解析相对路径并核对章节锚点。未验证事项继续明确标注，不能把历史全绿或文档整理写成本次验证结果。

## 核心约束

- **git 写操作默认由用户执行**：commit / push / merge / branch / stash 等 AI 不主动碰；用户明确要求或授权时可执行（范围以当次授权为准）。例外：用户批准计划 = 预授权创建并切换计划所示分支（标准工作流 S5/S6、plan 档 P3，[docs/plan-and-approval.md](./docs/plan-and-approval.md)）；commit / push / merge 仍由用户执行
- **数据与密钥落位**：全局数据（配置、会话、日志）在 `~/.codewave`，项目托管数据在 `<主目录>/.codewave/`；API key 应入系统钥匙串，避免明文落盘；当前保存流程先写配置再迁移，迁移失败会保留明文，勿把设计目标写成已落实的保证（见 [SECURITY.md](./SECURITY.md)）
- **纯 Rust 后端，框架边界明确**：`core/` 不引入 Tauri 类型；只有 `lib.rs` 与 `host/` 允许 `use tauri::*`；IPC 命令只做校验 + 转调 core。现有 core → `host::keyring` 凭据依赖例外见[架构与决策](./docs/technical-design.md)，不能据此宣称完全没有宿主层依赖
- **数据目录 `.codewave`**：全局与项目托管数据共用此目录名，勿再建议改名
- **项目 = 名称 + 单一主目录**；托管数据（project.json + temps/logs/memory/skills/tasks + mcp.json/lessons.md）存于 `<主目录>/.codewave/`；data_dir 随保存归一化持久化（[docs/technical-design](./docs/technical-design.md)：legacy 目录回退已移除）；`ProjectEntry.allowed_dirs`（serde default）记用户「始终允许」的项目外目录，会话创建/恢复时并入 `extra_roots`
- **文件入口统一为「原地引用」**：附件按钮与拖入窗口拿到的都是**真实路径**，非图片文件记进草稿 `refs` 并以 **chip** 展示（输入框里不出现路径），发送前一刻才合成 `@路径` 追加正文末尾（不复制副本、不占上下文；[docs/chat-and-interface](./docs/chat-and-interface.md)）；图片因要上 wire 才读成 base64；网页内 `<input type=file>` 拿不到路径，故附件按钮走原生选择框（[docs/documents](./docs/documents.md)）
- **会话语义**：项目会话（必须选项目，快照固化主目录）或临时会话（免目录，工作区 = 全局数据目录，写入仍走 fence 审批）；命令默认 cwd = 项目托管目录 `<主目录>/.codewave/`（临时会话 = 工作区根，通常为全局数据目录）；显式 cwd 按读取边界解析
- **删除项目** = 级联删除其下会话 + 托管目录（确认弹框列明影响）；用户代码目录永不动
- **界面风格**：自绘标题栏（[docs/chat-and-interface](./docs/chat-and-interface.md)：tauri-plugin-decoration v3，顶栏即标题栏；窗口 `visible:false` 起动，激活失败回退原生框）；主题跟随系统；**强调色 = 中性墨色**（`colorPrimary` 亮 `#1f1f1f`/暗 `#424242`，[docs/plan-and-approval](./docs/plan-and-approval.md)——勿再引入默认蓝或彩色 accent；色彩强度映射风险等级：无彩色=默认、橙=需注意、红=危险）；字体走 `--ws-font-sans/--ws-font-mono` 双槽 token（用户偏好由 `ui/src/utils/fonts.ts` 管理，勿再硬编码字体链）；组件样式一律 antd 组件库接管（`--ws-*` token 由 `ui/src/theme/bridge.tsx` 桥接），不手搓皮肤/自绘调色板

## 常用命令

> 环境准备见[开发指南](./docs/development.md)，门禁详情见[测试指南](./docs/testing.md)。Tauri CLI 走根依赖中的 pnpm CLI，无需另装 cargo-tauri；除注明外，命令均在仓库根执行。

| 用途 | 命令 | 说明 |
|---|---|---|
| 安装依赖 | `pnpm install --frozen-lockfile` | 安装根与 ui workspace 的锁定依赖；工具链与系统依赖仍需按平台准备 |
| 后端测试 | `cargo test --workspace` | 在 `src-tauri/` 执行，与 CI 命令对齐；新增 warning 必须处理，测试数与告警数以本次实际输出为准 |
| 前端测试 | `pnpm --dir ui test` | Vitest；用例数、失败与跳过项以本次实际输出为准 |
| 前端 lint | `pnpm --dir ui run lint` | ESLint |
| 前端构建 | `pnpm --dir ui build` | type check + vite build |
| 开发调试 | `pnpm tauri dev` | 仓库根执行 |
| 打包 | `pnpm tauri build --debug` | 仓库根执行 |
| 版本升级 | `pnpm bump <x.y.z>` | 统一改 4 处版本号 + 刷新两个锁文件；完整发版流程（门禁 → commit → 确认后推 tag）见[发布技能](./.agents/skills/codewave-release/SKILL.md)与[发布指南](./docs/release.md) |
| 开 PR 前本地门禁 | `pnpm prepr` | **开 PR 或发版前必须完整执行**：锁定安装、Rust fmt/clippy/test、前端 lint/test/build、Node 脚本测试；硬步骤失败即停，clippy 是唯一软步骤且失败必须明示，见[完整门禁](./docs/testing.md#开-pr-前的完整门禁) |

- 改后端 → 在 `src-tauri/` 跑 `cargo test --workspace`；改前端 → `pnpm --dir ui test` + `pnpm --dir ui build`；跨层变更两边都要验证
- **开 PR 前必须本地跑完 CI 全部检查**，CI 是复核而非第一道防线。局部 `--only` 或 `--skip-soft` 不算完整门禁；硬步骤失败后未执行的步骤不能报作通过，退出 0 也不证明 clippy 无告警
- 报告命令、工作目录、平台、退出码及失败/跳过原因；区分实际通过、环境阻塞与未执行。新增 warning 当场处理，不沿用历史“全绿”或固定用例数作为保证
- 本地只覆盖当前平台；`cfg(unix)` 在 macOS/Linux 编译，Windows 分支需 Windows 验证。三平台 CI、签名/更新链路和 GUI 人工验收分别记录，不能相互替代
- 纯文档改动至少核对相对链接、章节锚点、失效引用和 `git diff --check`；涉及读取文档的测试也要回归。仅通过这些检查不能宣称完整前后端或 GUI 验收通过
- **单实例互斥**：`tauri dev` 与打包版同 bundle id 不能同时跑；GUI 验证前先确认没有 dev 实例在运行
- **界面改动不做 GUI 自动点验**：完成后交付分步手动验证清单，由用户手动验证

## 工程分层

后端 `src-tauri/src/`（分层规则见 [docs/technical-design](./docs/technical-design.md)）：

| 层 | 职责 | 约束 |
|---|---|---|
| `core/` | agent 编排主循环、config、context（token 统计/自动压缩）、prompt、projects（目录式项目注册表）、quota（订阅额度：数据源是 `config.providers`，不再读 opencode 配置与 auth.json；7 家适配器 OpenCodeGo / DeepSeek / MiniMaxIntl / MiniMaxCn / Kimi / Zhipu / Zai）、openers（文件管理器与编辑器探测/打开）、scheduler、sessions、stats | 不依赖 tauri |
| `host/` | commands（全部 IPC 命令）、events（EventSink）、keyring | 除 `lib.rs` 外唯一允许 `use tauri::*`；命令只校验 + 转调 core |
| `provider/` | LLM 供应商层：anthropic / openai_chat / openai_responses 三协议 + dto / keys / proxy / retry / sse | 协议差异不出本层 |
| `tools/` | 工具实现（ToolKind 五类：ReadOnly / FileWrite / Network / Interactive / Meta，`scheduled_task` 等操作 agent 自身状态的工具属 Meta）；`tools/document/` = Office 与 PDF 的读/生成/保真修改（[docs/documents](./docs/documents.md)） | 纯函数化 |
| `agents/` | 内置子代理角色注册表（explore/backend-dev/frontend-dev/app-dev/reviewer/product-manager/code-reviewer/tester/title 定义 + 别名归一查找），被 `tools/subagent` 注入消费；title 例外——仅供 `core/title.rs` 自动命名消费（[docs/sessions-and-projects](./docs/sessions-and-projects.md)），不进 subagent 角色枚举 | 纯数据不依赖 tauri；编排角色已内置为标准工作流常驻提示（[docs/plan-and-approval](./docs/plan-and-approval.md)），不是本表角色 |
| `core/agent/` | 编排主循环的现行机制：`runtime.rs` 的 **inject 通道**（运行中注入消息**不结束当前 run**，即 steer，[docs/runtime-and-scheduling](./docs/runtime-and-scheduling.md)）、`text_ask.rs`（模型把 ask 透传成正文 XML 时的文本兜底）、`supervise.rs`（错误信号监管：重复失败 / 空转看门狗的纠偏与终止） | 不依赖 tauri |
| `safety/` | approval 审批门（默认无限等待用户应答；`auto_confirm` 勾选后 5 分钟自动确认推荐选项，[docs/sessions-and-projects](./docs/sessions-and-projects.md)）、fence 命令安全围栏（L1 黑名单 → L2 AST → L3 高危模式） | |
| `git/` | git2 只读集成（status / diff / log） | 只读，不提供写操作 |
| `skills/` `mcp/` `memory.rs` | 技能扫描（项目 `<主目录>/.codewave/skills`[project_dir] > 全局 `~/.codewave/skills`[data_dir] > 工作区 `.agents/skills` `.claude/skills` > `~/.agents/skills` 与 `~/.claude/skills` 兼容 > 内置；托管目录技能可在设置页删除，兼容/内置来源不可删，[docs/mcp-and-skills](./docs/mcp-and-skills.md)；注意 rt.data_dir 恒为全局，项目数据目录走 rt.project_dir）、rmcp 客户端（stdio + streamable-http）、记忆系统 | |

前端 `ui/src/`：

- `ipc/client.ts` 是唯一 invoke 入口；`ipc/types.ts` 为前后端契约类型
- zustand 5 store：`run.ts`（每 Tab 运行态，immer 中间件做流式高频更新）、`activity.ts`（当前进程会话与子代理总览快照）、`sessions.ts`（Tab/项目/会话）、`settings.ts`、`ui.ts`、`tasks.ts`（计划任务列表单一数据源）、`updater.ts`（更新状态机）
- `features/`：chat / shell（ProjectNav 导航：临时会话区 + 项目→会话树 + 任务区；RightBar 常驻右边栏，4 页签 info / logs / files / changes，Git 已收进「变更」页签，数据目录与会话信息在设置页「关于」）/ panels（设置 10 页 3 组：外观与模型 / 安全与能力 / 诊断与其他；TasksPage 为覆盖式全屏页、TokenStatsModal 为统计弹框）/ files（文件面板与查看器、表格与 PDF 预览）/ tools（ask 面板、工具卡、组件预览）/ workspace（仅 ChangesPanel：工作区 vs HEAD 聚合 diff）/ subagent / quota
- 左侧导航是一切会话/项目入口；顶栏只有 Tab 条与功能按钮（无「选择工作区」「新 Tab」）

## 契约锚点（改前必读）

- **运行总览**：修改跨会话状态、待处理回跳或子代理总览时先读 [运行与调度](./docs/runtime-and-scheduling.md)；后端 `core/activity.rs` 提供独立于 Tab 的观察摘要，前端 `stores/activity.ts` 消费同一快照。询问回跳以真实 responder 与 run 身份校验，恢复后沿用原 AskPanel。
- **事件面 29 键**（`lsp:server_missing` 已随 LSP 机制删除，见 [docs/tools-and-safety](./docs/tools-and-safety.md)；`goal:update` 已随目标模式整体删除移除）：handler 键名定义于 `ui/src/stores/run.ts` 的 `bindGlobalHandlers()`（事件键对象在 `stores/runHandlers.ts`；events.ts 是通用 bind），前后端契约受 `events.contract.test.ts` 双向守护，勿改键名
- **文档工具与保真修改（[docs/documents](./docs/documents.md)）**：`read_document` / `write_document` / `edit_document` 三个工具按扩展名分派表格、Word、PDF；保真修改一律走「解开压缩包 → 只替换目标内部文件 → 其余原样搬运重打包」，**绝不整份解析重建**（重建会丢图表、数据透视表、迷你图）；`read` 拒收二进制文档（按扩展名 + NUL 字节内容探测）并点名该用哪个工具；界面预览复用 `read_document` 的实现（`read_for_preview`），不给预览单写一套解析；最低 Rust 版本 **1.98**
- **SessionMeta `project_id + roots` 快照**是 @ 提及 / git 聚合的唯一数据源，勿绕过回查注册表（左栏文件树已随 [docs/chat-and-interface](./docs/chat-and-interface.md) 移除）
- `create_session(project_id?, workspace?)` 双形态；`delete_project` 级联删除（先取消运行中会话）
- **anthropic SSE 流内绝不调 `parser.finish()`**（由分片撕裂集成测试守护）
- **config schema v2（[docs/providers-and-network](./docs/providers-and-network.md)）**：`providers: Vec<ProviderConfig>` 嵌套 `ProviderModel`（wire id 即显示名），`active_model_id` 仍指向模型条目 id；`ModelConfig` 只是运行时摊平形态（`ConfigState::find_model`，前端对应 `ui/src/utils/models.ts`），两端摊平语义需同步改；schema v1 扁平 models 迁移已随全新发布移除（[docs/technical-design](./docs/technical-design.md)），不再受理旧格式
- **SessionStore 索引写必须走 `index_lock`**
- 配置结构变更必须 serde default（新字段向前兼容；旧版本/旧数据迁移代码已随全新发布移除，[docs/technical-design](./docs/technical-design.md) 后不再新增）
- **批准门 = 四选项显式选档**（[计划与审批](./docs/plan-and-approval.md)）：ask 选项带 `mode: Option<ApprovalMode>`（serde default + `skip_serializing_if`，`None` 时 wire 上不出现该键）声明目标档位；批准类判定 = `mode.is_some()`（`id="approve"` 与 `switchToAutoEdit` 锚点保留为兼容兜底，有测试钉死）；切档目标取**用户所选选项的 `mode`**（可从逐项确认档/计划档一次跨到完全访问档）；第四项 `preview`（先看预览）**不带 mode**，语义是不批准也不驳回（渲染预览后重发同一询问）。**预览项必须在三处都被排除**（少一处即「前端切档、后端不切」的单边静默提权）：结构化通道（不带 mode → `selected_target_mode` 返 `None`；前端 `planPath` / `modePath` 另乘 `!previewOnly`）、宽松批准匹配（`approved_hit` 与前端同款匹配都要先剔除预览项 id——模型自拟 id 含 `approve` 时会误命中）、轻量切档（`valid_answer` / `switch` 叠加 `!preview_only_answer`）。**反面纪律**：`approval_shape` 是「无安全语义」的宽松渲染标记，**不得**收紧成剔除预览项（`preview_option_ids` 以它为前置，收紧反而会打开 `valid_answer` 通道静默抬档）；`plan_approval_gate`（G2 分析产物 / G3 todos 基线）不变；`ask:opened` 只增字段、事件键名不动
- **子代理档位按 step 边界跟随根会话**（[docs/plan-and-approval](./docs/plan-and-approval.md)）：主会话档位变化在每个 LLM step 边界传播到在跑子代理（工具集 + `<plan-mode>` 系统块 + `sub_rt.prefs` 三处从**基座（`SubBase`）重建而非增量追加**——增量追加会让旧 `<plan-mode>` 块永久残留）；`model_id` / `reasoning_effort` 仍为 spawn 快照；嵌套子代理跟随根会话、父会话查不到时保持原档位不 panic；计划任务 runtime 独立且显式 FullAccess
- **Composer 工具条数据面**（[docs/chat-and-interface](./docs/chat-and-interface.md)）：`Frame::Usage` 的载荷可**增可选字段**（serde default；事件键名 `usage` 不变，`events.contract.test.ts` 只守键名）；工具条三段的数据源分工是——上下文/命中段读**会话级** `TabRunState.usage`（跨 run 累加、不持久化），速率段读**本轮** `TabRunState.runMetrics`（`send()` 处归零）；统计面板总览的耗时三项必须**分子分母同域**（只取 `gen_ms > 0` 的 `by_kind` 桶），否则子代理/压缩的 output 会把速率抬到数倍
- **Composer 触发符以光标为锚**（[docs/chat-and-interface](./docs/chat-and-interface.md)）：`/`、`$`、`@` 的判定与回填一律基于**光标前的片段**（`ui/src/features/chat/composerTriggers.ts` + `Composer` 的 `caretRef`），**勿再引入锚定整段末尾的正则**（`^/(\S*)$`、`@[^@\s]*$` 这类写法正是「正文里已有内容时拉不起菜单」的根因）；`/`、`$` 限「消息以它开头」是与模型侧点名契约绑定的（`src-tauri/src/skills/mod.rs` 的 `prompt_listing` 与 `core/prompt.rs` 的 `CORE_PROMPT` 都写死「以 … 开头」），要放宽必须同步改提示词
- **会话恢复的完整出参备份**（[docs/sessions-and-projects](./docs/sessions-and-projects.md)）：历史里只有**模型侧瘦身文本**，因此「解析不了」的出参（头尾截断、失败 `[error …]` 前缀）会由批次层额外落一份**整套 `ToolOutcome`** 到 `~/.codewave/sessions/<会话>.toolres/<call_id>.json`，恢复时经 IPC `load_tool_outcomes` 批量回填工具卡。键必须是 **provider 侧 `tool_use.id`**——实时 `ctx.call_key` 是批内随机 `batch:index`、不可持久，而前端恢复时的 `callKey` 正是前者；它**永不参与出网**（不改 `rt.history`、不改工具出参 JSON、不增计费），随会话级联删除，且**不进**右栏「文件」面板（那里只读产物登记边车）。前端**三个消费方共用一套回填**（判据 `lossyToolKeys` 与 `restoredToolData` 同源 → `backfillToolOutcomes`）：主会话工具卡、子代理卡（合成 key `restored:<callKey>` 在回填后**改名**成真实 `sub_id`，抽屉据此才能拉到真实过程流、`report` 同时补全）、以及 `openSubDrawer` 拉回的子代理过程流内工具卡

- **会话历史落盘：分段 append-only JSONL**（[会话与项目](./docs/sessions-and-projects.md)）：落盘是 `histories/<id>/0001.jsonl…`（明文不压缩、每个段首行是 header、段尾可能有一条 seal；水位边车 `.segmeta.json`），**不再 trim、也没有 8MB 上限**——落盘保留完整转录（压缩只记 `compaction` 边界，`wire` 由边界重建并守 256k；`display` 全量不裁，前端按段分页）。**任何 fallback（前缀哈希不匹配 → 基线段）只新增段，绝不覆盖/截断/删除既有段**——这是「不丢历史」的底线。段封口 = 条数 200 或 512KB 先触者，且**绝不切在 tool_use/tool_result 配对中间**；加载丢弃尾部残行、坏行跳过、坏段跳过并提示，**绝不因容错报「历史损坏」**。体积靠 `HISTORY_SOFT_WARN_BYTES`(200MB) / `HISTORY_HARD_FUSE_BYTES`(1GB)：软线照常写只提示，硬线**只停写、绝不删数据**且可自愈；状态仍挂 `SessionMeta.history_status` + `run:done.history_save`，且**新状态必须同步纳入 `SaveReport::is_clean()`**（`drive.rs` 的上报判据是 `filter(|r| !r.is_clean())`，漏改则提示永远发不出去）。**wire 装载必须有界**（从尾部读到够用即停，与「全量 + repair + trim」逐字节对拍），打开会话不得随历史总量线性增长。旧 `.json.gz` 读兼容不迁移，且**设置页清理入口的判据必须与读路径同源**（`segments::has_readable_messages`）——**无新格式可读数据时绝不删旧文件**（那是唯一副本）。**图片外置**：落盘走独立 DTO `PersistedMessage`/`PersistedContent`（`ImageInline`/`ImageBlob` 两态），blob 存 `sessions/<owner>.imgblob/<sha256 前 32 hex>`（内容寻址去重），`load_history` 读回 base64 后才交上层——**内存与 wire 零改动**（勿改 `core/types.rs` 的 `Content`，那会撞三协议字节断言与 match 穷尽点）。保存路径必须持 `save_lock`（锁序 `save_lock → index_lock`）——GC 用的是内存快照，不串行化会让并发保存互删对方仍引用的图；**GC 的引用集合是「磁盘上所有保留段引用的 blob 并集」**；子代理 blob owner 是 `<父>__<sub>`（子 id 只有 32 bit，裸用会跨会话撞目录）

## 踩坑清单（勿再踩）

- setup 同步上下文不能 `tokio::spawn` → 用 `tauri::async_runtime::spawn` 或惰性 `OnceLock`
- tauri-plugin-updater 必须有 `plugins.updater` 配置节点（active: false 也行），否则启动 panic
- antd 两字按钮会插空格（“保 存”）：测试按文本找按钮先去空白再 includes
- zustand store 是模块级单例：测试间面板开关残留 → afterEach 重置 settingsOpen / diffOpen / tasksOpen / statsOpen
- antd `<App>` 包裹层默认 height auto 会令全屏布局塌缩（`.ant-app{height:100%}`）
- happy-dom 需垫 matchMedia / ResizeObserver / `crypto.randomUUID`
- WKWebView 合成键盘输入中文不生效（自动化验证走剪贴板粘贴）
- `Arc<SessionRuntime>` 新建后改字段必须 `Arc::get_mut`（Arc 不支持 DerefMut）
- 含 `${}` 的 TS 模板串不要用 python/heredoc 脚本打补丁（转义必错），用 Read+Edit 工具
- antd Dropdown 菜单测试：按文本查 `.ant-dropdown-menu-item` 后 fireEvent.click
- antd `TextArea` 的 `autoSize` 会在 DOM 里另放一个测量用 textarea：测试里用 `getByPlaceholderText` 定位真身，别用 `querySelector("textarea")`（拿到替身后 fireEvent.change 静默无效）
- 草稿里有些内容**不在正文文本里**（文件引用 chip 存 `refs`、图片存 `images`）：凡「覆盖草稿」的链路（`ws:composer-fill`、队列编辑、历史召回）必须连带覆盖它们，只 `setText` 盖不住
- 触发菜单/回填要看**真实光标**：`onChange` 只给文本与 `selectionStart`，点击与方向键移动光标**不过** `onChange`（靠 `onSelect/onClick/onKeyUp` 补同步）；测试里要指定光标位置时给 `fireEvent.change` 传 `target.selectionStart`（不传就是文末，happy-dom 与浏览器一致）
- 测试里的 mock server 别「读完一次就 `drop(sock)`」：临时端口是共享资源——① 带未读数据 close 在 Windows 上会发 RST（客户端的「半截响应」因此忽 RST 忽 EOF）；② 任务结束后 listener 释放的临时端口可能被**并发用例**的 listener 抢到，客户端于是拿到别的用例的响应（表现为毫不相干的错误变体）。做法：listener 用 `Arc` 持有并活到用例结束 + 读干请求头（到 `\r\n\r\n`）+ `shutdown()` 写半部优雅收尾。`provider::tests_integration::midstream_disconnect_maps_to_network` 的间歇性 `got Server("")` 即此二因
- 写入后检查的结论必须写进 `outcome.data`（create 用 `check`，edit 用 `checks`）（`compact_for_model` **只序列化 `data`**），写进 `warnings` 模型一个字都收不到，详见[工具与安全](./docs/tools-and-safety.md)

## 可用专门代理

| 代理 | 触发场景 |
|---|---|
| **product-manager** | 需求分析、PRD、用户故事、优先级排序 |
| **code-reviewer** | 代码审查（正确性 / 安全 / 性能 / 可维护性 / 可读性 / 测试覆盖 / 最佳实践） |
| **tester** | 测试用例设计、测试策略、缺陷分析、回归要点 |

代理详细定义见 `.agents/agents/<name>.md`。plan 档的阶段化流程（P0 分类 → 澄清 → P2 分析 → P3 方案 → 批准 → P4 执行 → P5 审查 → P6 汇报，含轻量路径与缺陷路由）见 [docs/plan-and-approval.md](./docs/plan-and-approval.md)；按场景调用的要点：

### 1. 需求流程（用户提新需求时）

1. 调用 product-manager 分析需求，整理为结构化需求分析（用户故事 / AC / 边界 / 非目标 / 开放问题）
2. 基于需求分析编写技术方案（git 仓库内拟定分支名 `<type>/<slug>`），登记 plan todos（含验证项）
3. 方案经用户批准（ask，询问列明分支名；批准 = 预授权建分支）后实施；有效契约与验证要点更新到 `docs/` 对应长期主题，执行计划和一次性报告不再单独立篇

### 2. 缺陷流程（用户报 bug 时）

1. plan 档先做 P0 分类（附依据）；分类为缺陷则调用 tester 复现问题、分析根因，给出修复方案与回归测试要点
2. 批准后先用计划分支开发（批准 = 预授权建分支），落地修改并跑通对应测试（`cargo test` / `pnpm --dir ui test`）

### 3. 代码审查流程（开发完成后）

1. 多文件 / 跨层变更完成后强制调用 code-reviewer（轻量路径不强制），按 7 个维度审查：正确性 / 安全 / 性能 / 可维护性 / 可读性 / 测试覆盖 / 最佳实践
2. 严重问题（🔴）必须修复；长期约束与仍未完成的验证事项更新到 `docs/` 对应主题，不为已完成审查新增报告

## Git 约定

- **AI 默认禁止一切 git 写操作**（commit / push / merge / branch / stash 等），git 完全由用户处理；**在用户明确要求或授权时可执行**（范围以当次授权为准）。例外：用户批准计划 = 预授权创建并切换计划所示分支（[docs/plan-and-approval.md](./docs/plan-and-approval.md)）；commit / push / merge 仍由用户执行
- 用户需要起草提交信息时，按 Conventional Commits 给出文本：`<type>(<scope>): <subject>`，type ∈ `feat` · `fix` · `docs` · `refactor` · `test` · `chore`
- 例：`feat(projects): persist named projects to ~/.codewave/projects.json`

## 术语约定

- commit / rebase / merge / provider / prompt / BYOK / SSE / MCP 等技术词保留英文
- 中文用于叙述与判断
- 引用外部资料必须带 URL
