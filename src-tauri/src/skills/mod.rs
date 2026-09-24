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
- xlsx/docx/pdf → 文本：用 `read_document`（专门读 Office 与 PDF，比本 skill
  的通用转换路径可靠：表格会给结构摘要，文档会保留标题层级与表格）。
- 表格→ csv：先 `read_document` 取数再写成 csv。用其它工具直接转
  会丢掉图表与数据透视表。

约定：输出文件写到 `tmp/` 或用户指定路径；转换后 `read` 抽样验证首尾行。
"#,
        ),
        (
            "preview",
            r#"---
name: preview
description: Render a visual HTML preview of the current plan (or the object under discussion) as a sandboxed widget.
whenToUse: Use when the user asks to preview something (预览一下/看下效果/看看界面/mock 一下), or when the plan approval question offers「先看预览」and the user picks it.
---
# preview 工作流

目标：把「要做的方案」变成一眼能看的东西（方案概览 / 界面示意），供用户在批准前理解与决策。
产出统一经 `render_html`（自包含 HTML 小组件，1–50000 字符），前端以**大弹框**呈现（90vw × 85vh，
弹框内可复制源码 / 重新加载 / 切浅深底色）——不要在聊天里贴 HTML 源码，也不要写文件。

## 一、什么时候渲染
渲染（两种入口）：
1. 用户明确要求预览（预览一下 / 看下效果 / 看看界面 / mock 一下），且上下文里已有**成型方案**或**明确讨论对象**。
2. 批准门里用户选了「先看预览」：先本技能，再渲染，然后**重新发起同一批准询问**。

不渲染（先澄清，别硬渲）：
- 没有可预览对象（纯问答、闲聊、方案还没讨论出来）→ 先问清「要预览哪一部分」。
- 方案还在讨论中且用户没要求 → 不要自作主张打断讨论。

## 二、形态选择
- 默认 **A 方案概览**：目标 / 范围（文件级改动点）/ 关键流程或数据流 / 风险与回滚 / 验证方式。
- 需求以**界面与交互**为主体时升 **B 界面示意**：画出关键界面的布局与状态（HTML/CSS 手绘即可）。
- 两者都要时同卡分区（上 A 下 B）。B 形态**必须**在显著位置标注「示意稿 · 未实现」，避免被当成已完成的界面。

## 三、渲染纪律
- 自包含：只写内联 CSS 与内联 `<script>`；**不得**引外链 CSS/JS/字体、不得 fetch（沙箱无网络、无同源权限）。
- 单次 ≤50000 字符：超限就精简（去掉次要区块）或拆成多张卡片（多次调用，每次 ≤50000），
  **不得**因为超限就放弃预览，也不要只回一句「内容太长」。
- 面向用户可读：中文叙述；表格与流程用排版表达（可用内联 SVG 或纯 CSS 画，不要引 mermaid CDN）。
- 只呈现方案：不替代方案文本、不改方案内容、不写业务代码、不落盘。

## 四、产出之后
- 用一句话说明这张预览是什么，然后回到原任务：
  - 处于批准门：**重新发起同一批准询问**（题干加一句「预览已更新，是否按计划执行？」）——
    看预览不等于批准，绝不静默批准、绝不自行切档。
  - 只是讨论中：问一句是否需要按此继续。
- 同一方案连续 3 次预览后，重发询问时去掉预览项并说明原因（防来回空转）。
- `render_html` 报错（如 E_TOO_LARGE）或不可用时：如实说明，并用聊天内 markdown 概览降级呈现；
  **批准门照常存在**，不得因预览失败跳过批准。
"#,
        ),
        (
            "xlsx",
            r#"---
name: xlsx
description: Read, create, or edit Excel spreadsheets (.xlsx/.xlsm) with formulas, formatting, and full fidelity.
whenToUse: Use whenever a spreadsheet file is the primary input or output - reading data out of one, creating a new one, or changing cells in an existing one.
---
# 表格处理（.xlsx / .xlsm）

工具：`read_document`（读）、`write_document`（生成）、`edit_document`（改）。
改已有文件请一律用 `edit_document`，绝不要改成「读出来再整份写回」——那会丢掉图表、数据透视表、迷你图。

## 任务路由

| 要做的事 | 用什么 | 关键点 |
|---|---|---|
| 看表格里有什么 | `read_document`（不传 sheet） | 返回工作表名、行列数、表头与前几行 |
| 要具体数据 | `read_document`（传 sheet + range） | 单次 200 行 / 64 列 / 20000 格 |
| 改几个格子的值或公式 | `edit_document` | 只改指定单元格，其余一律原样 |
| 新建一份表格 | `write_document` | 派生值要写成公式 |

## 读：先看结构，再取数据

不要一上来就整表读。先不带 `sheet` 调一次拿到结构摘要，看清有几张表、表头是什么，
再按区域取数。表大时这一步能省掉大量上下文。

取数时 `range` 给得越准越好。拿到 `truncated: true` 说明还有剩余，用 `range` 接着取。

## 改：只改该改的格子

四种值四选一：`text` / `number` / `formula` / `bool`。

- 改公式后，能算的会把算好的值一并写上，不看公式的读取方也能立刻拿到数字。
  能算的只有一小块：纯数值四则运算（`+ - * /`、括号）与 `SUM` / `AVERAGE` / `COUNT` /
  `MIN` / `MAX`（参数是单元格或单元格区域）。其余（VLOOKUP、IF、数组公式……）一律不编数字，
  只写公式并让 Excel 打开时重算——算错一个数比不给数字糟糕得多，写错的数没有任何迹象。
- 算不出来的公式在回执里会写明「（打开时重算）」，能算的写明「（已算出 N）」。
- 单元格原本不存在时会写进它所属的行；整行为空时新建一个空行。新建空行不挪动
  任何已有内容，是安全的。
- **增删行列不在支持范围内**：往中间插一行会连带影响合并区域、数据验证范围、
  图表引用与所有相对引用，这件事我们不做。

## 生成：派生值必须写成公式

两条硬规则：

1. **派生值写成真公式**，不要写算好的数字。用户改了输入，汇总要跟着变——
   这是「活的模型」和「死的表」的区别。
2. **同时用 `cached` 给出算好的值**。不带这个值，不看公式的工具会读到一片空白。

## 已知坑（都踩过）

| 现象 | 原因 | 怎么办 |
|---|---|---|
| 公式格子读出来是空的 | 这个公式我们算不出来，只写了公式等 Excel 重算 | 正常。Excel 打开会重算；要立刻有值就写数值而不是公式 |
| 报「找不到名为 X 的工作表」 | 工作表名写错了 | 错误信息里会列出这份文件真实的工作表名，照抄 |
| 交叉表引用算不出来 | 工作表名里有空格时没加引号 | 写公式时给名字加单引号 |
| 含宏的表格改了之后宏没了 | 误用了「读出来整份写回」 | 用 `edit_document`：宏本身不会被编辑，但会原样保留 |
| 往非左上角的合并格子写值失败 | 合并区域只有左上角承载值 | 写左上角那个格子 |

## 失败案例索引（先匹配再动手）

| 任务特征 | 过去怎么错的 | 推荐做法 |
|---|---|---|
| 「把汇总值填上」 | 算好写成死数字 | 写成 SUM 之类的公式，并用 `cached` 给出算好的值 |
| 「csv 打开是乱码」 | 当成文件损坏 | 用 `read` 读（返回值里的 `encoding` 会说明实际用了什么编码；中文环境导出的表格常见 GBK，会被自动认出） |
| 「加一列占比」 | 直接改整表结构 | 不支持增删行列；改用 `write_document` 生成新表，或只改已有格子 |
| 「把这份表里的数改一下」 | 读出来改完整份写回，图表没了 | 用 `edit_document` 指定单元格 |
| 「这批数据做成表格」 | 只写数据不写表头、列宽挤在一起 | `header` 写表头，`columnWidths` 给关键列宽度 |

## 不支持（不要尝试，直接告知用户）

- 旧格式 `.xls`：请用户先用 Excel 或 WPS 另存为 `.xlsx`
- 带密码的文件、编辑宏本身、生成图表、增删行列
"#,
        ),
        (
            "docx",
            r#"---
name: docx
description: Read, create, or edit Word documents (.docx) while preserving formatting, headers, images, and comments.
whenToUse: Use whenever a Word document is the primary input or output - reading content out of one, drafting a new one, or changing text in an existing one.
---
# 文档处理（.docx）

工具：`read_document`（读）、`write_document`（生成）、`edit_document`（改文字）。
改已有文档请一律用 `edit_document`，绝不要「读出来再整份写回」——那会丢页眉页脚、图片、批注、样式。

## 任务路由

| 要做的事 | 用什么 | 关键点 |
|---|---|---|
| 读文档内容 | `read_document` | 返回段落、标题层级与表格，标题以井号开头 |
| 改已有文字 | `edit_document` + `textEdits` | `find` / `replace`；命中多处会报错 |
| 新建文档 | `write_document` + `docx` | 用 `blocks` 描述标题、段落、表格 |

## 读：标题层级就是大纲

返回里井号的个数就是标题层级。`headings` 数组单独列出了大纲，可用来快速定位。
文档很长时结果会截断并说明，此时结合实际段落再定位。

## 改：一次一处，命中必须唯一

- **默认只替换唯一命中**。命中多处会报错并给出次数——这是有意的：
  改错地方比不改更糟。这时要么给更长的上下文使其唯一，要么明确要求全部替换。
- 多个替换**依次生效**：第二处看到的是第一处改完的结果。
- 任一处失败则整体不生效，不会留下「改了一半」的文档。
- `replace` 留空字符串即为删除这段文字。

## 生成：标题用 heading，不要用加粗冒充

用 `heading` 而不是「加粗的一行」：前者会成为大纲层级，读者能用导航窗格跳转，
也能自动生成目录。段落里的换行会被拆成多个段落（Word 不认换行符）。

标题层级不要跳（1 → 2 → 3），跳级会让自动生成的目录结构错乱。

## 已知坑（都踩过）

| 现象 | 原因 | 怎么办 |
|---|---|---|
| 报「找不到某段文字」 | 要找的文字被拆散在多个片段里，或根本不存在 | 先 `read_document` 确认原文（注意标点、空格、全半角差异） |
| 报「出现了 N 次，无法确定改哪一处」 | 目标不唯一 | 给更长的上下文，或明确要求全部替换 |
| 改了之后格式丢了 | 用了别的方式整段替换 | `edit_document` 只在片段之间回填文字，加粗、批注锚点、书签都保留 |
| 替换后文件里留下空的文字片段 | 命中区间跨越了片段边界 | 正常现象，Word 会忽略空的片段，不影响显示 |
| 生成的文档里标题没出现在大纲里 | 用加粗段落冒充了标题 | 改用 `heading` 类型 |
| 表格列数不齐导致错位 | 各行列数不一致 | 生成时会自动补齐空列 |
| 改完批注或修订痕迹会不会乱 | — | 不会：批注锚点、修订标记都原样保留；但**不提供编辑批注或接受修订** |

## 失败案例索引（先匹配再动手）

| 任务特征 | 过去怎么错的 | 推荐做法 |
|---|---|---|
| 「把合同里的公司名换掉」 | 整段替换，把加粗和批注一起抹了 | 用 find/replace；命中不唯一就先补上下文 |
| 「把这份文档改成我们的格式」 | 读出来重新生成，页眉页脚和图片全没了 | 用 `edit_document` 改文字；换样式请用户手工做或用模板 |
| 「写一份报告」 | 用若干加粗段落假冒标题 | 用 `heading`，层级别跳 |
| 「文档里某句话读不到」 | 直接说文档里没有 | 先 `read_document` 看返回的完整正文，再判断 |
| 「改批注内容」 | 尝试改批注 | 不支持，明确告知用户 |

## 不支持（不要尝试，直接告知用户）

- 旧格式 `.doc`：请用户先另存为 `.docx`
- 编辑批注与修订痕迹、接受或拒绝修订、改页眉页脚文字、增删段落表格
"#,
        ),
        (
            "pdf",
            r#"---
name: pdf
description: Read text content out of PDF files, page by page, with clear handling of scanned documents.
whenToUse: Use when the user wants information out of a PDF (summarize it, answer questions about it, extract figures) or asks why a PDF cannot be read.
---
# PDF 处理

工具：`read_document`（传 `pages` 指定页码）。

**只支持读取**：不支持修改正文、不支持生成 PDF、不支持从图片里认字。

## 怎么读

不带 `pages` 时默认读前若干页；用 `pages` 指定范围，写法支持 `3`、`1-10`、`5-`。
返回按页分段，便于引用。

**页数规则**：单次最多 20 页；超过 10 页的文档**必须**显式指定页码——
一次拉太多会灌爆上下文，也会拖慢响应。

## 扫描件：读不出文字不是出错

如果整份文档的平均每页文字量极低，会被判为**扫描件**：页面其实是图片，没有文字层。
返回值里 `scanned` 为真、`text` 为空，并附一句说明。

**这时不要反复重试**，也不要猜内容。如实告诉用户：这份文档是扫描件，
需要先用带文字识别（OCR）的工具转一遍才能读到文字。本应用不做文字识别。

## 中文提取效果不保证

PDF 里的文字是「用哪个字形画在哪里」，而不是「这段文字是什么」。有些文件（尤其是
中文）没有内嵌字形对照表，提取出来会是乱码或缺失。**这时如实告知用户**，
不要拿乱码当原文去分析。

## 已知坑

| 现象 | 原因 | 怎么办 |
|---|---|---|
| `scanned` 为真、没有文字 | 扫描件 | 告知用户需要先做文字识别 |
| 中文乱码或缺字 | 文件没有内嵌字形对照表 | 如实告知，不要硬猜 |
| 报「超过 10 页时必须指定页码」 | 没给 `pages` | 给出 `pages`，或先只读前几页 |
| 某页没有文字 | 该页只有图或本来就是空白页 | 正常，继续看别的页 |
| 报「解析时遇到无法处理的内容」 | 文件损坏或用了不支持的格式 | 如实告知用户；这类文件有时用别的阅读器能打开 |

## 失败案例索引（先匹配再动手）

| 任务特征 | 过去怎么错的 | 推荐做法 |
|---|---|---|
| 「总结这份 PDF」 | 不指定页码一次全读 | 页面多时先读前几页摸清结构，再有针对性地读 |
| 「PDF 里这个数字是多少」 | 拿到扫描件后编数字 | 先看 `scanned`，是扫描件就如实说明 |
| 「把 PDF 改成 Word」 | 尝试转换 | 不支持；只能读内容，再由 `write_document` 生成新文档 |
| 「把这份 PDF 里的文字改掉」 | 尝试修改 | 不支持，明确告知 |
| 「PDF 读不了」 | 直接说文件有问题 | 区分三种情况：扫描件、加密、文件损坏，给出的说明要让用户知道下一步做什么 |

## 不支持（不要尝试，直接告知用户）

- 修改 PDF 正文、生成 PDF、合并拆分页面、加水印、填表单
- 从扫描件里识别文字（文字识别）
- 带密码的 PDF
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
            // whenToUse 进技能列表的「when: …」——preview 技能的两处触发场景全靠它，
            // 缺失等于「模型不知道何时该加载这个技能」，故一并钉住
            assert!(!s.meta.when_to_use.is_empty(), "{name} 缺 whenToUse");
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
