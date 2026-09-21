//! 工作簿结构解析：把「工作表名」映射到压缩包里的具体文件，并读共享字符串表。
//!
//! 为什么不能直接猜文件名：工作表名与文件名没有任何固定对应关系。真实文件里
//! `Sheet1` 完全可能是 `xl/worksheets/sheet7.xml`，而且工作表可以改名、重排、增删。
//! 唯一可靠的路径是走「工作簿声明 → 关系文件 → 目标文件」这条链：
//!
//! ```text
//! xl/workbook.xml        <sheet name="月度汇总" r:id="rId3"/>
//! xl/_rels/workbook.xml.rels   <Relationship Id="rId3" Target="worksheets/sheet2.xml"/>
//! → xl/worksheets/sheet2.xml
//! ```

use super::xml_util::{attr_of, find_tag_starts, split_attrs, unescape};

/// 工作簿里的一个工作表。
#[derive(Debug, Clone, PartialEq)]
pub struct SheetRef {
    /// 工作表名（显示给用户看的那个）。
    pub name: String,
    /// 工作表的声明编号。
    pub sheet_id: Option<u32>,
    /// 压缩包内的文件路径，如 `xl/worksheets/sheet2.xml`。
    pub entry: String,
}

/// 工作簿文件名（标准位置；极少数文件会用别的名字，此时无法处理并明确报错）。
pub const WORKBOOK_ENTRY: &str = "xl/workbook.xml";
/// 工作簿的关系文件。
pub const WORKBOOK_RELS_ENTRY: &str = "xl/_rels/workbook.xml.rels";
/// 共享字符串表。
pub const SHARED_STRINGS_ENTRY: &str = "xl/sharedStrings.xml";

/// 把关系文件里的目标路径归一成压缩包内路径。
///
/// 关系文件里的 `Target` 是相对工作簿所在目录（`xl/`）写的，可能写成
/// `worksheets/sheet1.xml`（相对）或 `/xl/worksheets/sheet1.xml`（绝对）两种形态。
fn normalize_target(target: &str) -> String {
    let t = target.trim();
    if let Some(stripped) = t.strip_prefix('/') {
        return stripped.replace('\\', "/");
    }
    let joined = format!("xl/{}", t.trim_start_matches("./"));
    // 折掉形如 `xl/worksheets/../worksheets/sheet1.xml` 这类写法
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// 从 `xl/_rels/workbook.xml.rels` 里取出 关系编号 → 压缩包内路径 的映射。
fn parse_rels(rels_xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    find_tag_starts(rels_xml, 0, rels_xml.len(), "Relationship", |at| {
        if let Some((gt, _)) = super::xml_util::start_tag_end(rels_xml, at) {
            let attrs = split_attrs(&rels_xml[at + "Relationship".len() + 1..at + gt]);
            if let (Some(id), Some(target)) = (attr_of(&attrs, "Id"), attr_of(&attrs, "Target")) {
                out.push((id.to_string(), normalize_target(target)));
            }
        }
        false
    });
    out
}

/// 从 `xl/workbook.xml` 里取出工作表声明（名称 + 关系编号）。
fn parse_sheets(workbook_xml: &str) -> Vec<(String, Option<u32>, Option<String>)> {
    let mut out = Vec::new();
    find_tag_starts(workbook_xml, 0, workbook_xml.len(), "sheet", |at| {
        if let Some((gt, _)) = super::xml_util::start_tag_end(workbook_xml, at) {
            let attrs = split_attrs(&workbook_xml[at + "sheet".len() + 1..at + gt]);
            if let Some(name) = attr_of(&attrs, "name") {
                let sheet_id = attr_of(&attrs, "sheetId").and_then(|s| s.parse().ok());
                // 关系编号的属性名带命名空间前缀，实际写法可能是 r:id / id
                let rid = attr_of(&attrs, "r:id")
                    .or_else(|| attr_of(&attrs, "id"))
                    .map(|s| s.to_string());
                out.push((name.to_string(), sheet_id, rid));
            }
        }
        false
    });
    out
}

/// 解析出工作簿里全部工作表及其文件路径（顺序与文件内一致）。
///
/// 某个工作表没能解析出文件路径时保留它但把 `entry` 置空——调用方据此给出
/// 「这个工作表在文件里找不到对应内容」的明确错误，而不是静默跳过一个工作表。
pub fn list_sheets(workbook_xml: &str, rels_xml: &str) -> Vec<SheetRef> {
    let rels = parse_rels(rels_xml);
    parse_sheets(workbook_xml)
        .into_iter()
        .map(|(name, sheet_id, rid)| {
            let entry = rid
                .and_then(|id| {
                    rels.iter()
                        .find(|(rid, _)| *rid == id)
                        .map(|(_, target)| target.clone())
                })
                .unwrap_or_default();
            SheetRef {
                name,
                sheet_id,
                entry,
            }
        })
        .collect()
}

/// 按名称找工作表；找不到返回 None。
pub fn find_sheet<'a>(sheets: &'a [SheetRef], name: &str) -> Option<&'a SheetRef> {
    sheets.iter().find(|s| s.name == name)
}

/// 解析共享字符串表：按出现顺序返回文字列表（下标即单元格里 `t="s"` 时引用的编号）。
pub fn parse_shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    find_tag_starts(xml, 0, xml.len(), "si", |at| {
        if let Some((gt, self_closing)) = super::xml_util::start_tag_end(xml, at) {
            let body_end = if self_closing {
                at + gt + 1
            } else {
                xml[at + gt..]
                    .find("</si>")
                    .map(|e| at + gt + e + "</si>".len())
                    .unwrap_or(xml.len())
            };
            let body = &xml[at..body_end];
            // 一个 <si> 里可能有多个 <t>（富文本分段），拼接起来
            let mut text = String::new();
            let mut cursor = 0usize;
            while let Some(rel) = body[cursor..].find("<t") {
                let t_at = cursor + rel;
                cursor = t_at + 2;
                let next = body[t_at + 2..].chars().next();
                if !matches!(next, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
                    continue;
                }
                let Some((t_gt, t_self_closing)) = super::xml_util::start_tag_end(body, t_at)
                else {
                    break;
                };
                if t_self_closing {
                    continue;
                }
                let start = t_at + t_gt + 1;
                let Some(end) = body[start..].find("</t>").map(|e| start + e) else {
                    break;
                };
                text.push_str(&unescape(&body[start..end]));
                cursor = end;
            }
            out.push(text);
        }
        false
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKBOOK: &str = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="月度汇总" sheetId="1" r:id="rId1"/><sheet name="明细" sheetId="2" r:id="rId3"/></sheets></workbook>"#;

    const RELS: &str = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet7.xml"/></Relationships>"#;

    #[test]
    fn sheet_name_maps_through_relationship_chain() {
        let sheets = list_sheets(WORKBOOK, RELS);
        assert_eq!(sheets.len(), 2);
        // 名与文件名毫无字面关联——只能靠关系链
        assert_eq!(sheets[0].name, "月度汇总");
        assert_eq!(sheets[0].entry, "xl/worksheets/sheet1.xml");
        assert_eq!(sheets[1].name, "明细");
        assert_eq!(
            sheets[1].entry, "xl/worksheets/sheet7.xml",
            "绝对写法也要归一"
        );
        assert_eq!(sheets[1].sheet_id, Some(2));
    }

    #[test]
    fn find_sheet_by_name() {
        let sheets = list_sheets(WORKBOOK, RELS);
        assert_eq!(
            find_sheet(&sheets, "明细").map(|s| s.entry.as_str()),
            Some("xl/worksheets/sheet7.xml")
        );
        assert!(find_sheet(&sheets, "不存在").is_none());
    }

    #[test]
    fn missing_relationship_leaves_empty_entry() {
        let broken = WORKBOOK.replace(r#"r:id="rId3""#, r#"r:id="rId99""#);
        let sheets = list_sheets(&broken, RELS);
        assert_eq!(sheets[1].entry, "", "解析不出路径时留空，由调用方报错");
    }

    #[test]
    fn relative_and_dotdot_targets_normalize() {
        assert_eq!(
            normalize_target("worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            normalize_target("/xl/worksheets/s.xml"),
            "xl/worksheets/s.xml"
        );
        assert_eq!(
            normalize_target("./worksheets/s.xml"),
            "xl/worksheets/s.xml"
        );
        assert_eq!(
            normalize_target("worksheets/../worksheets/s.xml"),
            "xl/worksheets/s.xml"
        );
    }

    #[test]
    fn shared_strings_indexed_in_order() {
        let xml = r#"<sst count="3" uniqueCount="3"><si><t>月份</t></si><si><t>金额</t></si><si><r><t>富</t></r><r><t>文本</t></r></si></sst>"#;
        let list = parse_shared_strings(xml);
        assert_eq!(list, vec!["月份", "金额", "富文本"]);
    }

    #[test]
    fn shared_strings_handles_entities_and_empty_entries() {
        let xml = r#"<sst><si><t>a&amp;b</t></si><si/></sst>"#;
        let list = parse_shared_strings(xml);
        assert_eq!(list, vec!["a&b", ""]);
    }
}
