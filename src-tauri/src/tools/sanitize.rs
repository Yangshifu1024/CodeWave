//! 子进程输出清洗（[docs/command-output-ansi-sanitize-plan](../../../docs/command-output-ansi-sanitize-plan.md)）：
//!
//! 1. **剥终端控制序列**：ECMA-48 的 CSI（`ESC [ … 终结字节`）、OSC（`ESC ] … BEL` 或 `ESC \`）、
//!    两字符转义（`ESC` + 单字节）统统丢弃；裸 `\r`（行内重绘，如进度条）也丢弃，
//!    只保留 `\n`、`\t` 与常规可打印字符（其余 C0/C1 控制字符一并丢弃）。
//!
//!    已知取舍：`\r` 只**丢弃**、不模拟「回到行首重写」（后者要把整行缓到换行才吐，会拖慢流式进度展示，
//!    也可能把长行攒在内存里）；因此进度条的多次重绘会在输出里连成一串（`10%20%100%`）。
//!    实测管道下的包管理器/测试运行器大多是带色而非带 `\r`，真正刷进度条的场景很少。
//! 2. **分块边界安全解码**：子进程输出按 4096 字节分块读取，多字节字符（`✓`、中文）常被拦腰切断——
//!    逐块 `from_utf8_lossy` 会把它变成 `�`/`?`。本类型把「不完整的多字节尾巴」缓存到下一块再解码，
//!    真非法字节仍按宽松解码兜底（替换符），不会吞字符。
//!
//! **本类型不是纯函数**：转义序列与多字节字符都可能跨块，状态必须跨 [`OutputSanitizer::push`] 保留。
//! 因此**每条输出流各持一个实例**（stdout / stderr 分开），绝不能跨流共用（会把两条流的半截状态拼在一起）。
//!
//! Non-goals（与方案文档一致）：不给子进程强设 `NO_COLOR` / `FORCE_COLOR`——抑制会改变用户显式依赖颜色的
//! 命令语义（如 `git diff --color=always` 的消费方），清洗比抑制更稳。

/// 转义序列解析状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    /// 常规文本
    #[default]
    Ground,
    /// 刚吃到 `ESC`
    Esc,
    /// 在 CSI 内（`ESC [`）
    Csi,
    /// 在 OSC 内（`ESC ]`，直到 BEL 或 ST）
    Osc,
    /// OSC 内刚吃到 `ESC`（可能是 ST 的前半 `ESC \`）
    OscEsc,
}

/// 不完整多字节尾巴的缓存上限（合法 UTF-8 前缀最长 3 字节，留点余量做防御）。
const MAX_PENDING: usize = 8;

/// 输出清洗器（见模块头注释：每条流一个实例，状态跨块保留）。
#[derive(Debug, Default)]
pub struct OutputSanitizer {
    state: State,
    /// 尚不能安全解码的尾部字节（被截断的多字节序列前缀）。
    pending: Vec<u8>,
}

impl OutputSanitizer {
    /// 新建（干净状态）。
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            pending: Vec::new(),
        }
    }

    /// 喂一块子进程输出，返回可安全展示 / 回喂模型的文本（可能为空串）。
    pub fn push(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let take = complete_prefix_len(&self.pending);
        let rest = self.pending.split_off(take);
        let bytes = std::mem::replace(&mut self.pending, rest);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.filter(&text)
    }

    /// 流结束：吐出残留（半截多字节按宽松解码兜底；半截转义序列直接丢弃），并复位状态。
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        let text = String::from_utf8_lossy(&rest).into_owned();
        let out = self.filter(&text);
        self.state = State::Ground;
        out
    }

    /// 按当前状态逐字符过滤（状态跨块保留）。
    fn filter(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            match self.state {
                State::Ground => match ch {
                    '\x1b' => self.state = State::Esc,
                    '\n' | '\t' => out.push(ch),
                    // 裸回车 = 行内重绘（进度条会把自己抹掉重画），丢了比留着干净
                    '\r' => {}
                    c if c.is_control() => {}
                    c => out.push(c),
                },
                State::Esc => {
                    self.state = match ch {
                        '[' => State::Csi,
                        ']' => State::Osc,
                        // 两字符转义（`ESC ( B`、`ESC =` 等）：吞掉 ESC 与这一个字符
                        _ => State::Ground,
                    };
                }
                State::Csi => {
                    // 参数字节 0x20..=0x3F，终结字节 0x40..=0x7E
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        self.state = State::Ground;
                    }
                }
                State::Osc => match ch {
                    '\x07' => self.state = State::Ground, // BEL 终结
                    '\x1b' => self.state = State::OscEsc, // 可能是 ST 前半
                    _ => {}
                },
                State::OscEsc => {
                    self.state = if ch == '\\' {
                        State::Ground
                    } else {
                        State::Osc
                    };
                }
            }
        }
        out
    }
}

/// 可安全解码的字节数：尾部若是被截断的多字节序列就留给下一块；真非法字节交宽松解码兜底。
fn complete_prefix_len(buf: &[u8]) -> usize {
    match std::str::from_utf8(buf) {
        Ok(_) => buf.len(),
        Err(e) => {
            // 尾部只是「不完整的多字节序列」且没攒太久 → 缓存到下一块；其余情况（真非法序列 /
            // 异常流攒得过久）整段交给 lossy，产出替换符。
            let incomplete = e.error_len().is_none();
            if incomplete && buf.len() - e.valid_up_to() <= MAX_PENDING {
                e.valid_up_to()
            } else {
                buf.len()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一行带色输出 → 干净的一行（vitest / Chalk 的典型形态）。
    #[test]
    fn strips_csi_color_sequences() {
        let mut s = OutputSanitizer::new();
        let out = s.push("\u{1b}[32m✓\u{1b}[39m done \u{1b}[90m(1 test)\u{1b}[0m\n".as_bytes());
        assert_eq!(out, "✓ done (1 test)\n");
        assert!(!out.contains('\u{1b}'));
    }

    /// 转义序列被分块切开：状态必须跨块保留（否则第一块会漏出 `ESC`）。
    #[test]
    fn keeps_escape_state_across_chunks() {
        let mut s = OutputSanitizer::new();
        assert_eq!(s.push(b"a\x1b"), "a");
        assert_eq!(s.push(b"[0m"), "");
        assert_eq!(s.push(b"b"), "b");
    }

    /// 多字节字符被 4096 边界切断：不得产出替换符（原缺陷：`✓` 变成 `?`）。
    #[test]
    fn multibyte_char_split_across_chunks_is_not_replaced() {
        let mut s = OutputSanitizer::new();
        let check = "✓".as_bytes(); // E2 9C 93
        let a = s.push(&[b'a', check[0], check[1]]);
        assert_eq!(a, "a");
        assert!(!a.contains('\u{FFFD}'));
        let b = s.push(&[check[2], b'b']);
        assert_eq!(b, "✓b");
        assert!(!b.contains('\u{FFFD}'));
    }

    /// 中文（3 字节）逐字节切块也不出替换符。
    #[test]
    fn chinese_text_split_byte_by_byte_survives() {
        let mut s = OutputSanitizer::new();
        let mut out = String::new();
        for byte in "中文输出".as_bytes() {
            out.push_str(&s.push(&[*byte]));
        }
        assert_eq!(out, "中文输出");
        assert!(!out.contains('\u{FFFD}'));
    }

    /// OSC（窗口标题 / 超链接）两种终结方式都要吞掉。
    #[test]
    fn strips_osc_with_bel_and_st_terminators() {
        let mut s = OutputSanitizer::new();
        assert_eq!(s.push(b"\x1b]0;title\x07after"), "after");
        assert_eq!(s.push(b"\x1b]8;;https://x\x1b\\link"), "link");
    }

    /// 裸回车（进度条行内重绘）丢掉，换行与制表保留。
    ///
    /// 已知取舍（见模块头）：不模拟「回到行首重写」，所以重绘内容会连成一串。
    #[test]
    fn drops_carriage_return_keeps_newline_and_tab() {
        let mut s = OutputSanitizer::new();
        assert_eq!(s.push(b"10%\r20%\r100%\n\tok"), "10%20%100%\n\tok");
        assert!(!s.push(b"").contains('\r'));
    }

    /// 其他控制字符（BEL / 退格 / 垂直制表）一并丢弃；真非法字节仍走宽松解码兜底。
    #[test]
    fn drops_other_control_chars_and_keeps_lossy_fallback() {
        let mut s = OutputSanitizer::new();
        assert_eq!(s.push(b"a\x07b\x08c\x0bd"), "abcd");
        assert_eq!(s.push(&[0xff, b'x']), "\u{FFFD}x");
    }

    /// finish：半截转义序列不得漏出；半截多字节按宽松解码兜底（不 panic、不吞日志）。
    #[test]
    fn finish_discards_partial_escape_and_flushes_partial_utf8() {
        let mut s = OutputSanitizer::new();
        assert_eq!(s.push(b"keep\x1b[3"), "keep");
        assert_eq!(s.finish(), ""); // 半截 CSI 整体丢弃
        assert_eq!(s.push(b"next"), "next"); // 状态已复位

        let mut s2 = OutputSanitizer::new();
        let check = "✓".as_bytes();
        assert_eq!(s2.push(&check[..2]), "");
        assert_eq!(s2.finish(), "\u{FFFD}"); // 残缺尾巴兜底成替换符
    }

    /// 空块与纯转义块都是空输出（调用方据此跳过空事件）。
    #[test]
    fn empty_and_escape_only_chunks_produce_nothing() {
        let mut s = OutputSanitizer::new();
        assert_eq!(s.push(b""), "");
        assert_eq!(s.push(b"\x1b[2K\x1b[1G"), "");
        assert_eq!(s.finish(), "");
    }
}
