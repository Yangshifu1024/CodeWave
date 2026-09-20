//! Word 文档的生成。
//!
//! 与读取、修改不同，**生成新文件用现成的库**（生成一份结构完整的 `.docx` 涉及样式表、
//! 关系文件、内容类型声明等一堆样板，自己拼没有意义）。读取与修改则是自己解包——
//! 那两件事的关键在于「原样保留用户文件里的一切」，交给库反而不可控。

use docx_rs::{Docx, Paragraph, Run, Table, TableCell, TableRow};
use serde::Deserialize;

/// 文档里的一个块。
#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BlockSpec {
    /// 标题（层级 1–6）。
    Heading {
        #[serde(default = "default_level")]
        level: u32,
        text: String,
    },
    /// 普通段落。
    Paragraph { text: String },
    /// 表格。
    Table {
        rows: Vec<Vec<String>>,
        /// 首行是否作为表头加粗（默认是）。
        #[serde(default = "default_true")]
        header: bool,
    },
}

fn default_level() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

/// Word 文档定义。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DocxSpec {
    /// 块序列。
    #[serde(default)]
    pub blocks: Vec<BlockSpec>,
}

/// 把表格的一行做成 docx-rs 的行。
fn table_row(cells: &[String], bold: bool) -> TableRow {
    TableRow::new(
        cells
            .iter()
            .map(|c| {
                let run = if bold {
                    Run::new().add_text(c).bold()
                } else {
                    Run::new().add_text(c)
                };
                TableCell::new().add_paragraph(Paragraph::new().add_run(run))
            })
            .collect(),
    )
}

/// 生成一份完整的 Word 文档字节。
pub fn generate(spec: &DocxSpec) -> Result<Vec<u8>, String> {
    if spec.blocks.is_empty() {
        return Err("文档至少要有一个块".to_string());
    }
    let mut doc = Docx::new();
    for block in &spec.blocks {
        doc = match block {
            BlockSpec::Heading { level, text } => {
                // Word 的内置标题样式名就是 Heading1..Heading9；越界要夹住，否则文档里会出现空标题
                let lv = (*level).clamp(1, 9);
                doc.add_paragraph(
                    Paragraph::new()
                        .style(&format!("Heading{lv}"))
                        .add_run(Run::new().add_text(text)),
                )
            }
            BlockSpec::Paragraph { text } => {
                // 段落里的换行要拆成多个段落——Word 不认 \n
                let mut d = doc;
                for line in text.split('\n') {
                    d = d.add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
                }
                d
            }
            BlockSpec::Table { rows, header } => {
                if rows.is_empty() {
                    continue;
                }
                let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                let built: Vec<TableRow> = rows
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let mut cells = r.clone();
                        // 补齐参差不齐的行，否则表格会缺列、显示错位
                        while cells.len() < width {
                            cells.push(String::new());
                        }
                        table_row(&cells, *header && i == 0)
                    })
                    .collect();
                doc.add_table(Table::new(built))
            }
        };
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    doc.build()
        .pack(&mut buf)
        .map_err(|e| format!("生成 Word 文档失败：{e}"))?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: serde_json::Value) -> DocxSpec {
        serde_json::from_value(json).expect("定义应能解析")
    }

    #[test]
    fn generates_a_readable_docx() {
        let bytes = generate(&spec(serde_json::json!({
            "blocks": [
                {"type": "heading", "level": 1, "text": "季度报告"},
                {"type": "paragraph", "text": "本季度表现良好。"},
                {"type": "table", "rows": [["项目", "金额"], ["差旅", "1200"]]}
            ]
        })))
        .unwrap();
        assert!(bytes.len() > 500, "产物太小，可能没写出内容");
        // 产物必须是一个包含正文的压缩包
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("word/document.xml").expect("要有正文"),
            &mut xml,
        )
        .unwrap();
        assert!(xml.contains("季度报告"), "标题缺失");
        assert!(xml.contains("本季度表现良好"), "正文缺失");
        assert!(xml.contains("差旅"), "表格缺失");
    }

    #[test]
    fn multiline_paragraph_becomes_separate_paragraphs() {
        let bytes = generate(&spec(serde_json::json!({
            "blocks": [{"type": "paragraph", "text": "第一行\n第二行"}]
        })))
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml)
            .unwrap();
        assert!(xml.contains("第一行") && xml.contains("第二行"));
        // 两行应各自成段，而不是塞进同一个段落（用解析结果判断，不依赖 XML 的具体书写形态）
        let blocks = super::super::docx::parse_blocks(&xml);
        let paras: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                super::super::docx::Block::Paragraph { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(paras, vec!["第一行", "第二行"], "换行应拆成两个段落");
    }

    #[test]
    fn ragged_table_rows_are_padded() {
        let bytes = generate(&spec(serde_json::json!({
            "blocks": [{"type": "table", "rows": [["a", "b", "c"], ["d"]]}]
        })))
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml)
            .unwrap();
        // 两行都应有三列（缺的补空），保证表格不错位（用解析结果判断，不依赖 XML 书写形态）
        let blocks = super::super::docx::parse_blocks(&xml);
        let rows = blocks
            .iter()
            .find_map(|b| match b {
                super::super::docx::Block::Table { rows } => Some(rows.clone()),
                _ => None,
            })
            .expect("应生成一个表格");
        assert_eq!(rows.len(), 2, "应有 2 行：{rows:?}");
        for row in &rows {
            assert_eq!(row.len(), 3, "每行都应补齐成 3 列：{rows:?}");
        }    }

    #[test]
    fn empty_document_is_refused() {
        let err = generate(&spec(serde_json::json!({"blocks": []}))).unwrap_err();
        assert!(err.contains("至少要有一个块"), "{err}");
    }

    #[test]
    fn heading_level_is_clamped() {
        // 超出范围不应写出无效样式名（那样文档里会出现空标题）
        let bytes = generate(&spec(serde_json::json!({
            "blocks": [{"type": "heading", "level": 99, "text": "x"}]
        })))
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml)
            .unwrap();
        assert!(xml.contains("Heading9"), "层级应被夹到 9：{xml}");
    }

    /// 端到端：生成 → 改文字 → 确认正文改了、其余内部文件逐字节未动。
    /// 这是 Word 侧的保真守护，与表格侧的真实样本逐字节比对同一个用意。
    #[test]
    fn round_trip_edit_preserves_other_parts() {
        let bytes = generate(&spec(serde_json::json!({
            "blocks": [
                {"type": "heading", "level": 1, "text": "原标题"},
                {"type": "paragraph", "text": "旧说法：请在下周一之前提交报告"},
                {"type": "table", "rows": [["项目", "金额"], ["差旅", "1200"]]}
            ]
        })))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.docx");
        std::fs::write(&src, &bytes).unwrap();

        let xml = String::from_utf8(
            super::super::patch::read_entry(&src, super::super::docx::DOCUMENT_ENTRY)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let (patched, applied) = super::super::edit_word::apply_text_edits(
            &xml,
            &[super::super::edit_word::TextEdit {
                find: "下周一".into(),
                replace: "本周三".into(),
                all: false,
            }],
        )
        .unwrap();
        assert_eq!(applied[0].count, 1);

        let dst = dir.path().join("b.docx");
        super::super::patch::apply(
            &src,
            &dst,
            &[super::super::patch::Replacement {
                name: super::super::docx::DOCUMENT_ENTRY.into(),
                bytes: patched.into_bytes(),
            }],
        )
        .unwrap();

        // 除了正文，其余内部文件必须逐字节不变（样式表、内容类型声明等）
        let entry = |p: &std::path::Path, name: &str| -> Vec<u8> {
            super::super::patch::read_entry(p, name).unwrap().unwrap_or_default()
        };
        for name in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/_rels/document.xml.rels",
            "word/styles.xml",
            "word/settings.xml",
        ] {
            assert_eq!(entry(&src, name), entry(&dst, name), "{name} 不得被改动");
        }
        // 正文里改动生效，其余内容还在
        let after = String::from_utf8(entry(&dst, super::super::docx::DOCUMENT_ENTRY)).unwrap();
        let text = super::super::docx::render_blocks(&super::super::docx::parse_blocks(&after));
        assert!(text.contains("请在本周三之前提交报告"), "{text}");
        assert!(text.contains("原标题"), "{text}");
        assert!(text.contains("差旅 | 1200"), "{text}");
    }

    /// 端到端：生成的文档要能被自己的读取实现读回来（读写两条路互相印证）。
    #[test]
    fn generated_document_reads_back_through_our_parser() {
        let bytes = generate(&spec(serde_json::json!({
            "blocks": [
                {"type": "heading", "level": 2, "text": "小节标题"},
                {"type": "paragraph", "text": "段落正文"},
                {"type": "table", "rows": [["列一", "列二"], ["v1", "v2"]]}
            ]
        })))
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("word/document.xml").unwrap(),
            &mut xml,
        )
        .unwrap();

        let blocks = super::super::docx::parse_blocks(&xml);
        let text = super::super::docx::render_blocks(&blocks);
        assert!(text.contains("## 小节标题"), "标题层级应被读回：{text}");
        assert!(text.contains("段落正文"), "{text}");
        assert!(text.contains("列一 | 列二"), "{text}");
    }
}
