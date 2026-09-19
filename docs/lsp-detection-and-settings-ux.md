# 语言服务器探测补齐 + 设置页状态说清（含两条界面缺陷）

> **⚠️ 已被取代（2026-09-20）**：LSP 机制整体删除，改为「写入后检查命令」——见
> [post-write-check-plan](./post-write-check-plan.md)。本文涉及的 `lsp_*` IPC、`ServerStatus` 状态徽标、
> 事件面第 29 键均已移除（现为 28 键）。本文保留为历史实施报告。

> 日期：2026-09-19 · 类型：缺陷修复 + 界面批次 · 分支：`fix/lsp-detect-status`（基线 `09bba86`，v0.4.0）
> 涉及：`src-tauri/src/lsp/*`（含新文件 `sdk.rs`）、`src-tauri/src/host/commands/lsp.rs`、`ui/src/` 前端 7 文件、`docs/` 2 文件
> 契约：`ServerStatus` / `InstallHint` **纯追加字段**；IPC 命令名与入参零变化；事件面仍为 **29 键**（`lsp:server_missing` 载荷是显式取字段拼装，`requires` / `sdk` 不进载荷）。

## 一、需求与根因

用户报告两条：

1. 「我明明已经安装了这些 SDK（Java / rustc / python3 / go），设置页还是提示未找到」；
2. 「这个 Switch 开关太长了」。

查证后是**三件事叠在一起**：

| 现象 | 真因 |
|---|---|
| Go 行「未找到」，但 `~/go/bin/gopls` 确实存在 | ① `fresh_env_path` 只跑 `zsh -lc`（登录**非交互**，不读 `~/.zshrc`），而 `~/go/bin` 正是 `~/.zshrc` 里追加的（本机实测：`zsh -lc` 的 PATH 里没有它，`zsh -lic` 里有）；② 探测顺序里没有「各语言约定的安装目录」这一档，`go install` / `rustup component add` / bun 全局的落点全都不在覆盖面上——**「一键安装成功」与「显示已找到」之间因此断链** |
| Rust / Python / Java / TypeScript「未找到」 | 事实正确（本机确实没装 `rust-analyzer` / `pyright-langserver` / `jdtls` / `typescript-language-server`），但页面把「缺语言服务器」与「缺语言工具链」显示成同一句「未找到」，还承诺了一个页面上并不存在的一键安装入口 |
| 每行 Switch 被拉到约 350px | `.validation-row` 第 2 列是 `auto`，被剩余空间撑开；antd `size="small"` 的 Switch 只设了 `min-width`（28px），作为网格项又被块级化 → 宽高拉满整列 |

追加的两条界面反馈（审查开始前用户提出）：

4. 「自定义代理」的代理地址输入单独占一行、漂在卡片外；
5. 「显示进阶项（1）」打开后页面没有任何变化——该页唯一的进阶项是供应商编辑视图里的「当前模型」标记，列表视图里没有它的落点。定稿方案（用户选定）：**把该标记从进阶项里摘掉**，编辑视图常态显示、该页不再出现这个开关行。

## 二、改动清单

### A. 探测补齐（后端）

| 文件 | 改动 |
|---|---|
| `lsp/discovery.rs` | 新增 `conventional_bin_dirs` / `conventional_bin_dirs_with` / `conventional_bin_dirs_for`（按语言给约定安装目录，`home` 与环境查找可注入）：Go `$GOBIN`→`$GOPATH/bin`→`~/go/bin`；Rust `$CARGO_HOME/bin`→`~/.cargo/bin`→**`$RUSTUP_HOME/toolchains/*/bin`（默认 `~/.rustup/…`，见下）**；TS/Python `$PNPM_HOME`→`~/.bun/bin`→`~/Library/pnpm`→`~/.local/share/pnpm`→`~/.local/bin`（Windows：`%APPDATA%\npm`、`%LOCALAPPDATA%\pnpm`、`~\.bun\bin`）；Dart `~/development/flutter/bin` / `~/flutter/bin` / `~/fvm/default/bin`；Java 为空（jdtls 无跨平台固定位置，引导走官方地址）。新增探测档插在「新鲜 PATH」之后、「进程快照 PATH」之前，`source` 新增取值 `lang_bin`；`resolve` 拆出可注入的 `resolve_with`（单测控制探测面）。`ServerResolution::missing` 增加语言与 discoverer 参数，以便组装带前置探测的安装引导（`install_hint`）。`fresh_env_path`（非 Windows）改为 `-lc` + `-lic` 两次查询并合并（`shell_env_path` + `merge_shell_paths`）：哨兵 `__cwave_path__` 提取（交互式 rc 的输出不混入）、每次 5s 超时、`-lc` 在前保持既有优先级、`-lic` 只做去重追加。新增 `locate_command`（生效 PATH → 快照 → 约定目录，不起进程）与 `find_in_dirs`。**补充档 ⑨（同批追加，见 §A2）**：`rustup_toolchain_bins` 扫 `$RUSTUP_HOME/toolchains/*/bin` |
| `lsp/sdk.rs`（新，120 行） | 语言工具链就绪探测：命令存在性（TS→`node`、Rust→`cargo`/`rustc`、Python→`python3`/`python`、Go→`go`、Dart→`dart`/`flutter`）；Java 走 `find_jdk21`（与 jdtls 启动同一套结论） |
| `lsp/mod.rs` | `ServerStatus` 追加 `sdk: SdkStatus`（`#[serde(default)]`）；`InstallHint` 追加 `requires: Option<InstallRequirement>`（`command` / `name` / `ready` / `docs_url`）；新增 `SdkStatus`、`InstallRequirement`；`source` 注释补 `lang_bin`；声明 `pub mod sdk;` |
| `lsp/server_spec.rs` | `InstallSpec` 追加 `runtime_docs_url`（Node 下载页 / `go.dev/dl` / `rustup.rs`）与 `runtime_name`（`Node.js` / `Go 工具链` / `rustup`）——**不能拿语言的 SDK 名充数**：Python 行缺的是 Node.js |
| `lsp/install.rs` | `InstallPlan` 带 `lang` / `runtime_name` / `runtime_docs_url`；新增 `resolve_install_program`（PATH → 语言约定目录，**与设置页 `requires.ready` 同源判据**）与 `missing_runtime_message`（「未找到 npm…请先安装 Node.js（下载页）」） |
| `host/commands/lsp.rs` | `lsp_install` 在跑命令前先做前置检查，提前给出「请先安装 Node.js（下载页）」，不再等用户点了按钮才拿到含糊报错 |

### A2. 补充档：rustup 工具链 bin（用户报「装了 rust-analyzer 仍显示未找到」）

现象与实测：用户在聊天里的引导卡上点了安装（`rustup component add rust-analyzer`），`rustup component list --installed` 确实有 `rust-analyzer-aarch64-apple-darwin`，二进制落在 `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rust-analyzer`；但 `~/.cargo/bin`（rustup 的 shim 目录）**根本不存在**（Homebrew 装的 rustup 不创建它），PATH 里自然也没有 `rust-analyzer` → 只查 PATH / shim 就永远「未找到」。

改动：新增 `rustup_toolchain_bins(home, rustup_home)`，扫 `$RUSTUP_HOME/toolchains/<toolchain>/bin`（默认 `~/.rustup/…`），只对 Rust 生效、目录名排序保证可复现、追加在最后（不抢 PATH 与 `~/.cargo/bin` 的优先级）；`conventional_bin_dirs_for` 负责接线。单测覆盖：工具链 bin 进扫描面 / 只对 Rust 生效 / `RUSTUP_HOME` 覆盖后不再看默认 home / 排序与跳过无 `bin` 的条目。

验证：`cargo test` **817 passed**（+17 集成）、`cargo fmt --check` 干净、`cargo clippy --lib` 零警告。

> 注意：本修复在**源码**里，需要重新构建（`pnpm tauri dev` 或重新打包）才会在界面上生效；旧打包版仍只查 PATH。

### B. 设置页状态与动作（前端）

| 文件 | 改动 |
|---|---|
| `ipc/types.ts` | 同步 `LspSdkStatus` / `LspInstallRequirement`、`source` 加 `"lang_bin"`、`LspServerStatus.sdk` |
| `features/panels/SettingsPage.tsx` | `lspBadge` 四态（已找到 / 语言服务器未安装 / 语言服务器未安装且工具链未找到 / 已关闭），后端 `detail` 走 Tooltip 不占行高；新增 `sdkText`（工具链段，开关关闭时不显示）与 `lspAction`（`installable` → 「安装」按钮，前置缺失时禁用并提示「需先安装 Node.js」+ 下载入口；`manual` → 「手动安装」链接走 `open_url`）；安装成功后重拉 `lsp_status` 刷新。**仅追加字段**：`.validation-status` 文本不变（SDK 段是同级新 span），既有断言不破 |
| `theme/app.css` | `.validation-row` 第 2 列 `auto` → `max-content`（附注释说明原因）；新增 `.validation-cell` / `.validation-sdk` / `.validation-hint` |
| `i18n/zh-CN.ts` / `en-US.ts` | `validationHint` 改为「探测的是各语言的**语言服务器**，不是语言本身」；新增 10 个键（未安装 / 工具链就绪 / 未找到工具链 / 安装 / 安装完成 / 安装失败 / 需先安装 / 下载 / 手动安装） |
| `features/panels/settingsRegistry.ts` | 10 个新键进豁免清单；`active_model_id` 去掉 `advanced: true` |
| `features/panels/ProvidersPanel.tsx` | 「当前」标记不再加 `settings-advanced-hidden`；`advancedVisible` prop 全链路移除（无死代码） |
| `features/panels/SettingsPage.tsx`（代理卡片） | 卡片结构改为「`div.proxy-mode-card` + 内部 `label.proxy-mode-main`（标题/说明/回显，整块可点）+ 兄弟 `div.proxy-mode-field`（仅在选中自定义模式时渲染）」，代理地址输入连同标签与说明移进卡片内部；顺带消除 label 嵌套（HTML 不允许 label 嵌 label） |

## 三、验证

- `cargo test`（`src-tauri/`）：**812 passed / 0 failed / 3 ignored**（lsp 模块 103 例）+ 集成测试 **17 passed**；`cargo fmt --check` 干净；`cargo clippy --lib` **0 warning**（仓库既有基线：生产代码零警告）。
- `pnpm --dir ui test`：**648 passed / 67 文件**；`pnpm --dir ui build`（tsc + vite）通过；`pnpm --dir ui lint` 干净。
- 本机实测（探测修复的证据）：`zsh -lc` 的 PATH 里**没有** `/Users/…/go/bin`，`zsh -lic` 里有且耗时约 41ms → Go 行应由「未找到」变为「已找到」（待手动确认）。

### 新增测试（要点）

- `discovery.rs`：约定目录按语言区分与环境变量优先、`merge_shell_paths` 保序追加、哨兵提取忽略 rc 噪声、`resolve_with` 命中 `lang_bin`、显式配置仍优先于约定目录。
- `install.rs`：安装计划带运行库名与下载页；解析不到前置时文案含「Node.js + 下载页」；**约定目录回退**（PATH 空、目录里有可执行文件时必须能执行）。
- `settings.lsp.test.tsx`：四态文案、工具链段、悬停显示后端原因、行内安装发 `lsp_install` 并重拉状态、前置缺失时按钮禁用 + 提示 Node.js + 下载入口、手动安装形态不得再给「安装」按钮。
- `settings.page.test.tsx`：`.validation-row` 列定义正则断言（**只锁规则文本**——happy-dom 无布局引擎，真实尺寸靠手动验证清单，测试里已写明）。
- `settings.registry.test.ts`：进阶项 11 → 10、providers 页 1 → 0。

## 四、代码审查（reviewer，跨层）

**结论：通过（无 🔴）**。对齐表逐项 ✅；重点核查：契约面向后兼容（`#[serde(default)]` + 前端类型 + 事件载荷显式取字段，`requires`/`sdk` 不进 `lsp:server_missing` 载荷）、事件面零变化、`lang_bin` 三处（类型 / 模块头注释 / 本文档）一致、约定目录不抢显式配置、Windows 分支自洽、`cfg(test)` 的 `Discoverer::with_path` 隔离了本机环境、CSS 断言诚实。

审查提出的 🟡 已全部修掉：

1. **判据不一致（本轮最重要）**：设置页的「安装按钮可点」走 `locate_command`（含约定目录），而安装执行器走 `resolve_program`（只查 PATH）→ Rust 行会出现「按钮可用、点了说未找到 rustup」。修法：新增 `resolve_install_program`，两处判据同源（含用例钉死）。
2. 文档里 `client.rs` / `manager.rs` / `pool.rs` 的行数与 `SettingsModal.tsx` 死链接 → 已更正。
3. `manager.rs` 的「不改 `ServerStatus` 字段」注释 → 统一为「前后端契约，只允许纯追加新字段」（`client.rs` 同步）。
4. 文档补「首次探测最坏 10s（两次 shell 各 5s）+ 可能落在首次写入路径 + 超时不杀进程」。
5. `install.rs` 文案说「请先安装 npm」→ 统一说「Node.js」（`InstallPlan` 带 `runtime_name`）。
6. Windows 的 `%APPDATA%\npm` / `%LOCALAPPDATA%\pnpm` 从 `home` 分支挪进环境变量分支（`home_dir()` 取不到时也要能推）。

**遗留（未修，已知并记录）**：① shell 读取超时只放弃结果、不杀进程（可复用 `kill_process_group` 同类手法）；② 前端 `st.sdk.ready` 未做可选链（前后端同一二进制、不存在「新前端配旧后端」，属理论风险；若要贴合页面自身的「旧后端」防御口径可写 `st.sdk?.ready ?? true`）；③ `sdk.rs` 里一条自洽性断言（`probe` 结论与 detail 不矛盾）在本机环境下偏弱，建议后续改成注入式断言；④ 跨语言并发点两次「安装」时，两次 `lsp_status` 回填顺序不保证（极小概率短暂显示旧结论）。

## 五、手动验证清单（界面改动不做自动点验）

前置：确认没有 dev 实例与打包版并存，再 `pnpm tauri dev`（仓库根）。

1. **探测补齐**：设置 → 工具与集成 → 写入后语义校验：Go 行应由「未找到」变为「已找到」（本机 `~/go/bin/gopls`），悬停状态文本能看到来源原因；Rust / Python / Java / TypeScript 行应显示「语言服务器未安装」+ 工具链就绪情况（如「Rust 工具链 已就绪」）。
2. **安装入口**：TypeScript / Python 行的「安装」按钮应为禁用态并提示「需先安装 Node.js」+ 「下载」（本机无 npm）；点「下载」应打开系统浏览器到 Node.js 下载页。Rust 行前置为 rustup（本机在 PATH 中）→ 按钮可点。
3. **手动形态**：Java 行（打开开关后）应给「手动安装」链接而非「安装」按钮。
4. **开关尺寸**：六语言行 + JSON 行的 Switch 应恢复为正常尺寸（约 28×16），宽窗与窄窗各看一次；确认开关与命令输入框之间没有异常空档。
5. **代理卡片**：「网络与连接」→「自定义代理」卡片内部应有「代理地址」标签 + 输入框 + 说明；切换「无代理」时输入框随卡片一起消失；填 `ftp://…` 应出现红字且保存被拦。
6. **进阶项**：「模型与供应商」页不应再出现「显示进阶项」开关行；进入某供应商的编辑视图，模型列表里的「当前」标记应常态可见。
7. **亮/暗主题**各看一次；中英切换检查新文案。
8. 「重新探测」按钮行为不变；安装成功后无需手动重探（页面自动刷新）。
