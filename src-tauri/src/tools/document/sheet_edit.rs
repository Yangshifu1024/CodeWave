//! 工作表 XML 的单元格编辑：在**纯文本**层面定位并替换单个单元格。
//!
//! **为什么不解析成 XML 树再序列化**：那样会把整个文件的书写形态一起改掉（命名空间前缀、
//! 属性顺序、自闭合写法、空白），保真修改的意义就没了。这里只对目标单元格做文本替换，
//! 其余字节一个不动。
//!
//! 文字值用**内联字符串**（`t="inlineStr"`）写入，而不是往共享字符串表里追加条目。理由是
//! 「改动面最小」：内联字符串只落在工作表这一个内部文件里，共享字符串表则要额外维护
//! `count` / `uniqueCount` 两个计数，任何一处算错都会让文件出问题。

use super::xlsx::{self, col_to_index, index_to_col};
use super::xml_util::{attr_of, escape_text, extract_tag, split_attrs, unescape};

/// 要写入单元格的值。
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// 文本。
    Text(String),
    /// 数值。
    Number(f64),
    /// 逻辑值。
    Bool(bool),
    /// 公式（不含前导等号）。
    Formula(String),
}

/// 把数值格式化成 Excel 能读的写法：整数不带小数点，其余保留足够精度。
/// 非有限值（NaN / 无穷）退化成 0——Excel 没有它们的字面写法，写出去会让文件打不开。
fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "0".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// 供展示用的数值写法（审批卡片、结果回执）。与写入文件的值保持一致，
/// 避免「卡片上显示 999」而文件里其实是 `999.0` 这种对不上的情况。
pub fn format_number_for_display(n: f64) -> String {
    format_number(n)
}

/// 定位 `<sheetData>` 的内容区间 `[start, end)`；无该节点或自闭合时报错/返回空。
fn sheet_data_range(xml: &str) -> Result<(usize, usize), String> {
    let open = xml
        .find("<sheetData")
        .ok_or_else(|| "工作表缺少 <sheetData> 节点".to_string())?;
    let after_open = xml[open..]
        .find('>')
        .map(|p| open + p)
        .ok_or_else(|| "<sheetData> 起始标签未闭合".to_string())?;
    // 自闭合：<sheetData/>
    if xml[open..after_open].ends_with('/') {
        return Ok((after_open + 1, after_open + 1));
    }
    let close = xml[after_open..]
        .find("</sheetData>")
        .map(|p| after_open + p)
        .ok_or_else(|| "<sheetData> 缺少结束标签".to_string())?;
    Ok((after_open + 1, close))
}

/// 一个已定位的 `<c>` 元素：整体字节区间 + 起始标签内容 + 是否自闭合。
struct CellSpan {
    start: usize,
    end: usize,
    tag_inner: String,
    self_closing: bool,
}

/// 读出一个起始标签的位置：返回（从 `at` 数起、`>` 的偏移，是否自闭合）。
/// 需要跳出 `/>` 里的 `/` 时，自行按 `gt - 1` 处理。
fn start_tag_inner(xml: &str, at: usize) -> Option<(usize, bool)> {
    super::xml_util::start_tag_end(xml, at)
}

/// 在 `[from, to)` 里找坐标等于 `coord` 的 `<c>` 元素。
///
/// 游标 `cursor` 是 `hay` 里的绝对位置，每次至少前进 2——**绝不能写成相对偏移累加**，
/// 那样游标会后退、同一单元格被反复检查而陷入死循环。
fn find_cell(xml: &str, from: usize, to: usize, coord: &str) -> Option<CellSpan> {
    let target = coord.to_ascii_uppercase();
    let hay = &xml[from..to];
    let mut cursor = 0usize;
    while cursor < hay.len() {
        let rel = hay[cursor..].find("<c")?;
        let local = cursor + rel;
        cursor = local + 2;
        let at = from + local;
        // 确认是 <c 而不是 <cols / <cfRule 之类：下一个字符必须是空白、'>' 或 '/'
        let next = xml[at + 2..].chars().next();
        if !matches!(next, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
            continue;
        }
        let (gt_rel, self_closing) = start_tag_inner(xml, at)?;
        // 自闭合时要排除 `/>` 里的 `/`，否则它会被当成一个属性带进重建结果
        let tag_end = if self_closing {
            at + gt_rel - 1
        } else {
            at + gt_rel
        };
        let tag_inner = xml[at + 2..tag_end].to_string();
        let attrs = split_attrs(&tag_inner);
        if attr_of(&attrs, "r") == Some(target.as_str()) {
            let end = if self_closing {
                at + gt_rel + 1 // 含 '>'
            } else {
                let close_rel = xml[at + gt_rel..to].find("</c>")?;
                at + gt_rel + close_rel + "</c>".len()
            };
            return Some(CellSpan {
                start: at,
                end,
                tag_inner,
                self_closing,
            });
        }
    }
    None
}

/// 构造替换后的 `<c>` 元素：保留原 `s`（样式）等全部属性，只按新值改写 `t` 与内容。
fn build_cell(coord: &str, original_attrs: &[(String, String)], value: &CellValue) -> String {
    // 只按值类型决定 t；数值不需要 t
    let want_t: Option<&str> = match value {
        CellValue::Text(_) => Some("inlineStr"),
        CellValue::Number(_) => None,
        CellValue::Bool(_) => Some("b"),
        CellValue::Formula(_) => None,
    };
    let mut parts: Vec<String> = Vec::new();
    for (name, raw) in original_attrs {
        if name == "t" {
            continue; // 稍后按需重新写出
        }
        parts.push(raw.clone());
    }
    if let Some(t) = want_t {
        parts.push(format!("t=\"{t}\""));
    }
    // 坐标排在首位（与 Excel 的书写习惯一致）
    let coord_attr = format!("r=\"{}\"", coord.to_ascii_uppercase());
    let others: Vec<String> = parts
        .iter()
        .filter(|p| !p.starts_with("r="))
        .cloned()
        .collect();
    let mut attr_text = coord_attr;
    for p in others {
        attr_text.push(' ');
        attr_text.push_str(&p);
    }

    let body = match value {
        CellValue::Text(s) => {
            let needs_preserve = s.starts_with(' ') || s.ends_with(' ') || s.contains('\n');
            if needs_preserve {
                format!("<is><t xml:space=\"preserve\">{}</t></is>", escape_text(s))
            } else {
                format!("<is><t>{}</t></is>", escape_text(s))
            }
        }
        CellValue::Number(n) => format!("<v>{}</v>", format_number(*n)),
        CellValue::Bool(b) => format!("<v>{}</v>", if *b { 1 } else { 0 }),
        CellValue::Formula(f) => {
            // 不带缓存值：由 Excel 在打开时算（文件同时被标记为「打开时重算」）
            format!("<f>{}</f>", escape_text(f.trim_start_matches('=')))
        }
    };
    format!("<c {attr_text}>{body}</c>")
}

/// 坐标 `B3` 拆成（列号, 行号）；非法返回 None。
fn split_coord(coord: &str) -> Option<(u32, u32)> {
    let letters: String = coord
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let digits: String = coord
        .chars()
        .skip_while(|c| c.is_ascii_alphabetic())
        .collect();
    let col = col_to_index(&letters)?;
    let row: u32 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((col, row))
}

/// 目标行的定位结果。
enum RowScan {
    /// 找到了该行。
    Found {
        start: usize,
        end: usize,
        self_closing: bool,
        /// 起始标签的属性区（新建行时若需保留原属性可用）
        attrs: String,
    },
    /// 没找到：新建行时应插到这个位置（保持行号升序）
    InsertAt(usize),
}

/// 在 `<sheetData>` 里定位行；没找到时给出保持升序的插入位置。
///
/// 游标是 `hay` 里的绝对位置，每次至少前进 4，不得回退。
fn locate_row(xml: &str, from: usize, to: usize, row_num: u32) -> Result<RowScan, String> {
    let hay = &xml[from..to];
    let mut cursor = 0usize;
    while cursor < hay.len() {
        let Some(rel) = hay[cursor..].find("<row") else {
            break;
        };
        let local = cursor + rel;
        cursor = local + 4;
        let at = from + local;
        let next = xml[at + 4..].chars().next();
        if !matches!(next, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
            continue;
        }
        let (gt_rel, self_closing) = start_tag_inner_row(xml, at);
        let inner_end = if self_closing {
            at + gt_rel - 1
        } else {
            at + gt_rel
        };
        let attrs = xml[at + 4..inner_end].to_string();
        let parsed = split_attrs(&attrs);
        let this_num = attr_of(&parsed, "r").and_then(|s| s.parse::<u32>().ok());
        if this_num == Some(row_num) {
            let end = if self_closing {
                at + gt_rel + 1
            } else {
                let close = xml[at + gt_rel..to]
                    .find("</row>")
                    .ok_or_else(|| format!("第 {row_num} 行的结束标签缺失，已中止修改"))?;
                at + gt_rel + close + "</row>".len()
            };
            return Ok(RowScan::Found {
                start: at,
                end,
                self_closing,
                attrs,
            });
        }
        // 遇到第一个行号更大的行：新行应插到它前面
        if matches!(this_num, Some(n) if n > row_num) {
            return Ok(RowScan::InsertAt(at));
        }
    }
    // 没有行号更大的行：插到 sheetData 末尾
    Ok(RowScan::InsertAt(to))
}

/// 往既有行里插入一个单元格：自闭合行就地展开，其余按列号升序插到第一个更大的单元格之前。
/// 参数多是这段 XML 改写本身的需要（行区间、行属性、目标列、新单元格、行号各自独立），
/// 合成结构体反而会把调用点变得不清楚。
#[allow(clippy::too_many_arguments)]
fn insert_cell_into_row(
    xml: &str,
    row_start: usize,
    row_end: usize,
    self_closing: bool,
    row_attrs: &str,
    new_cell: &str,
    target_col: u32,
    row_num: u32,
) -> Result<String, String> {
    if self_closing {
        let expanded = format!("<row{row_attrs}>{new_cell}</row>");
        let mut out = String::with_capacity(xml.len() + expanded.len());
        out.push_str(&xml[..row_start]);
        out.push_str(&expanded);
        out.push_str(&xml[row_end..]);
        return Ok(out);
    }
    let content_end = row_end - "</row>".len();
    let mut insert_at = content_end;
    let mut cursor = row_start + xml[row_start..row_end].find('>').unwrap() + 1;
    while cursor < content_end {
        let Some(rel) = xml[cursor..content_end].find("<c") else {
            break;
        };
        let at = cursor + rel;
        cursor = at + 2;
        let next = xml[at + 2..].chars().next();
        if !matches!(next, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
            continue;
        }
        let (gt_rel, cell_self_closing) = start_tag_inner(xml, at)
            .ok_or_else(|| format!("第 {row_num} 行的单元格列表结构异常，已中止修改"))?;
        let tag_end = if cell_self_closing {
            at + gt_rel - 1
        } else {
            at + gt_rel
        };
        let attrs = split_attrs(&xml[at + 2..tag_end]);
        let existing_col = attr_of(&attrs, "r")
            .and_then(split_coord)
            .map(|(c, _)| c)
            .unwrap_or(0);
        if existing_col > target_col {
            insert_at = at;
            break;
        }
    }
    let mut out = String::with_capacity(xml.len() + new_cell.len());
    out.push_str(&xml[..insert_at]);
    out.push_str(new_cell);
    out.push_str(&xml[insert_at..]);
    Ok(out)
}

/// 坐标对应的单元格不存在时写入：所在行已存在就插进那一行；
/// 行不存在就新建一个只含这个单元格的空行。
///
/// **这不算「增删行列」**：新建的是原本整行空白的行，不会挪动任何已有内容，
/// 也不影响任何公式引用与图表区域（与「在中间插入一行、后续行全部下移」是两回事）。
fn insert_cell(
    xml: &str,
    from: usize,
    to: usize,
    coord: &str,
    value: &CellValue,
) -> Result<String, String> {
    let (target_col, row_num) =
        split_coord(coord).ok_or_else(|| format!("坐标无法解析：{coord}"))?;
    let new_cell = build_cell(coord, &[], value);
    match locate_row(xml, from, to, row_num)? {
        RowScan::Found {
            start,
            end,
            self_closing,
            attrs,
        } => insert_cell_into_row(
            xml,
            start,
            end,
            self_closing,
            &attrs,
            &new_cell,
            target_col,
            row_num,
        ),
        RowScan::InsertAt(at) => {
            let new_row = format!("<row r=\"{row_num}\">{new_cell}</row>");
            let mut out = String::with_capacity(xml.len() + new_row.len());
            out.push_str(&xml[..at]);
            out.push_str(&new_row);
            out.push_str(&xml[at..]);
            Ok(out)
        }
    }
}

/// 必要时扩大 `<dimension>` 让新写入的坐标落在声明的范围内。
///
/// 它是可选的优化提示，Excel 自己会重算；但别的读取工具以它为准，
/// 范围过期会让滚动范围与选区不正确，所以顺手改准。
fn grow_dimension(xml: &str, coord: &str) -> String {
    let Some((col, row)) = split_coord(coord) else {
        return xml.to_string();
    };
    let Some(tag_start) = xml.find("<dimension") else {
        return xml.to_string();
    };
    let Some(rel_gt) = xml[tag_start..].find('>') else {
        return xml.to_string();
    };
    let tag = &xml[tag_start..tag_start + rel_gt];
    let Some(pos) = tag.find("ref=\"") else {
        return xml.to_string();
    };
    let val_start = tag_start + pos + 5;
    let Some(vlen) = xml[val_start..].find('"') else {
        return xml.to_string();
    };
    let val_end = val_start + vlen;
    let Some(current) = xlsx::parse_region(&xml[val_start..val_end]) else {
        return xml.to_string();
    };
    let inside = col >= current.start_col
        && col <= current.end_col
        && row >= current.start_row
        && row <= current.end_row;
    if inside {
        return xml.to_string();
    }
    let new_ref = format!(
        "{}{}:{}{}",
        index_to_col(current.start_col.min(col)),
        current.start_row.min(row),
        index_to_col(current.end_col.max(col)),
        current.end_row.max(row)
    );
    let mut out = String::with_capacity(xml.len() + new_ref.len());
    out.push_str(&xml[..val_start]);
    out.push_str(&new_ref);
    out.push_str(&xml[val_end..]);
    out
}

/// 行的起始标签解析（与 `start_tag_inner` 同构，只是标签名长度不同）。
fn start_tag_inner_row(xml: &str, at: usize) -> (usize, bool) {
    let rest = &xml[at..];
    let gt = rest.find('>').unwrap_or(rest.len().saturating_sub(1));
    let self_closing = xml[at..at + gt].ends_with('/');
    (gt, self_closing)
}

/// 在整份工作表 XML 里把某个坐标的单元格设成给定值。
///
/// 单元格已存在则原地替换其内容（保留样式与其余属性）；不存在则写入所属行
/// （行不存在就新建一个空行，不挪动已有内容）。必要时同步扩大 dimension。
pub fn set_cell(xml: &str, coord: &str, value: &CellValue) -> Result<String, String> {
    let (from, to) = sheet_data_range(xml)?;
    let out = match find_cell(xml, from, to, coord) {
        Some(span) => {
            let attrs = split_attrs(&span.tag_inner);
            let new_cell = build_cell(coord, &attrs, value);
            let mut s = String::with_capacity(xml.len() + new_cell.len());
            s.push_str(&xml[..span.start]);
            s.push_str(&new_cell);
            s.push_str(&xml[span.end..]);
            Ok(s)
        }
        None => insert_cell(xml, from, to, coord, value),
    }?;
    Ok(grow_dimension(&out, coord))
}

/// 读出某个坐标当前的值文本（用于审批卡片展示「旧值 → 新值」）。
/// 只处理直接写在单元格里的值与内联字符串；共享字符串表的值返回占位说明。
pub fn peek_cell(xml: &str, coord: &str, shared_strings: Option<&[String]>) -> Option<String> {
    let (from, to) = sheet_data_range(xml).ok()?;
    let span = find_cell(xml, from, to, coord)?;
    let attrs = split_attrs(&span.tag_inner);
    let t = attr_of(&attrs, "t").unwrap_or("n");
    let body = if span.self_closing {
        ""
    } else {
        &xml[span.start..span.end]
    };
    // 公式优先展示
    if let Some(f) = extract_tag(body, "<f", "</f>") {
        return Some(format!("={f}"));
    }
    match t {
        "inlineStr" => extract_tag(body, "<t", "</t>").map(|s| unescape(&s)),
        "s" => {
            let idx: usize = extract_tag(body, "<v", "</v>")?.parse().ok()?;
            shared_strings
                .and_then(|list| list.get(idx).cloned())
                .or_else(|| Some(format!("（共享字符串 #{idx}）")))
        }
        "b" => extract_tag(body, "<v", "</v>").map(|v| {
            if v.trim() == "1" {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }),
        "str" => extract_tag(body, "<v", "</v>").map(|s| unescape(&s)),
        _ => extract_tag(body, "<v", "</v>"),
    }
}

/// 列字母转列号（对外暴露，供工作簿层判断行列范围）。
pub fn col_letters(coord: &str) -> Option<u32> {
    let letters: String = coord
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    col_to_index(&letters)
}

/// 列号转列字母（供工作簿层拼坐标）。
pub fn col_name(n: u32) -> String {
    index_to_col(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个贴近真实的最小工作表：含样式属性、共享字符串、公式与空单元格。
    const SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:C3"/><sheetData><row r="1" spans="1:3"><c r="A1" s="1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" s="2"><v>120</v></c></row><row r="2" spans="1:3"><c r="A2" t="s"><v>2</v></c><c r="B2" s="3"><f>SUM(C1:C1)</f><v>120</v></c></row><row r="3" spans="1:3"><c r="A3" t="inlineStr"><is><t>第三行</t></is></c></row></sheetData><conditionalFormatting sqref="A1:A3"><cfRule type="cellIs" dxfId="0" priority="1" operator="greaterThan"><formula>100</formula></cfRule></conditionalFormatting></worksheet>"#;

    #[test]
    fn attrs_split_preserves_raw_form() {
        let attrs = split_attrs(r#" r="A1" s="5" t='s'"#);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0], ("r".into(), r#"r="A1""#.into()));
        assert_eq!(attrs[1], ("s".into(), r#"s="5""#.into()));
        assert_eq!(attrs[2], ("t".into(), r#"t='s'"#.into()));
        assert_eq!(attr_of(&attrs, "t"), Some("s"));
        assert_eq!(attr_of(&attrs, "zz"), None);
    }

    /// 取出某个单元格元素在 xml 里的完整文本（供断言用）。
    fn cell_element(xml: &str, coord: &str) -> String {
        let (from, to) = sheet_data_range(xml).unwrap();
        let span = find_cell(xml, from, to, coord).unwrap();
        xml[span.start..span.end].to_string()
    }

    /// 取出某个单元格某个属性的值（供断言样式等属性是否原样保留）。
    fn cell_attr(xml: &str, coord: &str, name: &str) -> Option<String> {
        let (from, to) = sheet_data_range(xml).unwrap();
        let span = find_cell(xml, from, to, coord)?;
        let attrs = split_attrs(&span.tag_inner);
        attr_of(&attrs, name).map(|s| s.to_string())
    }

    #[test]
    fn set_existing_number_keeps_style_and_drops_old_value() {
        let out = set_cell(SHEET, "C1", &CellValue::Number(999.5)).unwrap();
        assert_eq!(peek_cell(&out, "C1", None).as_deref(), Some("999.5"));
        assert_ne!(peek_cell(&out, "C1", None).as_deref(), Some("120"));
        assert_eq!(
            cell_attr(&out, "C1", "s").as_deref(),
            Some("2"),
            "样式必须保留"
        );
        // 其余单元格不受影响
        assert_eq!(peek_cell(&out, "A3", None).as_deref(), Some("第三行"));
        assert_eq!(cell_attr(&out, "A1", "s").as_deref(), Some("1"));
    }

    #[test]
    fn set_text_uses_inline_string_and_keeps_style() {
        let out = set_cell(SHEET, "A1", &CellValue::Text("改后的值".into())).unwrap();
        assert!(
            out.contains(r#"<c r="A1" s="1" t="inlineStr"><is><t>改后的值</t></is></c>"#),
            "{out}"
        );
    }

    #[test]
    fn formula_replaces_cached_value_and_keeps_style() {
        let out = set_cell(SHEET, "B2", &CellValue::Formula("=SUM(C1:C5)".into())).unwrap();
        // 前导等号被剥掉（带上等号会让 Excel 报错）
        assert_eq!(peek_cell(&out, "B2", None).as_deref(), Some("=SUM(C1:C5)"));
        // 旧公式不得残留
        assert!(!out.contains("SUM(C1:C1)"), "旧公式未清：{out}");
        // 元素里不得再有缓存值
        let elem = cell_element(&out, "B2");
        assert!(!elem.contains("<v>"), "不得保留旧缓存值：{elem}");
        // 样式保留
        assert_eq!(cell_attr(&out, "B2", "s").as_deref(), Some("3"));
    }

    #[test]
    fn self_closing_cell_is_replaced_in_place() {
        let xml = r#"<worksheet><sheetData><row r="1"><c r="A1" s="7"/><c r="B1"><v>1</v></c></row></sheetData></worksheet>"#;
        let out = set_cell(xml, "A1", &CellValue::Number(3.0)).unwrap();
        assert!(out.contains(r#"<c r="A1" s="7"><v>3</v></c>"#), "{out}");
        assert!(
            out.contains(r#"<c r="B1"><v>1</v></c>"#),
            "邻居不受影响：{out}"
        );
    }

    #[test]
    fn attribute_order_variations_are_found() {
        // 属性顺序与写法都可能不同，定位必须靠属性名而不是位置
        let xml = r#"<worksheet><sheetData><row r="1"><c s="4" r="B2"><v>1</v></c></row></sheetData></worksheet>"#;
        let out = set_cell(xml, "B2", &CellValue::Number(7.0)).unwrap();
        assert!(out.contains("<v>7</v>"), "{out}");
        assert!(!out.contains("<v>1</v>"), "{out}");
    }

    #[test]
    fn insert_into_existing_row_keeps_column_order() {
        let xml = r#"<worksheet><sheetData><row r="1"><c r="A1"><v>1</v></c><c r="C1"><v>3</v></c></row></sheetData></worksheet>"#;
        let out = set_cell(xml, "B1", &CellValue::Number(2.0)).unwrap();
        let a = out.find(r#"r="A1""#).unwrap();
        let b = out.find(r#"r="B1""#).unwrap();
        let c = out.find(r#"r="C1""#).unwrap();
        assert!(a < b && b < c, "必须按列号升序：{out}");
    }

    #[test]
    fn insert_expands_self_closing_row() {
        let xml = r#"<worksheet><sheetData><row r="1" spans="1:1"/></sheetData></worksheet>"#;
        let out = set_cell(xml, "A1", &CellValue::Text("新".into())).unwrap();
        assert!(
            out.contains(
                r#"<row r="1" spans="1:1"><c r="A1" t="inlineStr"><is><t>新</t></is></c></row>"#
            ),
            "{out}"
        );
    }

    /// 目标行整行空白时：新建一个只含该单元格的空行。
    /// **不挪动任何已有内容**（与「在中间插入一行」不同），并同步扩大 dimension。
    #[test]
    fn absent_row_is_created_without_shifting_anything() {
        let out = set_cell(SHEET, "A99", &CellValue::Number(1.0)).unwrap();
        assert_eq!(peek_cell(&out, "A99", None).as_deref(), Some("1"));
        // 原有单元格全部原样
        assert_eq!(peek_cell(&out, "C1", None).as_deref(), Some("120"));
        assert_eq!(peek_cell(&out, "A3", None).as_deref(), Some("第三行"));
        // 新行排在已有行之后（行号升序）
        let last_existing = out.find("r=\"3\"").unwrap();
        let created = out.find("r=\"99\"").unwrap();
        assert!(last_existing < created, "新行必须在已有行之后：{out}");
        // dimension 同步扩大
        assert!(out.contains("A1:C99"), "dimension 未同步：{out}");
    }

    /// 新行插在中间时，必须保持行号升序（否则部分读取工具会读错）。
    #[test]
    fn new_row_keeps_ascending_order_in_the_middle() {
        let xml = "<worksheet><sheetData><row r=\"1\"><c r=\"A1\"><v>1</v></c></row><row r=\"5\"><c r=\"A5\"><v>5</v></c></row></sheetData></worksheet>";
        let out = set_cell(xml, "B3", &CellValue::Number(9.0)).unwrap();
        let p1 = out.find("r=\"1\"").unwrap();
        let p3 = out.find("r=\"3\"").unwrap();
        let p5 = out.find("r=\"5\"").unwrap();
        assert!(p1 < p3 && p3 < p5, "新行必须插在行 1 与行 5 之间：{out}");
        assert_eq!(peek_cell(&out, "B3", None).as_deref(), Some("9"));
        // 邻居不受影响
        assert_eq!(peek_cell(&out, "A1", None).as_deref(), Some("1"));
        assert_eq!(peek_cell(&out, "A5", None).as_deref(), Some("5"));
    }

    /// dimension 只在必要时扩大，范围内的写入不动它（改动最小化）。
    #[test]
    fn dimension_only_grows_when_needed() {
        let inside = set_cell(SHEET, "B2", &CellValue::Number(7.0)).unwrap();
        assert!(
            inside.contains("ref=\"A1:C3\""),
            "范围内的写入不应改 dimension：{inside}"
        );
        let beyond = set_cell(SHEET, "E1", &CellValue::Number(7.0)).unwrap();
        assert!(beyond.contains("ref=\"A1:E3\""), "{beyond}");
    }

    #[test]
    fn special_characters_are_escaped_and_control_chars_dropped() {
        let out = set_cell(SHEET, "A1", &CellValue::Text("a<b>&c\"d\u{1}".into())).unwrap();
        assert!(out.contains("a&lt;b&gt;&amp;c\"d"), "{out}");
        assert!(!out.contains('\u{1}'), "控制字符必须剔除：{out:?}");
        // 转义后仍是合法 XML（能再被本模块解析）
        assert!(peek_cell(&out, "A1", None).is_some());
    }

    #[test]
    fn leading_trailing_spaces_use_preserve_flag() {
        let out = set_cell(SHEET, "A1", &CellValue::Text("  留白  ".into())).unwrap();
        assert!(out.contains(r#"xml:space="preserve""#), "{out}");
    }

    #[test]
    fn peek_reads_back_each_value_type() {
        assert_eq!(peek_cell(SHEET, "C1", None).as_deref(), Some("120"));
        assert_eq!(peek_cell(SHEET, "A3", None).as_deref(), Some("第三行"));
        assert_eq!(peek_cell(SHEET, "B2", None).as_deref(), Some("=SUM(C1:C1)"));
        // 共享字符串：给了表就解出文字，没给就给出可读的占位
        let shared = vec!["月份".to_string(), "金额".to_string(), "1月".to_string()];
        assert_eq!(
            peek_cell(SHEET, "A1", Some(&shared)).as_deref(),
            Some("月份")
        );
        assert_eq!(
            peek_cell(SHEET, "A1", None).as_deref(),
            Some("（共享字符串 #0）")
        );
        // 不存在的坐标
        assert_eq!(peek_cell(SHEET, "Z99", None), None);
    }

    #[test]
    fn round_trip_set_then_peek() {
        for v in [
            CellValue::Number(42.5),
            CellValue::Text("中文 mixed".into()),
            CellValue::Formula("A1+B1".into()),
            CellValue::Bool(true),
        ] {
            let out = set_cell(SHEET, "A2", &v).unwrap();
            let got = peek_cell(&out, "A2", None).unwrap();
            let expect = match &v {
                CellValue::Number(n) => format_number(*n),
                CellValue::Text(s) => s.clone(),
                CellValue::Formula(f) => format!("={f}"),
                CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            };
            assert_eq!(got, expect, "{v:?} 往返失败");
        }
    }

    #[test]
    fn number_formatting_is_excel_friendly() {
        assert_eq!(format_number(120.0), "120");
        assert_eq!(format_number(-3.0), "-3");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(f64::NAN), "0", "NaN 退化成 0 而不是坏文件");
        assert_eq!(format_number(f64::INFINITY), "0");
    }

    #[test]
    fn malformed_sheet_is_reported_not_guessed() {
        assert!(
            set_cell("<worksheet/>", "A1", &CellValue::Number(1.0))
                .unwrap_err()
                .contains("sheetData")
        );
    }

    /// 回归：目标不存在且单元格很多时，扫描游标必须始终前进。
    /// 曾经把「相对本次起点的偏移」当成绝对位置，游标会后退并把同一单元格反复检查，陷入死循环。
    #[test]
    fn scanning_many_cells_without_match_terminates() {
        let mut xml = String::from("<worksheet><sheetData>");
        for row in 1..=300u32 {
            xml.push_str(&format!("<row r='{row}'>"));
            for col in 0..10u32 {
                let coord = format!("{}{}", index_to_col(col + 1), row);
                xml.push_str(&format!("<c r='{coord}' s='1'><v>{row}</v></c>"));
            }
            xml.push_str("</row>");
        }
        xml.push_str("</sheetData></worksheet>");

        // 在最后一行插一个新单元格：会被迫扫完整张表才找到行
        let out = set_cell(&xml, "K300", &CellValue::Number(1.0)).unwrap();
        assert!(out.contains("K300"), "{out}");

        // 行不存在：必须立即返回（新建一个空行）而不是卡住
        let far = set_cell(&xml, "A999", &CellValue::Number(1.0)).unwrap();
        assert_eq!(peek_cell(&far, "A999", None).as_deref(), Some("1"));

        // 同一张表上读不存在的坐标同样必须立即返回
        assert_eq!(peek_cell(&xml, "Z999", None), None);
    }
}
