# 34 · 内置工具优化与补齐实施方案（基于 [docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) 的工具设计说明）

> 依据：[docs/builtin-tools-source-comparison.md](./builtin-tools-source-comparison.md) §15 的分级建议（P0×5 / P1×5 / P2×8 / P3×7）。本文档把其中"现有工具优化"与"缺失工具补齐"整理为**可直接实施的工程方案**：每项含改动文件、详细设计（数据流/函数签名/错误码）、schema 与契约影响、测试要点、验收标准、风险与回滚。
>
> 实施约束（全程遵守）：工具 schema 一律 `additionalProperties:false` 并同步 registry 契约测试；配置结构变更 serde default 兼容；事件面不新增键（复用既有 `approval:*` / `sub:*` / `run:inject`）；core 不依赖 tauri；改后端 `cargo test` 全绿 0 warning，涉前端 `pnpm --dir ui test` + `build`；GUI 行为交付手动验证清单；git 由用户执行。
>
> 已核验的实施事实（本文引用）：
> - `similar = "2"` 已在 `src-tauri/Cargo.toml:45`（diff 渲染零新增依赖）
> - 审批弹窗 detail 渲染于 `ui/src/features/tools/AskPanel.tsx:203` 的 `<pre className="ask-detail">`（diff 文本可直接展示，**零前端改动**）
> - `SkillMeta.origin` = 技能文件路径，内置为 `"<builtin>"`（`src-tauri/src/skills/mod.rs:17,274`）——技能目录可由 `origin.parent()` 取得
> - 主会话 run 链路：`start_chat`（`core/agent.rs:294`，`rt.running` CAS 守卫）→ `run_chat`（`core/agent.rs:378`）；运行中注入通道 `rt.inject_tx` 由 run 循环每步排空（`core/agent.rs:600-613`）
> - `format_lines` 仅 `tools/read.rs` 内部消费（单行截断改造无外溢）
> - reqwest 0.13（支持 per-request `.timeout()`）

---

## 1. 批次总览与依赖

| 批次 | 内容 | 条目 | 后端规模估算 | 前端 | 依赖 |
|---|---|---|---|---|---|
| **A · 高收益低风险**（P0 全部 + 2 个轻 P1） | 现有工具优化：edit 容错、command 动态描述、read 三项、截断落盘、list_files glob、skill 清单 | A1–A7 | ~700 行（含测试） | 无 | A1→B1（EOL 归一复用）；A2→C1（动态描述机制复用）；A3→A4/B4（per-file 错误通道） |
| **B · 审批表达力与体验**（P1 剩余 + 高价值 P2） | diff 审批、工作区外路径放行、web_fetch markdown、P2 系列七项 | B1–B10 | ~600 行 | 无（B1/B2 复用现有弹窗） | B1 依赖 A1 |
| **C · 缺失工具补齐** | `web_search` 新工具、后台子代理 + 续接、按模型工具裁剪 | C1–C3 | ~800 行 | C1 设置页 ~150 行；C2/C3 无 | C1 依赖 A2 |
| **D · 远期评估不排期** | LSP / code-mode / apply_patch 通道 | — | — | — | 见 §5 |

每批次一组提交（用户执行 git），批次内条目可独立提交。建议顺序 A → B → C；A 内部建议 A1 → A3 → A4 → A2 → A5 → A6 → A7（A3 先行为 A4/B4 铺 per-file 错误通道）。

---

## 2. 批次 A：现有工具优化（高收益低风险）

### A1（P0-1）edit：EOL 归一匹配 + 两级低风险模糊匹配

**现状**：`apply_changes`（`tools/edit.rs:54-89`）对原始文本字节精确匹配——CRLF 文件 + 模型给 LF oldText = `oldText 在文件中不存在`（Windows 项目高频）；匹配失败只能 lineRange 兜底或报错重读。

**设计**（改 `tools/edit.rs`，纯函数层改动，无 schema 变更）：

1. **EOL 归一**：
   ```rust
   fn detect_eol(text: &str) -> &'static str { if text.contains("\r\n") { "\r\n" } else { "\n" } }
   ```
   `run()` 中读文件后：`let eol = detect_eol(&text);` → 内容、oldText、newText 全部 `replace("\r\n", "\n")` 后进入 `apply_changes`；写回前若 `eol == "\r\n"` 则 `new_text.replace('\n', "\r\n")` 再 `into_bytes()`。**version 令牌仍按原始字节计算**（不变）。局限（注释说明）：混合 EOL 文件会被整体归一为 CRLF。
2. **两级模糊替换器**（仅当 oldText 精确命中 0 次、且在 lineRange 兜底之前尝试）：
   ```rust
   /// 返回 (匹配到的原文段, 匹配方式说明)；0 处 None，>1 处由调用方报"需加长上下文"
   fn fuzzy_line_trimmed(content: &str, find: &str) -> Vec<String>   // 逐行 trim() 相等的滑窗，yield 原文段（保真实缩进）
   fn fuzzy_indent_flex(content: &str, find: &str) -> Vec<String>    // 去公共最小缩进后比较同行数窗口，yield 原文段
   ```
   `apply_changes` 的 oldText 分支改为：精确 `count()==0` 时依次尝试两级 fuzzy；恰 1 处 → 用该原文段做 `replacen`，并把「oldText 未精确命中，已按（行首尾空白归一 / 缩进平移归一）匹配」追加进返回的 warnings；0 处 → 走 lineRange 兜底或报错；≥2 处 → `oldText 模糊匹配到 N 处，需加长上下文使其唯一`。`apply_changes` 签名改为 `-> Result<(String, Vec<String>), String>`（第二项 warnings，run 中与 stale_warnings 合并）。
3. **明确不做**：Levenshtein 块锚定、转义归一、空白归一等激进策略（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §15 不采纳清单——错杀风险 + 与 version 令牌哲学冲突）。

**测试要点**（`edit.rs` tests 模块新增）：
- CRLF 文件 + LF oldText 成功编辑且写回保持 CRLF（`crlf_file_edited_with_lf_oldtext`）
- LF 文件 + CRLF oldText 同样成功（对称）
- 缩进整体平移（原 2 空格、oldText 给 4 空格）→ fuzzy_indent_flex 命中 + warning
- 行首尾空白差异 → fuzzy_line_trimmed 命中 + warning
- 模糊命中 2 处 → 报错含"需加长上下文"
- version 令牌不受 EOL 归一影响（stale 判定用原始字节）

**验收**：上述测试全绿；`full_edit_flow_with_rollback` 等既有测试零修改通过（默认行为仅在原报错路径上增强）。
**风险与回滚**：fuzzy 误匹配面被"唯一性强制 + warnings 透明化"约束；回滚 = revert 单文件。

### A2（P0-2）command：动态 description（shell/平台/常量注入）

**现状**：`fn description(&self) -> &'static str` 静态文案（`tools/mod.rs:150-152` trait 约束 `&'static str`），模型不知道实际 shell、截断预算、PowerShell 5.1 无 `&&` 等。

**设计**（改 `tools/mod.rs` + `tools/command.rs` + `tools/registry.rs`，零 breaking）：

1. trait 新增**默认方法**（其余 19 个工具零改动）：
   ```rust
   /// 渲染后的 description（可含运行时环境注入）；默认静态原文
   fn description_rendered(&self) -> String { self.description().to_string() }
   ```
2. `CommandTool::description()` 返回含占位符的模板串（仍为 `&'static str`），如 `${shell}` / `${platform}` / `${ps_chain_note}` / `${default_timeout}` / `${max_timeout}` / `${tail_short}` / `${tail_long}`；`description_rendered()` 做替换：
   - `shell` = `shell_description()`（已有全局探测，`command.rs:58-60`）
   - `platform` = `std::env::consts::OS`
   - `ps_chain_note`：shell 为 PowerShell 时插入「Windows PowerShell 5.1 不支持 `&&`；依赖前序成功请用 `; if ($?) { cmd2 }`」，bash 时为 `&&` 指引
   - 超时/截断常量直接引用 `DEFAULT_TIMEOUT_SECS` / `MAX_TIMEOUT_SECS` / `MODEL_TAIL_CHARS` / `FULL_TAIL_CHARS`（改常量描述自动同步）
3. `registry.rs tool_defs()`（`registry.rs:53-65`）：`description: t.description_rendered()`。排序与 cache 稳定性不受影响（进程内 shell 探测是 OnceLock，描述恒定）；`schemas_token_estimate` 自动反映新长度。

**测试要点**：registry 测试新增断言——command 的 rendered description 不含 `"${"` 残留且包含 `shell_description()` 子串；既有 `default_tools_sorted_and_strict` 不变。

**验收**：单测过；手动 `pnpm tauri dev` 起会话，系统提示词工具列表可见实际 shell 与常量（手动清单项 M-1）。

### A3（P0-3）read：per-file 局部错误（失败隔离）

**现状**：`read.rs` 主循环三处早退 `return ToolOutcome::err(...)`（resolve 失败 `read.rs:153`、metadata 失败 `:157`、非文件 `:160`、读取失败 `:164`）——批量调用中一个文件失败丢弃全部已读结果。

**设计**（改 `tools/read.rs`，无 schema 变更）：
- 循环体统一错误出口 `push_file_error(&mut out_files, &f.path, code, msg); continue;`，写入形如 `{"path": ..., "error": "E_PATH_OUTSIDE: 路径超出可读边界"}`（成功项结构不变，模型侧向后兼容）。
- 循环结束：全部条目带 error → 整体 `err`（取首个错误码，`files` 结果仍放进 `data` 供前端展示）；部分成功 → `ok` + warnings 汇总「N 个文件读取失败：path1、path2…」。
- 图片分支的 `E_TOO_LARGE` 同样走 per-file error。

**测试要点**：2 成功 + 1 越界路径 → ok、错误项含 error 字段、warnings 有汇总；全部失败 → err 且 data.files 保留错误项；单文件失败不污染 images 通道。

### A4（P0-4）read：单行 2000 字符截断 + 二进制采样防护

**设计**（改 `tools/read.rs`，无 schema 变更）：

1. `const MAX_LINE_CHARS: usize = 2000;`；`format_lines`（`read.rs:86-100`）逐行渲染时超长截断并追加后缀 `…（行已截断至 2000 字符）`。仅 read.rs 内部消费，无外溢。
2. 二进制防护（仅采样启发，不做扩展名黑名单）：
   ```rust
   /// 顺序敏感：必须在 looks_utf16 判定之后对"非 UTF-16 原始字节"采样，否则 UTF-16 高 NUL 密度被误杀
   fn looks_binary(bytes: &[u8]) -> bool {
       let sample = &bytes[..bytes.len().min(256)];
       if sample.contains(&0) { return true; }                       // NUL 即二进制
       let non_printable = sample.iter().filter(|&&b| b < 9 || (b > 13 && b < 32)).count();
       sample.len() > 0 && non_printable * 10 > sample.len() * 3     // >30%
   }
   ```
   文本分支顺序：`looks_utf16 → decode → 图片扩展名 → looks_binary（对原始 bytes）→ 1MB 检查 → 行窗口`；命中二进制 → per-file error（依赖 A3 通道）「疑似二进制文件，请改用 command 处理或确认文件类型」。

**测试要点**：5000 字符单行 → 截断后缀且 content 长度受限；含 `\0` 文件 → error 项；UTF-16 BOM 文件（既有 `utf16_detection` 用例数据）**不受**二进制误判；既有测试全过。

### A5（P0-5）compact：模型侧截断统一"凡截断必落盘"

**现状**：`compact_for_model`（`tools/compact.rs:8-34`）对非 command/read 工具做 `truncate_head_tail`（4KB+8KB），被截掉的中间段模型永远拿不回（command 已有 spill，[docs/tool-optimizations-port](./tool-optimizations-port.md)）。

**设计**（改 `tools/compact.rs` + `tools/batch.rs` + `tools/command.rs`）：

1. 签名扩展：`pub fn compact_for_model(kind, name, outcome, spill_dir: Option<&Path>) -> String`（None = 无落盘能力，行为同旧——单测兼容）。
2. `truncate_head_tail` 超限时：全量写 `<spill_dir>/<uuid>.txt`（先 `create_dir_all`），截断标记从 `…[已截断 N 字节]…` 升级为 `…[输出已截断 N 字节，完整输出：<绝对路径>]…`。
3. 落盘目录定 `<data_dir>/tmp/tool-output`（与 command 的 `cmd-output` 分离）；`batch.rs model_content`（`batch.rs:338-342`）拿到 `execute_batch` 作用域的 `rt`，传 `Some(rt.data_dir.join("tmp/tool-output").as_path())`。
4. 清理复用：`command.rs purge_stale_spills`（`command.rs:492-524`）参数化为 `fn purge_stale_dir(dir: &Path)`（24h 过期、每小时至多一次的 AtomicU64 节流保持），对 `cmd-output` 与 `tool-output` 两目录各跑一次；`write_spill_file` 抽公共（或 compact 内自实现同款，二选一，倾向抽到 `tools/` 内部小工具函数避免循环依赖）。

**测试要点**：50KB data 截断 → spill 文件存在且内容完整、标记含路径；`None` 时输出与旧行为逐字节一致；read/command 特判路径不落盘（已有直通）。

### A6（P1-2）list_files：可选 glob 参数

**设计**（改 `tools/list_files.rs`）：Args 增 `glob: Option<String>`（schema 同步，`additionalProperties:false`）；`walk_with_budget` 增 `overrides: Option<ignore::overrides::Override>` 参数，构建代码照搬 `grep.rs:133-141`（非法 glob → `E_ARGS`）；多根列表形态（无 path）不与 glob 组合（有 glob 必须给 path，description 说明）。占位符折叠机制不变。

**测试要点**：glob `*.rs` 过滤生效；非法 glob 报错；glob + 每目录预算共存（node_modules 折叠不受影响）。

### A7（P1-5）skill：附属文件清单

**设计**（改 `tools/skill.rs`）：命中技能且 `origin != "<builtin>"` 时，`dir = Path::new(&s.meta.origin).parent()`；`ignore::WalkBuilder`（hidden，max_depth 3，收集非 `SKILL.md` 文件，**限 10 个**绝对路径）；content 末尾追加：
```
<skill-files>
<file>/abs/path/script.sh</file>
…
</skill-files>
（清单为采样，最多 10 个；相对路径相对于技能目录）
```
内置技能（`<builtin>`）无目录，跳过清单段。

**测试要点**：临时技能目录含 `scripts/run.sh` → 清单出现绝对路径；内置技能输出无 `<skill-files>` 段。

---

## 3. 批次 B：审批表达力与体验

### B1（P1-1）edit/create 审批：diff 预览替代参数 JSON dump

**现状**：ConfirmEach 档 FileWrite 预弹窗 detail = `serde_json::to_string_pretty(&call.args)`（`batch.rs:217-239`）——用户看到的是 JSON 参数而非语义变更；diff 级审批体验显著更好（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §4.3/§5.3）。

**设计**（改 `tools/mod.rs` + `tools/edit.rs` + `tools/create.rs` + `tools/batch.rs`；**前端零改动**——detail 已在 `AskPanel.tsx:203` 的 `<pre>` 渲染）：

1. trait 新增默认方法（async_trait 支持默认 async 方法）：
   ```rust
   /// ConfirmEach 档 FileWrite 预弹窗的 detail 预览；None = 回退通用 JSON dump
   async fn approval_detail(&self, _ctx: &ToolCtx, _args: &Value) -> Option<String> { None }
   ```
2. **EditTool::approval_detail**：核心逻辑抽纯函数便于单测（绕开 sink 断言难题）：
   ```rust
   pub fn edit_approval_detail(roots: &WriteRoots, files: &[FileEdit]) -> Option<String>
   ```
   逐文件 resolve + read（任一失败 → None 回退通用路径）→ `read_text_content` + EOL 归一（复用 A1）→ `apply_changes` 预演 → `similar::TextDiff::from_lines(old, new)` 生成 unified diff（`header`/`missing_alternative` 简单配置，上下文 3 行）→ 多文件拼接 `### <path>` 分节。TOCTOU 说明：预览仅用于展示，edit run 内会再预演一次，两次间文件被外部改动时 run 自身报 version/匹配错误——可接受。
3. **CreateTool::approval_detail**：目标存在 → 旧内容 vs 新内容 diff；新文件 → 「新文件，内容如下」+ 全文。
4. `batch.rs` ConfirmEach 分支：`detail = tool.approval_detail(ctx, &call.args).await.unwrap_or_else(|| to_string_pretty(&call.args))`。delete 保持 JSON dump（删除语义路径即全部信息）。

**测试要点**：`edit_approval_detail` 纯函数单测——两文件改动生成含 `+/-` 行与文件分节头的 diff；EOL 归一参与（CRLF 文件 diff 干净）；预演失败返回 None。既有 ConfirmEach 集成测试（预取消令牌拒绝路径）不破。

**验收**：手动清单 M-2——ConfirmEach 档发起 edit，弹窗 detail 为 diff 文本（`<pre>` 等宽展示）。

### B2（P1-3）工作区外路径：询问放行（运行时扩根）

**现状**：`resolve_read/write` 对白名单根外路径硬拒 `E_PATH_OUTSIDE`（`pathutil.rs:111-146`），无运行时扩权通道；候选机制允许"询问一次、目录本会话可访问"（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §1.6）。

**设计**（改 `tools/mod.rs`，接入 `read.rs` / `create.rs` / `edit.rs`；**delete 不接**，保持硬拒——删除区外路径风险收益比不划算）：

1. `ToolCtx` 新增方法：
   ```rust
   /// resolve 失败为 E_PATH_OUTSIDE 时弹审批请求放行；批准 = 目录进会话级 extra_roots
   pub async fn resolve_or_ask(&self, raw: &str, write: bool) -> Result<PathBuf, (String, String)>
   ```
   流程：
   - 先 `pathutil::resolve_read/write`，成功直返；
   - `Err((code, msg))` 且 code == `E_PATH_OUTSIDE` →
     a. **硬拒名单不询问**：`canonical_best_effort(target)` 命中 `is_dangerous_delete_multi` 同源的系统目录/家目录判定（复用 `pathutil.rs:155-193` 的系统根清单逻辑，抽 `is_system_path(p)` 小函数）→ 直接透传原错误；
     b. 取目标**父目录** canonical 形式；`rt.extra_roots` 已含 → 重跑 resolve（现已在根内）返回；
     c. `rt.extra_roots.len() >= 8` → `E_PATH_OUTSIDE`（"会话外部目录已达上限"）；
     d. `approval::confirm`（title「访问工作区外路径」，detail = 目标路径 + 所属目录，`allow_always: false`——放行本身即"本会话始终允许"，与项目级白名单语义区分）；
     e. 批准 → `rt.extra_roots.lock().push(parent)` + session_log 记录 → 重跑 resolve 返回；拒绝/超时 → `E_PATH_OUTSIDE`（"用户拒绝访问工作区外路径：<dir>"）。
2. 接入点替换：`read.rs` / `create.rs` / `edit.rs` 的 `resolve_read/resolve_write` 调用点换 `ctx.resolve_or_ask(...)`（编辑工具第一遍预演处）。command/service 不动（fence 已有 OutsideCreate 询问，语义不同：那是"命令将创建区外文件"，这是"文件工具直接读写区外"）。
3. 会话级生效：`extra_roots` 是 `SessionRuntime` 内存字段（`mod.rs:105-119` 已有 `write_roots()` 聚合），不落配置、不跨会话——最小权限面。前端复用审批弹窗，零新事件键。

**测试要点**：`resolve_or_ask` 三态——区外+预取消令牌（= 拒绝）→ `E_PATH_OUTSIDE` 且 extra_roots 未增；手动预置 extra_roots 包含父目录 → 直通无询问（等价批准后二次调用的行为锚定）；系统路径（如 `/` 或 `C:\Windows`）→ 不询问直接拒。集成：ConfirmEach 档 read 区外文件 → 事件流出现 approval 请求（复用既有 ask 事件断言模式）。

**验收**：手动清单 M-3——普通档 read 一个工作区外文件 → 弹窗 → 批准 → 成功读取；同会话再次 read 同目录 → 不再弹。

**风险**：放行目录过宽（父目录粒度）——父目录粒度（`<dir>/*`），且 8 个上限 + 会话级失效约束暴露面；回滚 = 接入点换回原 resolve。

### B3（P1-4）web_fetch：format=markdown（保结构正文转换）

**设计**（改 `tools/web_fetch.rs` + `Cargo.toml` 新增 `htmd`）：

1. 入参增 `format: Option<String>`（enum `text|markdown`，缺省 `text` 保持现行为；schema 同步）。serde `#[serde(default)]` 兼容旧 payload。
2. `format=markdown` 且 content-type 为 html/xhtml 时：优先取 dom_smoothie 抽取后保留的 **article HTML** 转 markdown（`htmd::convert`）；**实施验证点**：确认 dom_smoothie 0.18 的 `Article` 是否暴露 HTML 内容字段——若无，退化路径为对原始 body 直接 `htmd`（整页转换含导航噪声，仍优于纯文本丢结构，description 中注明局限）。转换失败（htmd error）→ 回退现有 Readability 文本 + warning。
3. 依赖选型说明：`htmd`（纯 Rust async html→md，维护活跃）；备选 `html2text` 仅纯文本不符合需求；自写转换器 300+ 行不值。新增依赖 1 个。

**测试要点**：`format=text` 回归不变；markdown 路径 smoke（含 `<h1>`/`<pre><code>` 的 html → 转出 `#`/围栏代码块）；转换异常回退路径。

### B4–B10（P2 系列，逐项短方案）

| 编号 | 工具 | 方案 | 改动点 |
|---|---|---|---|
| B4 | read | **误拼建议**：per-file error 路径（A3）列同目录候选——`read_dir` + 双向大小写不敏感子串匹配取 3，拼进 error 文案「Did you mean: a.ts, b.ts?」 | `read.rs` ~20 行 + 测试 |
| B5 | create | description 增「整文件重写现有文件请直接带 `overwrite:true`」（省一次 E_EXISTS 失败往返） | `create.rs` 文案 |
| B6 | grep | **并行遍历 + 单文件支持**：`WalkBuilder::build_parallel` + visitor 返回 `WalkState::Quit` 实现 HARD_CAP 早停（容器已是 `Arc<Mutex>`/`AtomicU64`，线程安全就绪）；`path` 指向文件时跳过 walk 直接 `searcher.search_path` 单文件 | `grep.rs` ~40 行改写 + 测试（结果序稳定性：并行下按文件序排序后再分页，保证 offset 语义确定） |
| B7 | http_request | **连接复用 + 流式限量**：全局 `OnceLock<Client>`（`redirect(none)`，无全局 timeout）+ per-request `.timeout(d)`（reqwest 0.13 支持）；body 读取从 `resp.bytes()` 换 `net::read_body_limited`（50MB 流式上限） | `http_request.rs` ~20 行 |
| B8 | service | start 复用 shell 探测：`command.rs` 的 `detect_shell`/`find_windows_bash` 改 `pub(crate)`，`start_service` 按 shell 构造命令（bash `-lc` / PowerShell `-NoProfile -Command`，分支照搬 `run_process`）——修「无 Git Bash 的 Windows 机 service 必挂」 | `command.rs` 可见性 + `service.rs` ~15 行 |
| B9 | web_fetch | **响应图片通道**：content-type `image/*` 时 ≤3MB → base64（把 `read.rs:174-186` 手写 base64 抽 `util/b64.rs` 公共）→ `extra_model_content` 注入 + data url 字段；>3MB 或非图片走现路径 | `web_fetch.rs` + `util/b64.rs` ~30 行 |
| B10 | command | **workdir 别名**：schema 增 `workdir`（与 `cwd` 同义；两者都给时 `cwd` 优先 + warning「workdir 与 cwd 同时提供，已采用 cwd」）——贴合模型调用习惯（训练语料中 workdir 更常见） | `command.rs` schema + Args + 3 行解析 |

---

## 4. 批次 C：缺失工具补齐

### C1 新工具 `web_search`：BYOK 网络搜索

**定位**：补齐 [docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §13.2 识别的缺口——CodeWave 无搜索工具（web_fetch 只能抓已知 URL）。BYOK 原则：provider 与 key 用户自配，key 入 keyring 不落明文。

**后端设计**（新增 `tools/web_search.rs` + `core/config.rs` 增量）：

1. **Provider 抽象**（文件内私有）：
   ```rust
   pub struct SearchHit { pub title: String, pub url: String, pub snippet: String }
   #[async_trait] trait SearchProvider { async fn search(&self, q: &str, max: usize) -> Result<Vec<SearchHit>, String>; }
   ```
   三个实现，全部复用 `net.rs` 的 `guard_host`（SSRF）+ 共享 `HostThrottle`：
   - **DuckDuckGo**（默认，免 key）：`GET https://html.duckduckgo.com/html/?q=<urlencoded>`，dom_smoothie/strip_tags 解析结果列表（`.result` 块的标题/链接/摘要；HTML 结构变更风险标注 best-effort，失败报干净错误）；
   - **Tavily**：`POST https://api.tavily.com/search`（Bearer `search:tavily` keyring key，body `{query, max_results}`）；
   - **Exa**：`POST https://api.exa.ai/search`（`x-api-key` 头，`search:exa` key）。
2. **配置**（config schema v2 增量，serde default 兼容旧配置）：
   ```rust
   #[serde(default)] pub search: SearchSettings   // { provider: SearchProviderKind (default DuckDuckGo), max_results: u8 (default 8) }
   ```
   key 存取复用 `provider/keys.rs` 的 keyring 模式（账户名 `search:<provider>`；未配 key 的 provider 在设置页保存时校验提示）。
3. **工具面**：
   - 入参 `{query: string, maxResults?: integer}`（schema `additionalProperties:false`；maxResults clamp 1–10）；
   - 出参 `{provider, hits: [{title, url, snippet}], count}`；kind `Network`；plan 档不排除（与 web_fetch 同策略）；
   - description 用 **A2 的 `description_rendered` 机制**动态注入当前 provider（「Current provider: tavily (key configured) / duckduckgo (no key required)」）——这是 C1 依赖 A2 的原因；
   - compact 层走通用路径（A5 spill 兜底）。
4. **注册**：`registry.rs` 注册 + 契约测试名单更新（20 → 21 个，排序插入 `web_search` 于 `web_fetch` 之后——字母序 `web_fetch < web_search` 成立）。

**前端**（~150 行）：设置弹窗「安全」页签（或网络区块，实施时按现有页签结构落）增 Search 区块：provider `Select` + 对应 key `Input.Password`（antd Form vertical，[docs/settings-forms-vertical](./settings-forms-vertical.md) 约定）+ 保存走既有 config save 命令；key 写 keyring 走既有 key 命令通道。`ipc/types.ts` 同步 SearchSettings 类型。

**测试要点**：provider 响应解析单测（固定 JSON/HTML 样本 → hits）；未配 key 的 Tavily/Exa 请求前报 `E_ARGS`（"请先在设置中配置 key"）；DDG HTML 解析容错（无结果/结构变化 → 干净错误）；registry 契约名单 21 个断言；真实搜索 E2E 手动（M-4）。

### C2 subagent：后台执行 + 续接（resumeId）

**定位**：补齐 [docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §8.3 识别的两项能力——后台子代理（长任务不阻塞主线）与 resumeId 续接（同一子会话多轮协作）。

**设计**（改 `tools/subagent.rs` + `core/agent.rs` 小增量）：

1. **入参增量**（schema 同步）：`background?: boolean`（缺省 false）、`resumeId?: string`。两者独立可组合。
2. **resumeId**：`core.subs.get(resume_id)` 命中且该 runtime 未在驱动（复用 `rt.running` CAS 判定，sub runtime 与主 runtime 同结构）→ 复用既有 `SessionRuntime`（history 保留），push 新 `<subagent-task>` 消息后 drive；未命中/正忙 → `E_ARGS`（"续接目标不存在或仍在运行"）。现状 `core.subs.insert`（`subagent.rs:160`）已保留注册，StopSubagent 语义不变。
3. **background=true**：spawn 驱动、立即返回：
   ```
   {role, sub_id, state: "running", note: "后台运行中，完成会自动通知；请勿 sleep/poll 或重复该任务的工作，可继续不相关任务"}
   ```
   （文案要点：显式指挥模型行为——勿 sleep/poll/重复该任务的工作。）
4. **完成注入**：`core/agent.rs` 新增辅助函数（供本特性与未来复用）：
   ```rust
   /// 运行中 → inject_tx 排空进下一轮；空闲 → 以合成消息启动新 run（提取 start_chat 的 CAS 前置为 try_start 内部函数共用）
   pub fn inject_or_start(core: &Arc<AgentCore>, rt: &Arc<SessionRuntime>, msg: Message)
   ```
   - `rt.running == true` → `rt.inject_tx.send(msg)`（run 循环 `agent.rs:600-613` 每步排空）；
   - 空闲 → 复用 `start_chat` 的前置校验（running CAS / compacting / 模型配置——提取为 `fn try_start(self, rt, message: Message) -> Option<String>`，`start_chat` 改为构造消息后调它，零行为变化）→ `tokio::spawn(run_chat(...))`。
   - 后台完成回调（spawn 的驱动 task 末尾）构造合成用户消息：`<subagent-result id="{sub_id}" role="{role}">\n{report}\n</subagent-result>`（失败态包 error 文本），经 `inject_or_start` 回主会话。主时间线以普通用户消息样式呈现（零新事件键）；`sub:report`/`sub:done` 事件照发（前端卡片收口）。
5. **并发与取消**：后台子代理同样占 `ACTIVE`（4）名额；StopSubagent 取消令牌 → 驱动中止 → 合成消息标注「被用户取消」；`SubCleanupGuard` panic 收口不变。
6. **明确不做**：后台任务上下文追加（向既有后台任务追加工作项的形态）——首版只支持"完成注入"，追加语义留给后续（合成消息里模型自然会再委派 resumeId，可覆盖多数场景）。

**测试要点**：background=true 立即返回（不 await 驱动完成——用短任务 mock）；resume 命中复用 runtime（history 累积断言）；resume 未命中 `E_ARGS`；`try_start` 提取后 `start_chat` 行为回归（既有测试）。`inject_or_start` 空闲分支集成测试：后台完成后主会话 history 出现 `<subagent-result>` 消息。

### C3 按模型动态工具裁剪

**定位**：[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §1.1/P2-8——按模型裁剪工具列表可省 token；现状为全量注入。

**设计**（改 `core/config.rs` + `core/agent.rs`）：

1. config v2 增量（serde default）：`tools: ToolSettings { hidden_by_model: HashMap<String, Vec<String>> }`（model_id → 隐藏工具名列表；通配 `"*"` 键表示全部模型隐藏）。
2. 注入点：`agent.rs` 组装本轮 `effective_excludes` 处（plan 档排除集同源机制，[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §14.10）合并 `hidden_by_model[active_model_id]`——**天然获得 batch 硬门语义**（spawn 前 `E_TOOL_BLOCKED` 终结性文案，`batch.rs:72-103` 现成），无需新过滤层。
3. 工具列表过滤：发给模型的 `tool_defs` 按 excludes 过滤（与 plan 档同一处代码路径，改动即两行）。
4. 首版不做设置 UI（config 文件手编；设置页编辑留后续——方案里明确降级路径）。

**测试要点**：excludes 合并单测（模型隐藏 + plan 档隐藏取并集）；通配键；未知工具名静默忽略 + warning。

---

## 5. 不做与远期（批次 D，不排期）

| 项 | 结论 | 理由（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) 出处） |
|---|---|---|
| LSP 横切服务（read 预热/write 诊断/lsp 工具） | 远期评估（P3-1） | 常驻进程管理成本高；现有 validation.rs 轻量语法校验是务实取舍（**历史文案**：本篇成文时的旧写法，用户可见的现行定名是「写入后语义校验」，见 [settings-terminology](./settings-terminology.md) §1）。触发条件：用户高频反馈"语义错误要到运行才发现" |
| code-mode 沙箱编排 | 远期评估（P3-2） | 依赖 Rust 侧 JS 沙箱选型（deno_core/rquickjs）；仅 MCP 密集场景收益明显 |
| apply_patch 通道 | 不做（P3-4） | 仅 gpt-5 系模型需要；现有 edit 多文件能力已覆盖；接入该类模型时再评估 |
| invalid 兜底工具 / plan_exit 工具 | 不做 | `E_UNKNOWN_TOOL` 与 ask 批准协议已分别覆盖（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §13.6/§13.3） |
| 统一截断管道 | 不做 | 双通道 + command 语义化裁剪是更精细设计；A5 落盘补齐已解决盲区（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §15） |
| edit 激进模糊链（Levenshtein/转义/空白归一） | 不做 | 错杀风险 + 与 version 令牌哲学冲突；A1 两级低风险替换器已覆盖最高频失败（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §15） |
| shell 的 tree-sitter 路径扫描 | 不做 | fence L2 AST 已覆盖命令安全面；文件面由 B2 统一解决（[docs/builtin-tools-source-comparison](./builtin-tools-source-comparison.md) §2.3） |
| http_request 307/308 重发 body、303 转 GET 语义（P3-5） | 远期 | 低频边界；正确实现需按状态码分派，B7 改造时若顺手可一并（非必须） |
| service RingLog 按 chunk 淘汰（P3-6） | 远期 | 大流量日志下的 CPU 优化；现状逐字节正确性无问题 |
| Todo detail 字段 / ask per-question custom 开关（P3-7） | 远期 | 契约变更需前端联动；当前 title+note 已够用 |
| 插件工具动态加载 | 不做 | 无插件体系（MCP 已覆盖扩展面） |

---

## 6. 契约影响面汇总

| 契约面 | 变更 | 条目 | 兼容性 |
|---|---|---|---|
| 工具 schema（模型可见） | `list_files` +`glob`；`web_fetch` +`format`；`command` +`workdir`；`subagent` +`background`/`resumeId`；新工具 `web_search` | A6/B3/B10/C1/C2 | 工具 schema 无持久化，仅影响模型可见性；全部保持 `additionalProperties:false`；registry 契约测试名单更新（C1 后 21 个） |
| 工具出参（data JSON） | `read` 失败项新增 `error` 字段形态（成功项不变）；`command` 无变化；`web_fetch` 无变化 | A3/A4 | 前端按 path 渲染，新增字段向后兼容；`compact_for_model` 通用路径照常 |
| trait `Tool` | +`description_rendered()`（默认实现）、+`approval_detail()`（默认 None） | A2/B1 | 默认方法零破坏；仅 command/edit/create override |
| 配置 schema | +`search: SearchSettings`、+`tools: ToolSettings` | C1/C3 | `#[serde(default)]` 兼容旧配置（契约锚点约定） |
| keyring | +账户 `search:tavily` / `search:exa` | C1 | 复用 provider/keys.rs 模式，无迁移 |
| 事件面 27 键 | **零新增** | 全部 | B1/B2 复用 approval 事件；C2 复用 `sub:*` 与 `run:inject`；`events.contract.test.ts` 不动 |
| 依赖 | +`htmd`（B3）；`similar` 已有 | B3 | Cargo.toml 增 1 项 |

---

## 7. 验证策略

- **后端**：每条目自带单测（各节"测试要点"），批次合并后 `cargo test` 全绿 0 warning（Windows 基线 243 passed 之上只增不减）；真实网络路径（web_search/web_fetch markdown）不进 CI，走手动。
- **前端**：仅 C1 涉设置页——`pnpm --dir ui test` + `build`；新增设置区块测试（provider 切换显隐 key 输入、保存校验）。
- **GUI 手动验证清单**（按仓库约定交付用户执行）：
  - M-1（A2）：起会话检查系统提示词工具列表中 command 描述含实际 shell 与常量
  - M-2（B1）：ConfirmEach 档 edit → 审批弹窗 detail 为 diff 文本
  - M-3（B2）：普通档 read 工作区外文件 → 弹窗批准 → 成功；同会话二次 read 同目录不再弹
  - M-4（C1）：设置页配置 provider/key → 会话中 web_search 真实搜索返回结果
  - M-5（C2）：subagent background=true 立即返回 → 主线继续其他工作 → 后台完成后主时间线出现结果消息
  - M-6（A1）：CRLF 文件 + LF oldText 编辑成功（找一个真实 Windows CRLF 仓库文件）

---

## 8. 风险与回滚

| 风险 | 概率/影响 | 缓解 | 回滚 |
|---|---|---|---|
| A1 模糊匹配误替换 | 低/中 | 唯一性强制 + warnings 透明化 + 仅两级保守策略；不命中即报错（绝不猜） | revert edit.rs 单文件 |
| A2 描述变长推高 token | 低/低 | schemas_token_estimate 可观测；文案控制在 ~2 倍内 | revert trait 方法调用点 |
| B2 外部路径放行过宽 | 中/中 | 父目录粒度 + 8 个上限 + 会话级失效 + 系统路径硬拒 + 审批弹窗留痕 | 接入点换回 resolve_*（一行） |
| B3 htmd 转换质量/维护 | 中/低 | 失败回退 Readability 文本；format 缺省 text 零风险 | 还原默认值即可（功能可选） |
| C1 DDG HTML 结构变更 | 高/低 | provider 可切换；失败报干净错误引导换 provider | 默认 provider 可配置 |
| C2 后台完成注入触发意外 run | 中/中 | 注入消息为普通用户消息，走全部既有档位/审批链路；空闲分支才起新 run | background 参数不传即旧行为 |
| C3 配置误隐藏关键工具 | 低/中 | 隐藏名校验 + warning；`E_TOOL_BLOCKED` 终结性文案防模型死循环 | 清空 hidden_by_model 配置 |

---

## 9. 实施顺序与提交切分建议

```
批次 A（7 个独立提交，建议顺序）：
  A3 read 失败隔离 → A4 read 单行/二进制 → A1 edit EOL+模糊 → A2 动态描述
  → A5 截断落盘 → A6 list_files glob → A7 skill 清单
批次 B（B1 依赖 A1 先行）：
  B1 diff 审批 → B2 外部路径放行 → B3 markdown → B4–B10 打包一个小批次提交
批次 C（C1 依赖 A2 先行）：
  C1 web_search（后端+前端各一提交）→ C2 后台子代理 → C3 按模型裁剪
```

每批次完成后按 AGENTS.md 约定写实施批次报告（docs/ 新编号），GUI 项交付手动清单。


