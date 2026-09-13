//! 手写增量 SSE 解析器（零第三方依赖、纯函数可测，[docs/p0-plan](../../../docs/p0-plan.md) §4 实施时定稿）。
//! 处理 `event:` / `data:` / 注释行 / CRLF；空行标志事件结束；流结束时冲刷未下发事件。

/// 单个 SSE event：`event` 字段可缺省（None = 纯 data 事件），多行 data 以 '\n' 连接。
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    /// 事件类型名（`event:` 行内容），未声明则为 None
    pub event: Option<String>,
    /// 事件数据（`data:` 行拼接结果）
    pub data: String,
}

/// 增量 SSE 解析器：以字节缓冲吸收任意分片（TCP 撕裂安全），
/// 只在确认事件完整（遇空行）后下发，未完结名必须等到 `finish`。
#[derive(Default)]
pub struct SseParser {
    /// 尚未构成完整行的字节缓冲
    buf: Vec<u8>,
    /// 当前累积事件的 event 名
    cur_event: Option<String>,
    /// 当前累积事件的 data（多行以 '\n' 连接）
    cur_data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一个网络分片：逐行解析，完整事件写入 `out`；
    /// 半行/未完结事件留在缓冲，绝不提前下发（分片撕裂防护）。
    pub fn feed(&mut self, chunk: &[u8], out: &mut Vec<SseEvent>) {
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let mut line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            if line.ends_with('\r') {
                line.pop();
            }
            self.handle_line(&line, out);
        }
    }

    /// 流结束：把未被空行终结的挂起事件一并下发。
    pub fn finish(&mut self, out: &mut Vec<SseEvent>) {
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            self.handle_line(&rest, out);
        }
        self.dispatch(out);
    }

    fn handle_line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.cur_event = Some(rest.trim_start().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !self.cur_data.is_empty() {
                self.cur_data.push('\n');
            }
            self.cur_data
                .push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // 以 ":" 开头的行是注释，忽略；其余字段（id:/retry:）P0 不需要
    }

    /// 下发当前累积的事件并清空状态（遇空行或 finish 时调用）。
    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.cur_data.is_empty() && self.cur_event.is_none() {
            return;
        }
        out.push(SseEvent {
            event: self.cur_event.take(),
            data: std::mem::take(&mut self.cur_data),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&str]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            p.feed(c.as_bytes(), &mut out);
        }
        p.finish(&mut out);
        out
    }

    #[test]
    fn basic_events() {
        let evs = parse(&["event: foo\ndata: {\"a\":1}\n\ndata: two\n\n"]);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event.as_deref(), Some("foo"));
        assert_eq!(evs[0].data, "{\"a\":1}");
        assert_eq!(evs[1].event, None);
        assert_eq!(evs[1].data, "two");
    }

    #[test]
    fn crlf_and_split_chunks() {
        let evs = parse(&[
            "event: me",
            "ssage\r\ndata: hel",
            "lo\r\ndata: world\r\n\r\n",
        ]);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event.as_deref(), Some("message"));
        assert_eq!(evs[0].data, "hello\nworld");
    }

    #[test]
    fn comments_ignored_and_finish_flush() {
        let evs = parse(&[": keepalive\n", "data: tail"]);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "tail");
    }

    #[test]
    fn multiline_data() {
        let evs = parse(&["data: a\ndata: b\n\n"]);
        assert_eq!(evs[0].data, "a\nb");
    }
}
