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
| 界面 | [ui/src/](../ui/src/) | IPC 封装、运行状态投影、聊天/工具卡、导航/设置与文件预览 |

### 分层约束

- Rust 的 Tauri API 边界限定在 `lib.rs` 和 `host/`；core/provider/tools/safety 不引入 Tauri 类型。`EventSink` 隔离事件投递，不把 `AppHandle` 传进算法或工具。
- **存在 core → host 的凭据依赖例外**：主循环、压缩、标题与额度模块仍调用 `host::keyring`；不引入 Tauri 类型不等于完全没有宿主层依赖。
- IPC 保持薄校验/转调，业务判断留在核心和工具层；前端 invoke 唯一入口是 [ipc/client.ts](../ui/src/ipc/client.ts)，类型在 [ipc/types.ts](../ui/src/ipc/types.ts)。
- 手写协议适配与 IPC 类型降低生成器耦合，但兼容性靠双端同步和契约测试保证；Provider wire 差异不能外泄成工具/UI 对某一家协议的依赖。
- 主会话、子代理、计划任务共用参数化驱动器，不能复制主循环分别修复；测试与重构纪律见 [development](./development.md)、[testing](./testing.md)。

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
- web_fetch/http_request 的 SSRF 层不覆盖用户配置的 Provider 端点；DNS 校验后连接仍有二次解析窗口，不能宣称完整消除 rebinding。
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
- API key 当前**先落配置再迁 keyring**，成功后重存占位符，失败保留明文并告警，不能保证从未明文落盘。

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

界面由 antd 与主题桥接统一管理，字体用双槽 token；非图片附件保留真实路径引用，发送时合成正文，图片才读取为 base64。Composer 光标触发、draft 的 text/refs/images、运行中状态和异步恢复须一起维护。

- 聊天、导航、工具卡、通知与主题：[chat-and-interface](./chat-and-interface.md)
- 设置的保存边界、即时偏好与页面组织：[settings-guide](./settings-guide.md)
- 构建环境与代码组织：[development](./development.md)
- 门禁、回归策略与已知验证边界：[testing](./testing.md)
- 安装包、签名、公证与更新：[release](./release.md)
- 全部主题导航：[0-README](./0-README.md)

维护本文时逐字保留 D1–D8，只更新跨层事实和必要摘要；具体状态机、问题排查和边界放在对应主题。不要重新加入完成计划、一次性验收数字、旧目录全表或“全部交付/全绿”快照。
