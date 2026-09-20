//! Word 文档（`.docx`）的解析与文本替换。
//!
//! ## 为什么读取自己解包，而不用现成的库
//!
//! `.docx` 就是一个压缩包里的若干 XML。读取需要的只是「正文段落 + 表格 + 标题层级」，
//! 自己解包的覆盖面反而更可控；写入侧才用现成的库（生成复杂结构它更省事）。
//!
//! ## 替换文字为什么难
//!
//! Word 会把一段连续的文字**拆成多个片段**（`<w:r>`）。同一句话里只要有一个词加粗、
//! 或者夹了一处拼写检查标记、或者有批注锚点，就会变成好几个片段。直接「整段替换」
//! 会把这些格式和标记一起抹掉。
//!
//! 所以替换必须在**片段之间**做：先拼出整段的文字找到目标位置，再回填到若干片段里。
//! 实现上只改动 `<w:t>` 里的文字，**不新增也不删除任何其它节点**——拼写检查标记、
//! 书签、批注锚点全部原样留着。

use super::xml_util::{escape_text, extract_tag, find_tag_starts, start_tag_end, unescape};

/// 正文所在的内部文件。
pub const DOCUMENT_ENTRY: &str = "word/document.xml";

/// 正文里的一个块。
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// 段落：文字 + 标题层级（不是标题时为 None）。
    Paragraph { text: String, heading: Option<u32> },
    /// 表格：若干行，每行若干单元格的文字。
    Table { rows: Vec<Vec<String>> },
}

/// 从 `<w:p>` 的属性区里读出标题层级（`Heading1` / `标题 1` 都认）。
fn heading_level(p_pr: &str) -> Option<u32> {
    let mut level = None;
    find_tag_starts(p_pr, 0, p_pr.len(), "w:pStyle", |at| {
        if let Some((gt, _)) = start_tag_end(p_pr, at) {
            let attrs = super::xml_util::split_attrs(&p_pr[at + "w:pStyle".len()..at + gt]);
            if let Some(v) = super::xml_util::attr_of(&attrs, "w:val") {
                // 英文 Heading1 / 中文「标题 1」/ 样式 id 都可能出现
                let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
                if v.to_ascii_lowercase().contains("heading") || v.contains("标题") {
                    level = digits.parse::<u32>().ok().or(Some(1));
                }
            }
        }
        false
    });
    level
}

/// 拼出一个段落里的全部文字（按片段顺序），并返回各片段的文字区间。
fn paragraph_text(para: &str) -> String {
    let mut out = String::new();
    find_tag_starts(para, 0, para.len(), "w:t", |at| {
        if let Some((gt, self_closing)) = start_tag_end(para, at)
            && !self_closing
            && let Some(end) = para[at + gt..].find("</w:t>").map(|e| at + gt + e)
        {
            out.push_str(&unescape(&para[at + gt + 1..end]));
        }
        false
    });
    out
}

/// 解析正文 XML 的顶层块序列。
pub fn parse_blocks(document_xml: &str) -> Vec<Block> {
    // 正文在 <w:body> 里；没有 body 时退化为整体扫描
    let (from, to) = match (document_xml.find("<w:body"), document_xml.find("</w:body>")) {
        (Some(s), Some(e)) => {
            let start = document_xml[s..].find('>').map(|p| s + p + 1).unwrap_or(s);
            (start, e)
        }
        _ => (0, document_xml.len()),
    };

    let mut blocks = Vec::new();
    let mut cursor = from;
    while cursor < to {
        let next_p = document_xml[cursor..to].find("<w:p").map(|p| cursor + p);
        let next_tbl = document_xml[cursor..to].find("<w:tbl").map(|p| cursor + p);
        let (kind, at) = match (next_p, next_tbl) {
            (Some(p), Some(t)) if p < t => ("p", p),
            (Some(p), None) => ("p", p),
            (None, Some(t)) => ("tbl", t),
            (Some(_), Some(t)) => ("tbl", t),
            (None, None) => break,
        };
        // 确认确实是 <w:p 或 <w:tbl（不是 <w:pPr / <w:tblPr 之类）
        let name_len = if kind == "p" { 4 } else { 6 };
        let next_char = document_xml[at + name_len..].chars().next();
        if !matches!(next_char, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
            cursor = at + name_len;
            continue;
        }
        let Some((gt, self_closing)) = start_tag_end(document_xml, at) else {
            break;
        };
        let body_end = if self_closing {
            at + gt + 1
        } else {
            let close = if kind == "p" { "</w:p>" } else { "</w:tbl>" };
            match document_xml[at + gt..to].find(close) {
                Some(e) => at + gt + e + close.len(),
                None => break,
            }
        };
        let element = &document_xml[at..body_end];
        if kind == "p" {
            blocks.push(Block::Paragraph {
                text: paragraph_text(element),
                heading: heading_level(element),
            });
        } else {
            blocks.push(Block::Table {
                rows: parse_table(element),
            });
        }
        cursor = body_end;
    }
    blocks
}

/// 解析表格：行 → 单元格文字。
fn parse_table(tbl: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    find_tag_starts(tbl, 0, tbl.len(), "w:tr", |at| {
        let self_closing = start_tag_end(tbl, at).map(|(_, sc)| sc).unwrap_or(false);
        if !self_closing
            && let Some(gt) = tbl[at..].find('>')
            && let Some(end) = tbl[at + gt..].find("</w:tr>").map(|e| at + gt + e)
        {
            let row = &tbl[at..end];
            let mut cells = Vec::new();
            find_tag_starts(row, 0, row.len(), "w:tc", |c_at| {
                let c_sc = start_tag_end(row, c_at).map(|(_, sc)| sc).unwrap_or(false);
                if !c_sc
                    && let Some(c_gt) = row[c_at..].find('>')
                    && let Some(c_end) = row[c_at + c_gt..].find("</w:tc>").map(|e| c_at + c_gt + e)
                {
                    cells.push(paragraph_text(&row[c_at..c_end]));
                }
                false
            });
            rows.push(cells);
        }
        false
    });
    rows
}

/// 把块序列渲染成给模型读的文本（带标题标记与表格）。
pub fn render_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Paragraph { text, heading } => {
                if text.trim().is_empty() {
                    continue;
                }
                match heading {
                    Some(lv) => out.push_str(&format!("{} {text}\n", "#".repeat(*lv as usize))),
                    None => {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            Block::Table { rows } => {
                if rows.is_empty() {
                    continue;
                }
                for (i, row) in rows.iter().enumerate() {
                    out.push_str(&row.join(" | "));
                    out.push('\n');
                    // 表头下面画一条分隔线，保持与 markdown 表格一致的观感
                    if i == 0 {
                        out.push_str(&vec!["---"; row.len()].join(" | "));
                        out.push('\n');
                    }
                }
            }
        }
    }
    out.trim_end().to_string()
}

/// 文字片段在文档里的位置。
struct RunText {
    /// `<w:t>` 开标签的 `>` 的位置
    tag_gt: usize,
    /// 文字内容的起止（XML 里的原始字节区间，可能含实体转义）
    text_start: usize,
    text_end: usize,
}

/// 收集一个段落里所有 `<w:t>` 的文字区间（按出现顺序）。
fn collect_runs(para: &str) -> Vec<RunText> {
    let mut runs = Vec::new();
    find_tag_starts(para, 0, para.len(), "w:t", |at| {
        if let Some((gt, self_closing)) = start_tag_end(para, at)
            && !self_closing
            && let Some(end) = para[at + gt..].find("</w:t>").map(|e| at + gt + e)
        {
            runs.push(RunText {
                tag_gt: at + gt,
                text_start: at + gt + 1,
                text_end: end,
            });
        }
        false
    });
    runs
}

/// 在段落里做一次跨片段替换。
///
/// 返回替换后的段落文本；找不到目标（或目标跨段落的边界不可用）时返回 None。
/// **只改 `<w:t>` 的文字，不增删任何节点**——拼写检查标记、书签、批注锚点全部保留。
pub fn replace_in_paragraph(para: &str, needle: &str, replacement: &str) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let runs = collect_runs(para);
    if runs.is_empty() {
        return None;
    }
    // 用文件里的书写形态匹配（写入侧会转义，所以先把要找的文字也转义一次）
    let escaped = escape_text(needle);
    let mut concat = String::new();
    for r in &runs {
        concat.push_str(&para[r.text_start..r.text_end]);
    }
    let hit = concat.find(&escaped).or_else(|| concat.find(needle))?;
    let hit_end = hit
        + if concat[hit..].starts_with(&escaped) {
            escaped.len()
        } else {
            needle.len()
        };

    // 定位命中的片段区间，以及在各片段内的偏移
    let mut spans = Vec::new();
    let mut offset = 0usize;
    for (i, r) in runs.iter().enumerate() {
        let len = r.text_end - r.text_start;
        spans.push((i, offset, offset + len));
        offset += len;
    }
    let touched: Vec<(usize, usize, usize)> = spans
        .iter()
        .filter(|(_, s, e)| *e > hit && *s < hit_end)
        .map(|(i, s, e)| {
            let local_start = hit.saturating_sub(*s);
            let local_end = (*e).min(hit_end) - *s;
            (*i, local_start, local_end)
        })
        .collect();
    if touched.is_empty() {
        return None;
    }

    // 从后往前改，避免前面的改动影响后面的偏移
    let mut out = para.to_string();
    for (order, (idx, local_start, local_end)) in touched.iter().enumerate().rev() {
        let r = &runs[*idx];
        let abs_start = r.text_start + local_start;
        let abs_end = r.text_start + local_end;
        // 命中的第一段放替换文字，其余片段只把命中的部分删掉
        let new_text = if order == 0 {
            escape_text(replacement)
        } else {
            String::new()
        };
        out.replace_range(abs_start..abs_end, &new_text);
    }
    Some(out)
}

/// 统计目标文字在整份文档里出现的次数（用于「不唯一就报错，让模型补充上下文」）。
pub fn count_occurrences(document_xml: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let escaped = escape_text(needle);
    let mut total = 0usize;
    for block in paragraphs_xml(document_xml) {
        let text = paragraph_text(&block);
        total += text
            .matches(needle)
            .count()
            .max(text.matches(&escaped).count());
    }
    total
}

/// 取出全部段落元素（含正文与表格单元格里的段落）。
fn paragraphs_xml(document_xml: &str) -> Vec<String> {
    let (from, to) = match (document_xml.find("<w:body"), document_xml.find("</w:body>")) {
        (Some(s), Some(e)) => {
            let start = document_xml[s..].find('>').map(|p| s + p + 1).unwrap_or(s);
            (start, e)
        }
        _ => (0, document_xml.len()),
    };
    let mut out = Vec::new();
    let mut cursor = from;
    while cursor < to {
        let Some(rel) = document_xml[cursor..to].find("<w:p") else {
            break;
        };
        let at = cursor + rel;
        let next_char = document_xml[at + 4..].chars().next();
        if !matches!(next_char, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
            cursor = at + 4;
            continue;
        }
        let Some((gt, self_closing)) = start_tag_end(document_xml, at) else {
            break;
        };
        if self_closing {
            cursor = at + gt + 1;
            continue;
        }
        let Some(end) = document_xml[at + gt..to]
            .find("</w:p>")
            .map(|e| at + gt + e + 6)
        else {
            break;
        };
        out.push(document_xml[at..end].to_string());
        cursor = end;
    }
    out
}

/// 在整份文档里替换文字，返回（新文档 XML, 实际替换次数）。
///
/// `replace_all=false` 时若命中多处则**不做任何修改**并返回错误说明——
/// 替换错地方比不替换更糟，所以把选择权交回给调用方（补充上下文或明确要求全替换）。
pub fn replace_text(
    document_xml: &str,
    needle: &str,
    replacement: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if needle.is_empty() {
        return Err("要找的文字不能为空".to_string());
    }
    let total = count_occurrences(document_xml, needle);
    if total == 0 {
        return Err(format!("文档里找不到「{needle}」"));
    }
    if total > 1 && !replace_all {
        return Err(format!(
            "「{needle}」在文档里出现了 {total} 次，无法确定要改哪一处。请给出更长的上下文使其唯一，或明确要求全部替换。"
        ));
    }

    let mut out = document_xml.to_string();
    let mut replaced = 0usize;
    // 逐个段落替换；每段只替换一次（replace_all 时逐段多次）
    let paras = paragraphs_xml(document_xml);
    for para in paras {
        let mut current = para.clone();
        let mut changed_here = 0usize;
        while let Some(updated) = replace_in_paragraph(&current, needle, replacement) {
            if updated == current {
                break;
            }
            current = updated;
            changed_here += 1;
            if !replace_all {
                break;
            }
        }
        if changed_here == 0 {
            continue;
        }
        if let Some(pos) = out.find(&para) {
            out.replace_range(pos..pos + para.len(), &current);
            replaced += changed_here;
            if !replace_all && replaced >= 1 {
                break;
            }
        }
    }
    if replaced == 0 {
        return Err(format!(
            "「{needle}」出现在无法安全替换的位置（可能被拆散在多个片段里且跨越了不可分割的边界）"
        ));
    }
    Ok((out, replaced))
}

/// 从文档 XML 里取出 `<w:t>` 文字的第一个可读片段（供诊断用）。
pub fn first_text(document_xml: &str) -> Option<String> {
    let t = extract_tag(document_xml, "<w:t", "</w:t>")?;
    Some(unescape(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份贴近真实的正文：标题、被拆成多段的段落（加粗 + 拼写标记夹在中间）、表格。
    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>季度报告</w:t></w:r></w:p><w:p><w:r><w:t>请在下周</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>一</w:t></w:r><w:r><w:t>之前提交报告</w:t></w:r></w:p><w:p><w:r><w:t>备注：请在下周确认预算</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>项目</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>金额</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>差旅</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>1200</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;

    #[test]
    fn parses_headings_paragraphs_and_tables() {
        let blocks = parse_blocks(DOC);
        assert_eq!(blocks.len(), 4, "{blocks:#?}");
        assert_eq!(
            blocks[0],
            Block::Paragraph {
                text: "季度报告".into(),
                heading: Some(1)
            }
        );
        // 跨三个片段的段落要拼成一句完整的话
        assert_eq!(
            blocks[1],
            Block::Paragraph {
                text: "请在下周一之前提交报告".into(),
                heading: None
            }
        );
        match &blocks[3] {
            Block::Table { rows } => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], vec!["项目", "金额"]);
                assert_eq!(rows[1], vec!["差旅", "1200"]);
            }
            other => panic!("最后一块应是表格：{other:?}"),
        }
    }

    #[test]
    fn renders_markdown_with_heading_and_table() {
        let text = render_blocks(&parse_blocks(DOC));
        assert!(text.starts_with("# 季度报告"), "{text}");
        assert!(text.contains("请在下周一之前提交报告"), "{text}");
        assert!(text.contains("项目 | 金额"), "{text}");
        assert!(text.contains("--- | ---"), "表格要有分隔线：{text}");
    }

    #[test]
    fn replacing_text_spanning_runs_keeps_other_runs_intact() {
        // 「下周一」跨了三个片段：
        //   「请在下周」|「一」(加粗) |「之前提交报告」
        let out = replace_in_paragraph(DOC, "下周一", "本周三").unwrap();
        // 替换文字落在命中区间，其它片段保持原样
        assert!(out.contains("请在本周三"), "{out}");
        // 加粗样式必须还在（那是第三个片段的样式，本来就不该动）
        assert!(out.contains("<w:b/>"), "不得丢失片段上的格式：{out}");
        // 段落外的一切原样
        assert!(out.contains("<w:pStyle w:val=\"Heading1\"/>"));
        assert!(out.contains("差旅"));
    }

    #[test]
    fn replacing_inside_one_run_works() {
        let out = replace_in_paragraph(DOC, "1200", "1500").unwrap();
        assert!(out.contains("<w:t>1500</w:t>"), "{out}");
        assert!(!out.contains("1200"), "{out}");
    }

    #[test]
    fn ambiguous_needle_is_refused_with_count() {
        let err = replace_text(DOC, "下周", "本周", false).unwrap_err();
        assert!(err.contains("2 次"), "{err}");
        assert!(err.contains("唯一"), "{err}");
        // 拒绝时必须一字未改
        let (all, n) = replace_text(DOC, "下周", "本周", true).unwrap();
        assert_eq!(n, 2);
        assert!(!all.contains("下周"), "全部替换后不该还有原词");
    }

    #[test]
    fn missing_needle_reports_clearly() {
        let err = replace_text(DOC, "这段文字不存在", "x", false).unwrap_err();
        assert!(err.contains("找不到"), "{err}");
    }

    #[test]
    fn empty_needle_is_refused() {
        assert!(
            replace_text(DOC, "", "x", true)
                .unwrap_err()
                .contains("不能为空")
        );
    }

    #[test]
    fn proofing_markers_and_bookmarks_survive() {
        let with_markers = r#"<w:document xmlns:w="http://x"><w:body><w:p><w:bookmarkStart w:id="1" w:name="b1"/><w:proofErr w:type="spellStart"/><w:r><w:t>待改文字</w:t></w:r><w:proofErr w:type="spellEnd"/><w:bookmarkEnd w:id="1"/></w:p></w:body></w:document>"#;
        let (out, n) = replace_text(with_markers, "待改文字", "改过了", true).unwrap();
        assert_eq!(n, 1);
        assert!(out.contains("改过了"));
        assert!(out.contains("<w:proofErr"), "拼写检查标记必须保留：{out}");
        assert!(out.contains("<w:bookmarkStart"), "书签必须保留：{out}");
        assert!(out.contains("<w:bookmarkEnd"), "{out}");
    }

    #[test]
    fn entity_escaped_text_is_matched() {
        let doc = r#"<w:document xmlns:w="http://x"><w:body><w:p><w:r><w:t>a&amp;b</w:t></w:r></w:p></w:body></w:document>"#;
        // 用户输入的是未转义的原文
        let (out, n) = replace_text(doc, "a&b", "c<d", true).unwrap();
        assert_eq!(n, 1);
        assert!(out.contains("c&lt;d"), "写入侧要转义：{out}");
    }

    #[test]
    fn count_occurrences_counts_across_paragraphs_and_tables() {
        assert_eq!(count_occurrences(DOC, "下周"), 2);
        assert_eq!(count_occurrences(DOC, "差旅"), 1);
        assert_eq!(count_occurrences(DOC, "完全没有"), 0);
    }
}
