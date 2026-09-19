//! 记忆系统（[docs/p1-plan](../../docs/p1-plan.md) §5.2）：~/.codewave/memories/*.md + frontmatter；
//! 无专用工具——目录在写白名单内，模型直接经 create/edit 维护文件；索引注入系统提示词。

use crate::skills::parse_skill_md;
use serde::Serialize;
use std::path::Path;

/// 记忆条目数量上限（索引与扫描共用，防御目录膨胀）。
pub const MEMORY_LIMIT: usize = 200;

/// 单条记忆的元数据（索引注入用）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryMeta {
    /// 记忆名（frontmatter name，缺省回退文件名）
    pub name: String,
    /// 描述（frontmatter 缺省时取正文首行前 80 字符）
    pub description: String,
    /// 文件绝对路径
    pub path: String,
}

/// 扫描用户级与项目级记忆目录并合并（按名排序，截断到 MEMORY_LIMIT）。
pub fn scan(data_dir: &Path, project_dir: Option<&Path>) -> Vec<MemoryMeta> {
    let mut out = scan_dir(&data_dir.join("memories"));
    if let Some(pd) = project_dir {
        // 项目级记忆：同名覆盖用户级（评审 M1：此前 append 导致同名两条一起注入）
        for m in scan_dir(&pd.join("memory")) {
            match out.iter_mut().find(|e| e.name == m.name) {
                Some(e) => *e = m,
                None => out.push(m),
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(MEMORY_LIMIT);
    out
}

/// 扫描单个目录下的 *.md 记忆文件（复用技能 frontmatter 解析；缺描述取正文首行）。
fn scan_dir(dir: &Path) -> Vec<MemoryMeta> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<MemoryMeta> = rd
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let stem = e
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".md")
                .to_string();
            let s = parse_skill_md(&text, &stem)?;
            Some(MemoryMeta {
                name: s.meta.name,
                description: if s.meta.description.is_empty() {
                    s.body
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect()
                } else {
                    s.meta.description
                },
                path: e.path().display().to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(MEMORY_LIMIT);
    out
}

/// 系统提示词注入：路径 + 描述索引。
pub fn prompt_listing(metas: &[MemoryMeta]) -> String {
    if metas.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| format!("- {}: {}（{}）", m.name, m.description, m.path))
        .collect();
    format!(
        "<persistent-memories>\n可直接用 create/edit 工具维护上述记忆文件（目录在可写白名单内）。\n{}\n</persistent-memories>",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_and_listing() {
        let dd = tempfile::tempdir().unwrap();
        let mem = dd.path().join("memories");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("rust-prefs.md"),
            "---\nname: rust-prefs\ndescription: 用户偏好 Rust edition 2024\n---\n正文",
        )
        .unwrap();
        std::fs::write(mem.join("no-fm.md"), "just a body line").unwrap();

        let metas = scan(dd.path(), None);
        assert_eq!(metas.len(), 2);
        assert!(
            metas
                .iter()
                .any(|m| m.name == "rust-prefs" && m.description.contains("edition 2024"))
        );
        let listing = prompt_listing(&metas);
        assert!(listing.contains("persistent-memories"));
        assert!(listing.contains("rust-prefs"));
    }

    #[test]
    fn project_memory_overrides_same_name() {
        // 评审 M1：项目级同名记忆必须覆盖用户级；绝不两条一起注入
        let dd = tempfile::tempdir().unwrap();
        let user = dd.path().join("memories");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("stack.md"),
            "---\nname: stack\ndescription: 用户级：Go\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            user.join("only-user.md"),
            "---\nname: only-user\ndescription: 仅用户级\n---\nbody",
        )
        .unwrap();
        let project = dd.path().join("proj");
        let project_mem = project.join("memory");
        std::fs::create_dir_all(&project_mem).unwrap();
        std::fs::write(
            project_mem.join("stack.md"),
            "---\nname: stack\ndescription: 项目级：Rust\n---\nbody",
        )
        .unwrap();

        // scan 的 project_dir 语义 = 项目根（内部自行拼接 memory/ 子目录）
        let metas = scan(dd.path(), Some(&project));
        assert_eq!(metas.len(), 2, "同名只保留一条");
        let stack = metas.iter().find(|m| m.name == "stack").unwrap();
        assert!(
            stack.description.contains("项目级"),
            "项目级应胜出：{:?}",
            stack.description
        );
        assert!(stack.path.contains("proj"), "路径应指向项目目录");
        assert!(metas.iter().any(|m| m.name == "only-user"));
    }
}
