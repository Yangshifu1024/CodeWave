# — 内置工具设计说明

> 目的：逐组解析 CodeWave 内置工具（`src-tauri/src/tools/`，Rust）的**入参 schema、出参结构与每段代码的实现逻辑**，并据此给出工具层的优化建议。每个工具独立成章；未内置的工具形态与配套基础设施单独列出。
>
> 源码基准：`src-tauri/src/tools/`（工作区当前状态，含未提交修改：`edit.rs` / `batch.rs` / `validation.rs` / `core/agent.rs`）。
>
> 阅读约定：代码引用形如 `tools/command.rs:237`（相对 `src-tauri/src/`）。
>
> **后续变更（2026-09-20，[post-write-check-plan](./post-write-check-plan.md)）**：写后校验已从「项目级常驻 LSP
> 语义诊断」再次改为「**写入后检查命令**」（用户配置一条命令，结论进 `outcome.data`）；本文 §4.2 / §13.4 / §15 P3-1
> 中关于 LSP 与「写入后语义校验」的记述均为历史沿革存档。

---

## 1. 总览：工具体系架构

### 1.1 工具清单

| # | CodeWave 工具（注册名） | 章节 |
|---|---|---|
| 1 | `command`（CommandTool） | §2 |
| 2 | `read`（ReadTool）+ 别名 `batch_read` | §3 |
| 3 | `create`（CreateTool） | §4 |
| 4 | `edit`（EditTool） | §5 |
| 5 | `list_files`（ListFilesTool） | §6 |
| 6 | `grep`（GrepTool） | §7 |
| 7 | `subagent`（SubagentTool） | §8 |
| 8 | `web_fetch`（WebFetchTool） | §9 |
| 9 | `plan`（PlanTool） | §10 |
| 10 | `ask`（AskTool） | §11 |
| 11 | `skill`（SkillTool） | §12 |
| — | 未内置工具形态评估（apply_patch / websearch / plan_exit / lsp / execute / invalid） | §13 |
| — | 其余内置工具（`batch_read` / `calculate` / `delete` / `http_request` / `render_html` / `scheduled_task` / `service` / `suggest` / `wait`） | §14 |

注册表与注册策略：

- `tools/registry.rs:12-42`：20 个工具硬编码注册（`reg!` 宏 → `HashMap<&'static str, Arc<dyn Tool>>`），`tool_defs()` 按名称排序输出（cache-first：顺序恒定，利于 provider 侧 prompt cache），测试 `default_tools_sorted_and_strict` 断言全部 schema `additionalProperties:false`。
- 注册策略为**静态全量注册**：所有工具对模型恒可见，仅按 `ToolKind` 在 batch 层做档位过滤（见 §14.10；plan 档排除集见 `core/agent.rs` main_drive_params），普通档对任何模型都注入全部 20 个工具。备选的「按模型动态裁剪工具列表」可进一步省 token（模型只见与其匹配的工具子集），作为可选优化评估（§15 P2-8）。

### 1.2 工具接口契约

**CodeWave**（`tools/mod.rs:148-156`）：

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;          // strict JSON Schema（additionalProperties:false）
    fn schema(&self) -> &'static str;
    fn kind(&self) -> ToolKind;                      // ReadOnly | FileWrite | Network | Interactive | Meta
    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome;
}
```

- 入参是**已解析的 `serde_json::Value`**，各工具自行 `serde_json::from_value` 成强类型 `Args`（`#[serde(rename_all = "camelCase")]`），失败统一 `E_ARGS`。schema 是手写 JSON 字符串常量，与 `Args` 结构靠人肉对齐（契约测试守护 `additionalProperties:false`）。
- `ToolCtx`（`mod.rs:91-98`）注入 `AgentCore`/`SessionRuntime` 共享句柄 + `batch_id`/`call_index`/`call_key` + `CancellationToken`；派生方法 `workspace()`/`write_roots()`/`approval_mode()`/`fence_policy()` 是工具获取环境的唯一通道。
- 未知字段宽容解码：`collect_unknown_fields`（`mod.rs:159-169`）在 batch 层对照 schema `properties` 收集"未知字段已忽略"warnings（`batch.rs:319`），模型写错键名能收到提示而非静默丢弃。
- 设计取舍：工具层不设统一截断包裹，也不设工具内通用审批回调——command/read 这类能语义化裁剪的工具自行收口，其余由 compact 层统一处理（§1.4）；审批由 fence + approval + 档位三层在 batch/工具内协作（§1.5）。回传模型的多模态附件（图片 base64 data URL）经 `ToolOutcome.extra_model_content` 通道。

### 1.3 结果双通道

- CodeWave `ToolOutcome`（`mod.rs:57-88`）：`{ ok, data, error?, warnings, extra_model_content }`。**前端收完整 JSON**（`data` 原样），**模型收 `compact_for_model` 压缩文本**（见 §1.4）。`is_error` 由 `!out.ok` 决定（`batch.rs:338-342`）。
- 设计说明：模型侧文本是**从 data JSON 序列化再截断**（除 command/read 特判），而非每个工具专门组织一份 prose——省去工具重复维护两份输出的成本，代价是模型侧可读性略逊于专门文案；command/read 的语义化特判即是对该权衡的补偿（详见 §1.4 与各章）。

### 1.4 输出截断管道

**CodeWave `tools/compact.rs`（91 行）**：

- 常量：`HEAD_BYTES = 4KB`、`TAIL_BYTES = 8KB`（`compact.rs:5-6`）。
- `compact_for_model(kind, name, outcome)`（`compact.rs:8-34`）分派逻辑：
  1. `command` 特判直通（工具内已语义化裁剪，见 §2）；**错误时仍拼 data**——`[error code: msg]\n<data>`，保住非零退出的构建输出（`compact.rs:59-75` 测试锚定该行为）；
  2. 任意失败 → `[error CODE: message]` 单行；
  3. `read` 直通不截断（保行号定位）；
  4. 其余工具 → `truncate_head_tail(raw, 4KB, 8KB)`：对 `data.to_string()`（整个 JSON 序列化）头 4KB + 尾 8KB，中间插 `…[已截断 N 字节]…`。
- **已知盲区**：截断对象是「JSON 序列化文本」，头尾拼接可能截断在 JSON 结构中间，且**除 command 外无落盘通道**（被截掉的中间部分模型永远拿不回）。[docs/tool-optimizations-port](./tool-optimizations-port.md) 已为 command 修复「凡截断必落盘」，其余工具（如 grep 大结果、http_request 大 body）仍有此盲区（§15 P0-5）。
- 设计参考（未整体采用，见 §13.7 与 §15 不采纳清单）：「统一截断管道对工具透明」的形态——全局统一行/字节预算 + 全文落盘 + 截断提示按模型能力分流（有子代理工具时引导委托子代理处理截断文件，否则引导用 grep / read offset 续读）。

### 1.5 权限/审批模型

CodeWave 三层：

1. `safety/fence`（命令围栏）：command/service 在执行前 `check_command_policy`（L1 黑名单→L2 AST→L3 高危模式），产出 `Allow/Block/Confirm(reason)`；
2. `safety/approval::confirm`：Confirm 走弹窗（超时拒绝），支持「始终允许本项目」白名单（command 专属，键 = `cwd\u{1}命令全文`，`command.rs:167-224`）；
3. 档位（`ApprovalMode`：Plan/ConfirmEach/AutoEdit/FullAccess）由 `ToolCtx::fence_policy()` 折算成 fence 策略 + batch 层硬门（ConfirmEach 对 FileWrite 预弹窗 `batch.rs:217-239`；plan 档排除工具 spawn 前拒绝 `batch.rs:77-103`）。

- **文件写入路径边界**：`pathutil::resolve_write` 白名单根硬校验（越界直接 `E_PATH_OUTSIDE`，无询问通道）——工作区即边界的封闭取向。备选的「询问式放行」（用户批准后目录进入会话级 allow 列表）作为 P1 增强评估（§15 P1-3）。

### 1.6 外部路径访问与运行时扩根（现状与缺口）

CodeWave 的路径边界由 `WriteRoots`（workspace + data_dir + extra_roots 白名单）圈定，extra_roots 由会话建立时的 @ 提及/项目快照决定，**无运行时扩根通道**——会话开始后模型请求访问白名单外目录一律 `E_PATH_OUTSIDE` 硬拒，用户无法在会话中按需放行某个区外目录（如配置文件、兄弟项目）。

候选机制（§15 P1-3）：resolve 命中区外时取目标**父目录**构造 `<dir>/*` 发起审批——**批准一次，该目录本会话内可访问**；由 `extra_roots` 运行时 append 承载，不落配置、不跨会话，保持最小权限面。

---

## 2. command

### 2.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/command.rs`（737） |
| 入参 | `command` / `cwd?` / `timeoutSeconds?` / `fullOutput`（required） |
| 出参 | `data: {exit_code, output, shell, cwd, output_file_path?, output_truncated?}` |
| 超时 | 默认 120s，clamp 1–600s |
| 截断 | 96KB 内存 + spill 文件；模型侧 tail 2000/16000 字符 + signal line |
| 权限 | fence 三层 + 档位 + 「始终允许本项目」白名单 |

### 2.1 设计要点与已知缺口

- **审批粒度**：fence 三层 + 档位 + `cwd+命令全文` 白名单；L2 AST 已覆盖命令解析（安全视角）。「AST 逐命令提取路径参数 → 对 `rm <区外路径>` 精确触发目录询问」的路径级审批粒度是候选增强方向，与运行时扩根一并考虑（§15 P1-3）。
- **描述策略**：当前为静态一段（含 fullOutput 决策指引）。增强方向是 shell/平台感知的动态 description——注入目录预检命令（`ls` vs `Test-Path` vs `if exist`）、专属工具替代清单（用 grep/list_files 替代 find/cat）、`&&` vs PowerShell `if ($?)` 链式指引、截断/超时常量、workdir 优先提示，让模型对输出策略有明确预期（§15 P0-2）。
- **cwd 默认语义**：项目会话默认项目托管 temps/（中立区），命令默认不落用户代码目录——写取向的刻意设计（见 §2.2）。
- **输出管道**：头部 96KB 内存 + 超限 spill 追加 + 200ms 节流进度帧 + 凡截断必落盘 + signal line；fullOutput 双档是 token 经济上的独特设计。
- **已知缺口**：shell 生态两套（Git Bash/PowerShell），PowerShell 不可用时不回退 cmd；`workdir` 别名缺（§2.3）。

### 2.2 源码解析

**入参**（`command.rs:21-32`）：`command`（必填）、`cwd?`（相对 workspace）、`timeoutSeconds?`、`fullOutput: bool`（**required**，`#[serde(default)]` 但 schema 标 required——旧 payload 缺省按 false 安全降级）。description 内嵌 fullOutput 决策指引："输出即答案→true；构建/测试/安装→false 拿尾部+signal line"。

**shell 探测**（`command.rs:36-99`）：全局 `OnceLock<Shell>`。Unix 探测 `bash -lc 'echo ok'` 是否可用（login shell 捕获 homebrew PATH，5s 超时探测一次缓存）；Windows 按固定候选路径找 Git Bash（`C:\Program Files\Git\bin\bash.exe` 等 3 处），找不到回退 PowerShell。`shell_description()` 供系统提示词环境层引用。

**执行主流程 `run`**（`command.rs:128-234`）：

1. 参数解析 + 空命令检查；**默认 cwd 语义特殊**：项目会话 = 项目托管目录（`project_dir`，中立区 temps/），自由会话 = workspace 根（`command.rs:136-149`）——命令默认不落在用户代码目录，代码操作须绝对路径显式触达；
2. **围栏 + 审批**（`command.rs:151-228`）：`check_command_policy(command, cwd, roots, policy)` 三档：
   - `Allow` 直接过；
   - `Block` → `E_COMMAND_BLOCKED`；
   - `Confirm(reason)` → 四类理由（OutsideCreate/HighRisk/Disaster/InsideWrite）：FullAccess 档跳过（灾难级仍拦，`command.rs:161-165`）；否则查「始终允许本项目」白名单（键 = `cwd显示\u{1}命令trim`，`command.rs:171-184`；灾难级不适用白名单）→ 未命中弹 `approval::confirm`（title/detail 按理由分类，allow_always=非灾难）；拒绝 → `E_APPROVAL_DENIED`；勾选 always → 白名单去重写入 + 持久化（已知与 save_config 的 read-modify-write 竞态，注释留档，`command.rs:167-170`）；
3. 超时 `unwrap_or(120).clamp(1, 600)`。

**进程执行 `run_process`**（`command.rs:237-410`）：

1. 按 shell 构造：bash `-lc`/`-c`（Windows Git Bash 用绝对路径 bash.exe）、PowerShell `-NoProfile -Command`；`stdout/stderr piped`；Unix `process_group(0)`（独立进程组），Windows `CREATE_NO_WINDOW`（`command.rs:266-274`）；
2. **输出收集**（`command.rs:284-357`）：两路 `pump` 协程（4KB 读块，`from_utf8_lossy` 容错）汇入 unbounded channel；collector 协程逐 chunk 统计 `total_bytes/total_lines`（按 `\n` 计数 + 末行无换行补 1 的标志位）；
   - `buf` 只收头部 96KB（`SPILL_THRESHOLD`）；超限一次性 spill：写 `<data_dir>/tmp/cmd-output/<uuid>.txt` 头部内容，此后 chunk 全部 append 到 spill 文件；
   - **节流进度**：累计 ≥2KB 且距上次 ≥200ms 时，取 buf 尾 2000 字符发 `Frame::ToolProgress` 事件（前端命令卡实时流）；
3. **三路 select**（`command.rs:360-384`）：完成 / 超时 / 取消。超时与取消都 `terminate_tree(pid)`（Unix：进程组 SIGTERM→轮询 100ms×3s→SIGKILL；Windows：`taskkill /T /F`；阻塞轮询放 `block_in_place`，`command.rs:526-557`）后 drain pump（锁中毒走 `lock_ok` 防二次 panic，[docs/tool-optimizations-port](./tool-optimizations-port.md) 修复）；超时返回 `E_TIMEOUT` + 尾部输出；
4. **模型侧装配 `model_tail`**（`command.rs:426-457`，本项目优化）：`full_output` 决定尾部预算 16000/2000 字符；`truncated = spilled || buf超预算`；spill 时尾部从 spill 文件读（字节预算 `want*4+8` 兜底 UTF-8 最坏 4 字节/字符，`tail_chars_from_file`，`command.rs:469-489`）；**凡截断必落盘**（未 spill 但超尾部预算 → 现写 spill 文件，修 16KB~96KB 盲区）+ signal line：`[exit N | 共 M 行 | 输出已截断，完整输出：<path>]`；
5. spill 清理：`purge_stale_spills` 惰性触发（AtomicU64 CAS 每小时一次），删 24h 前文件（`command.rs:492-524`）；
6. 非零退出码 → `ok:false` + `E_EXIT_CODE`（data 仍带完整 output——compact 层直通保住构建失败输出，`compact.rs:11-21`）。

### 2.3 优化建议

- 动态 description：注入实际 shell、平台、fullOutput 双档语义、截断/超时常量（§15 P0-2）。
- 工作区外路径询问放行机制（§15 P1-3）。
- 可选 `workdir` 参数别名（与 `cwd` 同义，贴合常见模型调用习惯）。

---

## 3. read

### 3.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/read.rs`（266）+ 别名 `batch_read.rs`（27） |
| 入参 | `files[1..20]`：`{path, startLine?, endLine?}`（负 startLine = 尾 N 行） |
| 目录语义 | 仅常规文件（目录走 list_files） |
| 默认窗口 | 无 startLine：1–2000 行；有 startLine 无 endLine：该行起 200 行 |
| 单行上限 | 无单行截断（已知缺口 → §15 P0-4） |
| 字节上限 | 1MB 预检（无 startLine 时拒绝） |
| 图片 | png/jpg/jpeg/webp/gif ≤3MB → `extra_model_content`（手写 base64） |
| 二进制 | 无检测（已知缺口 → §15 P0-4） |
| 令牌 | `version`（crockford 6 字符，edit 乐观并发用） |
| 编码 | UTF-16 BOM/NUL 密度启发 + `decode_utf16` 手工转码 |

### 3.1 设计要点与已知缺口

- **失败隔离缺口**：批量调用中一个文件失败（resolve/metadata/读取）即整体早退 `E_*`，其余文件结果全丢（`read.rs:153` 等三处早退）——批处理接口应 per-file 局部错误（§15 P0-3）。
- **单行超长**：minified js/巨型 JSON 行会在 1MB 检查下通过并全量进上下文，需单行截断（§15 P0-4）。
- **二进制防护**：非图片扩展名文件 `from_utf8_lossy` 硬读，读 .exe 会输出乱码损耗 token，需采样启发（§15 P0-4）。
- **大文件取向**：整读到内存 + 1MB 硬检 + 行窗口；对源码场景 1MB 上限够用，按 startLine 有无做 seek 优化可后置。
- **编码**：UTF-16 检测转码覆盖 Windows 生态常见编码；read/edit 复用同一解码函数保证编码视图一致。
- **并发防护**：version 令牌提供跨轮次/跨会话的乐观锁（防覆盖他人修改），edit 的 stale 判定与 lineRange 兜底依赖它。
- **误拼容错**：文件不存在直接 `E_NOT_FOUND`；同目录候选建议是低成本高收益增强（§15 P2-1）。
- **图片**：5 扩展名白名单 + 3MB 上限；无 PDF 通道、无 mime 嗅探（白名单内扩展名可信，不依赖内容嗅探）。
- **LSP 联动**：「读时预热 LSP 使后续 edit 诊断更及时」依赖 LSP 体系，见 §13.4 与 §15 P3-1。

### 3.2 源码解析

**入参**（`read.rs:20-34`）：`files[1..20]`（minItems 1 maxItems 20，`read.rs:143` 硬校验），每项 `{path, startLine?, endLine?}`。**批量接口**——一次调用读多文件，降低往返轮次的主动设计，与 batch 执行器配合天然并行。

**辅助函数**：

- `media_type_of(path)`（`read.rs:38-41`）：扩展名白名单 → mime（png/jpg/jpeg/webp/gif 五种）。
- `looks_utf16(bytes)`（`read.rs:43-54`）：BOM（FF FE / FE FF）或前 256 字节 NUL 密度 >1/8 启发。
- `decode_utf16(bytes)`（`read.rs:56-76`）：手工按 BE/LE 组 u16 对，`char::decode_utf16` 容错（`U+FFFD` 替换）。
- `read_text_content(bytes)`（`read.rs:78-84`）：UTF-16 判定命中则转码，否则 `from_utf8_lossy`。edit.rs 复用同一函数解码，保证 read/edit 编码视图一致。
- `format_lines(text, start, end)`（`read.rs:86-100`）：`split_inclusive('\n')` 切行（保留换行符，空文件 0 行 vs 单行无换行 1 行的语义正确）；`%>6|` 右对齐 6 位行号前缀；start>total 或 start>end 返回空。

**主流程 `run`**（`read.rs:138-230`）：逐文件：

1. `resolve_read`（多根：主目录未命中逐 extra 根找首个存在者，全 miss 回落主目录候选报不存在，`pathutil.rs:111-132`）；越界（canonical 后不在根内）→ `E_PATH_OUTSIDE`；非常规文件 → `E_ARGS`；>1MB 且无 startLine → `E_TOO_LARGE` 引导分段；
2. **图片分支**（`read.rs:168-196`）：≤3MB（超限 `E_TOO_LARGE`）；**手写 base64 编码器**（标准字母表 + padding，`read.rs:174-186`，注释"std 无"）；产出双通道：`extra_model_content` push `Content::Image`（模型侧多模态注入）+ data 里 `data_url`（前端 outcome 展示）；version 令牌照样返回；
3. **文本分支**（`read.rs:198-225`）：窗口计算——负 startLine = 尾 N 行（`saturating_sub(n)+1`）；正 startLine 无 endLine = 起 200 行；无 startLine = 1–2000 行；`format_lines` 渲染；data 带 `{path, kind, start_line, end_line, total_lines, truncated, version, content}`。

**version 令牌**：`crockford::version_token(bytes)` 6 字符（`edit.rs:35-40` 的单哈希修复注释：曾双重哈希与 edit 永不匹配形成死循环）。edit 的 stale 判定 + lineRange 兜底依赖它（见 §5.2）。

**`batch_read`**（`batch_read.rs` 全文）：27 行纯别名——name/description 标 deprecated，schema 与 run 直接转发 `ReadTool`。历史会话兼容用（另见 §14.1）。

### 3.3 优化建议

- per-file 局部错误（失败隔离）→ §15 P0-3。
- 单行 2000 字符截断 + 二进制采样启发 → §15 P0-4。
- 误拼文件名建议（同目录双向子串匹配取 3）→ §15 P2-1。

---

## 4. create

### 4.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/create.rs`（136） |
| 入参 | `path` / `content` / `overwrite?` |
| 已存在文件 | 默认拒绝 `E_EXISTS`，`overwrite=true` 才覆盖（防误覆盖默认） |
| 审批 | 无前置审批（档位在 batch 层统一 gate；越界由 resolve_write 硬拒） |
| 原子写 | `atomic_write`（temp+rename，明确原子） |
| 写后校验 | `validation::validate_file` 按语言路由（python3/rustc/tsc/node --check/go vet + JSON 内建） |
| 产物登记 | [docs/session-artifacts-and-files-tab](./session-artifacts-and-files-tab.md) 产物边车（Create op，canonical 归一去重） |
| 已知缺口 | 无 BOM 处理；无格式化回写（可选增强，依赖本机工具链） |

### 4.1 设计要点与已知缺口

- **覆盖语义**：默认拒绝 + 显式 `overwrite` 防误覆盖，语义贴合 create 本名；代价是「整文件重写」模型须两步（先失败再补 overwrite），description 应强化指引（§15 P2-2）。
- **审批表达**：ConfirmEach 档 detail 目前是参数 JSON dump（`batch.rs:226`），对写类工具可专门渲染语义 diff 预览（§15 P1-1）。
- **写后反馈取向**：外部工具链语法校验（单文件、轻量）而非 LSP 语义诊断（常驻、重）——对无 LSP 场景更务实（LSP 见 §13.4 / §15 P3-1）。（**历史文案**：本篇成文时的旧写法；用户可见的现行定名是「写入后语义校验」，见 [settings-terminology](./settings-terminology.md) §1。）
- **BOM**：读侧 `from_utf8_lossy` 已丢 BOM 信息，UTF-8 BOM 文件 round-trip 会丢 BOM（Windows 记事本场景小坑）。
- **产物登记**：前端「文件」标签页数据源（[docs/session-artifacts-and-files-tab](./session-artifacts-and-files-tab.md)）。

### 4.2 源码解析

（`create.rs:42-88`）逐段：

1. `resolve_write`（相对路径 `safe_join`：拒绝对路径/`..`/Windows 保留名/控制字符，`pathutil.rs:34-71`；canonical 含中间 symlink 后必须落白名单根内，否则 `E_PATH_OUTSIDE`）；
2. 已存在且未 `overwrite` → `E_EXISTS`（**防误覆盖默认**：模型想覆盖必须显式声明）；
3. 建父目录 → `util::atomic::atomic_write`（原子写：先临时文件再 rename，防半写状态）；
4. 成功后：**产物登记**（`create.rs:62-75`）——非 task runtime 时 `append_artifact(owner=主会话 id, canonical 路径, Create)`（[docs/session-artifacts-and-files-tab](./session-artifacts-and-files-tab.md)：前端"文件"标签页数据源；子代理写入归属主会话；相对/绝对路径 canonical 归一去重）；失败仅 warn 不影响结果；
5. **写入后校验**（`create.rs:76-84`）：`validation::validate_file`（按扩展名路由：py→`python3 -m py_compile`、rs→`rustc --emit=metadata`、ts→`npx typescript@5 tsc --noEmit`、js/vue→`node --check`、go→`go vet`、json→serde 内建解析；工具链未装跳过、超时 10–60s、错误取 stderr 末 6 行截 2048 字符）；summary 非空进 warnings 回填模型（"写入后语法校验失败，请用 edit 修复"——**历史文案**：那时回填给模型的原话，用户可见的现行定名是「写入后语义校验」，见 [settings-terminology](./settings-terminology.md) §1）。

### 4.3 优化建议

- description 强化「整文件重写直接带 overwrite:true」（§15 P2-2）。
- ConfirmEach 审批弹窗渲染 diff 预览（§15 P1-1，edit/create 共用）。

---

## 5. edit

### 5.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/edit.rs`（430） |
| 入参 | 多文件多改：`files[]: {path, version?, changes[]: {oldText? \| lineRange?, newText?}}` |
| 匹配策略 | 精确唯一匹配；lineRange 兜底（备选模糊机制评估见 §5.1） |
| 并发防护 | version 乐观令牌（stale 软降级 + 硬冲突拒绝） |
| 换行处理 | 按字节精确匹配（CRLF 文件要求模型给 CRLF——实际模型常给 LF，已知缺口 → §15 P0-1） |
| 回滚 | 备份 → 倒序写 → 失败回滚已写文件 |
| 审批 | 档位 gate（batch 层） |
| 写后 | validation 语法校验 + 产物登记（**历史文案**：现行定名「写入后语义校验」，见 [settings-terminology](./settings-terminology.md) §1） |
| 附加 | stale warnings、产物 canonical 去重 |

### 5.1 备选匹配机制评估（多级模糊替换器链）

edit 容错的一种成熟路线是**多级模糊替换器链**：精确匹配 → 逐行 trim 滑窗（保缩进）→ 首尾锚定 + 中间行 Levenshtein 相似度块匹配 → 空白归一 → 缩进平移 → 转义归一（`\n \t \"` 双向）→ 首尾 trim → 上下文感知（等行数 + 中间行 trim 相等比例）→ 全量多命中，逐级 yield 候选、取唯一命中者，并配套**比例守卫**（匹配段行数/字符数远超 oldString 即拒绝，防模糊器错杀大段代码）。

评估：模糊链假设「模型给不准」而本工具假设「模型给得准」（靠 version + 报错引导重读）；Levenshtein 块锚定等激进级别有错杀风险，且复杂守卫与 version 令牌机制哲学冲突。**取舍**：只取其中两级低风险替换器（行首尾空白 trim、缩进平移），覆盖最高频的「缩进偏差」失败模式（§15 P0-1）。

### 5.2 源码解析

**入参**（`edit.rs:10-31`）：`files[]`（多文件）每项 `{path, version?, changes[]}`，change 三选一：`oldText`（须唯一）/ `lineRange "40-72"`（1-based 闭区间）/ `newText`（缺省 = 删除）。

**`version_of(bytes)`**（`edit.rs:35-40`）：crockford 单哈希 6 字符（注释记录双重哈希缺陷修复史）。**`parse_line_range`**（`edit.rs:43-51`）：`"a-b"` 解析 + a≥1、b≥a 校验。

**`apply_changes(content, changes)`**（`edit.rs:54-89`）纯函数（可单测）：

1. 逐 change：`oldText` 路径——`buf.matches(old).count()`：0 次且带 lineRange → `continue` 交给兜底；0 次无兜底 → 报错"不存在"；>1 次 → 报错"匹配 N 处需加长上下文"；恰好 1 次 → `replacen(…, 1)`；
2. `lineRange` 路径——解析区间；`split_inclusive('\n')` 切行；起点超总行数报错；终点 clamp；行区间替换（`newText` 行拼接）；
3. 都没有 → 报错。

**主流程 `run`**（`edit.rs:138-271`）：

1. **第一遍预演**（`edit.rs:146-196`）：逐文件 read 字节 → version 预检（`normalize_token` 比较）→ `read_text_content` 解码 → `apply_changes` 预演：
   - **stale 软降级**（当前工作区新增逻辑）：version 不匹配**不再硬拒**——变更仍能在最新内容上唯一应用 → 继续 + stale warning（"version 令牌已过期，但变更在最新内容上唯一命中，已按最新内容应用"）；应用失败（真冲突）才 `E_VERSION_STALE` 要求重新 read（`edit.rs:159-194`，测试 `full_edit_flow_with_rollback` 锚定三态：新令牌/过期可命中/过期且冲突）；
   - 任一文件预演失败 → 整体失败（原子性前置）；
2. **备份 → 倒序写 → 回滚**（`edit.rs:198-229`）：每文件原文备份到 `<data_dir>/tmp/edit-backup/<token>_<uuid>`；**倒序**写（`prepared.iter().rev()`，配合同批次写串行闸门减少中间态暴露窗口）；写失败 → 已写文件从备份回滚 → `E_IO`（"写入 X 失败，已回滚"）；成功清备份；
3. 产物登记（同 create：Edit op、主会话归属、canonical 去重、task runtime 跳过，`edit.rs:230-245`）；
4. **写入后校验**（`edit.rs:246-259`）：逐文件 `validate_file`（路径→label 的匹配用了 canonicalize 双重比较，稍绕但正确）→ `summarize` 拼 warnings；
5. 出参：`{edited: [...paths], count}` + warnings（校验失败/stale）。

### 5.3 优化建议

- CRLF 归一匹配 + 两级低风险模糊替换器（§15 P0-1）。
- ConfirmEach 审批 diff 预览（§15 P1-1，同 §4.3）。
- 文件级互斥可补：当前同批次写冲突直接拒绝 `E_WRITE_BATCH_CONFLICT`，跨批次并发轮次无锁。

---

## 6. list_files

### 6.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/list_files.rs`（296） |
| 入参 | `path?` / `maxDepth?`（默认 2，≤8）/ `limit?`（默认 200，≤2000） |
| 匹配语义 | 目录枚举（无模式参数；模式查找由 grep files 模式替代，见 §7） |
| 上限 | 全局 limit 2000 + **每目录 50 条预算折叠**（占位符不占全局配额） |
| 多根 | 无 path 时列全部根（multi-root 提示引导下钻） |
| 附带 | `search_workspace_paths`（@ 提及数据源，TTL 600s 缓存 + fuzzy 评分） |
| 已知缺口 | 无 glob/pattern 参数（→ §15 P1-2） |

### 6.1 设计要点与已知缺口

- **每目录预算**是独有优势：50 条折叠防 node_modules 式巨型目录吃光 listing，深子目录仍有独立预算，占位符不占全局配额、不污染 count。
- **多根引导**：根列表 + 下钻提示是 multi-root 项目语义的独有设计。
- **模式查找缺口**：无 glob/pattern 参数，「一次定位」场景需根→目录→下钻多轮往返；可加可选 `glob` 参数复用 ignore 库 overrides（§15 P1-2）。

### 6.2 源码解析

**入参语义**：无 glob 模式——是"目录浏览"工具而非"模式查找"工具（模式查找由 grep files 模式替代，见 §7）。

**主流程 `run`**（`list_files.rs:48-80`）：

1. **多根特判**（`list_files.rs:55-67`）：`path` 缺省且 extra_roots 非空 → 列全部根（`dir: <绝对路径>`），附 note 引导"用其中某目录作为 path 再调用下钻"；
2. `resolve_read` 解析 base；limit ≤2000、depth ≤8；
3. `walk_with_budget`（`list_files.rs:85-140`，本项目优化）：
   - `ignore::WalkBuilder`（git_ignore + hidden 过滤）+ max_depth；
   - 逐 entry：全局 limit 到 → truncated=true 停；相对 workspace 显示；
   - **每目录预算**：`per_dir[parent] >= 50` 的后续条目不入列，计数进 `overflow[parent_rel]`，**占位符不占全局配额**（深子目录仍有自己的 50 预算）——防 node_modules 式巨型目录吃光整个 listing；
   - 收尾：entries 排序 + `+N more in <dir>` 占位行（排序置尾）；count 只算真实条目；
4. 出参 `{root, count, truncated, entries}`。

**`search_workspace_paths`**（`list_files.rs:143-183`）——非工具路径，@ 文件提及的数据源：全根遍历（max_depth 12，MAX_ENTRIES 50000，超量截断）、TTL 600s 缓存进 `rt.paths_cache`；`fuzzy_filter`（`list_files.rs:186-228`）：大小写不敏感**子序列**匹配 + 连续命中/词首加分（+2+consec）+ 文件名整词命中 +10，按分排序取 limit。

### 6.3 优化建议

- 可选 `glob` 参数（复用 ignore overrides，§15 P1-2）。

---

## 7. grep

### 7.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/grep.rs`（327） |
| 入参 | `pattern` / `path?` / `glob?` / `outputMode?`(content/files/count) / `maxMatches?`(≤200) / `offset?` |
| 引擎 | **ripgrep 库 crate 内嵌**（grep-regex/grep-searcher/ignore，无二进制依赖） |
| 大小写 | `case_smart(true)` 显式 |
| 上限 | 全局 HARD_CAP 5000 命中；页 100（max 200）+ offset 翻页；行文本截 400 字符 |
| 输出结构 | 结构化 JSON（matches/file_counts/files/counts 按模式分形）+ file_counts top100 热点聚合 |
| 二进制 | `BinaryDetection::quit(0)`（NUL 即停） |
| 已知缺口 | path 指向单文件未支持；遍历单线程（→ §15 P2-3） |

### 7.1 设计要点与已知缺口

- **输出模式三态**（content/files/count）是 token 经济上的关键设计（[docs/tool-optimizations-port](./tool-optimizations-port.md) 批次落地）：大范围探查用 files/count 省大量 token。
- **翻页**：offset 翻页 + 精确 total，模型可控拿全量（截断即止的省 token 取向之外的另一种设计平衡）。
- **热点聚合**：content 模式附 file_counts top100（命中数降序），模型可直接看到热点文件再定向下钻。
- **行截断**：行文本截 400 字符防长行爆 token。
- **已知缺口**：path 指向单文件未支持（`resolve_read` 后直接 walk，文件会被当目录 walk 报空）；遍历单线程，大仓库可并行提速（§15 P2-3）。

### 7.2 源码解析

**入参**：`outputMode` 三态（[docs/tool-optimizations-port](./tool-optimizations-port.md) 批次落地）：content（默认零回归）/ files（只列命中文件路径）/ count（每文件精确计数）——**broad survey 场景 files/count 模式省大量 token**。`maxMatches` 默认 100 上限 200；`offset` 翻页（content 按命中序，files/count 按文件序）。

**`run_grep`**（`grep.rs:111-242`）逐段：

1. `RegexMatcherBuilder.case_smart(true)` 编译（非法正则 → `E_ARGS` 干净报错）；
2. 收集容器：`matches`（全部命中 `(path, line, text)`）、`file_counts`（每文件计数）、`total`（AtomicU64）；
3. `ignore::WalkBuilder`（git_ignore + hidden）；glob 参数走 `OverrideBuilder`（无效 glob 报错）；
4. **单线程遍历**（注释："P0 足够；并行遍历可后置优化"，`grep.rs:144`）+ `SearcherBuilder`（line_number + `BinaryDetection::quit(0)`）；
5. 每文件 sink 回调：**HARD_CAP 5000 双检**（walk 循环外层 `total >= 5000` break + sink 内 return false 双闸）；行文本 trim 换行 + **截 400 字符**（防长行爆 token）；计数与 matches 双写；
6. 出参按模式分形：
   - content：页 = skip(offset).take(max) 行对象；附 **file_counts top100**（命中数降序）——模型能直接看到热点文件再定向下钻；
   - files：首次命中序去重的文件列表，按文件翻页；
   - count：全量计数降序，按文件翻页；
   - 都带 `{pattern, total, shown, offset, truncated}` 分页元数据。

### 7.3 优化建议

- `WalkBuilder::build_parallel` 并行遍历 + path 指向单文件时单搜（§15 P2-3）。

---

## 8. subagent

### 8.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/subagent.rs`（401）+ `agents/` 角色注册表 |
| 入参 | `task` / `role` / `maxSteps`（required，1–1000）/ `description?` / `cleanContext?`（默认 true） |
| 角色来源 | 内置注册表（explore/dev/reviewer/code-reviewer/product-manager/tester/title），别名归一 `find()`；未知角色自由字符串 |
| 嵌套限制 | 硬禁（纪律块注入「不得派生子代理」+ exclude_tools 排除 subagent） |
| 权限 | 子代理 exclude_tools 七件固定（ask/subagent/plan/skill/scheduled_task/suggest/wait）+ **父档位排除集合并**（Plan 档的写排除一并继承，堵「借子代理绕过计划模式」）+ **只读角色额外排除 edit/create/delete**（`AgentDef.readonly`：explore/reviewer/code-reviewer，共十件；`command` 保留） |
| 上下文 | 默认干净；cleanContext=false 携带主会话近 6 条消息 |
| 并发 | 全局 4（ACTIVE 原子计数，超出 `E_SUBAGENT_BUSY` 不排队） |
| 进度 | 800ms 轮询 history 发 `sub:step`（步骤数 + 最后工具 + 220 字符摘录） |
| 计费 | usage 记入 stats（kind=sub 挂父会话） |
| 结果 | `report` 全文 + 步数预算 |

### 8.1 设计要点与已知缺口

- **步数预算**：maxSteps required + force_report（步数耗尽强制汇报），成本可控（独有设计）。
- **父档位合并**是亮点：Plan 档排除集并入子代理 DriveParams，堵「借子代理绕过计划模式」。
- **角色全文注入**（[docs/arch-orchestrator](./arch-orchestrator.md)）：`<agent-definition>` 全文注入的信息量高于一行清单式描述；角色当前为内置注册表硬编码，用户自定义角色（工具集/模型/描述）是扩展方向。
- **并发闸**：全局 4 不排队（`E_SUBAGENT_BUSY` 让模型改道），RAII 递减。
- **已知缺口**：同步等待，长任务阻塞主线（后台化评估 → §15 P2-7）；无续接形态（同一子 runtime 多轮，→ §15 P2-7）；子代理不可独立配 model（全局 active_model）。

### 8.2 源码解析

（`subagent.rs:111-297`）逐段：

1. 参数校验：maxSteps clamp 1–1000（默认 25）；task/role 非空；**title 角色硬拒**（内部自动命名角色不可委派，注册表软契约变硬约束，`subagent.rs:121-124`）；
2. 角色命中注册表 → `build_system_extra`（`subagent.rs:61-74`）：`<subagent-discipline>`（无 ask、不派生、不写全局记忆、步数预算内必须输出最终汇报）+ `<agent-definition name>` 全文注入；别名显示规范名（PM→product-manager）；
3. **并发闸**（`subagent.rs:129-138`）：`ACTIVE >= 4` → `E_SUBAGENT_BUSY`（不排队，直接让模型改道）；`GuardGuard` RAII 保证任何退出路径递减；
4. 事件与日志：`sub:spawn` + session_log（spawn 记父会话，子代理轨迹落自己的日志文件）；
5. **独立 runtime**（`subagent.rs:159-169`）：`SessionRuntime::new_sub`（继承 workspace/data_dir/extra_roots）；注册进 `core.subs`（支持 StopSubagent）；cleanContext=false 携带近 6 条消息；任务包装成 `<subagent-task role>` 用户消息；
6. **DriveParams**（`subagent.rs:318-351`）：exclude_tools 七件固定（ask/subagent/plan/skill/scheduled_task/suggest/wait）+ `idle_policy`（只读角色 `NudgeOnly`：16 步纠偏、空转层不终止——失败重复层 3/5 与 6/10、`max_steps` 与强制汇报门仍照常生效，[subagent-idle-watchdog-misfire](./subagent-idle-watchdog-misfire.md)）+ `budget_notice`（低预算提醒）+ `force_report`（步数耗尽强制汇报）+ **父档位合并**：`main_drive_params(&prefs)` 的 exclude_tools/system_extra 并入——Plan 档下子代理同样无写工具；+ **只读角色额外排除 edit/create/delete**（`readonly_extra_excludes`，与父档位集合并存，共十件；`command` 保留）；
7. **进度轮询**（`subagent.rs:199-209`）：800ms tick 读子 history：`summarize_sub_tail`（逆序找最后 ToolUse 名 + 最后 Text/Thinking 尾 220 字符）发 `sub:step`；`SubCleanupGuard`（Drop：abort 轮询 + 摘注册 + armed 时补发 sub:error——panic unwind 收口，防前端卡永久"运行中"）；
8. `drive_agent` 全程驱动；usage >0 记 stats（kind=sub）；**分析角色标记**（`subagent.rs:253-258`）：product-manager/tester 成功返回 → `analysis_done` 置位（G2 批准门消费，见 §11.2）；
9. 事件序：`sub:report`（汇报先入卡）→ `sub:usage` → `sub:done`（注释锚定前端展示顺序）；失败路径 session_log + `sub:error`。

### 8.3 优化建议

- 后台子代理（background=true 立即返回 + 完成注入合成消息）与续接（resumeId 同一子 runtime 多轮）评估（§15 P2-7）。

---

## 9. web_fetch

### 9.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/web_fetch.rs`（228）+ `tools/net.rs`（157） |
| 入参 | `url` / `maxChars?`（默认 60000，1k–500k） |
| 方法 | 仅 GET（POST 等走 http_request） |
| 重定向 | **手动逐跳**，每跳 SSRF 校验 + 限流 |
| SSRF 防护 | **三重**：IP 字面量私网段 + DNS 解析逐 IP 校验 + IPv4-mapped IPv6 归一（net.rs） |
| 限流 | 每主机 1s 最小间隔（HostThrottle，web_fetch/http_request 共享） |
| 正文抽取 | **dom_smoothie Readability** 正文抽取（标题+正文）+ 失败退化粗剥标签 |
| 图片 | 拒绝（content-type 白名单外报错引导 http_request） |
| 大小上限 | 50MB（net.rs MAX_BODY_BYTES，流式限量读取） |
| UA | `CodeWave/0.1 (+local-first desktop agent)`（诚实 UA） |

### 9.1 设计要点与已知缺口

- **安全底线**：SSRF 三重防护 + 手动逐跳重定向（每跳校验）+ 每主机限流 + 50MB 流式限量 + 诚实 UA——本地桌面 agent 的安全取向。
- **正文抽取取向**：Readability 抽「主正文」，噪声少但丢结构（代码块/标题 markdown 保真差）；技术文档场景 markdown 保真更有用 → §15 P1-4。
- **已知缺口**：content-type image/* 一律拒绝，图片直读通道缺失（→ §15 P2-6）。
- **token 经济**：maxChars 显式预算（默认 60k 字符）模型可调。

### 9.2 源码解析

**`tools/net.rs` 公共层**（web_fetch/http_request 共用）：

- `is_private_ip`（`net.rs:13-36`）：v4 私网/回环/链路本地/未指定/广播/CGNAT 100.64/10 + v6 回环/未指定/ULA fc00::/7/链路本地 fe80:: + **IPv4-mapped IPv6 先归一再判**（::ffff:10.0.0.1 绕过漏洞封堵）；
- `guard_host`（`net.rs:102-133`）：`allow_private` 开关（设置项）；IP 字面量直判；域名 **DNS 解析后逐 IP 全查**（多 A 记录里有任一私网即拒——防 DNS rebinding 的静态面）；
- `HostThrottle`（`net.rs:39-72`）：每主机 1s 最小间隔（锁内计算等待，sleep 后重试循环）；
- `read_body_limited`（`net.rs:80-99`）：`bytes_stream()` 流式累计，超 50MB 即断（chunked 响应同样受限）。

**`guarded_get`**（`web_fetch.rs:33-90`）——手动重定向逐跳：

1. 独立 `reqwest::Client`（`redirect(Policy::none())`，connect_timeout 10s，OnceLock 复用；入参 `_client` 保留签名但被禁用——H1 修复注释：自动重定向会绕过逐跳校验）；
2. 逐跳（≤10 次）：URL 解析 → scheme 白名单（http/https）→ `guard_host` + `throttle().wait` → GET（CodeWave UA）→ 3xx 时取 Location `parsed.join` 解析相对地址继续（跨源重定向不回传凭据——本就不附）；无 Location 报错；非 3xx 返回响应。

**主流程 `run`**（`web_fetch.rs:140-196`）：

1. maxChars clamp 1k–500k；`allow_private` 读配置；
2. `guarded_get` → 状态非 2xx → `E_HTTP_STATUS`；**content-type 白名单**（text/html、text/plain、application/xhtml、application/xml）之外 → `E_CONTENT_TYPE`（引导"二进制/媒体走 http_request"——但 http_request 对二进制也只给 binary_size，见 §14.4）；
3. `read_body_limited` 50MB → utf8 lossy；
4. html/xhtml → `extract_article`（dom_smoothie Readability：new(html, url) → parse → title + text_content；Readability 算法提取"主正文"，导航/侧栏/广告噪声被剥掉）失败 → `strip_tags` 粗剥兜底（字符级状态机剥 `<...>` + 空白折叠）并标注退化；
5. 截 maxChars + truncated 标记；出参 `{url, title?, content, chars, truncated}`。

### 9.3 优化建议

- `format: markdown` 可选参数（保结构正文转换，§15 P1-4）。
- 响应图片 → `extra_model_content` 通道（§15 P2-6）。

---

## 10. plan

### 10.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/plan.rs`（217） |
| 入参 | `todos?`（缺省 = 读；给 = 全量替换） |
| 状态机 | ≤100 项、title 非空、**同刻最多一个 in_progress**（`E_PLAN_INVALID`） |
| 持久化 | `store.save_todos`（会话持久化）+ `plan:update` 事件（前端时间线/任务面板） |
| 读取通道 | `{}` 读回 `{todos, rendered}` |
| 渲染 | `render_todos`：`[ ]/[~]/[x]` 文本（模型侧） |
| 系统联动 | G3 范围 gate（全量替换 vs 批准基线 diff 置位计划外标记，`plan.rs:131-145`） |

### 10.1 设计要点

- **读写双形态**：读形态供模型失忆自查（`{}` 读回 `{todos, rendered}`）；写形态走状态机校验（≤100 项 / title 非空 / 同刻最多一个 in_progress）。
- **G3 联动**（[docs/plan-mode-workflow](./plan-mode-workflow.md) §7）：批准基线存在时全量替换 diff 出新增标题 → 置位 `scope_expanded`，下一个写操作触发范围确认弹窗——计划纪律在工具面的落点。
- **已知局限**：todo 只有 title 一级，长计划步骤描述只能塞 title（→ §15 P3-7 detail 字段）。

### 10.2 源码解析

（`plan.rs:107-158`）

- **读写双形态**：`args.todos` 缺省 → 返回当前 todos + rendered（模型失忆时自查）；给了 → 状态字符串映射（非法值 `E_ARGS`）+ `validate_todos` 状态机；
- **G3 联动**（[docs/plan-mode-workflow](./plan-mode-workflow.md) §7）：批准基线存在时，全量替换 diff 出新增标题（`diff_new_todos` 纯函数：保序过滤基线不存在的；重命名按删+增处理）→ 非空且未获 scope_allowed → `scope_expanded` 置位（下一个写操作触发范围确认弹窗，见 §14.10 batch）；
- `plan:update` 事件 + `save_todos` 持久化；description 引导"任何实现请求开头就建 todo + 随手更新 + 新用户轮自动注入当前计划"。

### 10.3 优化建议

- Todo 加 detail 字段（长步骤描述，§15 P3-7；契约变更需前端联动）。

---

## 11. ask

### 11.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/ask.rs`（535） |
| 入参 | `questions[1..5]: {id, question, options[≤6]: {id,label,description?,recommended?}}` + `lightweight?`/`skipAnalysis?`/`switchToAutoEdit?`（gate 声明） |
| 交互 | `ask:opened` 事件 + oneshot channel（[docs/run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) 询问窗口：编号选项/键盘导航/分页） |
| 取消语义 | 取消/超时 → `E_ASK_CANCELLED`（视为拒绝） |
| 批次约束 | **批次唯一调用**（Interactive，`batch.rs:31-43`） |
| 计划批准 | **内嵌批准协议**：approve 选项命中 → 切 AutoEdit 档 + 冻结基线 + 系统消息（G2/G3 硬门校验） |

### 11.1 设计要点

- **批准协议内嵌**：计划批准不设独立出口工具——ask 的 approve 选项 + 切档判定 + G2/G3 硬门（todos 基线必存、分析前置、口令通道防误触）承担完整流程（见 §11.2 第 2/6/7 步），比独立 plan-exit 工具形态更产品化。
- **防升档约束**：Plan 档批准即切；ConfirmEach 档仅 `arch_gate_shape && switchToAutoEdit` 切档（防模型借任意 ask+flag 自由升档）；AutoEdit/FullAccess 不切。
- **已知局限**：问题对象无分组 header、无 per-question 自由输入开关（note 输入恒开）→ §15 P3-7。

### 11.2 源码解析

（`ask.rs:121-251`）功能远超提问本身，逐段：

1. 校验非空；`ask_id` + oneshot channel 注册进 `rt.open_ask`（host 命令回答时 close）；
2. **计划批准形态**（`ask.rs:134-150`）：`arch_gate_shape`（单问题 + 含 `id="approve"` 选项）→ `save_plan_file`：方案全文落盘 `<workspace>/.codewave/tasks/plan-<UTC时间戳>-<rand4>.md`（[docs/run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) 计划卡片"查看完整计划"数据源；失败 None 优雅降级）；
3. `ask:opened` 事件（携带 questions + `switch_to_auto_edit` 标记 + plan_file 路径）；
4. **select 三路**：取消 / oneshot 回答；回答 None → `E_ASK_CANCELLED`；
5. 答案解析（`ask.rs:163-191`）：逐题取 `selections` + `note`；**approve 判定**（宽松匹配：`approve`/含"执行"/含"Approve"）；拼模型可读行 `Q(id): 问题 → 选定：…；补充：…`（未回答标注）；
6. **切档判定 `wants_mode_switch`**（`ask.rs:289-301`，测试矩阵锚定）：Plan 档批准即切；ConfirmEach 仅 `arch_gate_shape && switchToAutoEdit`（防模型借任意 ask+flag 自由升档）；AutoEdit/FullAccess 不切；
7. **G2/G3 批准硬门 `plan_approval_gate`**（`ask.rs:304-349`）：
   - todos 为空 → `E_PLAN_TODOS_REQUIRED`（消灭"批准时无基线"）；
   - **口令通道**：`skipAnalysis` 声明 + 最近用户消息命中口令表（"不用分析/跳过分析/直接改/无需分析/skip analysis"）——Y3 修复：口语"直接改"须与"分析"语义同现才认可（防闲聊误伤）；`lightweight` 声明 + todos ≤3 → 放行（超限报错文案不再重复建议 lightweight，Y4）；
   - `analysis_done` 标记缺失（pm/tester 子代理未成功返回过）→ `E_PLAN_ANALYSIS_REQUIRED`（第 2 次起附升级提示引导用户口令）；
8. 切档执行（`ask.rs:216-246`）：冻结 todos 标题为 `approved_plan` 基线；重置 scope 三态（Y5 防旧放行延带）；prefs 切 AutoEdit（保留 model/effort）；`run:inject` 事件；返回 `plan_approved:true` + 系统指令（"已切换自动编辑模式，立即执行，不要再问；新增计划外步骤会触发范围确认"）。

### 11.3 优化建议

- ask 问题加 per-question custom 开关（自由输入可控，§15 P3-7）。

---

## 12. skill

### 12.0 速览

| 维度 | 设计 |
|---|---|
| 源文件 | `tools/skill.rs`（62） |
| 入参 | `skill` / `args?`（调用方上下文） |
| 技能发现 | `core.skills` 索引：workspace `.codewave/skills` + 项目目录 + 全局 + `~/.claude/skills` 兼容 |
| 未命中 | `E_SKILL_NOT_FOUND` + **可用技能名单**（引导自纠） |
| 已知缺口 | 无技能附属文件清单（→ §15 P1-5） |

### 12.1 设计要点

- **自纠友好**：未命中报错附全部可用技能名（引导模型改用存在的技能）。
- **上下文通道**：`<caller-context>`（args）把调用方上下文注入正文。
- **已知缺口**：无技能附属文件清单——技能目录里的脚本/参考文件，模型拿到清单才知道能直接调用什么（§15 P1-5）。

### 12.2 源码解析

（`skill.rs:39-61`）

索引 `get(workspace, data_dir, disabled_skills, project_dir, name)` 未命中 → 报错附**全部可用名**；命中 → `<skill-loaded name>` 包裹正文 + 可选 `<caller-context>`（args 注入）+ origin 元数据。

### 12.3 优化建议

- 技能附属文件清单（目录内非 SKILL.md 文件，限 10 个，§15 P1-5）。

---

## 13. 未内置的工具形态评估

> 以下工具形态在本项目工具层中**无对应实现**（或仅有机制片段），逐个记录形态定位与不引入/后置评估的理由，作为设计决策存档。

### 13.1 `apply_patch`（补丁格式编辑通道）

**定位**：gpt-5 系模型的**补丁格式编辑通道**——该系模型被训练用 `*** Begin Patch` 格式输出多文件补丁，工具原生承接；启用时通常替代 edit/write。

**机制要点**：入参单字段 `patchText`；解析产出 hunks（add/update/delete + move_path）；**逐 hunk 预演后单次审批**（全部文件相对路径 + 拼接 totalDiff + 每文件结构化统计，一次弹窗看全多文件改动）→ 应用（add/update/delete/move）→ 逐文件格式化与诊断，输出 git 风格状态行（A/D/M）。

**评估**：补丁格式是**单请求多文件原子编辑**的另一形态（本项目 edit 已支持多文件，靠 oldText/lineRange 描述变更）；若未来接入 gpt-5 系模型可再评估（§15 P3-4）。当前不引入。

### 13.2 `websearch`（联网搜索）

**定位**：联网搜索工具——接外部搜索 provider（Exa/Parallel 类搜索服务，BYOK 或平台方 key），provider 侧可直接产出 LLM 优化的上下文片段。

**机制要点**：入参 `query` / `numResults?` / `livecrawl?` / `type?` / `contextMaxCharacters?`；实现上按 provider 发 JSON-RPC `tools/call`（或等价 REST），响应兼容直接 JSON 与 SSE 两种形态；工具对模型的可见性与 provider 配置/开关绑定。

**评估**：本项目 web_fetch 只能抓已知 URL，无搜索工具——该形态识别的缺口可由 BYOK 可配置搜索 provider 补齐（用户自带 Exa/Tavily/SerpAPI key，走 http_request 基建即可），见 §15 P3-3。

### 13.3 `plan_exit`（计划批准出口工具形态）

**定位**：plan agent 完成计划后的**批准出口**，入参空对象；流程为「取计划文件路径 → 弹 Yes/No 确认 → 批准后合成一条 build 态用户消息驱动执行」。

**机制本质**：该形态的 plan→build 切换是 **agent 切换**（plan 态无写工具，build 态有），本项目等价物是**档位切换**（ApprovalMode Plan→AutoEdit，同一 agent 改权限），已在 ask 批准协议中实现且带 G2/G3 硬门（§11.2）——不引入独立工具。

### 13.4 `lsp`（LSP 查询工具）

**定位**：把 LSP 查询直接暴露为工具（goToDefinition/findReferences/hover/documentSymbol/workspaceSymbol 等 9 种操作，1-based line/character 定位）。

**配套生态价值**：LSP 作为工具层的**横切服务**价值更大——read 预热（读取时后台通知 LSP 解析，后续 edit 诊断更及时）、edit/write 写后全项目诊断（本文件 + 受波及文件限量，防诊断风暴）。

**评估**：**已落地**（原结论「引入 LSP（rust-analyzer/tsserver 常驻）是大工程，当前 validation 方案是合理取舍」作废）——写后校验已从单文件外部命令升级为项目级常驻 LSP 语义诊断（六语言 + 写前基线差集 + 三态文案，**未运行绝不出「通过」**），实施报告见 [lsp-post-write-diagnostics](./lsp-post-write-diagnostics.md)。本节保留作决策沿革存档；§15 P3-1 已完成，read 预热 / LSP 查询工具仍是后续可选项（客户端层已做成通用通道，只差封装）。

### 13.5 `execute` / code-mode（MCP 沙箱编排）

**定位**：**沙箱解释器编排 MCP 工具**——模型写一段受限脚本，脚本内以循环/条件/变量调用全部 MCP 工具，一次 tool call 完成多步 MCP 编排；可见 MCP 工具按 server 分组动态生成解释器 instructions。

**机制要点**：abort 竞速（沙箱执行 vs 取消）；按 server 名分组工具目录；每个 MCP 工具包成沙箱工具（调用前过权限）；`calls[]` 状态实时推 UI；MCP 返回的 image/audio/resource 转 data URL 收集为附件；失败把 error + 建议拼给模型。

**评估**：MCP 密集场景「一 call 一动作」的往返成本高，脚本级编排是有效压缩手段；本项目有 batch 并发但**无循环/条件编排**。远期评估（依赖沙箱选型，Rust 侧可用 deno_core/rquickjs，成本高），见 §15 P3-2。

### 13.6 `invalid`（未知工具兜底工具形态）

**定位**：未知工具名的**兜底工具**：`{tool, error}` 入参；宿主把对不存在工具的调用改道到它，让错误以正常 tool_result 形态回流模型（而非协议层报错）。

**评估**：本项目 batch 层 `E_UNKNOWN_TOOL` 错误结果（`batch.rs:202-208`，文案"可用工具见系统提示词工具列表"）**语义已覆盖**，不引入。

### 13.7 设计参考附注（未采用的机制）

- **统一截断管道对工具透明**：截断细节见 §1.4 末条——提示按模型能力分流（有子代理工具引导委托处理，否则引导 offset/limit 续读）是可取细节；整体管道不采用（§15 不采纳清单：本项目双通道 + 工具语义化裁剪更精细）。
- **effect Schema → JSON Schema 规范化器**（$defs 内联、anyOf 简并、integer 补 min/max、缓存）：本项目 schema 是手写 JSON 常量，无此需求。
- **时序递增 ID**（前缀 + 递增，截断文件命名用）：本项目用 uuid。

---

## 14. 其余内置工具与基础设施附注

> 以下工具与执行/安全基建在 §2–§12 之外，按注册名/文件列出。

### 14.1 `batch_read`（`tools/batch_read.rs`，27 行）

read 的 deprecated 别名（历史会话兼容）：name/description 声明弃用，schema 与 run 直接转发 `ReadTool`。设计上仅过渡期保留。

### 14.2 `calculate`（`tools/calculate.rs`，355 行）

**定位**：精确数学求值，杜绝模型心算错误，且**不经过 shell**（无命令注入面）。

**入参**：`expression: string`。**出参**：`{expression, result}`（结果 `%.12f` 去尾零格式化）。

**实现**（手写递归下降，零依赖）：

- **词法**（`tokenize`，`calculate.rs:30-84`）：数字（多小数点拒绝）/ 标识符 / `+ - * / % ^` / 括号 / 逗号；非法字符逐字符报错；
- **文法**：`expr(+,-) → term(*,/,%) → factor(^ 右结合，一元负号低于 ^：-2^2=-4，指数侧允许 2^-1) → atom(数字 | 括号 | 常量 pi/e/tau | 函数调用)`；
- **函数**（`apply_function`，`calculate.rs:216-270`）：sqrt/cbrt/ln/log(a,b)/log2/log10/exp/abs/floor/ceil/round/sin/cos/tan/asin/acos/atan/atan2/min(变参)/max(变参)/pow；参数数校验 + 未知函数报错附全量清单；
- **守卫**：除零/模零、幂溢出、结果非有限数（NaN/Inf）、尾部多余 token——全部干净报错（`E_CALC`）；
- 测试锚定：右结合 `2^3^2=512`、一元 `2^-1=0.5`、`10^10^10` 溢出拒绝。

对账目/位运算/浮点比较场景是实用补充。

### 14.3 `delete`（`tools/delete.rs`，77 行）

**入参**：`path` / `recursive?`。**流程**：`resolve_write` 白名单 → **危险路径守卫** `is_dangerous_delete_multi`（`pathutil.rs:155-193`：全部根自身、每根 `.git`（含内容）、家目录、系统目录（/、macOS /System//usr//etc、Windows SystemRoot）——H2 修复 extra 根）→ `symlink_metadata`（不跟随链接）→ 目录：空则 remove_dir，非空需 recursive；文件 remove_file。

### 14.4 `http_request`（`tools/http_request.rs`，203 行）

**定位**：通用 REST 调用（web_fetch 面向"读网页"，本工具面向"调 API"）。

**入参**：`url` / `method?`（GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS 白名单）/ `headers?` / `query?`（map 拼接到 URL，手写 urlencode）/ `body?`（JSON 序列化）/ `timeoutSeconds?`（默认 60，clamp 1–120）。

**流程**：

1. 每请求新建 `reqwest::Client`（`redirect(none)` + timeout；**未复用连接池**——对比 web_fetch 的 OnceLock 全局 client，多次调用会有 TLS 握手重复成本，P2 优化点）；
2. **手动逐跳重定向**（≤10）：每跳 `guard_host` SSRF + `throttle_pub().wait` 限流（与 web_fetch 共享限流器）；重定向**不重发 body**（当前实现 continue 后 body 参数仍在，重定向后的 POST 会再带 body——语义上 307/308 应重发、303 应转 GET，当前一律原样重发，P3 修正点）；
3. 响应头**脱敏**：`set-cookie`/`www-authenticate` → `[redacted]`；
4. content-length 预检 50MB + `resp.bytes()`（**非流式**——对比 web_fetch 的 `read_body_limited` 流式限量，超长 chunked 响应会先全量入内存，P2 对齐）；
5. 预览分形：JSON/JS → 尝试 parse + pretty（24KB 截断）；text/xml/html → 96KB 截断；二进制 → `binary:true + binary_size + binary_mime`（不回内容）。

**kind**：`Network`。

### 14.5 `render_html`（`tools/render_html.rs`，59 行）

**入参**：`html`（≤50k 字符，非空）/ `title?`。**出参**：`{title, html, chars}`——前端 sandbox iframe（无网络/无同源）渲染小部件。纯展示通道，无文件系统副作用。用于快速可视化/原型演示。

### 14.6 `scheduled_task`（`tools/scheduled_task.rs`，125 行）

**入参**：`action`(create|list|delete) + 对应字段。**create**：name/instruction/schedule 三必填；`core::scheduler` 语法：`cron:<5 字段>` / `every:<n> <m|h|d>` / `once:<RFC3339>`；`initial_next` 计算（once 已过期报错）+ `parse_schedule` 双校验；任务挂 `project_id`（项目任务持久化，自由会话进程本地）。**list/delete** 走 `core.tasks` 表。任务运行时以隔离 task runtime 驱动 agent（`is_task_runtime=true`：跳过产物登记，见 §5.2）。

### 14.7 `service`（`tools/service.rs`，405 行）

**定位**：长驻后台进程管理（dev server 等）——command 是"跑完即收"，service 是"常驻可查"。

**入参**：`action`(start|stop|list|read) + `name/command/cwd?`（start）或 `id/tailBytes?`（read）。

**实现要点**：

1. **start**（`service.rs:210-351`）：command/cwd 必填校验 → **同款 fence 三档审批**（与 command 完全一致的 Confirm 分派）→ `bash -lc` spawn（进程组/CREATE_NO_WINDOW）→ `RingLog`（**512KB 环形字节缓冲**，`service.rs:33-58`：VecDeque 逐字节 push，超 cap pop_front——**逐字节而非按 chunk 淘汰**，注释未提性能考量，大流量下有优化空间，P3）→ stdout/stderr **双独立泵**（M9 修复：顺序 read 单流静默会卡死另一路）→ 1s ticker 发 tail 增量事件（内容变化才发）→ 双泵 join 的 watcher 置 done + exited 事件；进程本体 wait 防僵尸；`ServiceTable`（dashmap）上限 16（超出明确报错——此前静默失败修复）；
2. **stop**（`service.rs:103-138`）：cancel 令牌 → 进程组 SIGTERM（Windows taskkill /T 无 /F）→ 10s 宽限轮询 → 仍活 SIGKILL（/F）；
3. **read**：`log.tail(n)`（默认 8192 字节）；**list**：id/name/command/pid/uptime/log_bytes。

注意：`start_service` 硬编码 `bash -lc`（`service.rs:257`）——**Windows 无 Git Bash 时 PowerShell 用户会 spawn 失败**，与 command.rs 的 shell 探测不一致（P2 修）。

### 14.8 `suggest`（`tools/suggest.rs`，103 行）

**入参**：`items[1..4]`（非空、trim 后 80 字符截断）。**行为**：`prioritize_commit_first` 确定性重排（含 "commit" 的授权建议置顶——产品约定 git 由用户执行，授权 commit 是最常见收尾动作）；发 `run:suggestions` 事件（前端 chip）；**成功即结束 run**（run 循环对 suggest 特判，`BatchOutcome.suggest_items`，`batch.rs:149-155`）。批次唯一调用约束（Interactive）。

### 14.9 `wait`（`tools/wait.rs`，46 行）

**入参**：`seconds`(1–3600) + `reason`（必填，形成自解释历史）。**行为**：`tokio::select!` sleep vs cancel——可取消的被动等待（等 dev server 起之类的场景），取消 → `E_CANCELLED`。批次唯一调用约束（Interactive）。设计动机：防止模型用 `sleep 999` 命令占用 command 通道。

### 14.10 基础设施附注（非模型面工具，属工具层执行/安全基建）

- **`batch.rs`（639 行）——批次执行器**（[docs/p0-plan](./p0-plan.md) §6.1.1/§6.3.3）：五步策略：① Interactive 批次唯一（违者全批 `E_BATCH_POLICY`）；② 同批同物理路径多写全拒（`canonical_arg_paths`：相对路径 join workspace 再 canonical，M1 修复 `./` 前缀漏判）；③ plan 档/MCP 排除**硬门**（spawn 前拒绝 + 终结性文案防"重新 read"死循环，`batch.rs:72-103`）；④ 执行：写串行（`file_ops` 互斥锁）/ 其余并发 4（信号量）+ **catch_unwind panic 兜底**（`E_TOOL_PANIC`）；⑤ ConfirmEach 预弹窗 + G3 范围 gate（command 写目标探测专用 policy，堵 shell 重定向绕过）+ 结果事件（`tool:result/error`，args_preview 200KB 上限保 JSON 可解析）。
- **`net.rs`（157 行）**：§9.2 已解析（SSRF/限流/限量读取三件套）。
- **`pathutil.rs`（255 行）**：§1.5/§5.2/§14.3 已覆盖核心（safe_join/resolve_read/resolve_write/canonical_best_effort/危险删除守卫）。
- **`validation.rs`（163 行）**：§4.2 已解析（按语言路由的写入后校验）。
- **`compact.rs`（91 行）**：§1.4 已解析（模型侧压缩）。
- **`fuzz.rs`（78 行，`#[cfg(test)]`）**：edit 路径解析的模糊测试，非运行时组件。

---

## 15. 优化建议汇总（指导本项目工具优化）

> 依据前文逐章分析汇总，按优先级分级。P0 = 高收益低风险，建议尽快；P1 = 明确收益需设计；P2 = 机会性优化；P3 = 远期/低频。每项标注依据章节，细节回看对应章节的「设计要点与已知缺口 / 优化建议」小节。

### P0（高收益低风险）

| 编号 | 工具 | 建议 | 依据 | 要点 |
|---|---|---|---|---|
| P0-1 | `edit` | **CRLF 归一匹配 + 2 个低风险模糊替换器** | §5.3 | 匹配前把文件与 oldText 都归一 `\n`、写回按文件原 EOL（约 5 行逻辑）；再补 LineTrimmed（缩进偏差）与 IndentationFlexible（整块缩进平移）两级兜底——这两类是模型 oldText 失配的最高频原因，且无"错杀大段"风险；**不引入** Levenshtein 块锚定等激进模糊链（比例守卫复杂度高、错配风险与本工具的 version 令牌机制冲突，见 §5.1） |
| P0-2 | `command` | **动态 description**：注入实际 shell（bash login/PowerShell）、平台、fullOutput 双档语义、截断/超时常量 | §2.3 | 让模型对 `&&` vs `if ($?)`、workdir 优先、输出策略有明确预期。已有 `shell_description()` 探测（`command.rs:58-60`），只差把它与常量拼进 description；顺带补 PowerShell 5.1 不支持 `&&` 的提示（Windows 回退 PowerShell 时关键） |
| P0-3 | `read` | **per-file 局部错误**：单文件失败不炸整批 | §3.3 | 当前 `read.rs:153` 等早退 return 丢弃已读文件结果（`resolve_read`/metadata/read 三处）；改为失败文件进结果列表标 `error` 字段，成功文件照常返回。批量接口的失败隔离是批处理基本功 |
| P0-4 | `read` | **单行 2000 字符截断 + 二进制采样启发** | §3.3 | 单行截断防 minified 文件单行几十万字符灌爆上下文（截断加后缀标注）；读文件头 256 字节做 NUL/非打印密度判定，二进制直接报错不输出乱码。两者合计约 30 行 |
| P0-5 | 全局 | **截断落盘通道统一**：compact_for_model 的 head/tail 截断同样"凡截断必落盘" | §1.4 | command 已有 spill（[docs/tool-optimizations-port](./tool-optimizations-port.md)），但 grep 大结果/http_request 96KB 预览/其他工具被 `truncate_head_tail` 截掉的中间段模型永远拿不回；把 spill 文件路径塞进截断标记即可复用 command 的 `<data>/tmp/cmd-output` 清理机制 |

### P1（明确收益，需要小设计）

| 编号 | 工具 | 建议 | 依据 | 要点 |
|---|---|---|---|---|
| P1-1 | `edit`/`create` | **ConfirmEach 审批弹窗渲染 diff 预览**（替代参数 JSON dump） | §4.3/§5.3 | diff 级审批的用户可读性显著优于 `to_string_pretty(args)`；edit 预演阶段已算出新内容（`edit.rs:168`），生成 diff 只差一个 diff 库（similar crate 或手写 LCS）；create 直接展示新内容全文 |
| P1-2 | `list_files` | **可选 `glob` 参数**（复用 ignore overrides） | §6.3 | 一次 pattern 定位替代根→目录→下钻多轮往返；`grep.rs:133-141` 已有 `OverrideBuilder` 用法可复用；每目录预算机制保留（比固定条数硬截断更稳） |
| P1-3 | 权限体系 | **工作区外路径询问放行**（运行时扩根） | §1.5/§2.3 | 当前 `E_PATH_OUTSIDE` 硬拒，模型无法操作用户指定的区外文件（如配置文件、兄弟项目）；机制 = 取目标父目录构造 `<dir>/*` glob 询问 → 用户批准 → 加入会话级 allow roots。`extra_roots` 已有数据结构（`WriteRoots.extra`），只差审批 UI 与运行时 append；配 fence 的既有 Confirm 通道即可 |
| P1-4 | `web_fetch` | **`format: markdown` 可选参数**（保结构正文转换） | §9.3 | Readability 纯文本丢代码块/标题结构，技术文档场景 markdown 保真；Rust 侧可 `htmd`/`readability` 双输出或 dom_smoothie 的 html 输出再转；默认仍 text 不改现行为 |
| P1-5 | `skill` | **技能附属文件清单**（目录内非 SKILL.md 文件，限 10 个） | §12.3 | 模型拿到清单才知道技能目录里有什么脚本/参考可直接用；`ignore::WalkBuilder` 一把梭即可 |

### P2（机会性优化）

| 编号 | 工具 | 建议 | 依据 | 要点 |
|---|---|---|---|---|
| P2-1 | `read` | 误拼文件名 "Did you mean" 建议（同目录双向子串匹配取 3） | §3.3 | 约 20 行 |
| P2-2 | `create` | description 强化"整文件重写请直接 overwrite:true"（省一次失败往返） | §4.3 | 文案级修改 |
| P2-3 | `grep` | `WalkBuilder::build_parallel` 并行遍历；顺带支持 path 指向单文件 | §7.3 | ignore 库原生并行；HARD_CAP 原子计数已线程安全 |
| P2-4 | `http_request` | 全局 OnceLock client（复用连接池）+ `read_body_limited` 流式限量（对齐 web_fetch） | §14.4 | 每请求新建 client 有重复 TLS 握手成本；非流式 bytes() 对长 chunked 响应先全量入内存 |
| P2-5 | `service` | start 复用 command.rs 的 shell 探测（当前硬编码 `bash -lc`，无 Git Bash 的 Windows 机直接失败） | §14.7 | `detect_shell()` 已导出 |
| P2-6 | `web_fetch` | 响应图片 → `extra_model_content` 通道（网页图表/截图直读） | §9.3 | read 的图片注入管道现成 |
| P2-7 | `subagent` | 评估后台子代理（background=true 立即返回 + 完成注入合成消息）与 resumeId 续接 | §8.3 | `run:inject` 事件与 `core.subs` 注册表已具备底座；收益 = 长任务不阻塞主线 |
| P2-8 | registry | 按模型动态裁剪工具列表（如某些模型不给 suggest/render_html） | §1.1 | schemas_token_estimate 已统计成本，可做条件注入 |

### P3（远期/低频）

| 编号 | 领域 | 建议 | 依据 | 要点 |
|---|---|---|---|---|
| P3-1 | LSP | **已完成**：写后语义诊断已落地（六语言常驻 server + 写前基线差集 + 三态文案），报告见 [lsp-post-write-diagnostics](./lsp-post-write-diagnostics.md) | §13.4 | 原「成本高 → 故意后置」结论已推翻（误报结构性 + 每写必付冷启动 + 失败静默）；read 预热 / 语义查询工具仍为后续可选项 |
| P3-2 | MCP 编排 | 评估沙箱脚本编排（循环/条件调用 MCP 工具） | §13.5 | Rust 侧需 deno_core/rquickjs 选型；仅 MCP 密集场景收益明显 |
| P3-3 | 搜索 | 可配置 websearch provider（用户自带 Exa/Tavily key，复用 http_request 基建） | §13.2 | BYOK 定位自然延伸，非急需 |
| P3-4 | apply_patch | 若接入 gpt-5 系模型再评估补丁格式通道 | §13.1 | 当前多文件 edit 已覆盖需求 |
| P3-5 | `http_request` | 307/308 重发 body、303 转 GET 的语义修正 | §14.4 | 低频边界 |
| P3-6 | `service` | RingLog 按 chunk 淘汰替代逐字节 | §14.7 | 大流量日志下 CPU 优化 |
| P3-7 | `plan`/`ask` | Todo 加 detail 字段；ask 问题加 per-question custom 开关 | §10.3/§11.3 | 契约变更需前端联动 |

### 不采纳方案清单（明确不做）

| 方案 | 不采纳原因 |
|---|---|
| 未知工具兜底以独立工具承载（§13.6） | `E_UNKNOWN_TOOL` 错误结果已覆盖同等语义 |
| 计划批准独立出口工具（§13.3） | ask 批准协议 + G2/G3 硬门是更完整的产品化方案 |
| 统一截断管道包裹全部工具（§1.4/§13.7） | 双通道（data 全量给前端 + compact 给模型）与 command 语义化裁剪是更精细的设计；要做的是 P0-5 的落盘补齐，不是换管道 |
| edit 的 Levenshtein 块锚定/转义归一等激进模糊链（§5.1） | 错杀风险 + 与 version 令牌机制哲学冲突；只取 P0-1 的两级低风险替换器 |
| shell 的 tree-sitter AST 路径扫描审批（§2.1） | fence 的 L2 AST 已覆盖命令解析（安全视角），read/write/edit 的路径白名单已管文件面；外部路径场景由 P1-3 统一解决 |
| 插件工具动态加载 / 动态 schema 桥（§13.7） | 无插件体系（MCP 已覆盖扩展面），暂无需求 |

---

## 16. 结语

CodeWave 工具层的设计取向：

- **本地优先安全**：SSRF 三重防护、fence 三层、危险路径守卫、version 版本令牌、诚实 UA。
- **token 经济**：fullOutput 双档、grep 三模式、每目录预算、语义化 signal line、maxChars 显式预算。
- **产品化深度**：批次策略、产物登记、G2/G3 计划硬门、事件面 27 键。

工具层的短板集中在**模型侧输入容错**（edit CRLF/缩进、read 单行与二进制）与**审批表达力**（diff 级预览）两点——P0/P1 建议均围绕此展开；其余设计差异多为取向取舍而非优劣。
