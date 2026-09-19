# 写入后检查命令（取代 LSP 写后语义校验）· 方案

> 状态：**已实施**（2026-09-20）。实施结果：后端 `cargo test` 768 passed / 3 ignored、0 warning；前端
> `pnpm --dir ui test` 702 passed / 71 文件、`build` 通过；事件面 29 → 28 键。
> 实施补强（审查后）：`{file}` 替换按当前 shell 家族引用（防路径注入），命令未启动时 `ran=false`。
> 拟定分支：`feat/post-write-check`（实施基线取当时最新 `main`，非本文成文时的 `55fba2e`）。
> 相关文档：[lsp-post-write-diagnostics](./lsp-post-write-diagnostics.md)（现有 LSP 实现，本方案取代它）、[lsp-detection-and-settings-ux](./lsp-detection-and-settings-ux.md)、[settings-terminology](./settings-terminology.md)。

## 1 背景与动因

用户报告「LSP 似乎有问题」。核查后确认两个缺陷，第二个比第一个更根本。

### 1.1 缺陷 A：TypeScript/JavaScript 的写后校验永久处于「未就绪」

判定「写前那份诊断能不能作为对照基准」的规则在 `src-tauri/src/lsp/client.rs:70-76`：

```rust
pub fn ready_for(self, empty_baseline: bool) -> bool {
    match self {
        Readiness::Quiescent | Readiness::Analyzed => true,
        Readiness::Warm => !empty_baseline,
        Readiness::None => false,
    }
}
```

- 写前文件是干净的（诊断集为空）时，只承认两种证据：`Quiescent`（server 主动报告索引完成）或 `Analyzed`（本 server 实例推送过有内容的诊断）。
- `Quiescent` 只有 rust-analyzer 会报（`src-tauri/src/lsp/server_spec.rs`）；TypeScript 的语言服务器不报。
- 进入 `Analyzed` 的唯一途径是「某个被写的文件在写入之前本来就带着错误」。
- `Warm`（预热时间已过且 server 出过声）**不给干净文件的基准背书**，理由是设计者有意为之：全干净的项目连推几次空数组就能凑出 `Warm`，此时把空数组当成「本来就没错」，server 随后推出来的项目存量错误会被当成「本次写入引入」全部喷给模型。

结果是：**写前干净的文件，即使写入引入了新错误，也永远判「未就绪」、不回喂**。判定不可信后返回 `Skipped{ServerNotReady}`（`src-tauri/src/lsp/manager.rs:143-169`、`:407-414`），最终渲染成用户看到的那句「（… 语义校验未就绪：server 正在预热，本轮未回喂诊断）」（`src-tauri/src/tools/validation.rs:218-219`）。

运行日志实证（`~/.codewave/logs/codewave.log.2026-09-19`），四条同形态告警，覆盖 `ui/src/i18n/zh-CN.ts`、`FontSettings.tsx`、`run.ts`、`settings.ts`，时间从 10:47 到 15:24：

```
WARN lsp::manager: LSP 基线就绪证据不足：本轮不回喂、也不补发（同一实例仅报这一次）
  lang="typescript" server="typescript-language-server" executable="npx"
  evidence="warmup-elapsed" empty_baseline=true path=ui/src/i18n/zh-CN.ts
```

`empty_baseline=true` 且从未出现过 `Analyzed` 标签，说明这些 server 实例在整个生命周期里一条诊断都没成功回喂过。

附带发现：`executable="npx"` 说明本机没有安装 `typescript-language-server`，走的是 npx 临时解析启动，冷启动更慢、更容易撞上预热窗（预热档 10 秒，见 `src-tauri/src/lsp/server_spec.rs:68`；写后同步等待窗默认 1500 毫秒，见 `src-tauri/src/core/config.rs:419`）。

### 1.2 缺陷 B：校验结论从未到达模型（根本问题）

写后校验的结论只写进 `ToolOutcome.warnings`：

- `src-tauri/src/tools/create.rs:131-138`
- `src-tauri/src/tools/edit/tool.rs:547-558`

而送给模型的文本由 `compact_for_model()` 生成，它**只序列化 `outcome.data`**：

- `src-tauri/src/tools/compact.rs:12-42`（四个分支分别读 `error` 与 `data`，没有任何一处读 `warnings`）
- `src-tauri/src/tools/batch.rs:593-613`（`model_content()` = `compact_for_model(...)` + 可选的一条 plan 提醒）
- 那条 plan 提醒的来源极窄（`src-tauri/src/tools/batch.rs:582-587`：主会话 + 文件写工具 + 无错误 + 提醒未发过），与校验文案无关。

逐条核查全仓 `warnings` 的 62 处使用点，没有一条通向模型；唯一例外是「异步补条」那条注入路径（`src-tauri/src/tools/validation.rs:373` 的 `spawn_async_supplement`），而用户遇到的「未就绪」恰恰不走它。

最直接的证据是同一份代码里两句互相矛盾的注释：

- `src-tauri/src/tools/edit/tool.rs:349`：「回传模型（**warnings 只达前端，不进 compact 模型通道**）」
- `src-tauri/src/tools/edit/tool.rs:547` 与 `src-tauri/src/tools/validation.rs:1-2`：「经 warnings 回喂模型」

结论：**写后语义校验的结论只显示在界面上，模型一个字都收不到。** 这套机制的实际价值大致等于一个界面装饰。

### 1.3 缺陷 B 为什么长期没人发现

所有写后校验的用例断言的都是 `out.warnings`：

- `src-tauri/src/tools/create.rs:318-353`
- `src-tauri/src/tools/edit/tests.rs:178`、`:333`、`:530-532`

也就是说，测试断言的是一条宽松通道（前端可见），而产品实际依赖的是另一条通道（模型可见）。批次层其实早有断言模型侧文本的写法（`src-tauri/src/tools/batch.rs:1344` 的 `model_text_chunks`，用例在 `:1524`、`:1528`），却从未用在校验文案上。

### 1.4 规模

现有 LSP 机制合计约 8700 行（含测试）：后端 `src-tauri/src/lsp/` 11 个文件 6496 行、命令层 245 行、假测试 server 404 行、集成测试 933 行、`tools/validation.rs` 701 行，另有前端 9 个文件、21 个测试文件、13 篇文档、事件面第 29 键。

## 2 同类工具怎么做的

| 工具 | 是否用 LSP | 反馈方式 | 出处 |
|---|---|---|---|
| opencode | 有（内置约 35 种语言服务器），但**默认关闭** | 打开文件时取诊断；官方文档明确说 LSP 不总是净收益（会失同步、吃内存、随版本与项目而异、拖慢流程），**建议让 agent 直接跑 lint / typecheck 命令并写进 AGENTS.md** | https://opencode.ai/docs/lsp/ |
| aider | 没有 | 一条可配置的 lint 命令（`--lint-cmd`）：命令接收文件名、把错误打到输出、非零退出码即视为有错；测试同理走 `--test-cmd` | https://aider.chat/docs/usage/lint-test.html |
| codex CLI | 没有 | 靠 agent 自己跑命令 | https://github.com/openai/codex |

对照结论：LSP 矩阵本身不算过度（opencode 更宽），**「写后自动回喂」才是复杂度源头**——因为它必须自己回答「这条错误是我刚写出来的，还是项目本来就有的」，于是才有了写前基准、就绪证据、预热窗、差集、指纹刹车这一整套。aider 把「用什么命令检查」交给项目配置，命令本来就懂工程上下文，于是既不需要基准也不需要判据，同样做到了写后自动反馈。

而且本项目的 `AGENTS.md` 里已经写着「改后端 → `cargo test`；改前端 → `pnpm --dir ui test` + `build`」——这正是 opencode 推荐的那条路，只是还没接成自动反馈。

## 3 决策记录

| 决策项 | 结论 |
|---|---|
| 总体方向 | 删除整套 LSP 机制（含自动回喂），改为「写入后检查命令」 |
| 配置粒度 | 全局一条命令，在项目根目录执行，支持 `{file}` 占位符（对齐 aider 的 `--lint-cmd` 思路） |
| 安全检查 | 过安全围栏即可；用户自己配置的命令视为已授权，不逐次弹审批 |
| 实施范围 | 新增与删除同一批完成 |

## 4 契约

### 4.1 配置

后端 `src-tauri/src/core/config.rs` 新增结构（带 `#[serde(default)]`，顶层键 `post_write_check`）：

```rust
pub struct PostWriteCheckSettings {
    pub enabled: bool,          // 默认 false
    pub command: String,        // 默认空串；在项目根目录执行
    pub timeout_seconds: u64,   // 默认 30
    pub tail_chars: usize,      // 默认 3000，交给模型的输出尾部字符数
}
```

旧的 `validation` 段（`ValidationSettings` / `LspCommands` / `LspSettings`，`src-tauri/src/core/config.rs:330-428`）整体删除。旧配置里的这些字段会被 serde 静默忽略、首次保存后消失——这是删功能的必然结果，需写进发布说明。

### 4.2 工具结果

写入类工具的结果里增加 `check` 字段，**放在 `outcome.data` 里**（这是缺陷 B 的修复要点）：

```json
{ "path": "ui/src/x.ts", "bytes": 123,
  "check": { "ran": true, "ok": false, "command": "npx eslint ui/src/x.ts",
             "output": "…输出尾部…", "skipped": null } }
```

字段语义：

- `ran`：是否真的执行了命令。没执行时为 `false`。
- `ok`：命令退出码是否为零（`ran=false` 时无意义）。
- `command`：实际执行的命令（`{file}` 已替换）。
- `output`：合并后的输出尾部（最多 `tail_chars` 个字符）。
- `skipped`：未执行的原因（`null` 表示没有跳过）。取值：`disabled`（开关关闭）、`empty-command`（未配置命令）、`no-project`（临时会话无项目目录）、`blocked-by-fence`（被安全围栏拦下）、`timeout`（超时）、`cancelled`（用户取消）。

`ok` 保持为真（检查失败不改变工具自身的成败语义）。原因：`compact_for_model()` 在 `ok=false` 时只回一行错误文本、整段 `data` 会被丢弃（`src-tauri/src/tools/compact.rs:30-35`），一旦用「工具失败」表达检查失败，模型又会看不到输出，本需求的价值当场归零。

纪律沿用现有约定：**没真的跑过，文案里绝不出现「通过」**。

### 4.3 行为约定

| 情形 | 行为 |
|---|---|
| 开关关闭 | 不执行，`ran=false`、`skipped="disabled"` |
| 命令为空 | 不执行，`ran=false`、`skipped="empty-command"` |
| 命令不含 `{file}` | 整个工具调用执行一次 |
| 命令含 `{file}` | 每个被写文件执行一次、串行、逐文件成条 |
| 临时会话（无项目目录） | 跳过，`skipped="no-project"` |
| 命令被围栏判为 Block | 不执行，把原因写入 `skipped="blocked-by-fence"` |
| 命令超时 | 终止整个进程组，`skipped="timeout"`，输出如实带上已有部分 |
| 用户取消运行 | 终止整个进程组，`skipped="cancelled"` |
| 退出码非零 | `ok=false`，输出照常交给模型 |

## 5 删除清单

### 5.1 后端

| 文件 | 规模 | 处理 |
|---|---|---|
| `src-tauri/src/lsp/`（mod / protocol / client / discovery / server_spec / workspace / diagnostics / pool / manager / install / sdk） | 6496 行 | 删除整个目录 |
| `src-tauri/src/host/commands/lsp.rs` | 245 行 | 删除（含 5 条命令与 3 个单测） |
| `src-tauri/src/bin/codewave-fake-lsp.rs` | 404 行 | 删除 |
| `src-tauri/tests/lsp_integration.rs` | 933 行 | 删除（17 个集成用例） |
| `src-tauri/Cargo.toml` | — | 删 `lsp-types` 依赖与 `[[bin]]` 段 |

需同步改写的引用点：

- `src-tauri/src/lib.rs`：模块声明 `:11`、闲置回收定时器 `:177-185`、命令注册 `:387-391`、退出时关闭 `:438-449`；
- `src-tauri/src/host/commands/mod.rs:8`、`:25`；
- `src-tauri/src/host/commands/project.rs:206-207`（删除项目时不再需要关闭 server）；
- `src-tauri/src/core/agent/runtime.rs:320-321`、`:351`（`AgentCore` 去掉 `lsp` 字段）；
- `src-tauri/src/core/config.rs:330-428`、`:540`、`:567`。

### 5.2 前端

- 删除 `ui/src/features/chat/LspGuideCard.tsx` 与 `ChatMessages.tsx` 的挂载点；
- 删除 `ui/src/stores/run.ts`、`runHandlers.ts`、`run.types.ts` 里的 `lsp:server_missing` 事件处理与 LspHint 状态；
- 删除 `ui/src/ipc/client.ts` 的五条命令与 `ui/src/ipc/types.ts` 的全部 LSP 类型；
- 删除 `ui/src/features/panels/settingsRegistry.ts` 里九个 `validation.lsp.*` 设置项；
- 删除 `ui/src/theme/app.css` 的 LSP 样式类；
- 删除 `ui/src/i18n/zh-CN.ts`、`en-US.ts` 里约 56 个文案键。

**不删**：`warnings` 通道及其前端渲染——它还有别的生产者（未知字段告警、版本令牌过期提示、模糊命中提示、参数复原提示），删掉会连带打挂多条用例（`ui/src/features/tools/ToolCallCard.tsx:289`）。JSON 内置解析保留，改为不依赖开关。

### 5.3 测试

- 删除 `ui/src/__tests__/settings.lsp.test.tsx`、`ui/src/__tests__/lsp.guide-card.test.tsx`；
- 改 `ui/src/__tests__/events.contract.test.ts`（**事件面 29 → 28 键**，同时删掉对 `lsp:server_missing` 的点名）；
- 改 `settings.registry.test.ts`（进阶项数与分组数字断言）、`settings.page.test.tsx`、`app.smoke.test.tsx`、`i18n.keys.test.ts`，以及依赖旧 `ValidationSettings` 类型的若干 fixture；
- 后端因删除会减少约 134 个用例（`lsp/` 内联 103、`validation.rs` 10、`create.rs` 1、`commands/lsp.rs` 3、集成 17），实测后更新 `AGENTS.md` 里的基线数字。

## 6 新增设计

### 6.1 `src-tauri/src/tools/postcheck.rs`（新文件）

职责：执行用户配置的检查命令并把结果整理成 `check` 结构。

- 命令解析复用 `tools::command::{resolve_shell, shell_invocation}`（与 command 工具同一套 shell 语义），不另起一套；
- `{file}` 替换为相对项目根的路径；
- 工作目录 = 会话工作区根；`process_group(0)`（Unix）与 `CREATE_NO_WINDOW`（Windows）；标准输出与标准错误并发读取（避免写满管道卡死）；
- 用 `tokio::select!` 同时监听超时与 `ctx.cancel`；超时或取消都终止整个进程组并收尸，绝不留孤儿进程；
- 输出取尾部 `tail_chars` 个字符（编译错误通常出现在末尾，尾部命中率最高）；
- 执行前过 `safety::fence::check_command_policy`，判为 Block 则不执行并把原因写入 `skipped`。

因为 LSP 删除会一并带走 `lsp/client.rs` 的进程组终止函数，实现时需把等价能力搬进本文件（或复用 command 工具里已有的同类实现），不依赖被删模块。

### 6.2 `src-tauri/src/tools/validation.rs` 重写

- 保留：JSON 内置解析（`json_check`，改为总是执行）、逐文件成文的纪律（不因为批里某个文件跑过就给整批打「通过」）；
- 删除：写前基准、差集、三态、异步补条、安装引导事件；
- 改为：调用 postcheck，把结果写进 `outcome.data.check`。

### 6.3 接线

- `src-tauri/src/tools/create.rs:97-138`：去掉写前基准调用，写后接新机制；
- `src-tauri/src/tools/edit/tool.rs:466-486`、`:546-559`：同上。

### 6.4 前端设置页

在「工具」页新增「写入后检查」分组：

- 开关（默认关）；
- 命令输入框，说明文字写明「在项目根目录执行」并给示例（如 `npx eslint {file}`）与 `{file}` 的含义；
- 超时秒数（默认 30）；
- 交给模型的输出尾部字符数（默认 3000）。

登记改动点：`settingsRegistry.ts` 的设置项定义、字段路径、页面字段、宽度豁免四处，加上 `ui/src/stores/settings.ts` 的默认值与 `ui/src/ipc/types.ts` 的类型镜像。

## 7 测试计划

### 7.1 后端新增用例

开关关闭 / 命令为空 / 非零退出 / 超时 / 尾部截断 / `{file}` 替换（有占位符与无占位符两种跑法）/ 围栏 Block / 用户取消 / 临时会话跳过 / 多文件批次逐文件成条。

**关键闸门**：为 `create` 与 `edit` 各加一条断言「结论确实出现在 `outcome.data` 里」的用例，并额外断言模型侧文本包含该结论（写法参考 `src-tauri/src/tools/batch.rs:1344` 的 `model_text_chunks`）。这是防止缺陷 B 重演的防线。

### 7.2 删除集成测试时的等价回归

`lsp_integration.rs` 里三类高价值守护不能随之消失，新机制要补等价用例：

1. 子进程不留孤儿（原 `lsp_integration.rs` 的 pid 存活断言）；
2. 超时与取消不挂死；
3. 输出分片安全（原 framing 分片测试）。

若新机制确实不再启动子进程，应在测试里显式写下说明，避免日后有人加回进程却无人守护。

### 7.3 前端

事件契约 28 键、设置页新分组渲染与保存、i18n 键集同步。

## 8 文档更新

- 新增本文（`docs/post-write-check.md` 或 `post-write-check-plan.md`）；
- 更新 `docs/0-README.md` 的条目登记；
- 更新 `AGENTS.md`：事件面 29 → 28 键、测试基线数字、契约锚点里与 LSP 相关的段落；
- `docs/lsp-post-write-diagnostics.md`、`docs/lsp-detection-and-settings-ux.md` 标注「已被 post-write-check 取代」；
- `docs/settings-ia.md`、`docs/settings-terminology.md`、`docs/builtin-tools-source-comparison.md` 相关段落同步；
- 发布说明写明：旧 `validation` 配置字段不再受理。

## 9 验证方式

1. `cd src-tauri && cargo test` 全绿；
2. `pnpm --dir ui test` 全绿，`pnpm --dir ui build` 通过；
3. 基线数字实测后写回 `AGENTS.md`；
4. 手动验收（界面改动不做自动点验，交付分步清单）：
   - 设置页配置检查命令（如 `npx eslint {file}`）并打开开关；
   - 故意写一个类型错误；
   - **模型回复里应能看到错误输出**——这是本批最重要的验收点，也是缺陷 B 的回归防线；
   - 关掉开关 → 不再执行；把命令写成灾难级 → 被围栏拦下且如实告知。

## 10 风险与回滚

- 删除面大，漏引用会编译失败；以 `cargo check` 与前端类型检查为准绳逐个清零。
- 事件面 29 → 28 是硬编码锚点，三处联动必须同批改（`ui/src/__tests__/events.contract.test.ts`、`AGENTS.md`、文档）。
- 旧配置的 `validation` 字段首次保存后消失，需在文档与发布说明写明。
- 用户失去的能力与缓解方式：
  - 失去精确的 `文件:行:列` 诊断 → 主流工具（eslint、tsc、ruff、go vet）本身就按这个格式输出，文档优先推荐这类命令；
  - 失去「只报本次新增错误」的差集能力 → 全项目命令会带上存量错误，文档建议优先配单文件命令（带 `{file}`）；
  - 失去 Rust 的工程级类型检查质量 → `cargo check` 能力不差但慢，文档建议把超时调大（如 120 秒）；
  - 失去零配置体验 → 默认关闭且需自己填一行，文档给各技术栈的推荐命令对照表。
- 回滚：改动集中在 LSP 与 postcheck 相关文件组，按文件还原即可。

## 11 未尽事项

- 各技术栈推荐命令的对照表需实测后定稿（TypeScript / JavaScript、Python、Rust、Go）。
- 是否禁止会改写文件的检查命令（如带 `--fix`、`--write`）：当前建议不禁止、只如实提示。
- 检查命令是否应纳入用户的「额外 SDK 根目录」那类发现配置：本批不做。
- LSP 的跳转、符号查询等能力是否以「模型按需调用」的形式回归：本批不做。
