# 开发与工程约定

本文维护环境准备、代码组织和依赖维护规则。架构见 [technical-design](./technical-design.md)，验证见 [testing](./testing.md)，发版见 [release](./release.md)；协作与 Git 授权以 [AGENTS.md](../AGENTS.md) 为准。

## 环境与启动

| 工具 | 要求与事实源 |
|---|---|
| Rust | edition 2024，MSRV **1.98**，见 [Cargo.toml](../src-tauri/Cargo.toml)；CI 使用 stable，不能把本机更高版本的成功当作最低版本验证 |
| Node.js | CI 使用 22，见 [test.yml](../.github/workflows/test.yml)；升级构建工具时同步检查其 engines |
| pnpm | CI 使用 12；根与 `ui/` 构成同一 workspace，见 [pnpm-workspace.yaml](../pnpm-workspace.yaml) |
| Tauri CLI | 根 [package.json](../package.json) 的 npm CLI，统一用 `pnpm tauri …`；不要求安装 cargo-tauri |
| 系统依赖 | 由当前平台的 Tauri/WebView、Rust 链接器和 libgit2 构建链决定，不能仅安装 npm 包 |

### 平台准备

- macOS：Xcode 命令行工具（`xcode-select --install`）、Rust、Node 与 pnpm；Apple Silicon 与 Intel 目标需分开核对，不从一个目标推导全架构支持。
- Linux（与 Ubuntu CI 同源）：安装 `cmake build-essential pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev`。
- Windows：VS Build Tools 的 MSVC 工具链、CMake、WebView2 Runtime、Rust、Node 与 pnpm。测试 CI 用 [ensure-cmake.ps1](../scripts/ci/ensure-cmake.ps1) 检测并复用 CMake ≥ 3.21（VS 2022 generator 的最低版本）；缺失、不可用或版本不足时才通过 Chocolatey 安装/升级，再验证实际 PATH 命中的程序。release 的独立安装链路不受此优化影响。
- git2 使用源码构建的 libgit2，当前关闭默认 features，不要求另外安装系统 libgit2 或 libssh2。是否新增 SSH 依赖应回到 Cargo 配置核对。
- Linux 的系统钥匙串功能依赖可用 Secret Service；无图形会话或 CI 中可能不可用；此时新增/修改凭据安全失败，保留旧配置，不降级写明文。旧文件已有明文的启动迁移失败会明确告警，且禁止任何写入口重新复制明文。

在仓库根执行：

```bash
pnpm install --frozen-lockfile
pnpm tauri dev
# 打包验证仍在仓库根
pnpm tauri build --debug
```

`pnpm tauri …` 依赖根 `package.json` 与 `src-tauri/tauri.conf.json` 的相对布局，不能在 `ui/` 或 `src-tauri/` 随意运行。Rust 命令在 `src-tauri/` 执行；前端命令从根用 `pnpm --dir ui …`。

首次启动创建 `~/.codewave/`；项目托管数据位于 `<主目录>/.codewave/`。应用日志在全局数据目录的 `logs/`，`RUST_LOG=debug` 可临时提高日志级别。日志和配置可能含用户信息，分享前先脱敏。

## 日常改动流程

1. 读 [AGENTS.md](../AGENTS.md)、架构与受影响主题，确认现行行为、边界与测试入口。
2. 先检查工作区现有改动，不撤销或混入别人的变更；跨模块工作明确各文件负责人。
3. 缺陷优先补能触发原问题的回归测试，再改实现；重构尽量先机械搬移，行为变化单独解释和验证。
4. Rust 改动跑后端测试；UI 改动跑测试与构建；跨层变更两边都跑。开 PR 前完整执行 `pnpm prepr`，见 [testing](./testing.md)。
5. 多文件或跨层变更做代码审查；严重问题必须处理，结论区分已验证、推断和待人工检查。
6. 更新现有主题中的契约、限制和排查入口。完成的计划、一次性测试计数和提交草案不再另建长期文档；历史在 Git 中查阅。

Git 写操作默认由用户执行。批准计划仅覆盖计划明确列出的分支创建/切换；commit、push、merge、stash 等仍需当次明确授权。发布另有推送前确认点，见 [release](./release.md)。

## 代码组织与重构纪律

- `mod.rs` 优先作为声明、re-export 与测试模块的入口；实现按职责进入 `tool.rs`、`store.rs`、`runtime.rs` 等，测试过大时移入独立测试模块。
- 机械拆分保留外部 import 路径，用门面 re-export 维持调用方；可见性只扩大到必要的模块子树，优先 `pub(super)`，不要为测试方便扩大 crate 公共面。
- Tauri `#[command]` 会生成隐藏宏项。`host/commands/mod.rs` 的 glob re-export 用于保住 `generate_handler!` 的注册路径；改成逐个函数导出后须专门验证宏解析。
- 主循环按语义段提取请求重试、自动压缩、批次执行等函数；保留取消优先级、跨重试用量累计、成功后重试计数归零、末步执行与汇报顺序，不能只凭“函数体搬过去了”判断等价。
- 前端 store 门面、类型、帧 reducer、事件 handler 工厂分开。`bindGlobalHandlers()` 仍是事件装配入口；不要在拆文件时复制另一份事件清单。
- Rust edition 2024 中 `gen` 为保留字，内部使用 `generation`，既有 wire 字段以 `#[serde(rename = "gen")]` 保持。内部重命名不构成修改 IPC 契约的理由。
- 长文件优先按职责拆分，但**当前 workflow 没有行数上限 job**。历史上曾有 `>800` 告警、`>1500` 失败的方案，不应当作仍在执行的 CI 保证。

框架边界、数据真值与跨层不变量见 [technical-design](./technical-design.md)，各主题维护具体字段与状态机。

## 依赖、构建与平台变更

权威版本来自 [Cargo.toml](../src-tauri/Cargo.toml)、[Cargo.lock](../src-tauri/Cargo.lock)、[根 package.json](../package.json)、[ui/package.json](../ui/package.json) 与 [pnpm-lock.yaml](../pnpm-lock.yaml)，不维护另一份“最新版本”总表。

- Rust Tauri、npm CLI/SDK 与各插件独立发版，版本号不要求一一对应；先确认实际可解的依赖图、插件注册和权限，再升级。
- `cargo update -w` 的“无变动”不证明依赖已是最新，也不证明某个候选版本可解。区分 manifest 范围、lockfile 解析结果、工具链要求与注册表可达性。
- 同步 manifest 和锁文件；已有 `node_modules` 上测试成功不能证明 frozen install 可复现。缓存不足或注册表不可达是环境阻塞，不能用旧依赖绿态关闭升级任务。
- [pnpm-workspace.yaml](../pnpm-workspace.yaml) 用 `allowBuilds` 明确允许 esbuild 构建脚本；不要恢复“允许所有构建脚本”的宽泛配置。浏览器 PDF 预览不使用 Node 侧 `@napi-rs/canvas`，目前排除其可选安装。
- 只声明未注册、未消费的插件会增加构建与供应链面；新增/删除时同时核对 Rust 声明、`lib.rs` 注册、前端调用与 capability。
- 平台条件编译必须由对应目标验证。Windows 的绿态不会编译 `cfg(unix)`；例如进程 `try_wait()` 返回 `Result<Option<ExitStatus>>`，不能按 `Option` 匹配后指望别的平台替你发现。
- [Dependabot](../.github/dependabot.yml) 继续每周检查 cargo、npm workspace 与 GitHub Actions；每个生态的 minor/patch 版本更新分组，major 仍独立提 PR，安全更新不套用此分组。分组降低重复全矩阵运行次数，但扩大单次变更面；组合不兼容时应拆组定位，不能跳过验证。

## 标识、配置与开源卫生

- 应用名 `CodeWave`，bundle id `xyz.yangshifu.codewave`，keyring service `codewave.yangshifu.xyz`。Windows 安装器、通知与 AUMID 需要一致。
- `.codewave` 名称在后端由 `MANAGED_DIR_NAME`、前端由 `utils/path.ts` 的同名常量复用；日志基本名由 logging 模块集中维护，禁止散落另一套品牌/路径常量。
- 配置新增字段应有 serde 默认值；不要借小变更重新引入已删除的旧 schema 或旧品牌迁移。历史格式的明确只读兼容是专题契约，不能连带删除。
- API key 保存经统一提交器先准备不可变 keyring 世代并回读校验，再原子替换仅含安全引用的配置；迁移或校验失败保留旧配置且返回明确错误，无新增明文降级。安全说明必须与 [SECURITY.md](../SECURITY.md) 和 [providers-and-network](./providers-and-network.md) 一致。
- UI 文案走中英 i18n，键集合保持对称；非 React 上下文用 `i18n` 的 named export。源码解释性注释优先英文，字符串字面量、测试夹具、中文提示词不能机械翻译。
- 公开交付前检查许可证、仓库链接、个人绝对路径、`.env`/密钥/日志和作者元数据。历史是否重写、仓库是否更换及相关 Git 操作由用户决定；不把旧开源计划当作持续授权。

## 开发排查

| 现象 | 先查 |
|---|---|
| Tauri CLI 找不到配置或资源 | 是否在仓库根、workspace 是否已安装、CLI 是否来自根依赖 |
| 启动后立即 panic | updater 插件所需配置节点是否存在；setup 同步上下文是否错误直接调用 `tokio::spawn` |
| 打包版或 dev 看似打不开 | 同 bundle id 的单实例互斥；先确认没有另一实例占用 |
| 全屏布局塌缩 | antd `App` 包裹层高度、外层 flex/min-height，而非重新手写组件皮肤 |
| Windows 门禁步骤 0 秒失败 | PATH 与 `.cmd` 垫片；pre-pr 脚本在 Windows 使用 shell，其他平台逐参数 spawn |
| 密钥保存后仍鉴权失败 | keyring 迁移/回读日志、配置中的占位符和有效 provider；不要输出真实 key |

界面变更不做 GUI 自动点验，交付分步人工清单。桌面通知、WebView 输入法、单实例、安装与系统钥匙串等能力不能仅由 happy-dom 测试证明。
