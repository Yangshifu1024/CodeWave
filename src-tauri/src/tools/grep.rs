//! grep 工具：经官方 ripgrep crates（grep-regex/grep-searcher/ignore）内嵌引擎，
//! 不捆绑 rg 二进制；精确计数 + 按文件分布 + offset 分页。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 全局命中数硬上限（防止在超大仓库上失控）。
const HARD_CAP: u64 = 5000;
/// 单页匹配 / 文件数上限。
const MAX_MATCHES: usize = 200;

/// 输出模式（ally 163b0a6 移植；默认 content，零回归）：
/// content = 路径+行号+行文本；files = 仅命中的文件路径；count = 每文件精确命中数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    /// 路径+行号+行文本（默认）。
    Content,
    /// 仅命中文件路径。
    Files,
    /// 每文件命中计数。
    Count,
}

impl OutputMode {
    /// 解析 wire 字符串；未知值返回 None。
    fn parse(s: &str) -> Option<Self> {
        match s {
            "content" => Some(OutputMode::Content),
            "files" => Some(OutputMode::Files),
            "count" => Some(OutputMode::Count),
            _ => None,
        }
    }
}

/// grep 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// Rust regex 正则（smart case）。
    pattern: String,
    /// 子目录（工作区相对），默认工作区根。
    #[serde(default)]
    path: Option<String>,
    /// 文件 glob 过滤（如 `*.ts`）。
    #[serde(default)]
    glob: Option<String>,
    /// 输出模式：content | files | count。
    #[serde(default)]
    output_mode: Option<String>,
    /// 每页匹配 / 文件数，默认 100、上限 200。
    #[serde(default)]
    max_matches: Option<usize>,
    /// 分页偏移。
    #[serde(default)]
    offset: Option<usize>,
}

/// grep 工具：在工作区内按正则搜索文件内容（smart case），遵循 .gitignore。
/// 入参为 pattern（必填）+ path/glob/outputMode/maxMatches/offset；ReadOnly 分级免审批。
/// 三种输出模式服务不同场景：content 定位精确行、files/count 做广度普查；均支持 offset 分页，
/// 单行文本截取前 400 字符，命中总数有 HARD_CAP 熔断。
pub struct GrepTool;

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "在工作区内按正则搜索文件内容（smart case），遵循 .gitignore。outputMode：content = 路径+行号+行文本（默认）；files = 仅命中的文件路径；count = 每文件命中计数。广度普查用 files/count，精确定位行用 content。用 offset 分页。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["pattern"],
  "properties": {
    "pattern": {"type": "string", "description": "Rust 正则"},
    "path": {"type": "string", "description": "子目录，默认工作区根"},
    "glob": {"type": "string", "description": "文件 glob 过滤，如 \"*.ts\""},
    "outputMode": {"type": "string", "enum": ["content", "files", "count"], "description": "content（默认）返回路径+行号+行文本；files 返回命中的文件路径；count 返回每文件命中计数"},
    "maxMatches": {"type": "integer", "description": "content：每页匹配数；files/count：每页文件数。默认 100，上限 200"},
    "offset": {"type": "integer", "description": "content：跳过前 N 个匹配；files/count：跳过前 N 个文件"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let roots = ctx.write_roots();
        let base = match &args.path {
            Some(p) => match super::pathutil::resolve_read(&roots, p) {
                Ok(x) => x,
                Err((c, m)) => return ToolOutcome::err(&c, m),
            },
            None => roots.workspace.clone(),
        };
        let max = args.max_matches.unwrap_or(100).min(MAX_MATCHES);
        let offset = args.offset.unwrap_or(0);
        let mode = match args.output_mode.as_deref() {
            None => OutputMode::Content,
            Some(m) => match OutputMode::parse(m) {
                Some(m) => m,
                None => {
                    return ToolOutcome::err(
                        "E_ARGS",
                        format!("outputMode 无效：{m}（可选 content | files | count）"),
                    );
                }
            },
        };

        run_grep(
            &base,
            &args.pattern,
            args.glob.as_deref(),
            mode,
            offset,
            max,
        )
    }
}

/// 具体搜索实现：单线程遍历收集全部命中，再按输出模式分页组装。
fn run_grep(
    base: &std::path::Path,
    pattern: &str,
    glob: Option<&str>,
    mode: OutputMode,
    offset: usize,
    max: usize,
) -> ToolOutcome {
    let matcher = match grep_regex::RegexMatcherBuilder::new()
        .case_smart(true)
        .build(pattern)
    {
        Ok(m) => Arc::new(m),
        Err(e) => return ToolOutcome::err("E_ARGS", format!("正则无效：{e}")),
    };

    let matches: Arc<Mutex<Vec<(String, u64, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let file_counts: Arc<Mutex<HashMap<String, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    let total = Arc::new(AtomicU64::new(0));

    let mut builder = ignore::WalkBuilder::new(base);
    builder.git_ignore(true).hidden(true);
    if let Some(g) = glob {
        let mut overrides = ignore::overrides::OverrideBuilder::new(base);
        if let Err(e) = overrides.add(g) {
            return ToolOutcome::err("E_ARGS", format!("glob 无效：{e}"));
        }
        match overrides.build() {
            Ok(o) => builder.overrides(o),
            Err(e) => return ToolOutcome::err("E_ARGS", format!("glob 无效：{e}")),
        };
    }

    // 单线程遍历（P0 足够；并行遍历留作后续优化）
    let walk = builder.build();
    let mut searcher = grep_searcher::SearcherBuilder::new()
        .line_number(true)
        .binary_detection(grep_searcher::BinaryDetection::quit(0))
        .build();
    for entry in walk {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if total.load(Ordering::Relaxed) >= HARD_CAP {
            break;
        }
        let path = entry.into_path();
        let display = path.to_string_lossy().into_owned();
        let m2 = matches.clone();
        let f2 = file_counts.clone();
        let t2 = total.clone();
        let sink = grep_searcher::sinks::UTF8(move |line_no, line| {
            if t2.load(Ordering::Relaxed) >= HARD_CAP {
                return Ok(false);
            }
            let text = line
                .trim_end_matches(['\n', '\r'])
                .chars()
                .take(400)
                .collect::<String>();
            t2.fetch_add(1, Ordering::Relaxed);
            *f2.lock().unwrap().entry(display.clone()).or_insert(0) += 1;
            m2.lock().unwrap().push((display.clone(), line_no, text));
            Ok(true)
        });
        let _ = searcher.search_path(matcher.as_ref(), &path, sink);
    }

    let all = matches.lock().unwrap();
    let total_n = total.load(Ordering::Relaxed);

    match mode {
        OutputMode::Content => {
            let page: Vec<Value> = all
                .iter()
                .skip(offset)
                .take(max)
                .map(|(p, l, t)| json!({ "path": p, "line": l, "text": t }))
                .collect();
            let mut counts: Vec<(String, u64)> = file_counts
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            counts.truncate(100);
            ToolOutcome::ok(json!({
                "pattern": pattern,
                "total": total_n,
                "shown": page.len(),
                "offset": offset,
                "truncated": total_n > (offset + page.len()) as u64,
                "file_counts": counts,
                "matches": page,
            }))
        }
        OutputMode::Files => {
            // 按首次命中顺序去重即文件顺序；offset/max 按文件分页
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let files: Vec<&str> = all
                .iter()
                .filter(|(p, _, _)| seen.insert(p.as_str()))
                .map(|(p, _, _)| p.as_str())
                .collect();
            let page: Vec<&str> = files.iter().skip(offset).take(max).copied().collect();
            ToolOutcome::ok(json!({
                "pattern": pattern,
                "total": total_n,
                "files_total": files.len(),
                "shown": page.len(),
                "offset": offset,
                "truncated": files.len() > offset + page.len(),
                "files": page,
            }))
        }
        OutputMode::Count => {
            // 完整计数（不裁 top-100），按命中数降序，按文件分页
            let mut counts: Vec<(String, u64)> = file_counts
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let page: Vec<Value> = counts
                .iter()
                .skip(offset)
                .take(max)
                .map(|(p, c)| json!({ "path": p, "count": c }))
                .collect();
            ToolOutcome::ok(json!({
                "pattern": pattern,
                "total": total_n,
                "files_total": counts.len(),
                "shown": page.len(),
                "offset": offset,
                "truncated": counts.len() > offset + page.len(),
                "counts": page,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_search() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "fn main() {}\n// TODO fix\n").unwrap();
        std::fs::write(ws.path().join("b.rs"), "todo upper\n").unwrap();
        std::fs::create_dir(ws.path().join("target")).unwrap();
        std::fs::write(ws.path().join("target/ignored.rs"), "TODO in ignored\n").unwrap();
        std::fs::write(ws.path().join(".gitignore"), b"target/\n").unwrap();

        let out = run_grep(ws.path(), "TODO", None, OutputMode::Content, 0, 100);
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["total"], 2); // target/ 被 gitignore 排除
        let matches = out.data["matches"].as_array().unwrap();
        assert!(
            matches
                .iter()
                .any(|m| m["path"].as_str().unwrap().ends_with("a.rs") && m["line"] == 2)
        );
    }

    #[test]
    fn glob_and_pagination() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "x\nx\nx\n").unwrap();
        std::fs::write(ws.path().join("a.txt"), "x\n").unwrap();
        let out = run_grep(ws.path(), "x", Some("*.rs"), OutputMode::Content, 0, 2);
        assert_eq!(out.data["total"], 3);
        assert_eq!(out.data["shown"], 2);
        let out2 = run_grep(ws.path(), "x", Some("*.rs"), OutputMode::Content, 2, 2);
        assert_eq!(out2.data["shown"], 1);
    }

    #[test]
    fn files_mode_lists_paths_paged_by_file() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "hit\nhit\nhit\n").unwrap();
        std::fs::write(ws.path().join("b.rs"), "hit\n").unwrap();
        let out = run_grep(ws.path(), "hit", None, OutputMode::Files, 0, 100);
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["total"], 4, "total 仍是匹配总数");
        assert_eq!(out.data["files_total"], 2);
        assert_eq!(out.data["shown"], 2);
        assert!(
            out.data["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.is_string())
        );
        assert!(
            out.data.get("file_counts").is_none(),
            "files 模式不应带 file_counts"
        );

        // 按文件分页
        let out2 = run_grep(ws.path(), "hit", None, OutputMode::Files, 0, 1);
        assert_eq!(out2.data["shown"], 1);
        assert_eq!(out2.data["truncated"], true);
    }

    #[test]
    fn count_mode_pages_by_file_with_exact_counts() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "hit\nhit\nhit\n").unwrap();
        std::fs::write(ws.path().join("b.rs"), "hit\n").unwrap();
        let out = run_grep(ws.path(), "hit", None, OutputMode::Count, 0, 100);
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["total"], 4);
        assert_eq!(out.data["files_total"], 2);
        let counts = out.data["counts"].as_array().unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0]["count"], 3, "按命中数降序");
        assert!(counts[0]["path"].as_str().unwrap().ends_with("a.rs"));
        assert_eq!(counts[1]["count"], 1);
    }

    #[test]
    fn output_mode_whitelist() {
        assert_eq!(OutputMode::parse("content"), Some(OutputMode::Content));
        assert_eq!(OutputMode::parse("files"), Some(OutputMode::Files));
        assert_eq!(OutputMode::parse("count"), Some(OutputMode::Count));
        assert_eq!(OutputMode::parse("everything"), None);
        assert_eq!(OutputMode::parse(""), None);
    }

    #[test]
    fn bad_regex_is_clean_error() {
        let ws = tempfile::tempdir().unwrap();
        let out = run_grep(ws.path(), "(unclosed", None, OutputMode::Content, 0, 10);
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }
}
