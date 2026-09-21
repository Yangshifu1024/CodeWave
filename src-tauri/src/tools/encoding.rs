//! 文本编码探测（[docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)）。
//!
//! 为什么要做：导出成 `csv` / `tsv` 的表格在国内环境里很常见是 GBK 编码，按 UTF-8 硬读
//! 只会得到一片替换符乱码——而乱码比报错更糟，模型会拿它当真去分析。
//!
//! 顺序是 **BOM → UTF-8 → GBK → 放弃**：
//!
//! - BOM 是文件自己声明的编码，最可信，优先；
//! - 合法的 UTF-8 一律按 UTF-8（GBK 的中文字节几乎不可能是合法 UTF-8 序列，反过来则不然）；
//! - 都不是就试 GBK，但要过一道「像不像文本」的检查，免得把二进制垃圾解成汉字；
//! - 全都不行就按 UTF-8 宽松解码，并把编码标成 `unknown`，让调用方如实告诉使用者。

/// 解码结果：文本 + 实际用的编码名。
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// 解码后的文本。
    pub text: String,
    /// 实际采用的编码；`unknown` 表示没认出来（文本是宽松解码的结果，可能含替换符）。
    pub encoding: &'static str,
}

/// GBK 解码后允许的替换符比例上限。
///
/// GBK 的字节空间很大，随便一段二进制几乎都能「解出」一些汉字；但解不出合法序列的地方
/// 会变成替换符。用替换符占比当门槛，把「明显不是 GBK 文本」的东西挡在外面。
const MAX_REPLACEMENT_RATIO: f64 = 0.02;

/// 探测并解码：BOM → UTF-8 → GBK → 宽松 UTF-8。
pub fn decode_text(bytes: &[u8]) -> Decoded {
    // 1. UTF-8 BOM
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Decoded {
            text: String::from_utf8_lossy(rest).into_owned(),
            encoding: "utf-8-bom",
        };
    }
    // 2. UTF-16 BOM（顺序在 UTF-8 之后无所谓，BOM 互不重叠）
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Decoded {
            text: decode_utf16(bytes),
            encoding: "utf-16",
        };
    }
    // 3. 合法 UTF-8：不必再猜
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Decoded {
            text: s.to_string(),
            encoding: "utf-8",
        };
    }
    // 4. GBK
    let (cow, _actual, had_errors) = encoding_rs::GBK.decode(bytes);
    if is_plausible(&cow, had_errors) {
        return Decoded {
            text: cow.into_owned(),
            encoding: "gbk",
        };
    }
    // 5. 放弃：宽松解码如实交出，由调用方提示
    Decoded {
        text: String::from_utf8_lossy(bytes).into_owned(),
        encoding: "unknown",
    }
}

/// GBK 结果是否可信：没有硬错误，或者替换符占比很低。
fn is_plausible(text: &str, had_errors: bool) -> bool {
    if !had_errors {
        return true;
    }
    let total = text.chars().count();
    if total == 0 {
        return false;
    }
    let bad = text.chars().filter(|c| *c == '\u{FFFD}').count();
    (bad as f64 / total as f64) <= MAX_REPLACEMENT_RATIO
}

/// 按 BOM 判断字节序并解码 UTF-16；非法代理对以 U+FFFD 替换，绝不 panic。
pub fn decode_utf16(bytes: &[u8]) -> String {
    let (be, units): (bool, &[u8]) = if bytes.starts_with(&[0xFE, 0xFF]) {
        (true, &bytes[2..])
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (false, &bytes[2..])
    } else {
        (false, bytes)
    };
    let mut pairs = Vec::with_capacity(units.len() / 2);
    let mut i = 0;
    while i + 1 < units.len() {
        let v = if be {
            u16::from_be_bytes([units[i], units[i + 1]])
        } else {
            u16::from_le_bytes([units[i], units[i + 1]])
        };
        pairs.push(v);
        i += 2;
    }
    char::decode_utf16(pairs)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// 探测字节流是否为 UTF-16 编码：有 BOM 直接判定；无 BOM 时用 NUL 字节密度启发式
///（UTF-16 编码 ASCII 文本时每字符含一个 0x00，密度显著高于 UTF-8）。
pub fn looks_utf16(bytes: &[u8]) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    let sample = &bytes[..bytes.len().min(256)];
    let zeros = sample.iter().filter(|&&b| b == 0).count();
    zeros > sample.len() / 8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GBK 编码的「月份,金额\n1月,120」（用编码器生成，不是手写字节）。
    fn gbk(text: &str) -> Vec<u8> {
        let (bytes, _, had_errors) = encoding_rs::GBK.encode(text);
        assert!(!had_errors, "测试样本应能编成 GBK");
        bytes.into_owned()
    }
    #[test]
    fn utf8_without_bom_stays_utf8() {
        let d = decode_text("月份,金额\n1月,120\n".as_bytes());
        assert_eq!(d.encoding, "utf-8");
        assert_eq!(d.text, "月份,金额\n1月,120\n");
    }

    #[test]
    fn utf8_bom_is_stripped_and_reported() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("a,b\n".as_bytes());
        let d = decode_text(&bytes);
        assert_eq!(d.encoding, "utf-8-bom");
        // BOM 不能留在文本里（否则第一列的名字会多一个看不见的字符）
        assert_eq!(d.text, "a,b\n");
    }

    #[test]
    fn gbk_csv_is_decoded_instead_of_becoming_mojibake() {
        let d = decode_text(&gbk("月份,金额\n1月,120\n2月,150\n"));
        assert_eq!(d.encoding, "gbk");
        assert_eq!(d.text, "月份,金额\n1月,120\n2月,150\n");
        assert!(!d.text.contains('\u{FFFD}'), "不该有替换符：{}", d.text);
    }

    #[test]
    fn utf16_with_bom_is_decoded() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "hi 你".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let d = decode_text(&bytes);
        assert_eq!(d.encoding, "utf-16");
        assert_eq!(d.text, "hi 你");
    }

    #[test]
    fn undecipherable_bytes_fall_back_to_lossy_and_say_unknown() {
        // 随机二进制：既不是合法 UTF-8，也不像 GBK 文本
        let junk: Vec<u8> = (0..512u32)
            .map(|i| (i.wrapping_mul(97) % 256) as u8)
            .collect();
        let d = decode_text(&junk);
        assert_eq!(d.encoding, "unknown");
        assert!(!d.text.is_empty());
    }

    /// 合法 UTF-8 优先于 GBK：不能因为「GBK 也能解出来」就把正常文件解错。
    #[test]
    fn valid_utf8_wins_over_gbk() {
        let zh = "第一行\n第二行\n";
        assert_eq!(decode_text(zh.as_bytes()).encoding, "utf-8");
    }

    #[test]
    fn replacement_ratio_gate_rejects_noisy_gbk() {
        // 全是解不出来的字节 → 不算可信
        assert!(!is_plausible("\u{FFFD}\u{FFFD}\u{FFFD}", true));
        // 零星替换符（长文本里混几个坏字节，真实文件里很常见）→ 可以接受
        let mostly_ok = format!("{}文本正常", "行内容很长的正常文本。".repeat(6));
        assert!(
            is_plausible(&format!("{mostly_ok}\u{FFFD}"), true),
            "占比低于 2% 应放行"
        );
        // 替换符成片（>2%）→ 拒，宁可标成 unknown 也不拿乱码当原文
        let noisy = format!("{}{}", "\u{FFFD}".repeat(5), "正".repeat(100));
        assert!(!is_plausible(&noisy, true));
        // 没有硬错误时无条件可信（had_errors = false 说明每个字节都解出来了）
        assert!(is_plausible("随便什么", false));
    }
}
