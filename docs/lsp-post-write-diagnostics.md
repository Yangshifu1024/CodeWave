# LSP 写后语义校验 · 实施报告

> **⚠️ 已被取代（2026-09-20）**：本机制整体删除，改为「写入后检查命令」——见
> [post-write-check-plan](./post-write-check-plan.md)。删除动因：写前干净文件永远判「未就绪」不回喂（缺陷 A），
> 且结论只达前端 `warnings`、模型侧读不到（缺陷 B）。本文保留为历史实施报告。

> 本批次把 create / edit 写后校验从**单文件外部命令**（`tools/validation.rs` 按扩展名路由）升级为**项目级常驻 LSP 语义诊断**：新增后端顶层 `src-tauri/src/lsp/`（与 `mcp/` 平级，纯 Rust 不依赖 tauri），覆盖 TypeScript/JavaScript、Rust、Python、Go、Java、Dart 六语言，JSON 继续走内置 `serde_json` 解析。
> 同时记一笔决策推翻：本文件对应 [builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §13.4 / §15 P3-1 中「LSP 成本高、故意后置」的结论——该结论已作废，见 §2。

## 1. 背景与动因

### 1.1 旧实现

`src-tauri/src/tools/validation.rs` 按扩展名路由到**单文件外部命令**：

| 扩展名 | 命令 | 超时 |
|---|---|---|
| `.py` | `python3 -m py_compile <file>` | 60s |
| `.rs` | `rustc --emit=metadata --crate-type lib <file>` | 60s |
| `.ts` / `.tsx` | `npx --yes typescript@5 tsc --noEmit <file>` | 60s |
| `.js` / `.vue` | `node --check <file>` | 60s |
| `.go` | `go vet <file>` | 60s |
| `.json` | 内置 `serde_json` 解析 | — |

### 1.2 三条结构性问题

**（一）误报是结构性的，不是 bug。** 单文件编译器看不到工程上下文：

- `rustc` 单文件编译看不到 crate 兄弟模块与 Cargo 依赖 → 凡含 `use crate::...` 的文件几乎必挂（E0432/E0433 未解析导入）；
- `tsc` 单文件调用看不到 `tsconfig.json` 与 `node_modules` → 凡含 `import react from "react"` 的文件几乎必报 TS2307；
- `go vet` 单文件调用看不到同包其他文件 → 跨文件符号全部「未定义」。

**（二）冷启动成本。** `npx --yes typescript@5` 每次写入都走一遍解析 / 下载检查，60s 预算基本被它吃掉。

**（三）假通过。** 工具链缺失时旧路径返回 `ran=false, ok=true`，`summarize()` 把它渲染成**空串**——单文件场景下等于**静默不告知**；多文件批次里只要有一个文件真跑过（例如 `.json` 走内置解析），整批文案就显式打出「（写入后语法校验通过）」（**历史文案**：那是旧路径当时的实际输出，现行设置项定名是「写入后**语义**校验」，见 [settings-terminology](./settings-terminology.md) §1），即**假通过**。

本批次把第三条做成结构性不可达：**未真的运行过，文案里永不允许出现「通过」**（见 §3.4）。

## 2. 决策推翻记账

[builtin-tools-source-comparison](./builtin-tools-source-comparison.md) 曾把 LSP 评估为 P3（远期）项，理由为「常驻进程管理成本高，当前 validation 轻量方案是合理取舍」。本次正式推翻：

- §13.4 结论段 → 改为「已落地，见本文件」；
- §15 P3-1 行 → 标注已完成并给链接。

推翻依据即 §1.2 三条：单文件路线的问题不是「不够强」，而是**误报结构性、成本每写必付、失败静默**——三者都无法在单文件模型内修好。

## 3. 落地形态

### 3.1 架构与模块

新增后端顶层模块 `src-tauri/src/lsp/`，与 `mcp/` 平级，**纯 Rust 不依赖 tauri**；`tools/validation.rs` 消费它，`host/commands/lsp.rs` 只做参数校验 + 转调。

| 文件 | 行数 | 职责 | 关键位置 |
|---|---|---|---|
| `lsp/mod.rs` | 347 | 公共契约类型：`Lang`、`DiagnosticItem`、`ValidationOutcome`、`SkipReason`、`ValidateRequest`、`Baseline`、`InstallKind`/`InstallHint`（含 `requires`）、`SdkStatus`、`ServerStatus`（含 `sdk`） | `Lang` L37、`ValidationOutcome` L169、`SkipReason` L204、`ValidateRequest` L227、`Baseline` L246 |
| `lsp/protocol.rs` | 306 | `Content-Length` 分片 framing（编码 / 写 / 读 + 头部与帧长上限） | 上限 L11/L14、`encode_message` L45、`write_message` L53、`FrameReader` L69、`read_message` L154 |
| `lsp/client.rs` | 1348 | 单连接：spawn、stderr 独立泵、读循环三态分发、服务端主动请求应答、Full 同步、URI 编解码、shutdown | 超时、`spawn_resolved`、`initialize`、`did_open`、`did_change`、`diagnostics_for`、`read_loop`、`pump_stderr`、`kill_process_group`、`path_to_uri` |
| `lsp/discovery.rs` | 1495 | 新鲜 PATH（登录非交互 + 交互登录双壳合并）+ 六语言探测（含语言约定安装目录）+ JDK 21 定位 + 版本探测 | `merge_paths`、`conventional_bin_dirs`、`fresh_env_path`、`shell_env_path`、`which_in`、`find_jdk21`、`java_launch_env`、`install_hint`、`resolve` / `resolve_with` |
| `lsp/sdk.rs` | 120 | 语言工具链（SDK）就绪探测：命令存在性（`node`/`cargo`/`go`/`python3`/`dart`）+ JDK 定位；只做文件存在性检查、不起进程 | `targets`、`probe` |
| `lsp/server_spec.rs` | 334 | 六语言静态表（server 名 / languageId / init options / 安装形态与官方地址 / 前置运行库名与下载页） | `spec`、`all_specs`、`assemble_init_options` |
| `lsp/workspace.rs` | 191 | root 判定（按 manifest 就近上溯，找不到回落项目根） | `has_manifest` L27、`scan_roots` L38、`root_for_file` L77 |
| `lsp/diagnostics.rs` | 455 | 指纹 / 差集 / 刹车 / 解析 / 文案渲染 | `fingerprint` L26、`diff` L37、`DedupBrake` L54、`parse_diagnostics` L119、`format_feedback` L230 |
| `lsp/pool.rs` | 464 | 项目级池：key `(project_id, root, language)`，惰性启动 + 闲置回收 + 并发上限 | `PoolKey`、`acquire`、`tag_project`、`evict_idle`、`shutdown_project` |
| `lsp/manager.rs` | 702 | 门面：写前基线 / 写后校验 / 异步补条 / 状态 / 重探测 / 重启 / 回收 | `baseline`、`validate`、`validate_async`、`status`、`redetect`、`restart`、`evict_idle` |
| `lsp/install.rs` | 547 | 安装计划与执行（含前置运行库缺失时的可操作文案） | `InstallPlan`（含 `runtime_docs_url`）、`plan`、`execute`、`missing_runtime_message` |

客户端层是**通用通道**（能力协商 + 可发任意 request + 服务端主动请求应答分发），本批次只交付写后诊断；将来加语义查询（definition / references / hover）只需加封装，不动协议层。

### 3.2 六语言与 server

| 语言 | server | 启动 | 备注 |
|---|---|---|---|
| TypeScript / JavaScript | `typescript-language-server --stdio` | npx 降级；`tsserver.path` 指向项目内 typescript | `.js/.jsx/.mjs/.cjs` 并入同一 server，`languageId` 按扩展名区分（`typescriptreact` / `javascriptreact` / `javascript` / `typescript`，`mod.rs` L105） |
| Rust | `rust-analyzer` | 标准 stdio | 关闭 `checkOnSave`（走 native diagnostics，避免与自带的 cargo check 双跑） |
| Python | `pyright-langserver --stdio` | npx 降级 | — |
| Go | `gopls` | 标准 stdio | — |
| Java | `jdtls` | **默认关闭**，只探测不代装 | 启动显式使用**自己探测到的** JDK 21+（`java_launch_env` L525 注入子进程 `JAVA_HOME`），**绝不读机器的 `JAVA_HOME`**——实测它常指向旧版本 |
| Dart | `dart language-server` | 零安装 | 可由 `flutter` 位置反推 Dart SDK |

`.vue` / `.svelte` 不在六语言之列 → 明确 `Unsupported`（文案「该文件类型不做语义校验：vue」），**不静默**。`.json` 走内置 `serde_json` 独立轻量路径，不经过 LSP。

### 3.3 时序（混合策略）

```
写前：manager.baseline() → didOpen(旧内容) 建基线（此刻盘上还是旧内容）
写入：atomic_write            ← 不等任何 server
写后：manager.validate() → didChange(新内容) → 最多等 sync_window_ms（默认 1500ms）
```

等待窗口结束后按四种情形分流：

1. **基线与新诊断都有** → 算差集 → 只回喂 `severity == 1`（error）的**新增**项 → 指纹刹车（`DedupBrake`，同指纹同文件回喂上限 `dedupe_limit`）→ 预算截断（`max_diagnostics` 条 / `max_chars` 字符）；
2. **只有新诊断、基线不可信**（`Baseline.reliable == false`）→ **不回喂**，返回 `Skipped{ServerNotReady}`「语义校验未就绪：server 正在预热，本轮未回喂诊断」——**既不回喂、也不补偿补条**（补发同样没有可信基线）；
3. **超窗**（基线可信、只是诊断迟到）→ `Skipped{ServerLoading}`「server 正在启动，稍后补发诊断」+ 异步补条；
4. 异步补条经**既有** `run:inject` 通道（`SessionRuntime::inject_tx`，与 `inject_run_message` 同一通道）注入一条合成消息，前缀标注「异步补发的语义校验结果」（`tools/validation.rs` L362 `spawn_async_supplement`）；会话已结束 / 已取消 / 已删除时静默丢弃并记日志。

两句「未就绪」的分工（**不得混用**）：

| 情形 | `SkipReason` | 回喂 | 异步补条 |
|---|---|---|---|
| 基线不可信（冷启动 / 预热中） | `ServerNotReady` | 否 | **否** |
| 基线可信、诊断迟到（超窗） | `ServerLoading` | 否（本轮） | 是 |

设计要点：基线不可信时**宁可不回喂**。差集的前提是基线可信——拿不存在的基线去「新增」会退化成把全项目误报喷给模型。

### 3.4 三态与跳过原因

```rust
pub enum ValidationOutcome {
    Passed { lang },
    Diagnosed { lang, items, removed, truncated },
    Skipped { lang: Option<Lang>, reason: SkipReason },
}
pub enum SkipReason { NoServer { server }, ServerLoading, ServerNotReady, Unsupported, Disabled, FileTooLarge { bytes }, NoProject }
```

- `ServerLoading` = **server 正在启动 / 诊断超窗**（基线可信，承诺异步补条）；
- `ServerNotReady` = **基线不可信**（就绪证据不足 / 预热中）→ **不回喂也不补条**（见 §3.8）；两者文案必须可区分，不得互相冒充。

- `ValidationOutcome::ran()`（`mod.rs` L197）在 `Skipped` 时恒为 `false`；`outcome_text()`（`tools/validation.rs` L199）中**只有 `Passed` 分支允许出现「通过」字样**，且有单测硬锚点 `skipped_texts_are_explicit_and_never_claim_success`（断言文案不含「校验通过」/「语义校验通过」）。
- 逐文件成文：`summarize()`（L255）把每个文件的结论各自渲染后拼接，**绝不因为批里某个文件跑过就给整批打「通过」**（单测 `summarize_keeps_per_file_verdicts` 断言 «校验通过» 只出现 1 次）。
- `removed`（顺带消除的既有 error 条数）是正反馈；`truncated > 0` 时文案必须明示「已截断 N 条」。
- 单条渲染格式：`<相对路径>:<行>:<列> <code> <message>`（`DiagnosticItem::render`，`mod.rs` L157），行列均 **1-based**。

### 3.5 配置（`core/config.rs`，全部 serde default）

旧 `python` / `rust` / `typescript` / `go` / `json` 五个 bool **保留字段名与默认值**，语义升格为对应语言的 LSP 开关（`ValidationSettings` L323）；新增 `java`（默认 **false**）与 `dart`（默认 true）。

新增 `validation.lsp` 子对象（`LspSettings` L378，`Default` 见 L401–L414）：

| 字段 | 默认值 | 语义 |
|---|---|---|
| `commands{typescript,rust,python,go,java,dart}` | 空 | 各语言启动命令覆盖（留空 = 自动探测） |
| `java_home` | `""` | jdtls 使用的 JDK 21+ 路径（留空 = 自动探测） |
| `extra_roots` | `[]` | 额外 SDK 根目录（探测 `<root>/<lang>/bin`） |
| `sync_window_ms` | `1500` | 写后同步等待诊断的毫秒预算 |
| `max_diagnostics` | `20` | 单次回喂条数上限 |
| `max_chars` | `4000` | 单次回喂文本字符上限 |
| `idle_ttl_ms` | `600000` | 项目级 server 闲置回收时长 |
| `max_servers` | `8` | 单项目并发 server 上限 |
| `max_file_bytes` | `1048576`（1 MiB） | 超过该体积跳过语义校验 |
| `dedupe_limit` | `2` | 同一诊断指纹在同一文件最多回喂次数 |

全部新字段带 `#[serde(default)]`（结构级 + 字段级），旧配置读到新版本不会失败。

### 3.6 契约变更

**IPC（新增 5 条，`host/commands/lsp.rs`）**

| 命令 | 位置 | 用途 |
|---|---|---|
| `lsp_status` | L45 | 六语言状态（设置页徽标） |
| `lsp_redetect` | L52 | 换新 PATH 快照 + 清探测缓存后重查 |
| `lsp_restart` | L60 | 重启某语言全部实例 |
| `lsp_enable` | L71 | 启用并**持久化**（先落盘再改内存，失败时内存与磁盘一致） |
| `lsp_install` | L89 | 一键安装（顺序：幂等探测 → 形态校验 → 围栏 → 执行 → 重探测） |

**事件面 28 → 29 键**：新增 `lsp:server_missing`，`kind ∈ {installable, manual, confirm_enable}`。前后端双向契约测试守护（`ui/src/__tests__/events.contract.test.ts` L68「事件面键数为 29（最近一次新增：lsp:server_missing）」）；handler 落在 `ui/src/stores/runHandlers.ts` L376，视图模型见 `ui/src/stores/run.types.ts` L152。

**前端**

- 设置页六语言行（开关 / 命令覆盖 / 状态与动作：语言服务器状态 + 工具链状态 + 安装入口）+ 预算 + 发现区（额外 SDK 根目录、JDK 21 路径）+「重新探测」（`ui/src/features/panels/SettingsPage.tsx`；行渲染与 `lspBadge` / `sdkText` / `lspAction` 同文件）；
- 聊天内嵌引导卡片三景（`ui/src/features/chat/LspGuideCard.tsx`：`installable` 调 `lsp_install` L42、`manual` 展示前置条件与官方地址、`confirm_enable` 调 `lsp_enable` L58）；**同会话同语言不重复打扰**；挂载点在 `ui/src/features/chat/ChatMessages.tsx` L464；
- i18n 中英双语（`zh-CN.ts` / `en-US.ts` L98、L145）。

### 3.7 新鲜 PATH（本批次的真实痛点）

Rust GUI 应用从桌面启动时拿不到用户 login shell 里 export 的路径，且**进程环境快照会过期**。

实测：装完 JDK 21 与 Flutter 3.47.4 后，CodeWave 进程环境里 `JAVA_HOME` 仍指向 jdk-17，PATH 里既没有 jdk-21 也没有 `D:\Sdk\flutter\bin`。靠进程快照永远探测不到，除非重启应用。

因此 `discovery.rs` 的探测策略是：**先在进程 PATH 里找，未命中时再读系统权威 PATH**——

- Windows：读 `HKCU\Environment` + `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` 合并（用户优先，`merge_paths`），展开 `%VAR%`（`expand_vars`）；
- macOS / Linux：**登录非交互（`-lc`）与交互登录（`-lic`）各跑一次并合并**（`shell_env_path` + `merge_shell_paths`），查询用哨兵行 `printf '__cwave_path__%s\n' "$PATH"` 提取（交互式 rc 的输出不会混进 PATH），每次尝试 5s 超时；两段都失败才回落进程快照。

  为什么必须问两次（2026-09-19 用户反馈后补）：`~/.zshrc` / `~/.bashrc` 只被**交互式** shell 读取，而 `~/go/bin`、`~/.bun/bin`、nvm 的 node 这些目录恰恰写在里面——只问 `-lc` 会让「装好的 server 看不见」（本机实测：`zsh -lc` 的 PATH 里没有 `~/go/bin`，`zsh -lic` 里有，耗时约 40ms）；
- 该次读取**只做一次**并缓存（`OnceLock` 无锁快路径），另有手动「重新探测」（`lsp_redetect`）兜底；
- **最坏预算**：两次 shell 查询各 5s 超时，故首次探测最坏 10s，而它可能落在**首次写入路径**上（缓存建立后不再发生）；交互式 shell 实测约 40ms，正常机器无感。超时分支只放弃该次结果（读取线程孤立结束），**不杀进程**——已知留待改进项（可复用 `kill_process_group` 的同类手法）。

### 3.7b 语言约定安装目录（`go install` 类落点）

`go install` 落 `$(go env GOPATH)/bin`（默认 `~/go/bin`）、`rustup component add` 落 `~/.cargo/bin`（shim）**或工具链目录** `~/.rustup/toolchains/<toolchain>/bin`、bun/pnpm 全局落 `~/.bun/bin` / `~/Library/pnpm`——这些目录进不进 PATH 完全取决于用户的 shell 配置，故在探测顺序里单列一档（`conventional_bin_dirs`，见 §3.7c）；其中 rustup 的工具链目录还需要**读目录**才能得出（`rustup_toolchain_bins`，2026-09-19 补：Homebrew 装的 rustup 不建 shim 目录，实测「组件装成功却始终未找到」）。

为什么不能只靠 PATH：否则「一键安装成功」与「显示已找到」之间会断链（安装命令写的就是 `go install`，产物落到 `~/go/bin` 却探测不到）。

### 3.7c 探测顺序（2026-09-19 起）

① 设置里的命令覆盖 → ② 项目内 `node_modules/.bin` → ③ 新鲜 PATH → ④ **语言约定安装目录（`source = lang_bin`）** → ⑤ 进程快照 PATH → ⑥ `extra_roots` → ⑦ npx 降级（TS/Python）→ ⑧ Dart/Flutter 反推。

### 3.8 就绪判据（空集基线只认强证据）

server 启动后推的第一个 `publishDiagnostics` 往往是**空数组占位**，它与「这个文件真的 0 错误」在协议上
完全同形。把它当「文件本来就干净」会让 server 随后推出的**项目存量错误**在写后差集里全部变成「本次
新增」并回喂给模型——这正是本需求要消灭的痛点，故基线必须靠**实例级就绪证据**（`lsp/client.rs` 的
`Readiness`，`manager.rs::baseline` 消费）决定可信性：

| 证据 | 含义 | 空集基线 | 非空基线 |
|---|---|---|---|
| `Quiescent` | rust-analyzer 报 `experimental/serverStatus.quiescent == true`（索引完成，权威信号） | 可信 | 可信 |
| `Analyzed` | 本实例推送过**非空**诊断（任意文件）→ 确实在分析 | 可信 | 可信 |
| `Warm` | 距 `initialize` 成功已超过**该语言的预热窗**且累计推送 ≥ 2 次 | **不可信** | 可信 |
| `None` | 尚无证据（冷启动） | 不可信 | 不可信 |

- **预热窗按语言分档**（`server_spec.rs` 的 `ServerSpec::warmup_ms`，不再是统一的 3s）：TS/Python 10s、
  Go/Dart 20s、Java/Rust 30s（jdtls 还要索引依赖树；rust-analyzer 有 `quiescent` 兜底，档位取最保守值）。
  档位越重、索引越贵，等待越长；调大是安全方向。
- **`Warm` 不再单独为空集背书**：它只证明「时间够久且出过声」，而出声内容可能全是占位空集——
  全干净项目连推 N 次空集就能凑出 `Warm`。所以空集基线只认 `Quiescent` / `Analyzed`。
- **代价（有意接受）**：全干净项目在首轮会显示「未就绪」而不是「通过」；待 server 预热完毕（或一旦
  推过任何非空诊断）后的下一次写入就恢复正常。**宁可说未就绪，也不谎报通过**。
- 不可信基线一律：`reliable=false` → `ServerNotReady` → 不回喂、不承诺补发（与 §3.4 一致）。
- **可观测性**：同一 server 实例**首次**降级时记一条 `tracing::warn!`（含语言 / server 名 / 可执行文件名 /
  就绪证据标签 / 是否空集基线）——避免「功能哑火且查不出原因」；之后降回 `debug` 不刷屏。

### 3.9 安装链路：可执行文件解析（上一轮补记）

`lsp_install` 的执行层（`lsp/install.rs`）不做「拿裸名就 `spawn`」这种必败动作：

- **按 `PATHEXT` 补全形态优先**（`discovery::which_in` 的候选序：`npm.cmd`/`npm.exe` 在前，裸 `npm` 在最后）
  ——Windows 上 `npm` 是 Git-Bash 脚本、不是可执行文件，`CreateProcess` 直接失败；
- **`.cmd` / `.bat` 经 `%COMSPEC% /C`**（批处理不能被 `CreateProcess` 直接执行），命令行由
  `cmd_command_line` **显式拼**（每个 token 引号包裹）+ `/D /S /C` 原样追加，避免路径含空格 / `&` 时被
  切成两条命令；`%` / `!` 在 cmd 里无法转义，遇到只记 warn；
- **解析不到直接 `Err`**（文案写明名字：「未找到 `npm`，无法执行安装」），绝不静默地拿裸名去 spawn；
- 新鲜 PATH 优先、失败回落进程快照；两路读取 stdout/stderr 并发（否则写满 64KB 管道卡到超时）。

## 4. 责任边界（必须让用户看到的边界）

**Dart / Flutter 项目需先 `flutter pub get` 生成 `.dart_tool/package_config.json`**，否则 analysis server 会把 `package:` 导入报成未解析。

**这是项目依赖未装，不是校验器坏了**——与 §1.2（一）的旧误报是两回事：旧误报源于校验器缺工程上下文，这里校验器是正确的，缺的是项目自己的依赖解析信息。文档与引导卡片都应据此措辞。

## 5. 测试与验证结果

### 5.1 后端

- `src-tauri`：`cargo test` → **780 passed / 0 failed（另 3 ignored）**（交付前由主会话亲测；既有 flaky `provider::tests_integration::midstream_disconnect_maps_to_network` 本次同批通过，并行下仍可能偶发失败）；
- 新增集成测试 `src-tauri/tests/lsp_integration.rs`：**17 项全绿**，覆盖分片 framing、服务端主动请求应答、`initialize` 超时、崩溃重启一次后不可用、非协议输出不挂死、severity 过滤、指纹刹车、预算截断、基线差集、冷启动「未就绪」、**连续空集跨预热窗仍不回喂**、暖机后正常差集、三景卡片文案、临时会话跳过、`.vue` 未覆盖、JSON 保留、退出无孤儿进程；
- 假 server 用 `src-tauri/src/bin/codewave-fake-lsp.rs` + `CARGO_BIN_EXE_` 自举，**零外部依赖**（不需要真装 rust-analyzer / tsserver 等）。

### 5.2 前端

- `ui`：`pnpm test` → **545 passed / 65 文件**（含事件契约三条：双向扫描 + 29 键硬锚点）；
- `pnpm build`（tsc + vite）通过。

### 5.3 基线对照

| 侧 | 改动前 | 本次增量 | 现在 |
|---|---|---|---|
| 后端 | 670 passed | +110 | 780 passed |
| 前端 | 465 passed / 57 文件 | +80 / +8 文件 | 545 passed / 65 文件 |

## 6. 已知限制与偏离（如实记录）

1. **`lsp_install` 未走 `safety::approval` 审批门** —— 审批门 `safety::approval::confirm` 强绑定**运行中的会话 runtime**（`Arc<SessionRuntime>` + 取消 token + ask 应答路由），而 host 层无会话上下文、命令入参只有 `language`（`ui/src/ipc/client.ts` 的 `lspInstall` 契约），事件面又已锁 29 键不能再加审批事件。改为「**前端卡片已完整展示命令 + 用户显式点击**」作为确认依据；命令本身仍过 fence 策略检查（`Block` 直接拒绝，灾难级照样拦）。**这是本批次唯一的安全语义让步**。
2. `lsp:server_missing` 在「语言默认关闭（Java）」时也发 `kind=confirm_enable` —— 否则该分支不可达（`tools/validation.rs` L303–L309 有说明与守卫：只有**默认就关**的语言才走这条，用户自己关掉的语言不会收到这张 Java 专属文案的卡片）。
3. `codewave-fake-lsp` 会随生产构建一起编译出来（仅测试用，不被任何运行期路径引用）。
4. `install.rs` 的执行输出只保留前 4000 字符；host 侧再取尾部 800 字符入文案（`host/commands/lsp.rs` L20 `OUTPUT_TAIL_CHARS`），完整输出只进日志。安装超时预算 600s（L17）。
5. 下面是**未真机验证**项，见 §7。
6. **死路**：若某 server 在 `didOpen` 后从不推送诊断（连空集都不推），该文件永远建不起可信基线 → **永不回喂**。这是保守侧的代价：不误报，但也不说话。

## 7. 未真机验证项（待人工确认）

以下均**未真机验证**（本批次验证依赖假 server 集成测试，不依赖真实工具链）：

1. `gopls` 的 `analyses` 键名是否与 `server_spec.rs` 中写入的 init options 一致；
2. `pyright` 是否需额外 `initializationOptions`；
3. `jdtls` 启动参数是否需 `-configuration` / `--jvm-arg`；
4. `dart language-server` 子命令与最低 SDK 版本要求；
5. rust-analyzer 在写后窗口（默认 1500ms）内推送诊断是否稳定；
6. 新鲜 PATH 的端到端效果（装完工具链不重启应用即可发现）；
7. `typescript-language-server` 与项目内 `tsserver` 的版本匹配行为。

## 8. 手动验证清单（交付物）

按序逐条执行。前置：确认没有 dev 实例在跑（`tauri dev` 与打包版同 bundle id 互斥）。

| # | 步骤 | 期望 |
|---|---|---|
| ① | 前端项目里写 `ui/src/x.ts`，内容含 `import React from "react";` | **不出现** TS2307 假误报 |
| ② | 在该文件加一行类型错误 | 收到 `<相对路径>:<行>:<列> <code> <message>` 形态的回喂 |
| ③ | 写 `src-tauri/src/.../x.rs`，用 `use crate::...;` + 一行类型错 | 无 unresolved import 误报，只报那行类型错 |
| ④ | 打开设置页看六语言状态徽标与版本；把某语言命令覆盖改成非法值 | 徽标显示「未找到」；此后写入该语言文件的文案是「未找到 …」而**不是**「通过」 |
| ⑤ | 临时会话里写 `.ts` 文件 | 文案「（临时会话不做语义校验）」 |
| ⑥ | 把 `rust-analyzer` 从 PATH 挪走 | 聊天出现引导卡；点安装后走安装命令并自动重探测（自收） |
| ⑦ | 写 `.java` 文件 | 出现「默认关闭」卡；点启用后 `~/.codewave/config.json` 里 `validation.java=true` 生效 |
| ⑧ | 闲置 10 分钟（`idle_ttl_ms=600000`）后观察 | server 进程消失；再写文件重新冷启动 → 文案是「语义校验未就绪：server 正在预热，本轮未回喂诊断」，且**不出现**任何「异步补发的语义校验结果」消息（补条只在 `ServerLoading` 场合出现）；随后（server 预热完 / 推过非空诊断后）再写一次才开始正常回喂 |
| ⑨ | 写 `.vue` 文件 | 明确「（该文件类型不做语义校验：vue）」 |
| ⑩ | 观察内存占用 | 常驻 server 数与 `max_servers`（默认 8）与项目规模相符 |

## 9. 涉及文件

**新增（后端）**：`src-tauri/src/lsp/{mod,protocol,client,discovery,server_spec,workspace,diagnostics,pool,manager,install}.rs`、`src-tauri/src/host/commands/lsp.rs`、`src-tauri/src/bin/codewave-fake-lsp.rs`、`src-tauri/tests/lsp_integration.rs`。

**修改（后端）**：`src-tauri/src/tools/validation.rs`（单文件外部命令 → 三态 + LSP 接线）、`src-tauri/src/core/config.rs`（`java` / `dart` / `lsp` 子对象）、`src-tauri/src/tools/create.rs` 与 `src-tauri/src/tools/edit/`（写前基线 + 写后校验接线）、`src-tauri/src/host/commands/mod.rs`。

**修改（前端）**：`ui/src/ipc/{client,types}.ts`、`ui/src/stores/{run.types,runHandlers}.ts`、`ui/src/features/chat/{LspGuideCard.tsx,ChatMessages.tsx}`、`ui/src/features/panels/SettingsModal.tsx`、`ui/src/i18n/{zh-CN,en-US}.ts`。

**测试**：`ui/src/__tests__/{events.contract,lsp.guide-card,settings.lsp}.test.*`。

**外部资料**：

- LSP 3.17 规范（`Content-Length` framing / `textDocument/publishDiagnostics` / `didOpen` / `didChange` Full 同步）：https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/
- rust-analyzer：https://rust-analyzer.github.io/
- typescript-language-server：https://github.com/typescript-language-server/typescript-language-server
- Pyright：https://github.com/microsoft/pyright
- gopls：https://pkg.go.dev/golang.org/x/tools/gopls
- Eclipse JDT LS（jdtls）：https://github.com/eclipse-jdtls/eclipse.jdt.ls
- Dart analysis server：https://github.com/dart-lang/sdk/tree/main/pkg/analysis_server
- Flutter 依赖与 `pub get`：https://docs.flutter.dev/packages-and-plugins/using-packages
