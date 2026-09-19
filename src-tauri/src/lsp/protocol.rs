//! LSP 基础协议：`Content-Length` framing 的编解码。
//!
//! **分片安全是硬约束**——绝不允许假设「一次 read 得到完整帧」（anthropic SSE 曾被分片撕裂，
//! 同类坑由本模块的单测与眼冒金星的集成测试共同守护）。所有读路径都必须经 [`FrameReader`]
//! 累积缓冲，凑齐整帧才解析。

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 单帧消息体上限（64MB）：超限报错而不是按声明长度分配内存。
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// 头部区块上限：一直找不到 `\r\n\r\n` 说明对端吐的不是 LSP 协议（畸形流保护）。
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// 协议层错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// 底层 IO 失败
    Io(String),
    /// 头部非法（含缺 `Content-Length`）
    BadHeader(String),
    /// 消息体不是合法 JSON
    BadJson(String),
    /// 声明的帧长超过 [`MAX_FRAME_BYTES`]（携带声明值）
    FrameTooLarge(usize),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Io(e) => write!(f, "LSP 连接 IO 错误：{e}"),
            ProtocolError::BadHeader(e) => write!(f, "LSP 头部非法：{e}"),
            ProtocolError::BadJson(e) => write!(f, "LSP 消息体不是合法 JSON：{e}"),
            ProtocolError::FrameTooLarge(n) => {
                write!(f, "LSP 帧长 {n} 超过上限 {MAX_FRAME_BYTES}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

/// 把一条 JSON-RPC 消息编码为 `Content-Length` 帧字节。
pub fn encode_message(v: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// 写出一条 `Content-Length` 帧（写完即 flush，避免对端等到超时）。
pub async fn write_message<W: AsyncWrite + Unpin>(
    w: &mut W,
    v: &Value,
) -> Result<(), ProtocolError> {
    let bytes = encode_message(v);
    w.write_all(&bytes)
        .await
        .map_err(|e| ProtocolError::Io(e.to_string()))?;
    w.flush()
        .await
        .map_err(|e| ProtocolError::Io(e.to_string()))?;
    Ok(())
}

/// 分片安全的帧累积器：喂入任意粒度的字节，凑齐整帧才吐消息。
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// 空累积器。
    pub fn new() -> Self {
        FrameReader { buf: Vec::new() }
    }

    /// 追加刚读到的字节。
    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// 缓冲区是否为空（判断「EOF 是否发生在帧中途」用）。
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 尝试取出一条完整帧；字节不足返回 `None`（**不是错误**）。
    ///
    /// 解析失败时会把已消费的头部丢弃并返回错误，避免同一条坏帧被反复尝试。
    pub fn try_next(&mut self) -> Option<Result<Value, ProtocolError>> {
        let (hoff, sep) = match header_end(&self.buf) {
            Some(x) => x,
            None => {
                if self.buf.len() > MAX_HEADER_BYTES {
                    self.buf.clear();
                    return Some(Err(ProtocolError::BadHeader(
                        "头部超过 64KB 仍未出现分隔符（非 LSP 数据流？）".into(),
                    )));
                }
                return None;
            }
        };
        let header = String::from_utf8_lossy(&self.buf[..hoff]).into_owned();
        let mut len: Option<usize> = None;
        for line in header.split('\n') {
            let line = line.trim_end_matches('\r');
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            if !k.trim().eq_ignore_ascii_case("content-length") {
                continue;
            }
            match v.trim().parse::<usize>() {
                Ok(n) => len = Some(n),
                Err(_) => {
                    self.buf.drain(..hoff + sep);
                    return Some(Err(ProtocolError::BadHeader(format!(
                        "Content-Length 非法：{}",
                        v.trim()
                    ))));
                }
            }
        }
        let Some(len) = len else {
            self.buf.drain(..hoff + sep);
            return Some(Err(ProtocolError::BadHeader(
                "缺少 Content-Length 头".into(),
            )));
        };
        if len > MAX_FRAME_BYTES {
            self.buf.drain(..hoff + sep);
            return Some(Err(ProtocolError::FrameTooLarge(len)));
        }
        if self.buf.len() < hoff + sep + len {
            return None;
        }
        let body = self.buf[hoff + sep..hoff + sep + len].to_vec();
        self.buf.drain(..hoff + sep + len);
        Some(match serde_json::from_slice::<Value>(&body) {
            Ok(v) => Ok(v),
            Err(e) => Err(ProtocolError::BadJson(format!(
                "{e}（前 120 字节：{}）",
                String::from_utf8_lossy(&body[..body.len().min(120)])
            ))),
        })
    }
}

/// 从 reader 读下一条消息：`Ok(None)` = 流正常结束（对端退出）。
///
/// 内部循环喂 [`FrameReader`]，单次 read 拿到半帧/两条帧都正确处理。
pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
    frame: &mut FrameReader,
) -> Result<Option<Value>, ProtocolError> {
    loop {
        if let Some(res) = frame.try_next() {
            return res.map(Some);
        }
        let mut chunk = [0u8; 8192];
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| ProtocolError::Io(e.to_string()))?;
        if n == 0 {
            if frame.is_empty() {
                return Ok(None);
            }
            return Err(ProtocolError::Io("LSP 连接在帧中途关闭".into()));
        }
        frame.extend(&chunk[..n]);
    }
}

/// 定位头部结束位置，返回 `(头部长度, 分隔符长度)`；同时容忍严谨的 `\r\n\r\n` 与宽松的 `\n\n`。
fn header_end(buf: &[u8]) -> Option<(usize, usize)> {
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// 按预设分片吐字节的 reader：精确模拟「一次 read 只拿到半帧」。
    struct ChunkReader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl ChunkReader {
        fn new(chunks: Vec<&[u8]>) -> Self {
            ChunkReader {
                chunks: chunks.into_iter().map(|c| c.to_vec()).collect(),
            }
        }
    }

    impl AsyncRead for ChunkReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.chunks.pop_front() {
                None => Poll::Ready(Ok(())), // EOF
                Some(c) => {
                    let n = c.len().min(buf.remaining());
                    buf.put_slice(&c[..n]);
                    if n < c.len() {
                        self.chunks.push_front(c[n..].to_vec());
                    }
                    Poll::Ready(Ok(()))
                }
            }
        }
    }

    #[tokio::test]
    async fn message_split_into_three_reads() {
        let m1 = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let m2 = json!({"jsonrpc":"2.0","method":"initialized","params":{}});
        let f1 = encode_message(&m1);
        let f2 = encode_message(&m2);
        let (a, b, c) = (f1.len() / 3, f1.len() / 3 * 2, f1.len());
        let mut r = ChunkReader::new(vec![&f1[..a], &f1[a..b], &f1[b..c], &f2]);
        let mut fr = FrameReader::new();
        assert_eq!(read_message(&mut r, &mut fr).await.unwrap().unwrap(), m1);
        assert_eq!(read_message(&mut r, &mut fr).await.unwrap().unwrap(), m2);
        assert!(read_message(&mut r, &mut fr).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn two_messages_in_one_write() {
        let m1 = json!({"jsonrpc":"2.0","id":7,"result":null});
        let m2 = json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics"});
        let mut glued = encode_message(&m1);
        glued.extend(encode_message(&m2));
        let mut r = ChunkReader::new(vec![&glued]);
        let mut fr = FrameReader::new();
        assert_eq!(read_message(&mut r, &mut fr).await.unwrap().unwrap(), m1);
        assert_eq!(read_message(&mut r, &mut fr).await.unwrap().unwrap(), m2);
    }

    #[tokio::test]
    async fn missing_content_length_is_bad_header() {
        let mut r = ChunkReader::new(vec![b"Content-Type: application/vscode-jsonrpc\r\n\r\n{}"]);
        let mut fr = FrameReader::new();
        let err = read_message(&mut r, &mut fr).await.unwrap_err();
        assert!(matches!(err, ProtocolError::BadHeader(_)), "{err:?}");
    }

    #[tokio::test]
    async fn invalid_json_is_bad_json() {
        let body = b"{ not json";
        let mut raw = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        raw.extend_from_slice(body);
        let mut r = ChunkReader::new(vec![&raw]);
        let mut fr = FrameReader::new();
        let err = read_message(&mut r, &mut fr).await.unwrap_err();
        assert!(matches!(err, ProtocolError::BadJson(_)), "{err:?}");
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_without_buffering() {
        let mut fr = FrameReader::new();
        fr.extend(format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1).as_bytes());
        match fr.try_next() {
            Some(Err(ProtocolError::FrameTooLarge(n))) => assert_eq!(n, MAX_FRAME_BYTES + 1),
            other => panic!("应报超限，实得 {other:?}"),
        }
        // 坏帧已被丢弃，不残留在缓冲里
        assert!(fr.is_empty());
    }

    #[test]
    fn eof_mid_frame_is_an_error_not_silent_eof() {
        let mut fr = FrameReader::new();
        fr.extend(b"Content-Length: 100\r\n\r\n{\"a\"");
        assert!(fr.try_next().is_none());
        assert!(!fr.is_empty());
    }

    #[test]
    fn encode_is_byte_stable() {
        let v = json!({"jsonrpc":"2.0","id":1});
        let bytes = encode_message(&v);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("Content-Length: "));
        assert!(s.contains("\r\n\r\n"));
    }
}
