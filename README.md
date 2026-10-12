# CodeWave

> Tauri 2 单进程桌面 AI 编程 Agent：纯 Rust 后端承载全部 Agent 编排，React 19 + antd 6 WebView 只做 UI。

CodeWave 跑在你的本机（Windows / macOS / Linux）。打开一个项目目录，用对话驱动 Agent 读代码、改文件、跑命令、搜内容、管理任务——会话与配置保存在本机，无遥测；通过 BYOK（自带模型 Key）连接你配置的模型服务，对话上下文会发送至对应端点。API Key 优先存入系统钥匙串，限制见下方安全说明。

<!-- TODO(截图)：发布前补一张主界面截图（浅色/深色各一），放 docs/assets/ 或 .github/assets/ -->

## 特性

- **本地存储**：会话历史分段追加保存，图片按内容去重；超大历史设告警与停写保护，不因体积自动删除转录
- **多供应商 BYOK**：OpenAI 兼容 / Anthropic / OpenAI Responses 三协议，多 Key 轮换，端点与模型完全自定义（无内置模型目录）
- **项目 = 单目录**：选一个代码目录作为项目主目录，托管数据（任务/日志/记忆/技能）存于其下 `.codewave/`，数据随项目走；也支持免目录的临时会话直接开聊
- **权限四档**：`plan` → `confirm_each` → `auto_edit` → `full_access`；Composer 里 Shift+Tab 切换。内置文件写入、shell、MCP 与网络各有执行边界，见[计划与审批](./docs/plan-and-approval.md)和[工具与安全](./docs/tools-and-safety.md)
- **命令安全围栏**：删除黑名单、tree-sitter AST 写目标分析与高危模式判断结合审批；这是静态检查，不是 OS 沙箱，区外新建等操作有按权限与配置确认或放行的分支，见[工具与安全](./docs/tools-and-safety.md)
- **写入后检查**：可配置检查命令，在项目根执行，支持 `{file}` 占位符；通过安全围栏后执行，检查结论回喂模型，详见[工具与安全](./docs/tools-and-safety.md)
- **内置工具 + MCP 扩展**：文件读写、命令执行、grep、网络抓取、计划任务、后台服务管理、Office/PDF 文档处理等；rmcp 客户端接入 stdio / streamable-http MCP 服务器
- **技能与子代理**：`/slash` 技能与 `$角色` 子代理委派；兼容 `.claude/skills`、`.agents/skills` 目录约定
- **长会话友好**：完整分段历史、图片外置存储、token 明细统计、上下文自动压缩、会话自动命名、多 Tab 并行、运行队列
- **steer（运行中转向）**：运行时可将排队的纯文本消息通过「↑ 立即」并入当前运行，继续执行中的子代理任务；带图消息等待正常出队，详见[运行与调度](./docs/runtime-and-scheduling.md)
- **双语界面**：中文 / English 可切换；自绘标题栏，主题跟随系统

## 下载安装

从 [GitHub Releases](https://github.com/Yangshifu1024/CodeWave/releases) 下载对应平台的安装包：

| 平台 | 格式 |
|---|---|
| Windows | NSIS 安装包（`.exe`）/ MSI（`.msi`） |
| macOS | `.dmg` / `.app.tar.gz`（签名与公证状态以对应版本的 Release 说明为准） |
| Linux | `.AppImage` / `.deb` / `.rpm` |

> 更新器已启用，签名更新包与版本信息由已发布的 GitHub Release 提供；也可从 Releases 页面手动下载安装包。

## 从源码构建

前置依赖：

- Rust stable（最低 1.98）与平台构建链（Windows 需 MSVC Build Tools）
- Node.js 22 与 [pnpm](https://pnpm.io/) 12（与当前 CI 对齐）
- Tauri 2 的[系统依赖](https://v2.tauri.app/start/prerequisites/)（Linux 需 WebKitGTK）

平台依赖、启动排查与工程约定见[开发指南](./docs/development.md)。以下命令均在仓库根执行：

```bash
pnpm install --frozen-lockfile  # 一次安装根与 ui workspace 的锁定依赖
pnpm tauri dev                 # 启动完整桌面应用
pnpm tauri build --debug       # 调试打包
pnpm tauri build               # 发布模式打包；不会自动发布 Release
```

Tauri CLI 使用仓库的 pnpm 依赖，无需另装 cargo-tauri。`pnpm dev` 仅启动前端 Vite 服务；桌面功能使用 `pnpm tauri dev`。开发版与安装版共享单实例标识，启动前确认另一实例已退出。

### 测试与本地门禁

```bash
cargo test --manifest-path src-tauri/Cargo.toml --workspace  # 后端测试
pnpm --dir ui run lint      # 前端 ESLint
pnpm --dir ui test          # 前端 Vitest
pnpm --dir ui build         # TypeScript 检查 + Vite 构建
pnpm prepr                 # 开 PR 或发版前的完整本地门禁
```

改后端跑后端测试，改前端跑前端测试与构建，跨层改动两边都要验证。`pnpm prepr` 包含锁定安装、Rust 格式/clippy/测试、前端 lint/测试/构建与脚本测试；硬步骤失败即停，clippy 软失败须单独报告。完整规则见[测试与门禁](./docs/testing.md#开-pr-前的完整门禁)。

本地检查只覆盖当前平台，不能代替三平台 CI 或桌面 GUI 人工验收。当前限制见[已知验证边界](./docs/testing.md#已知验证边界与待核实事项)；文档更新不代表这些项目已验证通过。

### 版本发布

发布入口见[发布指南](./docs/release.md#发布流程与授权边界)与[发布技能](./.agents/skills/codewave-release/SKILL.md)。在干净且同步的 main、版本确认与完整门禁通过后，使用 `pnpm bump <x.y.z>` 同步版本及锁文件；commit、push 和 tag 按明确授权执行。tag 触发三平台构建与 draft Release，核对安装包、更新清单、签名及真机验证后再人工 Publish。

## 架构一览

```
┌───────────────────── CodeWave（Tauri 2 单进程）─────────────────────┐
│  WebView（React 19 + antd 6 + zustand + vite）                       │
│    AppShell / ProjectNav / ChatMessages+Composer / RightBar          │
│         ▲ invoke（IPC 命令）  ▲ Channel + emit（事件流）              │
│  Rust 后端                                                            │
│    host/     IPC 命令（校验+转调）· EventSink · keyring               │
│    core/     Agent 主循环 · config · context · prompt · sessions      │
│    provider/ anthropic · openai_chat · openai_responses               │
│    tools/    内置工具（纯函数化）   safety/ 审批门 + 命令围栏          │
│    skills/ mcp/ memory.rs agents/ git/(只读)                            │
│  数据：~/.codewave/（全局）+ <项目主目录>/.codewave/（项目作用域）     │
└───────────────────────────────────────────────────────────────────────┘
```

分层规则：只有 `lib.rs` 与 `host/` 允许使用 Tauri API，`core/` 不引入 Tauri 类型；当前仍有核心模块调用 `host::keyring` 的凭据依赖例外，详见[架构与决策](./docs/technical-design.md)。Agent 核心没有独立 sidecar，shell、后台服务和 MCP 可按需启动子进程。

### 仓库结构

| 目录 | 职责 |
|---|---|
| [src-tauri/src/](./src-tauri/src/) | Rust 核心、宿主适配、协议、工具与安全边界 |
| [ui/src/](./ui/src/) | React 界面、IPC 契约、状态管理与前端测试 |
| [scripts/](./scripts/) | 本地门禁、版本升级、更新清单修补与脚本测试 |
| [.github/workflows/](./.github/workflows/) | lint、三平台测试与发布工作流 |
| [docs/](./docs/) | 平铺的长期主题指南与文档索引 |
| [.agents/](./.agents/) | 子代理角色定义与项目技能 |

## 文档

[完整文档索引](./docs/0-README.md)包含 14 篇长期主题指南、1 篇待验收专项方案，连同索引共 16 篇。按任务选择入口：

| 任务 | 阅读入口 |
|---|---|
| 开始开发 | [开发与工程约定](./docs/development.md) → [架构与技术决策](./docs/technical-design.md) → [测试与本地门禁](./docs/testing.md) |
| 理解会话与执行 | [会话与项目](./docs/sessions-and-projects.md)、[运行与调度](./docs/runtime-and-scheduling.md)、[子代理](./docs/subagents.md) |
| 维护权限与工具 | [计划与审批](./docs/plan-and-approval.md)、[工具与安全](./docs/tools-and-safety.md)、[Office/PDF 文档工具](./docs/documents.md) |
| 配置模型与扩展 | [供应商与网络](./docs/providers-and-network.md)、[MCP 与技能](./docs/mcp-and-skills.md) |
| 维护产品交互 | [设置指南](./docs/settings-guide.md)、[聊天与界面](./docs/chat-and-interface.md) |
| 交付版本 | [发布、签名与自动更新](./docs/release.md) |
| 参与协作 | [贡献指南](./CONTRIBUTING.md)、[AI 协作规约](./AGENTS.md)、[安全策略](./SECURITY.md) |

文档维护当前行为、契约、排障和验证边界；优先更新已有主题，不为每次修复另建报告。历史方案与一次性交付记录通过 Git 历史查阅，维护规则见[文档索引](./docs/0-README.md#维护规则)。

## 隐私与安全

- 会话历史、配置、记忆与技能保存在本机磁盘（`~/.codewave` 与各项目主目录下的 `.codewave/`）。使用模型或网络工具时，相关上下文与请求会发送至对应服务；本地存储不代表离线运行
- API Key 通过 [`keyring`](https://crates.io/crates/keyring) 迁入系统钥匙串，成功后配置改存占位符。当前保存流程会先写配置再迁移，迁移失败则保留明文并记录警告，因此不能保证密钥从不落盘；备份或分享配置前请检查并移除密钥（详见[安全策略](./SECURITY.md)）
- 网络访问包括配置的 LLM 服务、工具请求、MCP 连接、供应商额度查询与 GitHub Releases 更新检查；命令和外部 MCP 服务也可能自行联网。网络与代理边界见[供应商与网络](./docs/providers-and-network.md)和[工具与安全](./docs/tools-and-safety.md)。启动时自动检查更新可在设置页「关于」关闭
- 发现安全漏洞请走 [SECURITY.md](./SECURITY.md) 的私密渠道，勿直接开公开 issue

## License

[MIT](./LICENSE) © 2026 Yangzhenbiao
