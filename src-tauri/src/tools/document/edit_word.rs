//! Word 文档的保真修改：在已有文档里替换文字。
//!
//! 只改 `word/document.xml` 里 `<w:t>` 的文字，其余内部文件（图表、页眉页脚、图片、
//! 批注、样式表）与所有非文字节点（拼写检查标记、书签、批注锚点）一律不动。

use serde::Deserialize;
use serde_json::json;

use super::docx;

/// 一处文字替换。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    /// 要查找的文字。
    pub find: String,
    /// 替换成什么（允许为空字符串，表示删除）。
    #[serde(default)]
    pub replace: String,
    /// 是否替换全部命中；默认否——命中多处时报错，避免改错地方。
    #[serde(default)]
    pub all: bool,
}

/// 一处已完成的替换（供审批卡片与结果回执）。
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub find: String,
    pub replace: String,
    pub count: usize,
}

/// 依次应用全部文字替换，返回（新的正文 XML, 实际生效的替换清单）。
///
/// 任何一处失败即整体失败（不产出一半改动的文档）——半改的文档比不改更麻烦。
pub fn apply_text_edits(
    document_xml: &str,
    edits: &[TextEdit],
) -> Result<(String, Vec<Applied>), String> {
    let mut current = document_xml.to_string();
    let mut applied = Vec::new();
    for e in edits {
        if e.find.is_empty() {
            return Err("要查找的文字不能为空".to_string());
        }
        let (next, count) = docx::replace_text(&current, &e.find, &e.replace, e.all)
            .map_err(|err| format!("替换「{}」失败：{err}", e.find))?;
        applied.push(Applied {
            find: e.find.clone(),
            replace: e.replace.clone(),
            count,
        });
        current = next;
    }
    Ok((current, applied))
}

/// 试运行：只做检查与统计，不返回改动后的内容（审批卡片用）。
///
/// 与真正执行共用同一套替换逻辑，避免「卡片上显示的」与「实际改的」不一致。
pub fn preview(document_xml: &str, edits: &[TextEdit]) -> Result<Vec<Applied>, String> {
    let (_, applied) = apply_text_edits(document_xml, edits)?;
    Ok(applied)
}

/// 渲染审批卡片正文。
pub fn render_plan(path: &str, applied: &[Applied]) -> String {
    let mut lines = vec![format!("将修改 {path}：")];
    for a in applied {
        let times = if a.count > 1 {
            format!("（{} 处）", a.count)
        } else {
            String::new()
        };
        let to = if a.replace.is_empty() {
            "（删除）".to_string()
        } else {
            a.replace.clone()
        };
        lines.push(format!("  「{}」→「{}」{times}", a.find, to));
    }
    lines.push("其余内容（图表、页眉页脚、图片、批注、样式）一律原样保留。".to_string());
    lines.join("\n")
}

/// 结果回执里的改动清单。
pub fn changes_json(applied: &[Applied]) -> Vec<serde_json::Value> {
    applied
        .iter()
        .map(|a| json!({"find": a.find, "replace": a.replace, "count": a.count}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<w:document xmlns:w="http://x"><w:body><w:p><w:r><w:t>请在下周</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>一</w:t></w:r><w:r><w:t>之前提交报告</w:t></w:r></w:p><w:p><w:r><w:t>备注：请在下周确认预算</w:t></w:r></w:p></w:body></w:document>"#;

    /// 把文档 XML 解析成可读文本（断言用）。
    /// 不能直接对原始 XML 做字符串匹配：一句连续的话往往分布在多个片段里。
    fn text_of(doc: &str) -> String {
        docx::render_blocks(&docx::parse_blocks(doc))
    }

    #[test]
    fn applies_sequential_edits_on_evolving_text() {
        // 第二处替换必须看到第一处的结果（顺序执行、不是各自独立跑）
        let edits = vec![
            TextEdit {
                find: "下周一".into(),
                replace: "本周三".into(),
                all: false,
            },
            TextEdit {
                find: "本周三".into(),
                replace: "本周五".into(),
                all: false,
            },
        ];
        let (out, applied) = apply_text_edits(DOC, &edits).unwrap();
        assert_eq!(applied.len(), 2);
        let text = text_of(&out);
        assert!(text.contains("请在本周五之前提交报告"), "{text}");
        assert!(!text.contains("下周一"), "{text}");
        // 未命中的段落不受影响
        assert!(text.contains("备注：请在下周确认预算"), "{text}");
    }

    #[test]
    fn ambiguous_edit_fails_the_whole_batch() {
        // 第一处唯一命中、能成功；第二处有歧义
        // → 整体失败，不产出只改了一半的文档（半改比不改更麻烦）
        let edits = vec![
            TextEdit {
                find: "提交报告".into(),
                replace: "提交文档".into(),
                all: false,
            },
            // 「请」在文档里出现两次（正文一处、备注一处）
            TextEdit {
                find: "请".into(),
                replace: "x".into(),
                all: false,
            },
        ];
        let err = apply_text_edits(DOC, &edits).unwrap_err();
        assert!(err.contains("2 次"), "{err}");
        assert!(err.contains("唯一"), "{err}");
    }

    /// 单独一处歧义：直接拒绝，不猜。
    #[test]
    fn ambiguous_single_edit_is_refused() {
        let edits = vec![TextEdit {
            find: "下周".into(),
            replace: "本周".into(),
            all: false,
        }];
        let err = apply_text_edits(DOC, &edits).unwrap_err();
        assert!(err.contains("2 次"), "{err}");
    }

    #[test]
    fn replace_all_covers_every_occurrence() {
        let edits = vec![TextEdit {
            find: "下周".into(),
            replace: "本周".into(),
            all: true,
        }];
        let (out, applied) = apply_text_edits(DOC, &edits).unwrap();
        assert_eq!(applied[0].count, 2);
        let text = text_of(&out);
        assert!(!text.contains("下周"), "{text}");
        assert!(text.contains("请在本周一之前提交报告"), "{text}");
        assert!(text.contains("备注：请在本周确认预算"), "{text}");
    }

    #[test]
    fn deletion_is_supported_by_empty_replacement() {
        let edits = vec![TextEdit {
            find: "备注：".into(),
            replace: String::new(),
            all: true,
        }];
        let (out, _) = apply_text_edits(DOC, &edits).unwrap();
        assert!(!out.contains("备注："), "{out}");
        assert!(out.contains("请在下周确认预算"), "其余文字要留下：{out}");
    }

    #[test]
    fn preview_agrees_with_apply() {
        let edits = vec![TextEdit {
            find: "下周一".into(),
            replace: "本周三".into(),
            all: false,
        }];
        let (out, applied) = apply_text_edits(DOC, &edits).unwrap();
        let shown = preview(DOC, &edits).unwrap();
        assert_eq!(shown, applied, "预览与实际执行必须一致");
        assert!(out.contains("本周三"));
    }

    #[test]
    fn plan_renders_readable_lines() {
        let applied = vec![
            Applied {
                find: "下周一".into(),
                replace: "本周三".into(),
                count: 1,
            },
            Applied {
                find: "备注：".into(),
                replace: String::new(),
                count: 3,
            },
        ];
        let text = render_plan("a.docx", &applied);
        assert!(text.contains("下周一"), "{text}");
        assert!(text.contains("本周三"), "{text}");
        assert!(text.contains("3 处"), "{text}");
        assert!(text.contains("删除"), "{text}");
        assert!(text.contains("原样保留"), "{text}");
    }

    #[test]
    fn empty_find_is_refused() {
        let edits = vec![TextEdit {
            find: String::new(),
            replace: "x".into(),
            all: false,
        }];
        assert!(
            apply_text_edits(DOC, &edits)
                .unwrap_err()
                .contains("不能为空")
        );
    }

    #[test]
    fn formatting_and_markers_survive_replacement() {
        let (out, _) = apply_text_edits(
            DOC,
            &[TextEdit {
                find: "下周一".into(),
                replace: "周五".into(),
                all: false,
            }],
        )
        .unwrap();
        assert!(out.contains("<w:b/>"), "加粗不得丢失：{out}");
        assert!(text_of(&out).contains("周五"), "{}", text_of(&out));
    }
}
