//! 诊断裁剪：指纹、差集、去重刹车、解析、回喂文本渲染。
//!
//! 核心语义：**只回喂本次写入新增的 error 级诊断**——项目存量错误不是本次改动的问题，
//! 回喂它们会把模型的注意力绑在无关代码上（差集不可用时宁可不回喂）。

use super::DiagnosticItem;
use serde::Deserialize;
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

/// 只回喂 error：LSP `DiagnosticSeverity::ERROR` 的数值。
///
/// 该 newtype 的内部值不可直接取，故用 serde（`transparent`）序列化求得；
/// 单测把「必须是 1」钉死，上游改语义时先失败。
pub static SEVERITY_ERROR: std::sync::LazyLock<i64> = std::sync::LazyLock::new(|| {
    serde_json::to_value(lsp_types::DiagnosticSeverity::ERROR)
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
});

/// 诊断指纹：severity + 行列 + code + message 的稳定哈希（16 位十六进制）。
pub fn fingerprint(severity: u8, line: u32, col: u32, code: &str, message: &str) -> String {
    let mut h = DefaultHasher::new();
    severity.hash(&mut h);
    line.hash(&mut h);
    col.hash(&mut h);
    code.hash(&mut h);
    message.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// 差集：返回（新增指纹，被消除的条数）。两侧都先去重。
pub fn diff(baseline: &[String], now: &[String]) -> (Vec<String>, usize) {
    let before: HashSet<&String> = baseline.iter().collect();
    let after: HashSet<&String> = now.iter().collect();
    let mut added: Vec<String> = Vec::new();
    let mut seen: HashSet<&String> = HashSet::new();
    for fp in now {
        if before.contains(fp) || !seen.insert(fp) {
            continue;
        }
        added.push(fp.clone());
    }
    let removed = before.iter().filter(|fp| !after.contains(*fp)).count();
    (added, removed)
}

/// 去重刹车：同一 (文件, 指纹) 最多回喂 `limit` 次（超限只记日志，不再打扰模型）。
#[derive(Default)]
pub struct DedupBrake {
    counts: HashMap<(String, String), usize>,
}

impl DedupBrake {
    /// 空刹车。
    pub fn new() -> Self {
        DedupBrake::default()
    }

    /// 是否放行本次回喂（放行时计数 +1）。
    pub fn allow(&mut self, path: &str, fingerprint: &str, limit: usize) -> bool {
        let key = (path.to_string(), fingerprint.to_string());
        let count = self.counts.entry(key).or_insert(0);
        if *count >= limit {
            tracing::debug!(path, fingerprint, limit, "同一诊断已达回喂上限，静默");
            return false;
        }
        *count += 1;
        true
    }

    /// 清空（设置变更/手动重置时用）。
    pub fn clear(&mut self) {
        self.counts.clear();
    }
}

/// `publishDiagnostics.params.diagnostics[]` 的宽松 DTO（比强类型更能容忍 server 变体）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDiag {
    #[serde(default)]
    severity: Option<i64>,
    #[serde(default)]
    range: Option<RawRange>,
    #[serde(default)]
    code: Option<Value>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    related_information: Option<Vec<RawRelated>>,
}

#[derive(Deserialize)]
struct RawRange {
    start: RawPosition,
}

#[derive(Deserialize)]
struct RawPosition {
    #[serde(default)]
    line: u32,
    #[serde(default)]
    character: u32,
}

#[derive(Deserialize)]
struct RawRelated {
    #[serde(default)]
    message: String,
}

/// 把原始诊断裁剪成回喂项：**只留 error**；缺 `range` 跳过；`code` 缺失填 `?`；
/// 行列 0-based → 1-based；`relatedInformation` 折进 message 尾部。
pub fn parse_diagnostics(
    raw: &[Value],
    rel_path: &str,
    project_root: &Path,
) -> Vec<DiagnosticItem> {
    let mut out: Vec<DiagnosticItem> = Vec::new();
    for value in raw {
        let Ok(d) = serde_json::from_value::<RawDiag>(value.clone()) else {
            tracing::debug!("诊断解析失败，跳过一条");
            continue;
        };
        if d.severity.unwrap_or(*SEVERITY_ERROR) != *SEVERITY_ERROR {
            continue;
        }
        let Some(range) = d.range else {
            continue;
        };
        let line = range.start.line + 1;
        let col = range.start.character + 1;
        let code = match d.code.as_ref() {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Object(o)) => o
                .get("value")
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "?".into()),
            _ => "?".into(),
        };
        let mut message = single_line(&d.message);
        message = relativize(&message, project_root);
        if let Some(related) = d.related_information.as_ref().filter(|r| !r.is_empty()) {
            let joined: Vec<String> = related
                .iter()
                .map(|r| single_line(&r.message))
                .filter(|m| !m.is_empty())
                .collect();
            if !joined.is_empty() {
                message.push_str(&format!("（相关：{}）", joined.join("；")));
            }
        }
        let fingerprint = fingerprint(1, line, col, &code, &message);
        out.push(DiagnosticItem {
            path: rel_path.to_string(),
            line,
            col,
            code,
            message,
            fingerprint,
        });
    }
    out
}

/// 多行 message 折成单行（换行 → 空格，压缩连续空白）。
pub fn single_line(s: &str) -> String {
    let folded: String = s
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut out = String::with_capacity(folded.len());
    let mut last_space = false;
    for ch in folded.chars() {
        if ch == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

/// 把 message 里的项目绝对路径改写成相对形态（模型读起来更干净）。
fn relativize(message: &str, project_root: &Path) -> String {
    let root = project_root
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    if root.is_empty() {
        return message.to_string();
    }
    let mut normalized = message.replace('\\', "/");
    let with_sep = format!("{root}/");
    if normalized.contains(&with_sep) {
        normalized = normalized.replace(&with_sep, "");
    } else if normalized.contains(&root) {
        normalized = normalized.replace(&root, "");
    }
    normalized.trim_start_matches('/').to_string()
}

/// 回喂文本：头 + 逐条 + 脚（正反馈 / 截断标注）。
///
/// ```text
/// （写入后语义校验发现 2 个错误，请用 edit 修复：
/// ui/src/x.ts:3:5 TS2322 Type 'string' is not assignable to type 'number'.
/// ）
/// ```
pub fn format_feedback(
    items: &[DiagnosticItem],
    removed: usize,
    truncated: usize,
    max_chars: usize,
) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = items.iter().map(|i| i.render()).collect();
    let mut cut = 0usize;
    loop {
        let total = lines.len() + truncated + cut;
        let mut text = String::new();
        text.push_str(&format!(
            "（写入后语义校验发现 {total} 个错误，请用 edit 修复：\n"
        ));
        for l in &lines {
            text.push_str(l);
            text.push('\n');
        }
        if removed > 0 {
            text.push_str(&format!("（本次改动同时消除了 {removed} 个既有错误）\n"));
        }
        if truncated + cut > 0 {
            text.push_str(&format!("…（已截断 {} 条）\n", truncated + cut));
        }
        text.push('）');
        if text.chars().count() <= max_chars || lines.is_empty() {
            return text;
        }
        lines.pop();
        cut += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn diag(severity: i64, line: u32, code: &str, message: &str) -> Value {
        json!({
            "range": { "start": { "line": line, "character": 4 }, "end": { "line": line, "character": 9 } },
            "severity": severity,
            "code": code,
            "message": message,
            "source": "tsserver"
        })
    }

    #[test]
    fn fingerprint_is_stable_and_distinguishing() {
        let a = fingerprint(1, 3, 5, "TS2322", "boom");
        let b = fingerprint(1, 3, 5, "TS2322", "boom");
        let c = fingerprint(1, 3, 5, "TS2322", "boom!");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn diff_reports_added_and_removed() {
        let base = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let now = vec!["b".to_string(), "d".to_string(), "d".to_string()];
        let (added, removed) = diff(&base, &now);
        assert_eq!(added, vec!["d".to_string()], "重复项只报一次");
        assert_eq!(removed, 2, "a 与 c 被消除");
    }

    #[test]
    fn diff_with_empty_baseline_is_all_new() {
        let (added, removed) = diff(&[], &["x".to_string()]);
        assert_eq!(added.len(), 1);
        assert_eq!(removed, 0);
    }

    #[test]
    fn brake_allows_limit_times_then_silences() {
        let mut brake = DedupBrake::new();
        assert!(brake.allow("a.ts", "fp1", 2));
        assert!(brake.allow("a.ts", "fp1", 2));
        assert!(!brake.allow("a.ts", "fp1", 2), "第 3 次必须静默");
        assert!(brake.allow("a.ts", "fp2", 2), "不同指纹互不影响");
        assert!(brake.allow("b.ts", "fp1", 2), "不同文件互不影响");
    }

    #[test]
    fn severity_error_constant_matches_lsp() {
        assert_eq!(*SEVERITY_ERROR, 1, "LSP DiagnosticSeverity::ERROR 必须是 1");
    }

    #[test]
    fn parse_keeps_only_errors_and_shifts_to_one_based() {
        let raw = vec![
            diag(1, 2, "TS1", "err"),
            diag(2, 3, "TS2", "warn"),
            diag(3, 0, "TS3", "info"),
            diag(4, 0, "TS4", "hint"),
        ];
        let items = parse_diagnostics(&raw, "ui/src/x.ts", &PathBuf::from("D:\\proj"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, 3);
        assert_eq!(items[0].col, 5);
        assert_eq!(items[0].code, "TS1");
        assert_eq!(items[0].path, "ui/src/x.ts");
    }

    #[test]
    fn parse_handles_missing_code_and_range() {
        let raw = vec![
            json!({ "range": { "start": { "line": 0, "character": 0 } }, "severity": 1, "message": "no code" }),
            json!({ "severity": 1, "message": "no range" }),
            json!({ "range": { "start": { "line": 1, "character": 1 } }, "severity": 1, "code": 42, "message": "num code" }),
            json!({ "range": { "start": { "line": 1, "character": 1 } }, "severity": 1, "code": {"value": "TS9", "target": "x"}, "message": "obj code" }),
        ];
        let items = parse_diagnostics(&raw, "a.rs", &PathBuf::new());
        assert_eq!(items.len(), 3, "缺 range 的必须跳过：{items:?}");
        assert_eq!(items[0].code, "?");
        assert_eq!(items[1].code, "42");
        assert_eq!(items[2].code, "TS9");
    }

    #[test]
    fn parse_folds_related_information_and_newlines() {
        let raw = vec![json!({
            "range": { "start": { "line": 0, "character": 0 } },
            "severity": 1,
            "code": "E0433",
            "message": "cannot find value\nmore detail\n  here",
            "relatedInformation": [
                { "message": "first note\nwith newline" },
                { "message": "second note" }
            ]
        })];
        let items = parse_diagnostics(&raw, "src/main.rs", &PathBuf::new());
        assert_eq!(items.len(), 1);
        assert!(!items[0].message.contains('\n'), "{}", items[0].message);
        assert!(
            items[0].message.contains("first note with newline"),
            "{}",
            items[0].message
        );
        assert!(
            items[0].message.contains("second note"),
            "{}",
            items[0].message
        );
    }

    #[test]
    fn parse_relativizes_project_root_in_message() {
        let raw = vec![diag(1, 0, "E1", r"error at D:\proj\src\x.rs:12")];
        let items = parse_diagnostics(&raw, "src/x.rs", Path::new(r"D:\proj"));
        assert!(
            items[0].message.contains("src/x.rs:12"),
            "{}",
            items[0].message
        );
        assert!(
            !items[0].message.contains("D:/proj"),
            "{}",
            items[0].message
        );
    }

    #[test]
    fn format_feedback_header_and_budget() {
        let raw = vec![diag(
            1,
            2,
            "TS2322",
            "Type 'string' is not assignable to type 'number'.",
        )];
        let items = parse_diagnostics(&raw, "ui/src/x.ts", &PathBuf::new());
        let text = format_feedback(&items, 0, 0, 4000);
        assert!(text.starts_with("（写入后语义校验发现 1 个错误，请用 edit 修复：\n"));
        assert!(
            text.contains(
                "ui/src/x.ts:3:5 TS2322 Type 'string' is not assignable to type 'number'."
            )
        );
        assert!(text.ends_with('）'));
        assert!(!text.contains("已截断"));
    }

    #[test]
    fn format_feedback_appends_removed_and_truncation() {
        let raw: Vec<Value> = (0..5)
            .map(|i| diag(1, i, "E1", &format!("message number {i}")))
            .collect();
        let items = parse_diagnostics(&raw, "src/x.rs", &PathBuf::new());
        let text = format_feedback(&items, 3, 2, 4000);
        assert!(text.contains("发现 7 个错误"), "{text}");
        assert!(
            text.contains("（本次改动同时消除了 3 个既有错误）"),
            "{text}"
        );
        assert!(text.contains("…（已截断 2 条）"), "{text}");
    }

    #[test]
    fn format_feedback_drops_lines_when_over_budget() {
        let raw: Vec<Value> = (0..5)
            .map(|i| diag(1, i, "E1", &format!("message number {i}")))
            .collect();
        let items = parse_diagnostics(&raw, "src/x.rs", &PathBuf::new());
        let full = format_feedback(&items, 0, 0, 100_000);
        let tight = format_feedback(&items, 0, 0, 120);
        assert!(tight.chars().count() <= full.chars().count());
        assert!(tight.contains("已截断"), "{tight}");
        assert!(tight.chars().count() <= 200, "{}", tight.chars().count());
    }

    #[test]
    fn format_feedback_empty_items_is_empty_text() {
        assert!(format_feedback(&[], 0, 0, 100).is_empty());
    }

    #[test]
    fn single_line_collapses_whitespace() {
        assert_eq!(single_line("a\n\n  b\tc "), "a b c");
    }
}
