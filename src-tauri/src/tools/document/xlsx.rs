//! `.xlsx` 读取的纯函数层：单元格区域地址解析与表格文本渲染。
//!
//! 与解析器解耦——这里的都是纯函数，便于单测；真正的解析在 `read.rs` 里调 umya。

/// 单元格区域；列号与行号均为 1-based，含端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start_col: u32,
    pub start_row: u32,
    pub end_col: u32,
    pub end_row: u32,
}

impl Region {
    /// 新建并归一化（起点大于终点时自动交换，容忍 `D50:A1` 这类写法）。
    pub fn new(c1: u32, r1: u32, c2: u32, r2: u32) -> Self {
        Region {
            start_col: c1.min(c2),
            start_row: r1.min(r2),
            end_col: c1.max(c2),
            end_row: r1.max(r2),
        }
    }

    /// 列数（含端点）；起止都是 1 时返回 1。
    pub fn cols(&self) -> u32 {
        self.end_col.saturating_sub(self.start_col) + 1
    }

    /// 行数（含端点）。
    pub fn rows(&self) -> u32 {
        self.end_row.saturating_sub(self.start_row) + 1
    }
}

/// Excel 的最大列数与最大行数（用于拒绝越界地址）。
const MAX_COLS: u32 = 16_384;
const MAX_ROWS: u32 = 1_048_576;

/// 列字母转列号：A → 1、Z → 26、AA → 27。空串、含非字母、越界均返回 None。
pub fn col_to_index(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut n: u32 = 0;
    for ch in s.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        n = n.checked_mul(26)?;
        n = n.checked_add(ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
        if n > MAX_COLS {
            return None;
        }
    }
    Some(n)
}

/// 列号转列字母：1 → A、26 → Z、27 → AA。0 返回空串。
pub fn index_to_col(mut n: u32) -> String {
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push((b'A' + rem) as char);
        n = (n - 1) / 26;
    }
    out.reverse();
    out.into_iter().collect()
}

/// 解析 `"B3"` → (列号, 行号)。非法返回 None。
fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let letters: String = s.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = s.chars().skip_while(|c| c.is_ascii_alphabetic()).collect();
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let col = col_to_index(&letters)?;
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > MAX_ROWS {
        return None;
    }
    Some((col, row))
}

/// 解析区域地址：`"A1:D50"`、`"B3"`（单格即 1×1 区域）。非法返回 None。
pub fn parse_region(raw: &str) -> Option<Region> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (s, s),
    };
    let (c1, r1) = parse_cell(a)?;
    let (c2, r2) = parse_cell(b)?;
    Some(Region::new(c1, r1, c2, r2))
}

/// 单元格值转成一行表格里的安全文本：制表符与换行替换为空格，
/// 避免值里的制表符把列结构撑错位（展示用，不做转义）。
pub fn cell_text(v: &str) -> String {
    v.replace(['\t', '\n', '\r'], " ").trim_end().to_string()
}

/// 把若干行渲染成制表符分隔的文本（每行以换行结尾；空表返回空串）。
pub fn render_tsv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for r in rows {
        out.push_str(
            &r.iter()
                .map(|c| cell_text(c))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters_round_trip() {
        assert_eq!(col_to_index("A"), Some(1));
        assert_eq!(col_to_index("Z"), Some(26));
        assert_eq!(col_to_index("AA"), Some(27));
        assert_eq!(col_to_index("d"), Some(4), "小写也接受");
        assert_eq!(col_to_index(""), None);
        assert_eq!(col_to_index("A1"), None, "混入数字即非法");
        assert_eq!(col_to_index("XFD"), Some(16384), "最大列（XFD 是 Excel 第 16384 列）");
        assert_eq!(col_to_index("XFE"), None, "越界（XFE = 16385）");

        for n in [1u32, 4, 26, 27, 52, 703, 16_384] {
            assert_eq!(col_to_index(&index_to_col(n)), Some(n), "往返失败：{n}");
        }
        assert_eq!(index_to_col(0), "");
    }

    #[test]
    fn region_parsing() {
        assert_eq!(
            parse_region("A1:D50"),
            Some(Region {
                start_col: 1,
                start_row: 1,
                end_col: 4,
                end_row: 50
            })
        );
        // 单格 = 1×1
        let one = parse_region("B3").unwrap();
        assert_eq!((one.cols(), one.rows()), (1, 1));
        // 反向书写自动归一
        let rev = parse_region("D50:A1").unwrap();
        assert_eq!(rev, parse_region("A1:D50").unwrap());
        // 空白宽容
        assert!(parse_region("  A1:C3  ").is_some());
        // 非法
        assert_eq!(parse_region(""), None);
        assert_eq!(parse_region("1A"), None);
        assert_eq!(parse_region("A0"), None, "行号从 1 起");
        assert_eq!(parse_region("A1:B"), None);
        assert_eq!(parse_region("A1:"), None);
        assert_eq!(parse_region("A1:B2:C3"), None, "只允许一段冒号");
    }

    #[test]
    fn tsv_rendering_sanitizes_layout_breakers() {
        let rows = vec![
            vec!["月份".to_string(), "金额".to_string()],
            vec!["1月\t(含制表符)".to_string(), "120\n换行".to_string()],
        ];
        let out = render_tsv(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "值里的换行不得撑出额外行：{out:?}");
        assert_eq!(lines[0], "月份\t金额");
        assert_eq!(lines[1], "1月 (含制表符)\t120 换行");
        assert_eq!(render_tsv(&[]), "");
    }
}
