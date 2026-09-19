//! 技能系统（[docs/p1-plan](../../../docs/p1-plan.md) §5.1）：多目录扫描 + YAML frontmatter + 索引注入 + slash/工具双通道。
//! 加载路径（[docs/slash-skills-and-dollar-agents](../../../docs/slash-skills-and-dollar-agents.md)）：
//! `.codewave/skills`（项目会话 = project_dir `<主目录>/.codewave/skills`；临时/全局 = data_dir `~/.codewave/skills`）、
//! `.agents/skills` 与 `.claude/skills`（工作区级）+ `~/.agents/skills` 与 `~/.claude/skills`（用户级只读兼容）+ 内置。
//! 注意 rt.data_dir 恒为全局数据目录（见 get_or_create_session），项目数据目录经 rt.project_dir 传入。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 技能元数据（列表/注入索引用的轻量视图）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SkillMeta {
    /// 技能名（frontmatter 的 name，缺省回退目录名/文件名）
    pub name: String,
    /// 功能描述
    pub description: String,
    /// 触发时机（wire 名 whenToUse；兼容 snake_case 别名）
    #[serde(rename = "whenToUse")]
    pub when_to_use: String,
    /// 技能内容的来源路径（内置为 "<builtin>"）
    pub origin: String,
    /// 是否可删除（仅托管目录：data_dir/skills 与 project_dir/skills；内置/兼容目录不可删）
    #[serde(default)]
    pub deletable: bool,
}

/// 完整技能：元数据 + 正文（调用时才注入全文）。
#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    /// 元数据
    pub meta: SkillMeta,
    /// 正文（frontmatter 之后的 markdown）
    pub body: String,
}

/// frontmatter 解析：`---\n<yaml>\n---\n<body>`；无 frontmatter 时 name 回退目录名。
pub fn parse_skill_md(text: &str, fallback_name: &str) -> Option<Skill> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let body = if let Some(rest) = text.strip_prefix("---\n") {
        let (fm, body) = rest.split_once("\n---")?;
        let body = body.trim_start_matches('\n');
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Fm {
            name: Option<String>,
            description: Option<String>,
            #[serde(alias = "when_to_use")]
            #[serde(rename = "whenToUse")]
            when_to_use: Option<String>,
        }
        let fm: Fm = serde_yaml::from_str(fm).ok()?;
        Skill {
            meta: SkillMeta {
                name: fm.name.unwrap_or_else(|| fallback_name.to_string()),
                description: fm.description.unwrap_or_default(),
                when_to_use: fm.when_to_use.unwrap_or_default(),
                origin: String::new(),
                deletable: false,
            },
            body: body.to_string(),
        }
    } else {
        Skill {
            meta: SkillMeta {
                name: fallback_name.to_string(),
                description: String::new(),
                when_to_use: String::new(),
                origin: String::new(),
                deletable: false,
            },
            body: text.to_string(),
        }
    };
    if body.meta.name.trim().is_empty() {
        return None;
    }
    Some(body)
}

/// 内置技能（原文随仓库分发；repo-index / doc-convert。原 arch 编排剧本已内置为标准工作流，
/// 常驻注入 core/prompt.rs WORKFLOW_SECTION，[docs/standard-workflow](../../../docs/standard-workflow.md)）。
pub fn builtin_skills() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "repo-index",
            r#"---
name: repo-index
description: Build or refresh a CODEGRAPH.md file that maps features to files for the current repository.
whenToUse: Use when the user asks to index/summarize the repo structure, or before large cross-cutting changes when no CODEGRAPH.md exists.
---
# repo-index 工作流

目标：生成或更新仓库根目录的 `CODEGRAPH.md`（功能 → 文件速查表），供后续会话快速定位。

步骤：
1. `list_files` 了解顶层结构（maxDepth 2-3）。
2. 对每个主要目录，抽样 `read` 关键入口文件（main/mod/lib/index 等）确认职责。
3. 若已存在 CODEGRAPH.md：先读取，只更新发生变化的部分，保持既有格式。
4. 输出格式（保持精简，一行一文件）：
   # Code Graph: <仓库名>
   ## 功能 → 文件速查表
   | 想找什么 | 去哪里 |
   5. 用 `create`/`edit` 写入，并提醒用户该文件会被注入后续会话的系统提示词。
"#,
        ),
        (
            "doc-convert",
            r#"---
name: doc-convert
description: Convert documents between plain formats (md/txt/csv/json) using local command-line tools.
whenToUse: Use when the user asks to transform or convert local text-based documents and needs a reliable recipe.
---
# doc-convert 指引

常用转换路径（优先用已安装的本地工具，先 `command --version` 探测）：
- csv → json：用 python3 一行脚本（csv.DictReader + json.dumps，ensure_ascii=False）。
- json → csv：python3 提取数组字段展平。
- md → 纯文本：去标记（标题/链接/表格），可用 python3 正则。
- xlsx → csv：若装了 python3+openpyxl 则脚本读取；否则提示用户安装或手动导出。
- pdf/docx：不在本 skill 范围（P0 的 read 会拒绝二进制文档）。

约定：输出文件写到 `tmp/` 或用户指定路径；转换后 `read` 抽样验证首尾行。
"#,
        ),
    ]
}

/// TTL 缓存槽位：(采集时刻, scope key, 以技能名为键的索引)。
type SkillCache = Mutex<Option<(Instant, String, HashMap<String, Skill>)>>;

/// 技能索引：带 scope key 的 TTL 缓存，负责扫描/查询/列表。
pub struct SkillIndex {
    /// 带 scope key 的 TTL 缓存（评审 M3：切换工作区/项目不得互串缓存）
    cache: SkillCache,
    /// 缓存有效期
    ttl: Duration,
}

/// 缓存 scope key：workspace + project_dir 唯一确定一份技能索引。
fn cache_key(workspace: &Path, project_dir: Option<&Path>) -> String {
    format!(
        "{}|{}",
        workspace.display(),
        project_dir
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    )
}

/// 路径归一为正斜杠形态（origin 与目录前缀比较时统一 Windows 反斜杠差异）。
fn norm_slash(s: &str) -> String {
    s.replace('\\', "/")
}

impl Default for SkillIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillIndex {
    /// 构造默认索引（TTL 10s）。
    pub fn new() -> Self {
        SkillIndex {
            cache: Mutex::new(None),
            ttl: Duration::from_secs(10),
        }
    }

    /// 清空 TTL 缓存（下次 get/list 重建索引）；「重新加载」与删除后同步均走此路径。
    pub fn invalidate(&self) {
        *self.cache.lock().unwrap() = None;
    }

    /// 删除托管技能（仅 data_dir/skills 与 project_dir/skills；内置与 .claude/.agents 兼容来源拒绝）。
    /// 全量索引查找、不施加 disabled 过滤（已禁用的托管技能同样可删，避免与「不存在」混淆）。
    /// 双防线：索引 deletable 桶标记 + canonicalize 后前缀校验（防路径穿越/符号链接绕过）；成功后缓存失效。
    pub fn delete_skill(
        &self,
        workspace: &Path,
        data_dir: &Path,
        project_dir: Option<&Path>,
        name: &str,
    ) -> Result<(), String> {
        let skill = Self::build(workspace, data_dir, &[], project_dir)
            .remove(name)
            .ok_or_else(|| format!("技能不存在：{name}"))?;
        if !skill.meta.deletable {
            return Err(format!(
                "该技能来源不可删除（仅托管 .codewave/skills 可删）：{}",
                skill.meta.origin
            ));
        }
        let canonical = PathBuf::from(&skill.meta.origin)
            .canonicalize()
            .map_err(|e| format!("来源路径已不存在：{e}"))?;
        let in_managed = |base: &Path| {
            base.join("skills")
                .canonicalize()
                .is_ok_and(|b| canonical.starts_with(b))
        };
        let managed = in_managed(data_dir) || project_dir.is_some_and(in_managed);
        if !managed {
            return Err(format!("来源路径不在托管目录内：{}", skill.meta.origin));
        }
        // 目录形态 origin 指向 <目录>/SKILL.md：删除目标取父目录；单文件形态（xxx.md）即文件本身
        let target = if canonical.file_name().is_some_and(|n| n == "SKILL.md") {
            canonical
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| canonical.clone())
        } else {
            canonical.clone()
        };
        let res = if target.is_dir() {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_file(&target)
        };
        res.map_err(|e| format!("删除失败：{e}"))?;
        self.invalidate();
        Ok(())
    }

    /// 扫描并构建索引（同名时高优先级目录覆盖低优先级）。
    /// 加载路径（低→高）：内置 < ~/.claude/skills 与 ~/.agents/skills（用户级兼容）
    /// < 工作区 .claude/skills < 工作区 .agents/skills
    /// < data_dir/skills（全局 .codewave/skills，用户级）
    /// < project_dir/skills（项目 `<主目录>/.codewave/skills`，托管最高）。
    pub fn build(
        workspace: &Path,
        data_dir: &Path,
        disabled: &[String],
        project_dir: Option<&Path>,
    ) -> HashMap<String, Skill> {
        Self::build_with_home(
            dirs::home_dir().as_deref(),
            workspace,
            data_dir,
            disabled,
            project_dir,
        )
    }

    /// build 的可注入形态（home = None 表示无用户主目录）；测试借此验证用户级目录扫描与优先级。
    pub fn build_with_home(
        home: Option<&Path>,
        workspace: &Path,
        data_dir: &Path,
        disabled: &[String],
        project_dir: Option<&Path>,
    ) -> HashMap<String, Skill> {
        let mut map: HashMap<String, Skill> = HashMap::new();
        // 按低 → 高优先级插入（后者覆盖）
        for (name, body) in builtin_skills() {
            if let Some(mut s) = parse_skill_md(body, name) {
                s.meta.origin = "<builtin>".into();
                map.insert(s.meta.name.clone(), s);
            }
        }
        if let Some(h) = home {
            // 用户级兼容：~/.claude/skills < ~/.agents/skills（延续 .agents > .claude 相对顺序）
            scan_dir_into(&h.join(".claude").join("skills"), &mut map);
            scan_dir_into(&h.join(".agents").join("skills"), &mut map);
        }
        scan_dir_into(&workspace.join(".claude").join("skills"), &mut map);
        scan_dir_into(&workspace.join(".agents").join("skills"), &mut map);
        scan_dir_into(&data_dir.join("skills"), &mut map);
        if let Some(pd) = project_dir {
            scan_dir_into(&pd.join("skills"), &mut map);
        }
        for d in disabled {
            map.remove(d);
        }
        // deletable 桶标记：仅托管目录（全局/项目 .codewave/skills）可删；
        // 与删除第二道校验同源（<dir>/skills 前缀），尾部分隔符防字符串前缀混淆（C:/a/b 误匹配 C:/a/bc）
        let mut managed: Vec<String> = vec![format!(
            "{}/",
            norm_slash(&data_dir.join("skills").display().to_string())
        )];
        if let Some(pd) = project_dir {
            managed.push(format!(
                "{}/",
                norm_slash(&pd.join("skills").display().to_string())
            ));
        }
        for s in map.values_mut() {
            let o = norm_slash(&s.meta.origin);
            s.meta.deletable = managed.iter().any(|p| o.starts_with(p.as_str()));
        }
        map
    }

    /// 按名取单个技能（命中 TTL 缓存则免扫描；未命中重建并回填缓存）。
    pub fn get(
        &self,
        workspace: &Path,
        data_dir: &Path,
        disabled: &[String],
        project_dir: Option<&Path>,
        name: &str,
    ) -> Option<Skill> {
        let key = cache_key(workspace, project_dir);
        {
            let g = self.cache.lock().unwrap();
            if let Some((at, cached_key, map)) = g.as_ref() {
                if *cached_key == key && at.elapsed() < self.ttl {
                    return map.get(name).cloned();
                }
            }
        }
        let map = Self::build(workspace, data_dir, disabled, project_dir);
        let v = map.get(name).cloned();
        *self.cache.lock().unwrap() = Some((Instant::now(), key, map));
        v
    }

    /// 列出全部技能元数据（显示排序：内置 > 项目 .codewave/skills > 全局 ~/.codewave/skills > 其他，
    /// 同桶按名排序；命中 TTL 缓存则免扫描）。
    pub fn list(
        &self,
        workspace: &Path,
        data_dir: &Path,
        disabled: &[String],
        project_dir: Option<&Path>,
    ) -> Vec<SkillMeta> {
        let key = cache_key(workspace, project_dir);
        // 路径统一正斜杠后做前缀归类（Windows 反斜杠与 origin 的 display 形态一致化）
        let norm = norm_slash;
        let proj_prefix = project_dir.map(|p| norm(&p.display().to_string()));
        let data_prefix = norm(&data_dir.display().to_string());
        let bucket = |origin: &str| -> u8 {
            let o = norm(origin);
            if o == "<builtin>" {
                0
            } else if proj_prefix.as_deref().is_some_and(|p| o.starts_with(p)) {
                1
            } else if o.starts_with(&data_prefix) {
                2
            } else {
                3
            }
        };
        let sorted = |mut v: Vec<SkillMeta>| -> Vec<SkillMeta> {
            v.sort_by(|a, b| (bucket(&a.origin), &a.name).cmp(&(bucket(&b.origin), &b.name)));
            v
        };
        {
            let g = self.cache.lock().unwrap();
            if let Some((at, cached_key, map)) = g.as_ref() {
                if *cached_key == key && at.elapsed() < self.ttl {
                    return sorted(map.values().map(|s| s.meta.clone()).collect());
                }
            }
        }
        let map = Self::build(workspace, data_dir, disabled, project_dir);
        let v = sorted(map.values().map(|s| s.meta.clone()).collect());
        *self.cache.lock().unwrap() = Some((Instant::now(), key, map));
        v
    }
}

/// 扫描一个技能目录并入索引：目录形态 `<name>/SKILL.md`，单文件形态 `<name>.md` 兼容。
fn scan_dir_into(base: &PathBuf, map: &mut HashMap<String, Skill>) {
    let Ok(rd) = std::fs::read_dir(base) else {
        return;
    };
    for entry in rd.flatten() {
        let dir = entry.path().join("SKILL.md");
        if !dir.is_file() {
            // 单文件技能：name.md
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("md") && p.is_file() {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                    if let Some(mut s) = parse_skill_md(&text, stem) {
                        s.meta.origin = p.display().to_string();
                        map.insert(s.meta.name.clone(), s);
                    }
                }
            }
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&dir) {
            let stem = entry.file_name().to_string_lossy().into_owned();
            if let Some(mut s) = parse_skill_md(&text, &stem) {
                s.meta.origin = dir.display().to_string();
                map.insert(s.meta.name.clone(), s);
            }
        }
    }
}

/// 系统提示词注入：仅三字段索引（不注入正文全文）。
pub fn prompt_listing(metas: &[SkillMeta]) -> String {
    if metas.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            let mut l = format!("- {}: {}", m.name, m.description);
            if !m.when_to_use.is_empty() {
                l.push_str(&format!("（when: {}）", m.when_to_use));
            }
            l
        })
        .collect();
    // 首行说明 /<name> 点名语义（composer 技能触发符为 /）：被点名时先加载技能正文再执行
    format!(
        "<available-skills>\n（用户消息以 /<name> 开头 = 点名该技能：先经 skill 工具加载对应技能，再执行其余内容）\n{}\n</available-skills>",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parsing() {
        let md = "---\nname: my-skill\ndescription: does things\nwhenToUse: when needed\n---\n# Body here\nStep 1.";
        let s = parse_skill_md(md, "fallback").unwrap();
        assert_eq!(s.meta.name, "my-skill");
        assert_eq!(s.meta.description, "does things");
        assert_eq!(s.meta.when_to_use, "when needed");
        assert!(s.body.contains("Step 1."));

        // snake_case 兼容
        let md2 = "---\nname: s2\ndescription: d\nwhen_to_use: w\n---\nbody";
        let s2 = parse_skill_md(md2, "f").unwrap();
        assert_eq!(s2.meta.when_to_use, "w");

        // 无 frontmatter
        let s3 = parse_skill_md("# Just body", "fb").unwrap();
        assert_eq!(s3.meta.name, "fb");
    }

    #[test]
    fn builtin_skills_parse() {
        let names: Vec<&str> = builtin_skills().iter().map(|(n, _)| *n).collect();
        assert!(
            !names.contains(&"arch"),
            "arch 已内置为常驻标准工作流，不得回填为技能"
        );
        for (name, body) in builtin_skills() {
            let s = parse_skill_md(body, name).unwrap();
            assert_eq!(s.meta.name, name);
            assert!(!s.meta.description.is_empty());
        }
    }

    #[test]
    fn scan_paths_and_priority() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let pd = tempfile::tempdir().unwrap(); // 项目数据目录 <主目录>/.codewave
        // 工作区 .claude/skills（claude 兼容）
        let c = ws.path().join(".claude/skills/common");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(
            c.join("SKILL.md"),
            "---\nname: common\ndescription: claude ver\n---\nclaude",
        )
        .unwrap();
        // 工作区 .agents/skills（agents 约定）
        let a = ws.path().join(".agents/skills/agent-only");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(
            a.join("SKILL.md"),
            "---\nname: agent-only\ndescription: agents ver\n---\nagents",
        )
        .unwrap();
        // 全局 .codewave/skills（data_dir，用户级）
        let w = dd.path().join("skills/common");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(
            w.join("SKILL.md"),
            "---\nname: common\ndescription: global ver\n---\nglobal",
        )
        .unwrap();
        // 项目 .codewave/skills（project_dir）同名覆盖 compat 与全局
        let p = pd.path().join("skills/common");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("SKILL.md"),
            "---\nname: common\ndescription: project ver\n---\nproject",
        )
        .unwrap();
        // 已移除的旧路径：工作区 .wavestudio/skills（改名前旧托管名，仍不得扫描）与裸 skills/ 不再扫描
        let legacy = ws.path().join(".wavestudio/skills/legacy-ws");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("SKILL.md"), "# legacy-ws").unwrap();
        let bare = ws.path().join("skills/legacy-bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(bare.join("SKILL.md"), "# legacy-bare").unwrap();

        let map = SkillIndex::build(ws.path(), dd.path(), &[], Some(pd.path()));
        assert_eq!(
            map.get("common").unwrap().meta.description,
            "project ver",
            "项目 .codewave/skills 优先级最高"
        );
        assert_eq!(
            map.get("agent-only").unwrap().meta.description,
            "agents ver",
            ".agents/skills 应被扫描"
        );
        assert!(
            !map.contains_key("legacy-ws"),
            "workspace/.wavestudio/skills 旧托管名已移除"
        );
        assert!(!map.contains_key("legacy-bare"), "裸 skills/ 已移除");
        assert!(map.contains_key("repo-index")); // 内置
        // disabled 过滤
        let map2 = SkillIndex::build(
            ws.path(),
            dd.path(),
            &["repo-index".into()],
            Some(pd.path()),
        );
        assert!(!map2.contains_key("repo-index"));
        // 无项目（临时会话）：project_dir None 时全局 data_dir/skills 兜底
        let map3 = SkillIndex::build(ws.path(), dd.path(), &[], None);
        assert_eq!(map3.get("common").unwrap().meta.description, "global ver");
    }

    #[test]
    fn list_orders_by_origin_bucket() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let pd = tempfile::tempdir().unwrap();
        let install = |base: &Path, rel: &str, name: &str| {
            let dir = base.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\nbody"),
            )
            .unwrap();
        };
        // 三桶 + 同桶两只（gamma/zeta 验证桶内按名排序）
        install(pd.path(), "skills/alpha", "alpha"); // 项目 .codewave/skills
        install(dd.path(), "skills/beta", "beta"); // 全局 ~/.codewave/skills
        install(ws.path(), ".claude/skills/zeta", "zeta"); // compat
        install(ws.path(), ".agents/skills/gamma", "gamma"); // compat
        let idx = SkillIndex::default();
        let metas = idx.list(ws.path(), dd.path(), &[], Some(pd.path()));
        // 只取本测试可控的技能（开发机 ~/.claude/skills 可能存在，属桶 3 不可控）
        let controlled = [
            "doc-convert",
            "repo-index",
            "alpha",
            "beta",
            "gamma",
            "zeta",
        ];
        let names: Vec<&str> = metas
            .iter()
            .map(|m| m.name.as_str())
            .filter(|n| controlled.contains(n))
            .collect();
        assert_eq!(
            names,
            [
                "doc-convert",
                "repo-index",
                "alpha",
                "beta",
                "gamma",
                "zeta"
            ],
            "显示序：内置 > 项目 .codewave/skills > 全局 > 其他，桶内按名"
        );
    }

    #[test]
    fn user_level_home_dirs_priority() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let install = |base: &Path, rel: &str, name: &str, desc: &str| {
            let dir = base.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {desc}\n---\nbody"),
            )
            .unwrap();
        };
        // 用户级：~/.claude/skills < ~/.agents/skills（同名后者覆盖）
        install(home.path(), ".claude/skills/dup", "dup", "home-claude");
        install(home.path(), ".agents/skills/dup", "dup", "home-agents");
        let map = SkillIndex::build_with_home(Some(home.path()), ws.path(), dd.path(), &[], None);
        assert_eq!(
            map.get("dup").unwrap().meta.description,
            "home-agents",
            "用户级 ~/.agents/skills 应覆盖 ~/.claude/skills"
        );
        // 工作区级覆盖用户级
        install(ws.path(), ".claude/skills/dup", "dup", "ws-claude");
        let map2 = SkillIndex::build_with_home(Some(home.path()), ws.path(), dd.path(), &[], None);
        assert_eq!(map2.get("dup").unwrap().meta.description, "ws-claude");
        // 单文件形态（~/.agents/skills/single.md）兼容
        std::fs::create_dir_all(home.path().join(".agents/skills")).unwrap();
        std::fs::write(
            home.path().join(".agents/skills/single.md"),
            "---\nname: single\ndescription: single-file\n---\nbody",
        )
        .unwrap();
        let map3 = SkillIndex::build_with_home(Some(home.path()), ws.path(), dd.path(), &[], None);
        assert_eq!(map3.get("single").unwrap().meta.description, "single-file");
    }

    #[test]
    fn deletable_flag_and_delete_skill() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let pd = tempfile::tempdir().unwrap();
        let install = |base: &Path, rel: &str, name: &str| {
            let dir = base.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\nbody"),
            )
            .unwrap();
        };
        install(dd.path(), "skills/global-skill", "global-skill");
        install(pd.path(), "skills/project-skill", "project-skill");
        install(ws.path(), ".claude/skills/compat-a", "compat-a");
        install(ws.path(), ".agents/skills/compat-b", "compat-b");
        let idx = SkillIndex::default();
        let metas = idx.list(ws.path(), dd.path(), &[], Some(pd.path()));
        let del = |n: &str| metas.iter().find(|m| m.name == n).unwrap().deletable;
        assert!(
            del("global-skill") && del("project-skill"),
            "托管目录技能可删"
        );
        assert!(
            !del("compat-a") && !del("compat-b"),
            "工作区 compat 目录不可删"
        );
        assert!(
            !metas
                .iter()
                .find(|m| m.name == "doc-convert")
                .unwrap()
                .deletable,
            "内置不可删"
        );
        // 删除成功：目录形态整目录移除 + 缓存自动失效（下次 list 立即不可见）
        idx.delete_skill(ws.path(), dd.path(), Some(pd.path()), "global-skill")
            .unwrap();
        assert!(!dd.path().join("skills/global-skill").exists());
        let metas2 = idx.list(ws.path(), dd.path(), &[], Some(pd.path()));
        assert!(!metas2.iter().any(|m| m.name == "global-skill"));
        // 拒绝：不可删来源 / 不存在
        assert!(
            idx.delete_skill(ws.path(), dd.path(), Some(pd.path()), "compat-a")
                .is_err()
        );
        assert!(
            idx.delete_skill(ws.path(), dd.path(), Some(pd.path()), "nope")
                .is_err()
        );
        // 前缀混淆负例：临时会话（workspace == data_dir）下，工作区 .agents compat 技能
        // origin 位于 data_dir 之下但不在 data_dir/skills 内 → 不可删（旧实现按 data_dir 整体前缀会误标）
        install(dd.path(), ".agents/skills/ddcompat", "ddcompat");
        let idx2 = SkillIndex::default();
        assert!(
            !idx2
                .list(dd.path(), dd.path(), &[], None)
                .iter()
                .find(|m| m.name == "ddcompat")
                .unwrap()
                .deletable
        );
        // 已禁用的托管技能同样可删（全量索引查找，不与「不存在」混淆）
        install(pd.path(), "skills/dis", "dis");
        idx2.delete_skill(ws.path(), dd.path(), Some(pd.path()), "dis")
            .unwrap();
        assert!(!pd.path().join("skills/dis").exists());
        // 项目托管技能可删
        idx.delete_skill(ws.path(), dd.path(), Some(pd.path()), "project-skill")
            .unwrap();
        assert!(!pd.path().join("skills/project-skill").exists());
    }

    #[test]
    fn invalidate_forces_rescan() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        // 长 TTL：测试期内缓存绝不自然过期，重载可见性只由 invalidate 决定
        let idx = SkillIndex {
            cache: Mutex::new(None),
            ttl: Duration::from_secs(3600),
        };
        assert!(
            !idx.list(ws.path(), dd.path(), &[], None)
                .iter()
                .any(|m| m.name == "later")
        );
        let dir = dd.path().join("skills/later");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: later\ndescription: new\n---\nbody",
        )
        .unwrap();
        // TTL 内缓存命中：新技能不可见
        assert!(
            !idx.list(ws.path(), dd.path(), &[], None)
                .iter()
                .any(|m| m.name == "later")
        );
        idx.invalidate();
        assert!(
            idx.list(ws.path(), dd.path(), &[], None)
                .iter()
                .any(|m| m.name == "later")
        );
    }
}
