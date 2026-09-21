//! XML 文本处理的共用辅助：属性拆分、标签内容抽取、实体转义。
//!
//! 这里的实现全部在**纯文本层面**工作，不解析成 XML 树——对 Office 文件的保真修改来说，
//! 「不改动未编辑部分的书写形态（命名空间前缀、属性顺序、空白）」本身就是需求，
//! 而任何「解析再序列化」的做法都会把整份文件重写一遍。

/// 把起始标签的内容切成 (属性名, 原始片段) 序列，保留原始书写形态（含引号风格与空白）。
pub fn split_attrs(inner: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let name = &inner[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
            }
        }
        if !name.is_empty() {
            out.push((name.to_string(), inner[name_start..i].to_string()));
        }
    }
    out
}

/// 取某个属性的值（去掉引号）；不存在返回 None。
pub fn attr_of<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, raw)| {
        let v = raw.split_once('=').map(|(_, v)| v).unwrap_or("");
        v.trim().trim_matches(['"', '\''])
    })
}

/// 取 `<tag ...>内容</tag>` 里的内容（`open` 含 `<`，如 `<v`）。
pub fn extract_tag(body: &str, open: &str, close: &str) -> Option<String> {
    let s = body.find(open)?;
    let gt = body[s..].find('>')? + s + 1;
    let e = body[gt..].find(close)? + gt;
    Some(body[gt..e].to_string())
}

/// 反转义 XML 文本实体（仅处理写入时会转义的那几个）。
pub fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// XML 文本转义。控制字符在 XML 1.0 里非法，会让整个文件打不开，所以剔除而不是写出去。
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// 找一个起始标签的 `>` 位置（相对 `at`），并判断是否自闭合。
pub fn start_tag_end(xml: &str, at: usize) -> Option<(usize, bool)> {
    let rest = &xml[at..];
    let gt = rest.find('>')?;
    let self_closing = xml[at..at + gt].ends_with('/');
    Some((gt, self_closing))
}

/// 扫描 `[from, to)` 区间里的标签起点（标签名 `name` 不含前导 `<`）。
///
/// 游标每次至少前进一个标签名的长度，**不会回退**——曾经写成相对偏移累加，
/// 导致同一位置被反复检查、陷入死循环，所以这里刻意用「绝对位置游标」。
pub fn find_tag_starts<F>(xml: &str, from: usize, to: usize, name: &str, mut visit: F)
where
    F: FnMut(usize) -> bool,
{
    let open = format!("<{name}");
    let hay = &xml[from..to];
    let mut cursor = 0usize;
    while cursor < hay.len() {
        let Some(rel) = hay[cursor..].find(&open) else {
            return;
        };
        let local = cursor + rel;
        cursor = local + open.len();
        let at = from + local;
        // 确认是 <name 而不是 <nameXxx：下一个字符必须是空白、'>' 或 '/'
        let next = xml[at + open.len()..].chars().next();
        if matches!(next, Some(c) if c.is_whitespace() || c == '>' || c == '/') && visit(at) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_preserve_raw_form_and_quote_style() {
        let attrs = split_attrs(r#" r="A1" s='5'  t="s""#);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0], ("r".into(), r#"r="A1""#.into()));
        assert_eq!(attrs[1], ("s".into(), r#"s='5'"#.into()));
        assert_eq!(attr_of(&attrs, "t"), Some("s"));
        assert_eq!(attr_of(&attrs, "missing"), None);
    }

    #[test]
    fn escaping_drops_illegal_control_chars() {
        assert_eq!(escape_text("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(escape_text("带\u{1}控制符"), "带控制符");
        assert_eq!(escape_text("保留\t制表\n换行"), "保留\t制表\n换行");
    }

    #[test]
    fn unescape_round_trips_escaped_text() {
        for raw in ["a<b>&c", "引号\"与'单引号'", "纯中文"] {
            assert_eq!(unescape(&escape_text(raw)), raw, "{raw}");
        }
    }

    #[test]
    fn extract_tag_reads_inner_text() {
        assert_eq!(
            extract_tag("<c><v>120</v></c>", "<v", "</v>"),
            Some("120".into())
        );
        assert_eq!(extract_tag("<c/>", "<v", "</v>"), None);
    }

    #[test]
    fn tag_scan_terminates_and_skips_longer_names() {
        // <rows 不该被当成 <row；<cols 不该被当成 <c
        let xml = "<worksheet><cols/><rows><row r='1'/><row r='2'/></rows></worksheet>";
        let mut rows = Vec::new();
        find_tag_starts(xml, 0, xml.len(), "row", |at| {
            rows.push(at);
            false
        });
        assert_eq!(rows.len(), 2, "应只命中两个 <row：{rows:?}");

        let mut cells = Vec::new();
        find_tag_starts(xml, 0, xml.len(), "c", |at| {
            cells.push(at);
            false
        });
        assert!(cells.is_empty(), "<cols 不该被当成 <c：{cells:?}");
    }

    #[test]
    fn tag_scan_returns_early_when_visit_asks_to_stop() {
        let xml = "<a x='1'/><a x='2'/><a x='3'/>";
        let mut seen = 0usize;
        find_tag_starts(xml, 0, xml.len(), "a", |_| {
            seen += 1;
            true // 第一次就要求停止
        });
        assert_eq!(seen, 1);
    }
}
