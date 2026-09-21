# CodeWave · AGENTS.md

> 项目入口指南。CodeWave 是 local-first 桌面 AI 编程 Agent：Tauri 2 + 纯 Rust 后端，React 19 + antd 6 前端。
> 核心概念：**项目 = 单个主目录**——项目数据存于主目录下 `.codewave/`；临时会话免目录直接开聊（[docs/session-semantics-and-ui-batch](./docs/session-semantics-and-ui-batch.md)）。

## 必读文档

docs/ 的完整目录与阅读路径见 [docs/0-README.md](./docs/0-README.md)——技术基准、各批次实施报告、契约来龙去脉均在其中。要点入口：

- 技术方案基准：[docs/technical-design](./docs/technical-design.md)（决策记录 D1–D7、分层规则）
- plan 档阶段化流程：[docs/plan-mode-workflow](./docs/plan-mode-workflow.md)；标准工作流（完整研发流水线，原 arch 技能内置化）：[docs/standard-workflow](./docs/standard-workflow.md)；arch 技能历史契约：[docs/arch-orchestrator](./docs/arch-orchestrator.md)
- 本文件「契约锚点」一节列出改前必读的关键契约

docs/ 目录约定：平铺结构，**文档文件名不带编号**（用英文主题 slug）；新增文档后在 [docs/0-README.md](./docs/0-README.md) 登记日期与条目；文中引用文档一律用 markdown 链接。

## 核心约束

- **git 写操作默认由用户执行**：commit / push / merge / branch / stash 等 AI 不主动碰；用户明确要求或授权时可执行（范围以当次授权为准）。例外：用户批准计划 = 预授权创建并切换计划所示分支（标准工作流 S5/S6、plan 档 P3，[docs/plan-branch-proposal.md](./docs/plan-branch-proposal.md)）；commit / push / merge 仍由用户执行
- **本地优先**：数据、配置、会话均在本地 `~/.codewave`；API key 入系统钥匙串，不落明文
- **纯 Rust 后端，零框架耦合**：`core/` 不依赖 tauri；只有 `lib.rs` 与 `host/` 允许 `use tauri::*`；IPC 命令只做校验 + 转调 core
- **数据目录 `.codewave`**：路径约定随 2026-09-13 品牌改名定稿（原 `.wavestudio`，无自动迁移），勿再建议改名
- **项目 = 名称 + 单一主目录**；托管数据（project.json + temps/logs/memory/skills/tasks + mcp.json/lessons.md）存于 `<主目录>/.codewave/`；data_dir 随保存归一化持久化（[docs/oss-prep-batch](./docs/oss-prep-batch.md)：legacy 目录回退已移除）；`ProjectEntry.allowed_dirs`（serde default）记用户「始终允许」的项目外目录，会话创建/恢复时并入 `extra_roots`
- **文件入口统一为「原地引用」**：附件按钮与拖入窗口拿到的都是**真实路径**，非图片文件记进草稿 `refs` 并以 **chip** 展示（输入框里不出现路径），发送前一刻才合成 `@路径` 追加正文末尾（不复制副本、不占上下文；[docs/composer-file-ref-chips](./docs/composer-file-ref-chips.md)）；图片因要上 wire 才读成 base64；网页内 `<input type=file>` 拿不到路径，故附件按钮走原生选择框（[docs/office-and-pdf-support](./docs/office-and-pdf-support.md)）
- **会话语义**：项目会话（必须选项目，快照固化主目录）或临时会话（免目录，工作区 = 全局数据目录，写入仍走 fence 审批）；命令 cwd = 项目数据目录下的 temps/（临时会话 = 全局数据目录）
- **删除项目** = 级联删除其下会话 + 托管目录（确认弹框列明影响）；用户代码目录永不动
- **界面风格**：自绘标题栏（[docs/custom-font-and-titlebar](./docs/custom-font-and-titlebar.md)：tauri-plugin-decoration v3，顶栏即标题栏；窗口 `visible:false` 起动，激活失败回退原生框）；主题跟随系统；**强调色 = 中性墨色**（`colorPrimary` 亮 `#1f1f1f`/暗 `#424242`，[docs/ask-ink-accent-and-composer-cover](./docs/ask-ink-accent-and-composer-cover.md)——勿再引入默认蓝或彩色 accent；色彩强度映射风险等级：无彩色=默认、橙=需注意、红=危险）；字体走 `--ws-font-sans/--ws-font-mono` 双槽 token（用户偏好由 `ui/src/utils/fonts.ts` 管理，勿再硬编码字体链）；组件样式一律 antd 组件库接管（`--ws-*` token 由 `ui/src/theme/bridge.tsx` 桥接），不手搓皮肤/自绘调色板

## 常用命令

> Tauri CLI 走 pnpm（本机无 cargo-tauri），且必须在仓库根执行。

| 用途 | 命令 | 说明 |
|---|---|---|
| 后端测试 | `cargo test` | 在 `src-tauri/` 执行；基线全绿 / 0 warning（本地实测 **941 passed / 3 ignored**；LSP 机制删除后 `tests/` 集成测试目录为空；个别 `cfg(unix)` 用例仅 macOS 执行；以本地最新全绿为准）；CI 用 `cargo test --workspace` |
| 前端测试 | `pnpm --dir ui test` | 基线全绿（本地实测 **800 passed / 74 文件**，以本地最新全绿为准；antd 已升 6.6，Tabs 用 tabPlacement/start） |
| 前端构建 | `pnpm --dir ui build` | type check + vite build |
| 开发调试 | `pnpm tauri dev` | 仓库根执行 |
| 打包 | `pnpm tauri build --debug` | 仓库根执行 |
| 版本升级 | `pnpm bump <x.y.z>` | 统一改 4 处版本号 + 刷新两个锁文件；完整发版流程（门禁 → commit → 确认后推 tag）见 `.agents/skills/codewave-release/SKILL.md` 与 [docs/version-bump-and-release](./docs/version-bump-and-release.md) |
| 开 PR 前本地门禁 | `pnpm prepr` | **开 PR 前必须跑完**：逐条覆盖 CI 的两个 workflow（`cargo fmt --check` / `cargo clippy`（软）/ `cargo test --workspace` / `pnpm --dir ui run lint` / `ui test` / `ui build` / `node --test scripts/**` / `pnpm install --frozen-lockfile`）；硬步骤失败即停并给出失败清单，见 [docs/pre-pr-local-gate](./docs/pre-pr-local-gate.md) |
- 改后端 → `cargo test`；改前端 → `pnpm --dir ui test` + `build`；两边都动 → 两者都要过才算完成
- **开 PR 前必须本地跑完 CI 全部检查**（`pnpm prepr`：fmt / clippy / eslint / 前后端测试 / 构建 / 脚本测试 / 锁定文件）——CI 只是复核，不是第一道防线；lint 与格式类问题绝不该由 CI 首次发现。本地只覆盖当前平台，三平台矩阵差异仍以 CI 为准
- **单实例互斥**：`tauri dev` 与打包版同 bundle id 不能同时跑；GUI 验证前先确认没有 dev 实例在运行
- **界面改动不做 GUI 自动点验**：完成后交付分步手动验证清单，由用户手动验证

## 工程分层

后端 `src-tauri/src/`（分层规则见 [docs/technical-design](./docs/technical-design.md)）：

| 层 | 职责 | 约束 |
|---|---|---|
| `core/` | agent 编排主循环、config、context（token 统计/自动压缩）、prompt、projects（目录式项目注册表）、quota（订阅额度：凭证链 + 7 家适配器）、openers（文件管理器与编辑器探测/打开）、scheduler、sessions、stats | 不依赖 tauri |
| `host/` | commands（全部 IPC 命令）、events（EventSink）、keyring | 除 `lib.rs` 外唯一允许 `use tauri::*`；命令只校验 + 转调 core |
| `provider/` | LLM 供应商层：anthropic / openai_chat / openai_responses 三协议 + dto / keys / proxy / retry / sse | 协议差异不出本层 |
| `tools/` | 工具实现（ToolKind 分 ReadOnly / FileWrite / Network / Interactive）；`tools/document/` = Office 与 PDF 的读/生成/保真修改（[docs/office-and-pdf-support](./docs/office-and-pdf-support.md)） | 纯函数化 |
| `agents/` | 内置子代理角色注册表（explore/backend-dev/frontend-dev/app-dev/reviewer/product-manager/code-reviewer/tester/title 定义 + 别名归一查找），被 `tools/subagent` 注入消费；title 例外——仅供 `core/title.rs` 自动命名消费（[docs/session-auto-title](./docs/session-auto-title.md)），不进 subagent 角色枚举 | 纯数据不依赖 tauri；编排角色已内置为标准工作流常驻提示（[docs/standard-workflow](./docs/standard-workflow.md)），不是本表角色 |
| `safety/` | approval 审批门（默认无限等待用户应答；`auto_confirm` 勾选后 5 分钟自动确认推荐选项，[docs/session-nav-row-states](./docs/session-nav-row-states.md)）、fence 命令安全围栏（L1 黑名单 → L2 AST → L3 高危模式） | |
| `git/` | git2 只读集成（status / diff / log） | 只读，不提供写操作 |
| `skills/` `mcp/` `memory.rs` | 技能扫描（项目 `<主目录>/.codewave/skills`[project_dir] > 全局 `~/.codewave/skills`[data_dir] > 工作区 `.agents/skills` `.claude/skills` > `~/.agents/skills` 与 `~/.claude/skills` 兼容 > 内置；托管目录技能可在设置页删除，兼容/内置来源不可删，[docs/slash-skills-and-dollar-agents](./docs/slash-skills-and-dollar-agents.md)；注意 rt.data_dir 恒为全局，项目数据目录走 rt.project_dir）、rmcp 客户端（stdio + streamable-http）、记忆系统 | |

前端 `ui/src/`：

- `ipc/client.ts` 是唯一 invoke 入口；`ipc/types.ts` 为前后端契约类型
- zustand 4 store：`run.ts`（每 Tab 运行态，immer 中间件做流式高频更新）、`sessions.ts`（Tab/项目/会话）、`settings.ts`、`ui.ts`
- `features/`：chat / shell（ProjectNav 导航：临时会话区 + 项目→会话树 + 任务区；RightBar 常驻右边栏：Git/模型/项目目录/会话信息）/ panels（设置/任务/统计）/ tools / workspace / subagent
- 左侧导航是一切会话/项目入口；顶栏只有 Tab 条与功能按钮（无「选择工作区」「新 Tab」）

## 契约锚点（改前必读）

- **事件面 29 键**：handler 键名定义于 `ui/src/stores/run.ts` 的 `bindGlobalHandlers()`（事件键对象在 `stores/runHandlers.ts`；events.ts 是通用 bind），前后端契约受 `events.contract.test.ts` 双向守护，勿改键名（`lsp:server_missing` 已随 LSP 机制删除，见 [docs/post-write-check-plan](./docs/post-write-check-plan.md)）
- **文档工具与保真修改（[docs/office-and-pdf-support](./docs/office-and-pdf-support.md)）**：`read_document` / `write_document` / `edit_document` 三个工具按扩展名分派表格、Word、PDF；保真修改一律走「解开压缩包 → 只替换目标内部文件 → 其余原样搬运重打包」，**绝不整份解析重建**（重建会丢图表、数据透视表、迷你图）；`read` 拒收二进制文档（按扩展名 + NUL 字节内容探测）并点名该用哪个工具；界面预览复用 `read_document` 的实现（`read_for_preview`），不给预览单写一套解析；最低 Rust 版本 **1.98**
- **SessionMeta `project_id + roots` 快照**是 @ 提及 / git 聚合的唯一数据源，勿绕过回查注册表（左栏文件树已随 [docs/workspace-explorer-removal-and-chat-scrollbar](./docs/workspace-explorer-removal-and-chat-scrollbar.md) 移除）
- `create_session(project_id?, workspace?)` 双形态；`delete_project` 级联删除（先取消运行中会话）
- **anthropic SSE 流内绝不调 `parser.finish()`**（由分片撕裂集成测试守护）
- **config schema v2（[docs/provider-management-refactor](./docs/provider-management-refactor.md)）**：`providers: Vec<ProviderConfig>` 嵌套 `ProviderModel`（wire id 即显示名），`active_model_id` 仍指向模型条目 id；`ModelConfig` 只是运行时摊平形态（`ConfigState::find_model`，前端对应 `ui/src/utils/models.ts`），两端摊平语义需同步改；schema v1 扁平 models 迁移已随全新发布移除（[docs/oss-prep-batch](./docs/oss-prep-batch.md)），不再受理旧格式
- **SessionStore 索引写必须走 `index_lock`**
- 配置结构变更必须 serde default（新字段向前兼容；旧版本/旧数据迁移代码已随全新发布移除，[docs/oss-prep-batch](./docs/oss-prep-batch.md) 后不再新增）
- **Composer 触发符以光标为锚**（[docs/composer-trigger-caret](./docs/composer-trigger-caret.md)）：`/`、`$`、`@` 的判定与回填一律基于**光标前的片段**（`ui/src/features/chat/composerTriggers.ts` + `Composer` 的 `caretRef`），**勿再引入锚定整段末尾的正则**（`^/(\S*)$`、`@[^@\s]*$` 这类写法正是「正文里已有内容时拉不起菜单」的根因）；`/`、`$` 限「消息以它开头」是与模型侧点名契约绑定的（`src-tauri/src/skills/mod.rs` 的 `prompt_listing` 与 `core/prompt.rs` 的 `CORE_PROMPT` 都写死「以 … 开头」），要放宽必须同步改提示词

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
- 测试里的 mock server 别「读完一次就 `drop(sock)`」：临时端口是共享资源——① 带未读数据 close 在 Windows 上会发 RST（客户端的「半截响应」因此忽 RST 忽 EOF）；② 任务结束后 listener 释放的临时端口可能被**并发用例**的 listener 抢到，客户端于是拿到别的用例的响应（表现为毫不相干的错误变体）。做法：listener 用 `Arc` 持有并活到用例结束 + 读干请求头（到 `\r\n\r\n`）+ `shutdown()` 写半部优雅收尾。2026-09-21 修 `provider::tests_integration::midstream_disconnect_maps_to_network` 的间歇性 `got Server("")` 即此二因（当时 20 次跑 4 次红，修后 30 次模块连跑 + 8 次全量跑 0 红）

## 可用专门代理

| 代理 | 触发场景 |
|---|---|
| **product-manager** | 需求分析、PRD、用户故事、优先级排序 |
| **code-reviewer** | 代码审查（正确性 / 安全 / 性能 / 可维护性 / 可读性 / 测试覆盖 / 最佳实践） |
| **tester** | 测试用例设计、测试策略、缺陷分析、回归要点 |

代理详细定义见 `.agents/agents/<name>.md`。plan 档的阶段化流程（P0 分类 → 澄清 → P2 分析 → P3 方案 → 批准 → P4 执行 → P5 审查 → P6 汇报，含轻量路径与缺陷路由）见 [docs/plan-mode-workflow.md](./docs/plan-mode-workflow.md)；按场景调用的要点：

### 1. 需求流程（用户提新需求时）

1. 调用 product-manager 分析需求，整理为结构化需求分析（用户故事 / AC / 边界 / 非目标 / 开放问题）
2. 基于需求分析编写技术方案（git 仓库内拟定分支名 `<type>/<slug>`），登记 plan todos（含验证项）
3. 方案经用户批准（ask，询问列明分支名；批准 = 预授权建分支）后实施；实施计划与报告写入 `docs/` 新主题 slug 文档

### 2. 缺陷流程（用户报 bug 时）

1. plan 档先做 P0 分类（附依据）；分类为缺陷则调用 tester 复现问题、分析根因，给出修复方案与回归测试要点
2. 批准后先用计划分支开发（批准 = 预授权建分支），落地修改并跑通对应测试（`cargo test` / `pnpm --dir ui test`）

### 3. 代码审查流程（开发完成后）

1. 多文件 / 跨层变更完成后强制调用 code-reviewer（轻量路径不强制），按 7 个维度审查：正确性 / 安全 / 性能 / 可维护性 / 可读性 / 测试覆盖 / 最佳实践
2. 严重问题（🔴）必须修复；审查结论可写入 `docs/` 新主题 slug 文档

## Git 约定

- **AI 默认禁止一切 git 写操作**（commit / push / merge / branch / stash 等），git 完全由用户处理；**在用户明确要求或授权时可执行**（范围以当次授权为准）。例外：用户批准计划 = 预授权创建并切换计划所示分支（[docs/plan-branch-proposal.md](./docs/plan-branch-proposal.md)）；commit / push / merge 仍由用户执行
- 用户需要起草提交信息时，按 Conventional Commits 给出文本：`<type>(<scope>): <subject>`，type ∈ `feat` · `fix` · `docs` · `refactor` · `test` · `chore`
- 例：`feat(projects): persist named projects to ~/.codewave/projects.json`

## 术语约定

- commit / rebase / merge / provider / prompt / BYOK / SSE / MCP 等技术词保留英文
- 中文用于叙述与判断
- 引用外部资料必须带 URL
