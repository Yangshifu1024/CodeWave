use crate::core::types::Content;
use crate::tools::pathutil;
use crate::tools::validation;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

/// edit 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 文件编辑请求列表，至少一项。
    files: Vec<FileEdit>,
}

/// 单个文件的编辑请求。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEdit {
    /// 工作区相对路径。
    pub(super) path: String,
    /// 之前 read 返回的 version token，用于乐观并发校验。
    #[serde(default)]
    pub(super) version: Option<String>,
    /// 变更列表，按顺序应用。
    pub(super) changes: Vec<Change>,
}

/// 单条变更：oldText 精确替换优先，lineRange 行区间替换兜底，newText 省略即删除。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// 待替换的精确文本，必须在文件中唯一命中。
    #[serde(default)]
    pub(super) old_text: Option<String>,
    /// 行区间（如 "40-72"，1-based 含端点）。
    #[serde(default)]
    pub(super) line_range: Option<String>,
    /// 替换后的文本；省略表示删除。
    pub(super) new_text: Option<String>,
}

/// edit 工具：多文件、多条变更的原子化文本编辑。
/// 入参为 files 数组（path + version + changes）；FileWrite 分级，ConfirmEach 档下经审批，
/// approval_detail 以与执行一致的链路产出 unified diff 预览。
/// 核心语义：version 乐观并发校验（不匹配但能唯一应用则以警告放行）、EOL 归一匹配、
/// 两级低风险模糊替换、进程级写互斥、倒序写入 + 备份回滚，任一文件写失败整体回滚。
pub struct EditTool;

/// 计算文件字节的 6 字符 version token（单次哈希，与 read 工具一致）。
pub fn version_of(bytes: &[u8]) -> String {
    // 单次哈希（缺陷修复）：此前把 Sha256 摘要再喂给 version_token 形成双重哈希，
    // 永远对不上 read 侧的单哈希 token → 每次 edit 都被判 stale；
    // 且「请重新 read」提示永远无法被满足（read 侧从不出双哈希）→ 死循环陷阱。
    crate::util::crockford::version_token(bytes)
}

/// 解析 "40-72" 形式的行区间（1-based，含端点）。
pub fn parse_line_range(s: &str) -> Option<(usize, usize)> {
    let (a, b) = s.split_once('-')?;
    let a: usize = a.trim().parse().ok()?;
    let b: usize = b.trim().parse().ok()?;
    if a == 0 || b < a {
        return None;
    }
    Some((a, b))
}

/// 探测文件的主导行尾：只要出现 \r\n 即判 CRLF（混合 EOL 文件被整体归一——已知取舍，与 opencode 一致）。
fn detect_eol(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

// ===== 两级低风险模糊匹配（[docs/tools-optimization-and-gap-fill-plan](../../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 1）=====
// 仅当 oldText 精确命中 0 处时逐级尝试（由严到松：缩进平移归一 → 行首尾空白归一），
// 唯一命中才替换并附警告；多命中报错；零命中落给 lineRange 兜底 / 报错。
// 不做 Levenshtein / 转义归一等激进策略（[docs/builtin-tools-source-comparison](../../../../docs/builtin-tools-source-comparison.md) §15 不移植清单：错配风险 +
// 与 version token「模型必须给准」的理念冲突）。所有匹配都在 LF 归一后的文本上进行。

/// 计算文本中非空行的最小前导空白宽度（按字符计）。
pub(super) fn min_indent(text: &str) -> usize {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        // 按字符而非字节——必须与 strip_indent 的 chars().skip(n) 口径一致（按字节统计会
        // 高估 U+00A0/U+3000 等多字节空白的宽度，导致过度剥除伤及内容）
        .map(|l| l.chars().count() - l.trim_start().chars().count())
        .min()
        .unwrap_or(0)
}

/// 每行剥除前 n 个字符（空行原样保留），返回按 \n 重组的文本。
pub(super) fn strip_indent(text: &str, n: usize) -> String {
    text.split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else {
                l.chars().skip(n).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 整块缩进平移归一：把 find 与每个等行数窗口各自剥掉「非空行最小前导空白」后比较
///（空行原样保留，块内相对缩进不变——比逐行 trim 更严格；第一优先级尝试）。
fn fuzzy_indent_flex(content: &str, find: &str) -> Vec<String> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.last() == Some(&"") {
        find_lines.pop();
    }
    let n = find_lines.len();
    if n == 0 || lines.len() < n {
        return Vec::new();
    }
    let find_block = find_lines.join("\n");
    let target = strip_indent(&find_block, min_indent(&find_block));
    let mut out = Vec::new();
    for i in 0..=(lines.len() - n) {
        let window = lines[i..i + n].join("\n");
        let stripped = strip_indent(&window, min_indent(&window));
        if stripped == target {
            out.push(window);
        }
    }
    out
}

/// 行首尾空白归一：等行数滑动窗口逐行 trim() 后比较（解决行尾空白 / 逐行空白漂移）。
fn fuzzy_line_trimmed(content: &str, find: &str) -> Vec<String> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.last() == Some(&"") {
        find_lines.pop();
    }
    let n = find_lines.len();
    if n == 0 || lines.len() < n {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=(lines.len() - n) {
        let window = &lines[i..i + n];
        if window
            .iter()
            .zip(&find_lines)
            .all(|(a, b)| a.trim() == b.trim())
        {
            out.push(window.join("\n"));
        }
    }
    out
}

/// 在单个文件内应用一组变更（content / oldText / newText 均为 LF 归一后的文本，EOL 转换由调用方处理）。
/// 返回（新内容，warnings）：模糊命中等非致命信息经 warnings 透明回传给模型。
pub fn apply_changes(content: &str, changes: &[Change]) -> Result<(String, Vec<String>), String> {
    let mut buf = content.to_string();
    let mut warnings: Vec<String> = Vec::new();
    for (i, ch) in changes.iter().enumerate() {
        if let Some(old) = &ch.old_text {
            if ch.new_text.as_deref() == Some(old.as_str()) {
                return Err(format!(
                    "change #{}：oldText 与 newText 相同（无变更）",
                    i + 1
                ));
            }
            let hits = buf.matches(old.as_str()).count();
            if hits == 0 {
                // 两级低风险模糊匹配（由严到松）：唯一命中才替换；多命中报错；零命中留给 lineRange 兜底
                let mut matched = false;
                for (name, matcher) in [
                    (
                        "缩进平移归一",
                        fuzzy_indent_flex as fn(&str, &str) -> Vec<String>,
                    ),
                    ("行首尾空白归一", fuzzy_line_trimmed),
                ] {
                    let cands = matcher(&buf, old);
                    match cands.len() {
                        0 => continue,
                        1 => {
                            buf = buf.replacen(&cands[0], ch.new_text.as_deref().unwrap_or(""), 1);
                            warnings.push(format!(
                                "change #{}：oldText 未精确命中，已按「{name}」唯一匹配并替换（newText 缩进以输入为准，请自查与上下文一致）",
                                i + 1
                            ));
                            matched = true;
                            break;
                        }
                        n => {
                            return Err(format!(
                                "change #{}：oldText 模糊匹配到 {n} 处，需加长上下文使其唯一",
                                i + 1
                            ));
                        }
                    }
                }
                if matched {
                    continue;
                }
                if ch.line_range.is_some() {
                    continue; // 交给 lineRange 兜底
                }
                return Err(format!("change #{}：oldText 在文件中不存在", i + 1));
            }
            if hits > 1 {
                return Err(format!(
                    "change #{}：oldText 匹配到 {hits} 处，需加长上下文使其唯一",
                    i + 1
                ));
            }
            buf = buf.replacen(old.as_str(), ch.new_text.as_deref().unwrap_or(""), 1);
        } else if let Some(range) = &ch.line_range {
            let (s, e) = parse_line_range(range)
                .ok_or_else(|| format!("change #{}：lineRange 格式应为 \"40-72\"", i + 1))?;
            let lines: Vec<&str> = buf.split_inclusive('\n').collect();
            if s > lines.len() {
                return Err(format!(
                    "change #{}：lineRange {s} 超出总行数 {}",
                    i + 1,
                    lines.len()
                ));
            }
            let e = e.min(lines.len());
            // UTF-8 BOM 交接：整段替换第一行时，把原 BOM 移植到新首行（防止 BOM 丢失）
            let bom = if s == 1 {
                lines
                    .first()
                    .filter(|l| l.starts_with('\u{FEFF}'))
                    .map(|_| "\u{FEFF}")
            } else {
                None
            };
            let mut new_lines: Vec<String> = Vec::new();
            new_lines.extend(lines[..s - 1].iter().map(|l| l.to_string()));
            let mut first = true;
            for l in ch.new_text.as_deref().unwrap_or("").split_inclusive('\n') {
                let mut l = l.to_string();
                if first {
                    if let Some(b) = bom {
                        if !l.starts_with('\u{FEFF}') {
                            l.insert_str(0, b);
                        }
                    }
                    first = false;
                }
                new_lines.push(l);
            }
            new_lines.extend(lines[e..].iter().map(|l| l.to_string()));
            buf = new_lines.concat();
        } else {
            return Err(format!("change #{}：需要 oldText 或 lineRange", i + 1));
        }
    }
    Ok((buf, warnings))
}

/// E_ARGS 解析失败报错附带的期望结构示例：让模型不查文档即可一步自纠
///（此前只回显 serde 原文，GLM 系模型缺结构提示会原样重发同参失败，形成「会话卡死」循环）。
const EXPECTED_STRUCTURE: &str = concat!(
    "期望结构（path/version/changes 必须写在 files 数组每个元素内，不能提到外层）：\n",
    r#"[{"path":"src/app.ts","version":"<read 返回的 6 字符令牌>","changes":[{"oldText":"被替换的唯一文本","newText":"替换后的文本"}]}]"#,
);

/// 参数解析失败的 E_ARGS 报错文本：serde 原文 + 期望结构示例。
fn e_args_message(e: &serde_json::Error) -> String {
    format!("参数解析失败：{e}\n{EXPECTED_STRUCTURE}")
}

/// 无歧义错位形态的保守修正：返回（修正后的参数, 给模型的修正说明）。
/// 仅还原两种可唯一确定的形态，其余（含顶层 path 与 files 并存等歧义）一律不动，
/// 交由 E_ARGS 示例报错（`e_args_message`）引导自纠：
/// 1) 顶层扁平单文件（含 path + changes、无 files）→ 包装为单元素 files 数组；
/// 2) files 是对象而非数组 → 包装为单元素数组。
pub fn salvage_args(args: &Value) -> Option<(Value, &'static str)> {
    let obj = args.as_object()?;
    if !obj.contains_key("files") {
        if obj.contains_key("path") && obj.contains_key("changes") {
            return Some((
                json!({ "files": [args] }),
                "参数已按顶层扁平形态自动包装为 files 数组，后续请直接使用 files 数组结构。",
            ));
        }
        return None;
    }
    if obj["files"].is_object() {
        return Some((
            json!({ "files": [obj["files"]] }),
            "参数中 files 为对象，已自动包装为单元素数组，后续请使用 files 数组结构。",
        ));
    }
    None
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> &'static str {
        "编辑工作区文件。入参 files 是数组，每个元素包含 path、version、changes 三个字段（path/version 必须写在数组元素内，不能提到外层）；changes 每条为 {oldText,newText} 或 {lineRange,newText}。每个文件必须携带此前 read 返回的 `version` 令牌。优先用 oldText（必须在文件中唯一命中）；lineRange（如 \"40-72\"，1-based 含端点）作兜底。单次调用可含多文件、多条变更，原子化应用，失败整体回滚。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["files"],
  "properties": {
    "files": {
      "type": "array",
      "minItems": 1,
      "description": "文件编辑请求数组；每个元素形如 {\"path\":\"src/app.ts\",\"version\":\"<read 返回的令牌>\",\"changes\":[{\"oldText\":\"…\",\"newText\":\"…\"}]}，path/version/changes 必须写在元素内",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "version", "changes"],
        "properties": {
          "path": {"type": "string", "description": "要编辑的文件路径（files 数组元素内的字段）"},
          "version": {"type": "string", "description": "read 返回的 6 字符令牌"},
          "changes": {
            "type": "array",
            "minItems": 1,
            "description": "按顺序应用的变更列表；每条为 {oldText,newText} 或 {lineRange,newText}",
            "items": {
              "type": "object",
              "additionalProperties": false,
              "properties": {
                "oldText": {"type": "string", "description": "精确文本，必须唯一命中"},
                "lineRange": {"type": "string", "description": "\"40-72\" 形式，1-based 含端点"},
                "newText": {"type": "string", "description": "替换后的文本；省略 = 删除"}
              }
            }
          }
        }
      }
    }
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::FileWrite
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        // 无歧义错位形态先保守还原（salvage_args）：坏形态直接放行，避免
        // 「E_ARGS 无引导 → 模型原样重发」死循环。修正说明走 extra_model_content
        // 回传模型（warnings 只达前端，不进 compact 模型通道），逐步引导回正确形态。
        let (args, salvage_note) = match salvage_args(&args) {
            Some((fixed, note)) => (fixed, Some(note)),
            None => (args, None),
        };
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", e_args_message(&e)),
        };
        let roots = ctx.write_roots();

        // 解析全部路径（锁外、无副作用）；任一路径越出写根立即失败
        let mut resolved: Vec<PathBuf> = Vec::with_capacity(args.files.len());
        for f in &args.files {
            match pathutil::resolve_read(&roots, &f.path) {
                Ok(p) => resolved.push(p),
                Err((c, m)) => return ToolOutcome::err(&c, m),
            }
        }

        // 严格文件隔离（[docs/subagent-file-isolation](../../../../docs/subagent-file-isolation.md)）：
        // 非主 runtime 编辑前认领全部目标；任一文件被兄弟任务认领则整体拒绝（多文件编辑不部分应用）
        if !ctx.rt.is_main_session {
            if let Err(conflicts) = crate::tools::claims::claim(&ctx.rt.id, &resolved) {
                return ToolOutcome::err(
                    "E_FILE_CLAIMED",
                    crate::tools::claims::denial_message(&conflicts),
                );
            }
        }

        // 进程级写互斥（[docs/tools-optimization-and-gap-fill-plan](../../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 2，跨 runtime）：按规范序加锁防死锁；
        // 锁内完成 读 → version 预检 → 试运行 → 备份 → 写入 → 回滚 全链路，消除试运行之后、
        // 写入之前文件被并发修改的 TOCTOU。同批次同路径写入已被批次层 E_WRITE_BATCH_CONFLICT 拒绝。
        // 等锁期间监听取消（批次取消盲区修复），取消则不执行编辑。
        let _guards = match crate::tools::writelock::acquire_all(&resolved, Some(&ctx.cancel)).await
        {
            Ok(g) => g,
            Err(_) => {
                return ToolOutcome::err("E_CANCELLED", "命令被用户取消");
            }
        };

        // 第一遍：读取并校验所有文件（version 预检 + EOL 归一 + 变更试运行）。
        // version 不匹配不再硬失败：记录 stale 后继续；若变更仍能在最新内容上唯一应用，
        // 则附警告放行；只有应用失败（真实内容冲突）才返回 E_VERSION_STALE 要求重新 read。
        let mut prepared: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        let mut stale_warnings: Vec<String> = Vec::new();
        let mut fuzzy_warnings: Vec<String> = Vec::new();
        for (f, resolved) in args.files.iter().zip(&resolved) {
            let bytes = match std::fs::read(resolved) {
                Ok(b) => b,
                Err(e) => {
                    return ToolOutcome::err(
                        "E_NOT_FOUND",
                        format!("{}: {e}（edit 前必须先 read）", f.path),
                    );
                }
            };
            let mut stale = false;
            if let Some(v) = &f.version {
                let expect = crate::util::crockford::normalize_token(v);
                let actual = version_of(&bytes);
                if expect != actual {
                    stale = true;
                }
            }
            // EOL 归一匹配：试运行内容与 oldText/newText 统一为 LF，写回时转回文件原 EOL；
            // version token 仍按原始字节计算（stale 判定不受归一影响）
            let text = crate::tools::read::read_text_content(&bytes);
            let eol = detect_eol(&text);
            let norm = if eol == "\r\n" {
                text.replace("\r\n", "\n")
            } else {
                text
            };
            let new_text = match apply_changes(&norm, &f.changes) {
                Ok((t, w)) => {
                    fuzzy_warnings.extend(w);
                    t
                }
                Err(e) => {
                    if stale {
                        let expect = f
                            .version
                            .as_deref()
                            .map(crate::util::crockford::normalize_token)
                            .unwrap_or_default();
                        return ToolOutcome::err(
                            "E_VERSION_STALE",
                            format!(
                                "{} 的 version 不匹配（期望 {}，收到 {expect}）且变更无法在最新内容上应用（{e}）——文件已被修改，请重新 read",
                                f.path,
                                version_of(&bytes)
                            ),
                        );
                    }
                    return ToolOutcome::err("E_EDIT_FAILED", format!("{}：{e}", f.path));
                }
            };
            if stale {
                stale_warnings.push(format!(
                    "{}：version 令牌已过期，但变更在最新内容上唯一命中，已按最新内容应用",
                    f.path
                ));
            }
            let out_text = if eol == "\r\n" {
                new_text.replace('\n', "\r\n")
            } else {
                new_text
            };
            prepared.push((resolved.clone(), out_text.into_bytes()));
        }

        // 写入后检查（[docs/post-write-check-plan](../../../../docs/post-write-check-plan.md)）：目标在写入**之后**构造，
        // 不再需要写前基线 / 差集 / 就绪判据（命令本就懂工程上下文）。
        let settings = ctx.core.cfg.read().unwrap().post_write_check.clone();
        let targets: Vec<validation::WriteTarget> = args
            .files
            .iter()
            .zip(&resolved)
            .map(|(f, p)| validation::WriteTarget::new(p.clone(), f.path.clone()))
            .collect();

        // 备份 → 倒序写入 → 失败回滚
        let backup_dir = ctx.rt.data_dir.join("tmp").join("edit-backup");
        let token = uuid::Uuid::new_v4().to_string();
        let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (p, _) in &prepared {
            if let Ok(orig) = std::fs::read(p) {
                let b = backup_dir.join(format!("{token}_{}", uuid::Uuid::new_v4()));
                if std::fs::create_dir_all(&backup_dir).is_ok() && std::fs::write(&b, &orig).is_ok()
                {
                    backups.push((p.clone(), b));
                }
            }
        }
        let mut written: Vec<PathBuf> = Vec::new();
        for (p, new_bytes) in prepared.iter().rev() {
            match crate::util::atomic::atomic_write(p, new_bytes) {
                Ok(()) => written.push(p.clone()),
                Err(e) => {
                    for w in &written {
                        if let Some((_, b)) = backups.iter().find(|(orig, _)| orig == w) {
                            if let Ok(data) = std::fs::read(b) {
                                let _ = std::fs::write(w, data);
                            }
                        }
                    }
                    return ToolOutcome::err(
                        "E_IO",
                        format!("写入 {} 失败，已回滚：{e}", p.display()),
                    );
                }
            }
        }
        // 清理备份
        for (_, b) in &backups {
            let _ = std::fs::remove_file(b);
        }
        // [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：产物登记（子代理归属主会话；路径规范化，
        // 同一文件的相对/绝对两种写法仍去重为一条；task runtime 跳过——该场景产物无消费方，
        // 避免边车泄漏；登记失败仅记日志，不影响工具结果）
        if !ctx.rt.is_task_runtime {
            let owner = ctx
                .rt
                .root_session_id
                .clone()
                .unwrap_or_else(|| ctx.rt.id.clone());
            for p in &written {
                let canonical = crate::tools::pathutil::canonical_best_effort(p)
                    .to_string_lossy()
                    .into_owned();
                if let Err(e) = ctx.core.store.append_artifact(
                    &owner,
                    &canonical,
                    crate::core::sessions::ArtifactOp::Edit,
                ) {
                    tracing::warn!("产物登记失败（edit {}）：{e}", p.display());
                }
            }
        }
        // 写入后检查（[docs/post-write-check-plan](../../../../docs/post-write-check-plan.md)）：结论进 `outcome.data.checks`
        // （模型侧读 data；warnings 只达前端是缺陷 B 的根因）。命令含 `{file}` 时逐文件成条，
        // 不含时整调用一条（path=null）——绝不因为批里某个文件跑过就给整批打「通过」。
        let checks = validation::run(ctx, &settings, &targets).await;
        let mut out = ToolOutcome::ok(json!({
            "edited": args.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            "count": written.len(),
            "checks": checks.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        }));
        for w in stale_warnings {
            out.warnings.push(w);
        }
        for w in fuzzy_warnings {
            out.warnings.push(w);
        }
        if let Some(note) = salvage_note {
            out.warnings.push(note.to_string());
            out.extra_model_content.push(Content::Text {
                text: format!("\n{note}"),
            });
        }
        out
    }

    /// ConfirmEach 审批详情：与执行同链路的试运行 diff（失败则回退批次层通用 JSON 展示）
    async fn approval_detail(&self, ctx: &ToolCtx, args: &Value) -> Option<String> {
        // 与 run 同链路先 salvage 再解析，保证审批预览与真实执行看到同一份参数
        let salvaged = salvage_args(args)
            .map(|(fixed, _)| fixed)
            .unwrap_or_else(|| args.clone());
        let args: Args = serde_json::from_value(salvaged).ok()?;
        edit_approval_detail(&ctx.write_roots(), &args.files)
    }
}

/// ConfirmEach 审批 diff 预览（纯函数便于单测）：与 run 相同的解码 / EOL 归一 / 试运行链路，
/// 用 similar 渲染 unified diff（3 行上下文），多文件以 `### <path>` 分节。
/// 任一文件读取或试运行失败返回 None（批次层退化为通用 JSON 展示）。
pub fn edit_approval_detail(roots: &pathutil::WriteRoots, files: &[FileEdit]) -> Option<String> {
    let mut out = Vec::new();
    for f in files {
        let resolved = pathutil::resolve_read(roots, &f.path).ok()?;
        let bytes = std::fs::read(&resolved).ok()?;
        let text = crate::tools::read::read_text_content(&bytes);
        let eol = detect_eol(&text);
        let norm = if eol == "\r\n" {
            text.replace("\r\n", "\n")
        } else {
            text.clone()
        };
        let (new_norm, _) = apply_changes(&norm, &f.changes).ok()?;
        let new_text = if eol == "\r\n" {
            new_norm.replace('\n', "\r\n")
        } else {
            new_norm
        };
        out.push(format!(
            "### {}\n{}",
            f.path,
            unified_diff(&text, &new_text)
        ));
    }
    Some(out.join("\n"))
}

/// 简易 unified 风格 diff 渲染（3 行上下文；`+/-` 前缀，在审批弹窗 `<pre>` 中展示）。
pub fn unified_diff(old: &str, new: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut buf = String::new();
    for group in diff.grouped_ops(3) {
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                    ChangeTag::Equal => ' ',
                };
                buf.push(sign);
                buf.push_str(change.value());
            }
        }
    }
    buf
}
