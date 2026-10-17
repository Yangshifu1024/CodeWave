# CodeWave 架构与技术决策

本文是跨模块架构入口，保留原始决策和长期不变量。具体行为集中在各主题文档；依赖版本、全量工具/命令列表与测试结果以源码和当次执行为准。

## 0. 决策记录

D1–D8 是历史记录，原文保留。后续实现差异在正文说明，不倒改当时结论。

| # | 决策点 | 结论 | 日期 |
|---|---|---|---|
| D1 | 桌面框架 | **Tauri 2**（tauri 2.11.x，已稳定） | 2026-08-29 |
| D2 | 后端路线 | **路线 A：纯 Rust 核心**（tokio 异步运行时，单进程，无 sidecar） | 2026-08-30 |
| D3 | 前端栈 | **Vue 3 + Naive UI + Pinia**（状态用 Pinia 分层） | 2026-08-30 |
| D4 | 命令安全策略 | **静态围栏 + 危险命令确认弹窗**（弹窗默认开启，设置可关） | 2026-08-30 |
| D5 | Git 集成 | **git2-rs**（rust-lang/git2-rs，libgit2 绑定）只读集成；P0 前移构建验证，功能 P1 | 2026-08-30 |
| D6 | 包标识与品牌 | bundle id `xyz.yangshifu.codewave`；数据目录 `~/.codewave/`；keyring 服务名 `codewave.yangshifu.xyz` | 2026-09-13 |
| D7 | 发布形态 | **私有项目起步**：不做签名/更新服务器，`tauri build` 产物自用；协议暂不设 | 2026-08-30 |
| D8 | 应用名 | **CodeWave** | 2026-08-29 |

## 1. 现行架构与边界

CodeWave 是 Tauri 桌面 AI 编程 Agent：纯 Rust 核心运行在主进程，React + antd + zustand 负责 WebView 界面。没有独立 Agent sidecar；shell、后台服务和 MCP 可以按需启动子进程。

D3 的 Vue/Naive UI/Pinia 与 D7 的私有分发设想是历史决策，当前已演进为 React 栈、开源仓库和签名/自动更新发布链路，不能据旧表推断现行实现。SSH、headless server 与移动端曾被评估，但未由这套桌面架构自动获得支持。

| 层 | 源码入口 | 职责 |
|---|---|---|
| 装配与宿主 | [lib.rs](../src-tauri/src/lib.rs)、[host/](../src-tauri/src/host/) | Tauri 启动、插件/命令注册、事件投递、钥匙串和系统通知 |
| 核心 | [core/](../src-tauri/src/core/) | 编排、上下文、配置、项目、会话、调度、统计与运行观察 |
| 协议 | [provider/](../src-tauri/src/provider/) | Anthropic Messages、OpenAI Chat/Responses、SSE、请求转换、key 池与重试 |
| 工具与安全 | [tools/](../src-tauri/src/tools/)、[safety/](../src-tauri/src/safety/) | 工具注册/执行、批次调度、路径/命令约束与审批 |
| 扩展 | [mcp/](../src-tauri/src/mcp/)、[skills/](../src-tauri/src/skills/)、[agents/](../src-tauri/src/agents/) | 外部工具、技能索引与内置子代理角色 |
| 存储与辅助 | [core/sessions/](../src-tauri/src/core/sessions/)、[git/](../src-tauri/src/git/)、[memory.rs](../src-tauri/src/memory.rs)、[util/](../src-tauri/src/util/) | 转录与元数据、只读 Git、记忆、原子写与公共算法 |
| 交付与编排 | [core/delivery/](../src-tauri/src/core/delivery/)、[core/workflow/](../src-tauri/src/core/workflow/) | 验收证据与边车、任务分类 / 并行准入 / 状态机 |
| 界面 | [ui/src/](../ui/src/) | IPC 封装、运行状态投影、聊天/工具卡、导航/设置与文件预览 |

### 分层约束

- Rust 的 Tauri API 边界限定在 `lib.rs` 和 `host/`；core/provider/tools/safety 不引入 Tauri 类型。`EventSink` 隔离事件投递，不把 `AppHandle` 传进算法或工具。
- **凭据由宿主统一注入**：`HostServices.credentials: Arc<dyn CredentialStore>` 经 `AgentCore::new_with_credentials` 进入主循环、压缩、标题、额度与配置提交器；解析逻辑在 [core/credentials.rs](../src-tauri/src/core/credentials.rs)，系统钥匙串实现留在 `host/keyring.rs`。core 不再为这些消费者直接调用系统钥匙串，测试与嵌入宿主使用同一 trait 注入。
- IPC 保持薄校验/转调，业务判断留在核心和工具层；前端 invoke 唯一入口是 [ipc/client.ts](../ui/src/ipc/client.ts)，类型在 [ipc/types.ts](../ui/src/ipc/types.ts)。
- 手写协议适配与 IPC 类型降低生成器耦合，但兼容性靠双端同步和契约测试保证；Provider wire 差异不能外泄成工具/UI 对某一家协议的依赖。
- 主会话、子代理、计划任务共用参数化驱动器，不能复制主循环分别修复；测试与重构纪律见 [development](./development.md)、[testing](./testing.md)。
- **交付域与编排域分工是硬的**：`core/delivery` 判定"凭什么算完成"，`core/workflow` 决定"怎么拆、谁来做、按什么顺序"；前者不调度单元，后者不判定验收。
- **工作流只有一个生产组合入口**：[workflow/service.rs](../src-tauri/src/core/workflow/service.rs) 在任务契约发布前组合 route、设计裁决、验证模板及冻结定义，追加必需检查与需求映射；非机械写任务由宿主强制独立审查，模型不能自称豁免。模板 cwd 记录为显式 PathRef，shell_hint 只表示解释器；文档链接/锚点与 GUI 保留人工未验证项，不以 diffcheck/lint 冒充。
- **工作流确认不切权限档**：[tools/workflow.rs](../src-tauri/src/tools/workflow.rs) 的配置、分类覆盖、设计选择和工作包采用走独立普通询问，应答只确认展示内容，不触碰 G2/G3 或 SessionPrefs。项目配置保存持跨进程租约完成读/CAS/原子替换，只对用户确认的确切序列化内容签发全局 workflow_trust 摘要；外部修改后必须重新审核，保存不是执行许可。
- **规则快照与运行状态分开**：`sessions/<session>.workflow/<task>.r<revision>.json` 保留定义全文、定义摘要、项目资源摘要、范围版本和契约修订；工作包计划、设计与节点状态按同一契约修订分侧文件，旧版不覆盖。状态写入持事务租约；节点校验 root run、执行 actor、attempt、generation、依赖和合法转换，pending 不能直接 passed。模型/角色被删阻塞，权限不在冻结快照内，恢复仍按当前授权。
- **工作包与阶段图均参与执行**：`workflow plan_units` 创建受控工作包，`subagent unitId` 读取冻结包后准入；阶段正常边形成依赖，只有显式 repair 边允许预算内重试。节点通过来自宿主真实验收，整体完成仍由 delivery gate 判定，不能反向以自然语言报告覆盖证据。范围变更后创建新修订设计和快照，旧结论不自动继承。

- `Coordinator` trait 定义跨进程协调契约。**C0 已有真跨进程实现**：[core/process_coordination/](../src-tauri/src/core/process_coordination/) 用 `create_new`（O_CREAT|O_EXCL，跨平台原子）落租约、代次由 test-and-set 锁串行分配（`read → +1 → write` 有窗口，会让两个持有者拿到同一 generation，"释放校验代次"当场失效），跨文件事务走「暂存 → 持久化意向日志 → rename → 删日志」并在恢复时**向前滚动**。崩溃 / PID 复用 / 删除中断一律**保守 busy**：按 PID 判活自动回收会在 PID 复用时抢走别人的租约，回收只能由 guardian 在确认持有者退出后显式发起。`InProcessCoordinator` 退为**隔离夹具专用**，不得用于真实共享目录。锁序固定为 `短锁 → owner try`，保存路径 `owner → save_lock → 磁盘事务 → index_lock`，不持锁等模型 / 用户 / 网络。
- **会话 owner 和物理根互斥**（#36-C0）：唯一入口 `AgentCore::start_chat_expected` 在同锁检查期望 run 与占位后认领 `history:<id>`，再认领 canonical 主目录下 `.codewave/coord` 的根租约；配置 profile 不同也不能同时操作同一根。历史保存先校验 owner，再取 `save_lock`；索引 RMW 另持 OS 文件短锁与 `index_lock`。`close`/`delete` 必须确认运行结束和 `cleanup_done`，未知清理保留 owner，迟到终态只可释放同 run/同代次租约。未认领的空闲历史可加载，但发现其他 owner 时拒绝写入；项目删除继续保留用户代码目录。
- **config / MCP / stats 三个域各自接了跨进程事务**（#36-C0），但**机制不同，别混用**：
  - **config** 走 [`ConfigCoordinator`](../src-tauri/src/core/config_commit.rs)：`config-transaction.lock` OS 文件锁覆盖磁盘摘要比对、凭据准备与原子提交的完整事务；忙或锁失败明确拒绝，绝不吞成尽力而为。摘要基线包含文件不存在的状态，外部修改必须刷新后重试。锁由 RAII/进程退出释放，恢复与 post-process 标记使用同一锁，避免 CAS 检查后提前释放的窗口；不持配置内存锁等待钥匙串或磁盘。
  - **MCP** 走 [`ScopeCommitGuard`](../src-tauri/src/mcp/config.rs)：进程内 `CONFIG_COMMIT` **之外**再加一把落盘租约。`CONFIG_COMMIT` 只挡得住本实例并发——两个实例各自读一份快照，后写者会把另一端刚做的改动整份覆盖且不留痕迹。守卫由**参数类型**强制传入写原语（`set_always_allow` 少一个参数就编译不过），不靠注释提醒。已有租约时的重试**必须有上限**：崩溃后的保守 busy 租约不能把本进程拖成长等待。协调根取 `mcp.json` **所在目录**的 `coord/` 而非全局数据目录，于是全局配置落在全局数据目录、项目配置落在项目托管目录——写同一份文件的两边必然算出同一个根，不同文件互不阻塞，`McpManager` 也无需为守卫穿线 data_dir。原有的「读回比对」乐观检查**保留**，它与 `atomic_write` 之间仍有一道窗口，租约把这段窗口关掉，两者叠加才算数。**只有真正写盘的 2 处**加租约：`manager.rs` 另外几处 `CONFIG_COMMIT` 锁的是内存连接池，加租约不仅无益，还会持锁等网络。
  - **stats** 按天分键（`stats:<canonical 目录>:<日期>`）持租约，因为「读当日旧文件 → 合并增量 → 整份写回」是典型读改写，`atomic_write` 只保证写那一下不撕裂，**不**保证读到的不是别人的旧值。取不到租约或写失败时**保留 pending** 而非清空——旧实现无条件 `pending.clear()`，一次 IO 抖动就变成永久统计缺口。重试安全的前提是写失败时磁盘没变，重读重并不会重复计数。90 天保留期清理**刻意不持锁**：它只删 90 天前的文件，与写入侧「只写今天」不相交。
  - 三者**不与 history 事务嵌套**，各自独立取锁。
- **桌面启动前恢复共享事务**（#36-C0）：`process_coordination::recover_shared_transactions(data_dir)` 接收数据根并在内部定位 `coord/`，启动前持事务短锁向前滚动已决定的写。恢复失败时桌面停止装配并报告错误；不能带着未恢复的共享状态继续启动。独立 guardian 在注册命令执行前记录 PID 与启动身份，宿主被 SIGKILL 后回收注册子树；只有同 session/run/generation 的清理证明允许恢复，不凭 PID 缺失抢锁。私有控制 token 位于 OS 临时目录随机 0700 子目录和 0600 文件，工作区只保留无 token 的租约/证明；helper 缺失时阻塞执行。
- **工具观察 ID 采用 batch append 日志和有界游标**：`sessions/<id>.toolres/_observations/ledger.jsonl` 每批追加一行，直接 batch 索引提供重试幂等；小型 pending 意向使半写只恢复本批未提交尾部，已提交前缀不重写。批记录读取上限 4MiB，读取方按历史分段和文件游标消费，不载入全量 occurrence 集合；已完成调用的配对状态及时释放。旧的无观察日志历史使用稳定的 replay fallback ID，不为这项未发布格式新增迁移。
## 2. 运行真值、事件与投影

[core/agent/runtime.rs](../src-tauri/src/core/agent/runtime.rs) 持有运行身份、状态与最近终态；[drive.rs](../src-tauri/src/core/agent/drive.rs) 负责请求、工具、压缩、监督和收尾；[stream.rs](../src-tauri/src/core/agent/stream.rs) 负责请求组装与流式处理。

- 启动先原子占位，同一会话不得并发运行；取消、错误、panic 与正常完成都要清理运行状态并保留已经产生的历史。
- “本轮没有 tool call”不是立即成功的充分条件：steer、被拒调用、本 run 的计划工作和子代理汇报分别有收尾规则。
- steer 注入当前 run，不结束它；接纳窗口与收尾同锁协调，收据以真正消费及检查点为边界，不能让未消费队列进入下一 run。
- 子代理档位在 step 边界从根会话与 `SubBase` 重建，模型和思考力度仍取 spawn 快照。计划任务 runtime 独立并显式 FullAccess。
- 终态事件可能丢失。`session_run_snapshot` 是权威对账入口，必须匹配 run 身份；成功才能续队，error/cancelled 不能伪装成功。快照兜底不等于底层事件丢失已修复。

| 通道 | 用途 | 契约 |
|---|---|---|
| `start_chat` 注册的 Channel | 文本/思考增量、工具进度、usage，以及包装后的子代理 Frame | 有序；`gen` 隔离重试残帧，宿主绑定/解绑成对 |
| `EventSink::emit` | run、工具、ask、子代理等具名状态事件 | 按 session 路由；run/sub 终态等关键载荷携带 run_id，但 tool:start、ask:closed 等仍有身份缺口，不能假定所有事件可跨 run 过滤 |
| 只读运行快照 | 终态恢复、跨会话观察、待处理回跳 | 真值来自后端；观察面不接管运行、取消或 responder |

前端 `stores/run.ts` 是按 Tab 组织的投影；handler 工厂在 `runHandlers.ts`，帧 reducer 在 `runFrames.ts`。事件名由 [events.contract.test.ts](../ui/src/__tests__/events.contract.test.ts) 双向守护；可选字段扩展仍需载荷/类型验证。项目、设置、任务、更新器分别有独立 store，不能从聊天记录推导这些真值。

完整生命周期与调度见 [runtime-and-scheduling](./runtime-and-scheduling.md)，委派见 [subagents](./subagents.md)，模式和计划门见 [plan-and-approval](./plan-and-approval.md)。

## 3. 请求、工具与安全的交界

- 中立消息类型在 [core/types.rs](../src-tauri/src/core/types.rs)。请求前修复作用于出网副本，不能随便污染展示转录；Anthropic SSE 流内不调用 `parser.finish()`。
- system 稳定块 run 内冻结、工具 schema 排序，瞬态内容置于稳定前缀之后；缓存字节稳定不能靠“语义相同”代替。
- 工具由 [registry.rs](../src-tauri/src/tools/registry.rs) 统一注册，`ToolKind` 分类不替代执行路径的安全判断。command 即使标为只读仍过 fence。
- 批次统一处理交互排他、写冲突、并发、取消和 panic；`ask/wait` 与 `suggest` 的混批语义不同，不能按枚举一概拒绝或放行。
- 前端拿完整 `ToolOutcome`，模型拿 `compact_for_model` 的文本。**写入后检查**结论必须进入 `outcome.data.check/checks`；仅写 warnings 模型看不到。未执行检查必须报告跳过原因。
- 文件工具共用 pathutil，命令共用 fence；plan 下需确认的命令转拒绝，FullAccess 不绕过直接 Block。方案批准和单次操作审批是不同机制，预览不授权。
- 内部宿主状态由 `core/protected_state` 按真实全局/项目快照分类，普通写工具与已识别命令目标均不可直接修改；业务入口仍走各自事务，不把整个 `.codewave` 变为只读。明确范围、非沙箱边界和回归由[工具与安全](./tools-and-safety.md#宿主内部状态写保护)主维护。
- web_fetch/http_request 共用逐跳地址固定、代理路由快照、总预算与流式限额；禁止内网时明确拒绝无法固定目标地址的远端 DNS 代理域名请求。该 SSRF 层不覆盖 Provider/MCP，也不宣称约束恶意代理或底层网络路由，详见工具指南。
- WebView CSP/capabilities、隔离预览和 Rust 工具安全层各有边界，不能互相代替。

具体语义见 [tools-and-safety](./tools-and-safety.md)、[providers-and-network](./providers-and-network.md)、[mcp-and-skills](./mcp-and-skills.md)。文档读写/保真修改见 [documents](./documents.md)：只替换目标包内成员，不整份解析重建。

## 4. 数据作用域与持久化

项目是名称加单一主目录；临时会话无需选项目，默认 workspace 为全局数据目录。项目注册表负责发现，会话元数据负责创建时快照，runtime 负责当前运行，三者不能互相替代。

| 位置 | 作用 |
|---|---|
| `~/.codewave/` | 全局配置、项目发现索引、会话、记忆、技能、统计与日志 |
| `<主目录>/.codewave/` | 项目元数据、任务、技能、日志、记忆、MCP 与经验 |
| `histories/<id>/`、`histories/subs/<父>/<子>/` | 主会话与子代理 append-only JSONL 历史段及水位 |
| `sessions/<owner>.imgblob/` | 内容寻址图片数据 |
| `sessions/<会话>.toolres/` | 仅供恢复 UI 工具卡的完整结果 |

`rt.data_dir` 恒为全局，项目作用域用 `rt.project_dir`。command 的实际默认 cwd 是项目托管目录本身，无项目时取 workspace；提示词中 temps 的描述不能当作执行事实。

- `SessionMeta.project_id + roots` 是会话目录快照；@ 提及和 Git 聚合不能绕回可变注册表重算。`allowed_dirs` 在创建/恢复时并入运行根。
- 项目删除取消相关运行并级联会话和托管目录，不删除用户代码主目录；迟到检查点不得复活已删除数据。
- providers 嵌套 models 是配置形态，`ModelConfig` 是运行时摊平形态；前后端摊平逻辑同步维护，新增字段使用 serde default。
- API key 通过 `core/config_commit.rs` 统一提交：先写/核验不可变 keyring generation，再一次性替换占位与引用配置；不降级明文。配置写者共用提交锁、字段域版本与操作回执，详情和历史明文/真平台限制见 [供应商与网络](./providers-and-network.md#2-api-key-与-key-池)。
- 资源持久信任使用宿主注入的 backend、专用钥匙串 service 和受保护 OS 协调锁；MCP 启动和指令资源共用底层权威服务、分别绑定用途。项目提示/记忆/技能按同份快照准入渲染，正文读取再验；本次许可按根会话隔离。冻结/估算及平台验收边界见 [MCP 与技能](./mcp-and-skills.md#指令资源准入)。

### 保存、展示与 wire 必须分离

1. 落盘是分段 append-only 转录；压缩只改变运行上下文并追加边界，不 trim、覆盖或截断旧段。
2. wire 从有效基线/压缩边界有界读取、repair 和预算裁剪；展示按段分页，不把模型摘要当完整历史。
3. 保存锁序固定为 `save_lock → index_lock`；索引读改写串行。段封口不能拆开 tool_use/tool_result 配对。
4. 损坏段/尾部残行容错保留可读数据；软体积线提示，硬线只停写、不删历史；新增降级状态必须纳入 `SaveReport::is_clean()`。
5. 图片外置使用独立持久化 DTO，不改内存/wire 的 Content；GC 看全部保留段引用并集，不能只看压缩后的内存快照。
6. 工具结果 sidecar 以 provider `tool_use.id` 为键，不用临时 call_key；只做 UI 回填，不出网，随会话级联删除。
7. 旧格式清理和新格式可读判断同源；没有可读新数据时绝不删唯一兼容副本。完整转录保留也不等于索引无限保留，现有 LRU 边界见专题。

具体格式、阈值、恢复、兼容与清理见 [sessions-and-projects](./sessions-and-projects.md)。

## 5. 界面与维护入口

界面由 antd 与主题桥接统一管理，字体用界面/内容/代码三槽 token；非图片附件保留真实路径引用，发送时合成正文，图片才读取为 base64。Composer 光标触发、draft 的 text/refs/images、运行中状态和异步恢复须一起维护。

- 聊天、导航、工具卡、通知与主题：[chat-and-interface](./chat-and-interface.md)
- 设置的保存边界、即时偏好与页面组织：[settings-guide](./settings-guide.md)
- 构建环境与代码组织：[development](./development.md)
- 门禁、回归策略与已知验证边界：[testing](./testing.md)
- 安装包、签名、公证与更新：[release](./release.md)
- 全部主题导航：[0-README](./0-README.md)

维护本文时逐字保留 D1–D8，只更新跨层事实和必要摘要；具体状态机、问题排查和边界放在对应主题。不要重新加入完成计划、一次性验收数字、旧目录全表或“全部交付/全绿”快照。
