//! PDF 文字提取。
//!
//! ## 为什么必须做崩溃隔离
//!
//! 所用的提取库有一批**尚未修复**的崩溃记录：畸形文件、内联图像、某些编码的
//! 中文字体都会让它 panic。这些文件用户随时可能碰到。如果不隔离，一次崩溃会带着
//! 整个命令线程一起完蛋——用户看到的是「应用出错了」，而不是「这份 PDF 读不了」。
//!
//! 所以调用一律包在 `catch_unwind` 里，崩溃时返回一条能理解的错误。
//!
//! ## 扫描件为什么读不出字
//!
//! 扫描件的页面其实是图片，没有文字层。任何「提取文字」的方案对它的输出都是空的——
//! 这不是缺陷，是这类文件的性质。所以这里明确识别并给出说明，而不是返回一片空白
//! 让模型和用户都摸不着头脑。

/// 单次请求的页数上限。
pub const MAX_PAGES_PER_CALL: usize = 20;
/// 超过这个页数就必须显式指定页码，不能默认整份返回。
pub const MUST_SPECIFY_PAGES_THRESHOLD: usize = 10;
/// 判定为扫描件的阈值：每页平均可打印字符数低于这个值。
pub const SCANNED_CHARS_PER_PAGE: usize = 20;

/// 页码区间（1-based，含端点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRange {
    pub first: usize,
    /// `None` 表示「到最后一页」（开放区间，如 `5-`）。
    pub last: Option<usize>,
}

/// 解析页码写法：`3`、`1-10`、`5-`。非法返回 None。
pub fn parse_pages(raw: &str) -> Option<PageRange> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix('-') {
        let first: usize = rest.parse().ok()?;
        if first == 0 || rest.is_empty() {
            return None;
        }
        return Some(PageRange { first, last: None });
    }
    match s.split_once('-') {
        None => {
            let p: usize = s.parse().ok()?;
            if p == 0 {
                return None;
            }
            Some(PageRange {
                first: p,
                last: Some(p),
            })
        }
        Some((a, b)) => {
            let first: usize = a.parse().ok()?;
            let last: usize = b.parse().ok()?;
            if first == 0 || last == 0 || last < first {
                return None;
            }
            Some(PageRange {
                first,
                last: Some(last),
            })
        }
    }
}

/// 校验页码参数并给出页数（`total` 为总页数）；返回错误信息或要返回的页码列表。
pub fn resolve_pages(range: Option<PageRange>, total: usize) -> Result<Vec<usize>, String> {
    let range = match range {
        Some(r) => r,
        None => {
            if total > MUST_SPECIFY_PAGES_THRESHOLD {
                return Err(format!(
                    "这份 PDF 有 {total} 页，超过 {MUST_SPECIFY_PAGES_THRESHOLD} 页时必须指定要看哪些页（例如 pages=\"1-5\"），以免一次返回过多内容。"
                ));
            }
            PageRange {
                first: 1,
                last: Some(total),
            }
        }
    };
    let last = range.last.unwrap_or(total).min(total);
    if range.first > total {
        return Err(format!(
            "指定的起始页 {} 超出了总页数 {total}",
            range.first
        ));
    }
    let count = last.saturating_sub(range.first) + 1;
    if count > MAX_PAGES_PER_CALL {
        return Err(format!(
            "一次最多读取 {MAX_PAGES_PER_CALL} 页，本次请求了 {count} 页，请缩小范围。"
        ));
    }
    Ok((range.first..=last).collect())
}

/// 提取每一页的文字（崩溃隔离）。
///
/// 提取库的崩溃只影响这一处调用，不会带走调用方。
pub fn extract_pages(bytes: &[u8]) -> Result<Vec<String>, String> {
    let owned = bytes.to_vec();
    let outcome = std::panic::catch_unwind(move || {
        pdf_extract::extract_text_from_mem_by_pages(&owned)
    });
    match outcome {
        Ok(Ok(pages)) => Ok(pages),
        Ok(Err(e)) => Err(format!(
            "无法解析这份 PDF：{e}。文件可能已损坏，或使用了本应用尚未支持的格式。"
        )),
        Err(_) => Err(
            "解析这份 PDF 时遇到本应用无法处理的内容（文件可能有损坏）。其余功能不受影响，可以改用其它方式查看。"
                .to_string(),
        ),
    }
}

/// 判断是否是扫描件（页面其实是图片、没有文字层）。
pub fn looks_scanned(pages: &[String]) -> bool {
    if pages.is_empty() {
        return false;
    }
    let total_chars: usize = pages
        .iter()
        .map(|p| p.chars().filter(|c| !c.is_whitespace()).count())
        .sum();
    total_chars / pages.len() < SCANNED_CHARS_PER_PAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_page_range_forms() {
        assert_eq!(
            parse_pages("3"),
            Some(PageRange {
                first: 3,
                last: Some(3)
            })
        );
        assert_eq!(
            parse_pages("1-10"),
            Some(PageRange {
                first: 1,
                last: Some(10)
            })
        );
        assert_eq!(
            parse_pages("5-"),
            Some(PageRange {
                first: 5,
                last: None
            })
        );
        assert_eq!(parse_pages(" 2-4 ").unwrap().first, 2);
        // 非法
        for bad in ["", "0", "3-1", "a", "-3", "1-a", "0-5"] {
            assert_eq!(parse_pages(bad), None, "{bad} 应判为非法");
        }
    }

    #[test]
    fn long_pdf_requires_explicit_pages() {
        let err = resolve_pages(None, 30).unwrap_err();
        assert!(err.contains("30 页"), "{err}");
        assert!(err.contains("pages="), "{err}");
        // 短的可以默认整份
        assert_eq!(resolve_pages(None, 3).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn page_count_is_capped() {
        let err = resolve_pages(parse_pages("1-25"), 30).unwrap_err();
        assert!(err.contains("最多读取 20 页"), "{err}");
        assert_eq!(resolve_pages(parse_pages("1-20"), 30).unwrap().len(), 20);
    }

    #[test]
    fn open_ended_range_is_clamped_to_total() {
        // 5- 在总共 8 页的文档上应给出 5,6,7,8
        assert_eq!(
            resolve_pages(parse_pages("5-"), 8).unwrap(),
            vec![5, 6, 7, 8]
        );
        // 超出总页数的起始页要报错
        let err = resolve_pages(parse_pages("9-"), 8).unwrap_err();
        assert!(err.contains("超出"), "{err}");
    }

    #[test]
    fn scanned_detection_uses_per_page_average() {
        // 每页几乎没字 → 判为扫描件
        let sparse = vec!["1".to_string(), "".to_string(), "  ".to_string()];
        assert!(looks_scanned(&sparse));

        // 真实正文的一页通常几百到几千字，当然不算扫描件
        let page = "这是一段正常的正文内容，长度足以代表真实文档的一页。".repeat(10);
        assert!(!looks_scanned(&vec![page.clone(); 3]));

        // 边界：跟阈值同一量级。阈值是「几乎空白」，不是「文字少」：
        // 一页仅一两个字符才算扫描件。
        let barely = vec!["标题".to_string()]; // 2 字
        assert!(looks_scanned(&barely), "几乎空白的页应判为扫描件");
        let a_line = vec!["附件一：说明".to_string(); 1]; // 6 字，仍远低于阈值
        assert!(looks_scanned(&a_line));

        // 长文档里夹杂一两页空白不应当成扫描件（用的是全篇平均值）
        let mut mixed = vec![page.clone(); 20];
        mixed.push(String::new());
        mixed.push("目录".to_string());
        assert!(!looks_scanned(&mixed), "整体有正文就不算扫描件");

        // 空输入不算扫描件（没有依据判断）
        assert!(!looks_scanned(&[]));
    }

    #[test]
    fn malformed_pdf_returns_error_instead_of_panicking() {
        // 乱码输入：必须返回错误，绝不能崩
        let err = extract_pages("这根本不是 PDF".as_bytes()).unwrap_err();
        assert!(!err.is_empty());
        // 空输入同样如此
        assert!(extract_pages(b"").is_err());
    }
}
