# CodeWave

> **本地优先的桌面 AI 编程 Agent**（local-first desktop AI coding agent）。
> Tauri 2 单进程：纯 Rust 后端承载全部 Agent 编排，React 19 + antd 6 WebView 只做 UI。

CodeWave 跑在你的本机（Windows / macOS / Linux）。打开一个项目目录，用对话驱动 Agent 读代码、改文件、跑命令、搜内容、管理任务——数据全部留在本地，API Key 存入系统钥匙串，没有任何遥测与云端依赖（BYOK：自带模型 Key）。

<!-- TODO(截图)：发布前补一张主界面截图（浅色/深色各一），放 docs/assets/ 或 .github/assets/ -->

## 特性

- **本地优先**：会话、配置、记忆、技能全部存于本机（`~/.codewave` 与项目内 `.codewave/`）；API Key 入系统钥匙串，配置文件不落明文；零遥测、零上报
- **多供应商 BYOK**：OpenAI 兼容 / Anthropic / OpenAI Responses 三协议，多 Key 轮换，端点与模型完全自定义（无内置模型目录）
- **项目 = 单目录**：选一个代码目录作为项目主目录，托管数据（任务/日志/记忆/技能）存于其下 `.codewave/`，数据随项目走；也支持免目录的临时会话直接开聊
- **权限四档**：`plan`（只读）→ `confirm_each`（每写必问）→ `auto_edit`（自动编辑）→ `full_access`，Composer 里 Shift+Tab 循环切换
- **命令安全围栏**：三层静态围栏（删除黑名单 → tree-sitter AST 写目标分析 → 高危模式审批）先于弹窗拦截危险命令；写操作全部圈定在会话可写根内
- **20 个内置工具 + MCP 扩展**：文件读写、命令执行、grep、网络抓取、计划任务、后台服务管理等；rmcp 客户端接入 stdio / streamable-http MCP 服务器
- **技能与子代理**：`/slash` 技能与 `$角色` 子代理委派；兼容 `.claude/skills`、`.agents/skills` 目录约定
- **长会话友好**：token 明细统计、上下文自动压缩、会话自动命名、多 Tab 并行、运行队列
- **双语界面**：中文 / English 可切换；自绘标题栏，主题跟随系统

## 下载安装

从 [GitHub Releases](https://github.com/Yangshifu1024/CodeWave/releases) 下载对应平台的安装包：

| 平台 | 格式 |
|---|---|
| Windows | NSIS 安装包（`.exe`）/ MSI（`.msi`） |
| macOS | `.dmg` / `.app.tar.gz`（当前未签名，首启需右键 → 打开绕过 Gatekeeper） |
| Linux | `.AppImage` / `.deb` / `.rpm` |

> 更新器已内置但默认停用（`updater.active: false`）；当前请通过 Releases 页面手动获取新版本。

### 从旧版 WaveStudio 升级（0.2.0 前品牌版本）

应用已由 WaveStudio 改名 CodeWave，数据标识随之更换且**不做自动迁移**：

1. 全局数据目录 `~/.wavestudio` 需手动改名为 `~/.codewave`（保留会话/配置/技能）；各项目主目录下的 `.wavestudio/` 同理整体改名
2. 系统钥匙串服务名已更换（`studio.gitwave.work` → `codewave.yangshifu.xyz`），已存的 API Key 需在设置中重新录入
3. 项目指令文件 `WAVESTUDIO.md` 改名 `CODEWAVE.md`（旧文件名仍兼容读取）

## 从源码构建

前置依赖：

- Rust stable（`rust-version = 1.85`）与平台构建链（Windows 需 MSVC Build Tools）
- Node.js 22+ 与 [pnpm](https://pnpm.io/)
- Tauri 2 的[系统依赖](https://v2.tauri.app/start/prerequisites/)（Linux 需 WebKitGTK）

```bash
pnpm install                # 安装依赖（workspace = ui）

pnpm tauri dev              # 开发调试（仓库根执行）
pnpm tauri build            # 打包产物

# 测试（改哪边跑哪边，两边都动则都要过）
cd src-tauri && cargo test  # 后端：全绿 / 0 warning 为基线
pnpm --dir ui test          # 前端：vitest
pnpm --dir ui build         # 前端：tsc --noEmit + vite build
```

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
│    skills/ mcp/ memory/ agents/ git/(只读)                            │
│  数据：~/.codewave/（全局）+ <项目主目录>/.codewave/（项目作用域）     │
└───────────────────────────────────────────────────────────────────────┘
```

分层规则：只有 `lib.rs` 与 `host/` 允许 `use tauri::*`，`core/` 保持零框架耦合、可独立单测。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/0-README.md](./docs/0-README.md) | 全部设计与批次报告的目录（技术基准、契约来龙去脉） |
| [docs/technical-design.md](./docs/technical-design.md) | 技术方案基准：决策记录、分层规则 |
| [AGENTS.md](./AGENTS.md) | AI 协作开发规约（约束 / 命令 / 契约锚点 / 坑清单） |
| [CONTRIBUTING.md](./CONTRIBUTING.md) | 贡献指南 |
| [SECURITY.md](./SECURITY.md) | 安全漏洞报告渠道 |

## 隐私与安全

- 所有数据（会话历史、配置、记忆、技能）仅存于本机磁盘，卸载应用不会触碰你的项目代码目录
- API Key 通过 [`keyring`](https://crates.io/crates/keyring) 存入系统钥匙串（Windows Credential Manager / macOS Keychain / Linux Secret Service），配置文件中只留占位符
- 应用不做任何网络请求，除了你配置的 LLM 端点、你显式触发的 `web_fetch`/`http_request` 工具、以及 MCP 服务器连接
- 发现安全漏洞请走 [SECURITY.md](./SECURITY.md) 的私密渠道，勿直接开公开 issue

## License

[MIT](./LICENSE) © 2026 Yangzhenbiao
