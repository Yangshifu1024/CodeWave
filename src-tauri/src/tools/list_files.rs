//! list_files 工具 + 工作区路径索引（@ 文件提及的搜索数据源，runtime 上 TTL 缓存）。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// 每目录直录条目预算（ally 2bda8df 移植）：超出部分折叠为 `+N more` 占位，
/// 防止 node_modules 式巨型目录吃光全局限额。占位不占配额、不计入 count。
const DIR_BUDGET: usize = 50;

/// list_files 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 子目录（工作区相对），默认工作区根。
    #[serde(default)]
    path: Option<String>,
    /// 遍历深度，默认 2、上限 8。
    #[serde(default)]
    max_depth: Option<u32>,
    /// 条目总数上限，默认 200、最大 2000。
    #[serde(default)]
    limit: Option<usize>,
}

/// list_files 工具：列出工作区文件树（遵循 .gitignore）。
/// 入参为可选 path / maxDepth / limit；ReadOnly 分级免审批。
/// 每目录直录条目有 DIR_BUDGET 预算，超出折叠为 `+N more` 占位（不占全局配额）；
/// 多根项目无 path 时先列出各根的绝对路径供 AI 按需下钻。
pub struct ListFilesTool;

#[async_trait::async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &'static str {
        "list_files"
    }
    fn description(&self) -> &'static str {
        "列出工作区文件（遵循 .gitignore）。可选子目录 path、maxDepth（默认 2）与 limit（默认 200）。每目录直录条目上限 50：超出部分折叠为 `+N more in <dir>` 占位行——请用 path 收窄后继续浏览。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "path": {"type": "string", "description": "子目录，默认工作区根"},
    "maxDepth": {"type": "integer", "description": "默认 2"},
    "limit": {"type": "integer", "description": "默认 200，上限 2000"}
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
        // 多根项目：未指定 path → 列出全部根（绝对路径），AI 按需下钻
        if args.path.is_none() && !roots.extra.is_empty() {
            let mut entries: Vec<String> = vec![format!("dir: {}", roots.workspace.display())];
            for e in &roots.extra {
                entries.push(format!("dir: {}", e.display()));
            }
            return ToolOutcome::ok(json!({
                "root": "<multi-root project>",
                "count": entries.len(),
                "truncated": false,
                "entries": entries,
                "note": "多根项目：请携带上列某个目录（绝对路径）重新调用 list_files 以浏览其内部。"
            }));
        }
        let base = match &args.path {
            Some(p) => match super::pathutil::resolve_read(&roots, p) {
                Ok(x) => x,
                Err((c, m)) => return ToolOutcome::err(&c, m),
            },
            None => roots.workspace.clone(),
        };
        let limit = args.limit.unwrap_or(200).min(2000);
        let depth = args.max_depth.unwrap_or(2).min(8) as usize;

        let (count, truncated, entries) = walk_with_budget(&base, &roots.workspace, limit, depth);
        ToolOutcome::ok(
            json!({ "root": args.path.unwrap_or_default(), "count": count, "truncated": truncated, "entries": entries }),
        )
    }
}

/// 带每目录预算的遍历（ally 2bda8df 移植）：单目录直录条目超过 DIR_BUDGET 即折叠为
/// `+N more in <dir>` 占位（追加在末尾、不占全局配额、不计入 count），防止巨型目录挤占整个列表。
fn walk_with_budget(
    base: &std::path::Path,
    workspace: &std::path::Path,
    limit: usize,
    depth: usize,
) -> (usize, bool, Vec<String>) {
    let mut builder = ignore::WalkBuilder::new(base);
    builder.max_depth(Some(depth)).git_ignore(true).hidden(true);
    let mut entries: Vec<String> = Vec::new();
    let mut truncated = false;
    // 每目录计数：父物理路径 → 已收集直录条目数；溢出按父目录相对路径聚合
    let mut per_dir: HashMap<std::path::PathBuf, usize> = HashMap::new();
    let mut overflow: HashMap<String, u64> = HashMap::new();
    for e in builder.build() {
        let Ok(e) = e else { continue };
        if entries.len() >= limit {
            truncated = true;
            break;
        }
        let rel = e.path().strip_prefix(workspace).unwrap_or(e.path());
        if rel.as_os_str().is_empty() {
            continue;
        }
        // 占位不占全局配额：超预算条目跳过收集但遍历继续（更深的子目录有各自预算）
        let parent = e.path().parent().map(|p| p.to_path_buf());
        let budget_hit = parent
            .as_ref()
            .map(|p| per_dir.get(p).copied().unwrap_or(0) >= DIR_BUDGET)
            .unwrap_or(false);
        if budget_hit {
            // 多根：对主根取相对路径失败时回退父目录本身（绝对路径），与多根提示风格一致
            let parent_rel: &std::path::Path = parent
                .as_ref()
                .map(|p| p.strip_prefix(workspace).unwrap_or(p))
                .unwrap_or(e.path());
            let key = if parent_rel.as_os_str().is_empty() {
                "<root>".to_string()
            } else {
                parent_rel.to_string_lossy().into_owned()
            };
            *overflow.entry(key).or_insert(0) += 1;
            continue;
        }
        if let Some(p) = parent {
            *per_dir.entry(p).or_insert(0) += 1;
        }
        let kind = if e.path().is_dir() { "dir" } else { "file" };
        entries.push(format!("{kind}: {}", rel.to_string_lossy()));
    }
    entries.sort();
    let count = entries.len();
    let mut notes: Vec<String> = overflow
        .into_iter()
        .map(|(d, n)| format!("+{n} more in {d}"))
        .collect();
    notes.sort();
    entries.extend(notes);
    (count, truncated, entries)
}

/// 面向 @ 文件提及与模糊搜索的完整路径索引（带 TTL 缓存）。
pub fn search_workspace_paths(
    rt: &std::sync::Arc<crate::core::agent::SessionRuntime>,
    query: &str,
    limit: usize,
) -> Vec<String> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(600);
    const MAX_ENTRIES: usize = 50_000;

    {
        let cache = rt.paths_cache.lock().unwrap();
        if let Some((at, paths)) = cache.as_ref() {
            if at.elapsed() < TTL {
                return fuzzy_filter(paths, query, limit);
            }
        }
    }
    // 跨根收集：多根返回绝对路径（可唯一定位）；单根保持相对路径
    let mut walk_roots: Vec<std::path::PathBuf> = vec![rt.workspace.clone()];
    walk_roots.extend(
        rt.extra_roots
            .lock()
            .unwrap()
            .iter()
            .map(std::path::PathBuf::from),
    );
    let multi = walk_roots.len() > 1;
    let mut paths: Vec<String> = Vec::new();
    'outer: for root in walk_roots {
        let mut builder = ignore::WalkBuilder::new(&root);
        builder.git_ignore(true).hidden(true).max_depth(Some(12));
        for e in builder.build() {
            let Ok(e) = e else { continue };
            if paths.len() >= MAX_ENTRIES {
                break 'outer;
            }
            if let Ok(rel) = e.path().strip_prefix(&root) {
                if rel.as_os_str().is_empty() {
                    continue;
                }
                if multi {
                    paths.push(e.path().to_string_lossy().into_owned());
                } else {
                    paths.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    paths.sort();
    *rt.paths_cache.lock().unwrap() = Some((std::time::Instant::now(), paths.clone()));
    fuzzy_filter(&paths, query, limit)
}

/// 大小写不敏感的子序列匹配；连续命中与文件名命中得分更高。
fn fuzzy_filter(paths: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let q = q.trim();
    if q.is_empty() {
        return paths.iter().take(limit).cloned().collect();
    }
    let mut scored: Vec<(i64, &String)> = Vec::new();
    for p in paths {
        let lp = p.to_lowercase();
        let file_name = lp.rsplit('/').next().unwrap_or(&lp);
        let mut score = 0i64;
        let mut pi = 0usize;
        let mut consec = 0i64;
        let chars: Vec<char> = q.chars().collect();
        let mut matched_all = true;
        for (qi, qc) in chars.iter().enumerate() {
            match lp[pi..].find(*qc) {
                Some(off) => {
                    if off == 0 && qi > 0 {
                        consec += 1;
                        score += 2 + consec;
                    } else {
                        consec = 0;
                        score += 1;
                    }
                    // 按字节步进已匹配字符（len_utf8）而非 1——多字节命中后步 1 字节
                    // 会落进字符中间，lp[pi..] 将 panic
                    pi += off + qc.len_utf8();
                }
                None => {
                    matched_all = false;
                    break;
                }
            }
        }
        if matched_all {
            if file_name.contains(q) {
                score += 10;
            }
            scored.push((score, p));
        }
    }
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, p)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranking() {
        let paths = vec![
            "src/lib/api.ts".to_string(),
            "src/App.vue".to_string(),
            "docs/readme.md".to_string(),
        ];
        let r = fuzzy_filter(&paths, "api", 5);
        assert_eq!(r.first().map(|s| s.as_str()), Some("src/lib/api.ts"));
        assert_eq!(fuzzy_filter(&paths, "", 2).len(), 2);
        let r = fuzzy_filter(&paths, "zzz", 5);
        assert!(r.is_empty());
    }

    #[test]
    fn fuzzy_multibyte_and_case_insensitive() {
        // [docs/arithmetic-audit](../../../docs/arithmetic-audit.md)#1：多字节命中此前按 1 字节推进 pi，把 lp 切进字符中间 → panic
        let paths = vec!["src/中文/笔记.md".to_string(), "src/lib/api.ts".to_string()];
        let r = fuzzy_filter(&paths, "中文", 5);
        assert_eq!(r.first().map(|s| s.as_str()), Some("src/中文/笔记.md"));
        // 打分前路径已转小写，query 也必须转小写
        let r = fuzzy_filter(&paths, "API", 5);
        assert_eq!(r.first().map(|s| s.as_str()), Some("src/lib/api.ts"));
    }

    #[test]
    fn per_dir_budget_folds_overflow_without_stealing_quota() {
        // 80 个平铺包的 node_modules（超 50 预算）+ 普通 src 目录：折叠不占全局配额，其他目录不受影响
        let ws = tempfile::tempdir().unwrap();
        let nm = ws.path().join("node_modules");
        std::fs::create_dir_all(&nm).unwrap();
        for i in 0..80 {
            std::fs::write(nm.join(format!("pkg{i:02}.js")), "").unwrap();
        }
        std::fs::create_dir_all(ws.path().join("src")).unwrap();
        std::fs::write(ws.path().join("src").join("a.ts"), "").unwrap();

        let roots = test_roots(ws.path());
        let (count, truncated, entries) =
            walk_with_budget(&roots.workspace, &roots.workspace, 200, 2);
        assert!(!truncated);
        // 条目路径分隔符随平台（Windows 为 `\`）；断言统一归一为 `/`
        let texts: Vec<String> = entries.iter().map(|s| s.replace('\\', "/")).collect();
        assert_eq!(
            count, 53,
            "折叠占位不计入 count：50(pkg)+1(node_modules)+1(dir)+1(file)"
        );
        let folded: Vec<&String> = texts.iter().filter(|t| t.contains("more in")).collect();
        assert_eq!(folded.len(), 1, "{texts:?}");
        assert!(folded[0].contains("+30 more in node_modules"), "{folded:?}");
        assert!(
            texts.iter().any(|t| t == "file: src/a.ts"),
            "其他目录不受影响"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.starts_with("file: node_modules/"))
                .count(),
            50
        );
    }

    #[test]
    fn root_overflow_reports_root_placeholder() {
        let ws = tempfile::tempdir().unwrap();
        for i in 0..55 {
            std::fs::write(ws.path().join(format!("f{i:02}.txt")), "").unwrap();
        }
        let roots = test_roots(ws.path());
        let (count, _, entries) = walk_with_budget(&roots.workspace, &roots.workspace, 200, 2);
        assert_eq!(count, 50);
        assert!(
            entries.iter().any(|t| t == "+5 more in <root>"),
            "{entries:?}"
        );
    }

    fn test_roots(ws_path: &std::path::Path) -> super::super::pathutil::WriteRoots {
        let dd = tempfile::tempdir().unwrap();
        let data_dir = std::fs::canonicalize(dd.path()).unwrap();
        drop(dd); // 遍历不触碰 data_dir；canonicalize 之后释放无碍
        super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws_path).unwrap(),
            extra: vec![],
            data_dir,
        }
    }
}
