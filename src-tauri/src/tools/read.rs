//! read 工具：一次批量读取 1–20 个文件，带行号预览、UTF-16 转码、图片 DataURL 注入与 version token。
//! 图片以 extra_model_content（多模态）注入给模型，前端 outcome 中同样携带。

use super::pathutil;
use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

/// 支持视觉读取的图片扩展名 → MIME 映射。
const IMAGE_EXTS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];
/// 单张图片的体积上限（超过直接报错，防止撑爆上下文）。
const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;
/// 文本文件整读上限；超过则要求模型用 startLine/endLine 分段。
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
/// 主会话单次读取的行数预算（[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）：
/// 结构化读单次调用拉入主会话上下文的文本行数上限，与单文件默认窗口（2000 行）对齐——
/// 超过即拒绝并指引「调研派 explore 子代理 / 编辑用窗口分段」。子代理与任务运行豁免
/// （读代码是其本职）；图片不计行数（有独立 3MB 体积上限）。
pub const MAIN_READ_LINE_BUDGET: usize = 2000;

/// read 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 文件请求列表，1–20 项。
    #[serde(default)]
    files: Vec<FileReq>,
}

/// 单个文件的读取请求。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReq {
    /// 工作区相对路径。
    path: String,
    /// 起始行（1-based；负数 N 表示读末尾 N 行）。
    #[serde(default)]
    start_line: Option<i64>,
    /// 结束行（1-based，含端点）。
    #[serde(default)]
    end_line: Option<i64>,
}

/// read 工具：批量读取文件并返回带行号预览。
/// 入参为 files 数组（path + 可选行区间）；ReadOnly 分级、免审批。
/// 每个结果附 `version` token（内容指纹），是 edit 工具乐观并发校验的依据；
/// 图片文件不走文本通道，而是以 DataURL + 多模态内容双路交付。
pub struct ReadTool;

/// 按扩展名推断图片 MIME 类型；非图片返回 None。
pub fn media_type_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    IMAGE_EXTS.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

/// 探测字节流是否为 UTF-16 编码：有 BOM 直接判定；无 BOM 时用 NUL 字节密度启发式
///（UTF-16 编码 ASCII 文本时每字符含一个 0x00，密度显著高于 UTF-8）。
fn looks_utf16(bytes: &[u8]) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    // 无 BOM：NUL 字节密度启发式
    let sample = &bytes[..bytes.len().min(256)];
    let zeros = sample.iter().filter(|&&b| b == 0).count();
    zeros > sample.len() / 8
}

/// 按 BOM 判断字节序并解码 UTF-16；非法代理对以 U+FFFD 替换，绝不 panic。
fn decode_utf16(bytes: &[u8]) -> String {
    let (be, units): (bool, Vec<u8>) = if bytes.starts_with(&[0xFE, 0xFF]) {
        (true, bytes[2..].to_vec())
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (false, bytes[2..].to_vec())
    } else {
        (false, bytes.to_vec())
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

/// 文本解码统一入口：先做 UTF-16 探测，命中则转码，否则按 UTF-8 宽松解码（非法字节以替换符保留）。
pub fn read_text_content(bytes: &[u8]) -> String {
    if looks_utf16(bytes) {
        decode_utf16(bytes)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// 截取 `[start, end]` 行区间并加行号前缀（右对齐 6 位 + `| `），供模型精确引用行号。
/// 返回（格式化文本，总行数）；区间越界时返回空文本与总行数。
pub fn format_lines(text: &str, start: usize, end: usize) -> (String, usize) {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    let s = start.max(1);
    let e = end.min(total);
    if s > total || s > e {
        return (String::new(), total);
    }
    let mut out = String::new();
    for (i, l) in lines[s - 1..e].iter().enumerate() {
        out.push_str(&format!("{:>6}| {}", s + i, l.trim_end_matches('\n')));
        out.push('\n');
    }
    (out, total)
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> &'static str {
        "按行号读取工作区文件。单次最多批量 20 个文件；大文件用 startLine/endLine 分段（startLine 为负数 N 表示读末尾 N 行）。图片以视觉方式交付。每个结果携带 `version` 令牌，edit 工具必需。主会话单次读取总量受限（约 2000 行）：大范围代码调研请派 explore 子代理，只回传结论；主会话只为将要编辑的文件精读目标区段。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["files"],
  "properties": {
    "files": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["path"],
        "properties": {
          "path": {"type": "string", "description": "工作区相对路径"},
          "startLine": {"type": "integer", "description": "1-based；负数 N 表示读末尾 N 行"},
          "endLine": {"type": "integer", "description": "1-based，含端点"}
        }
      },
      "minItems": 1,
      "maxItems": 20
    }
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        if args.files.is_empty() || args.files.len() > 20 {
            return ToolOutcome::err("E_ARGS", "files 必须包含 1–20 个文件");
        }
        let roots = ctx.write_roots();
        let mut out_files = Vec::new();
        let mut images: Vec<crate::core::types::Content> = Vec::new();
        // 主会话读取预算累计（子代理/任务 runtime 不检查）
        let mut budget_used: usize = 0;

        for f in args.files {
            let resolved = match pathutil::resolve_read(&roots, &f.path) {
                Ok(p) => p,
                Err((c, m)) => return ToolOutcome::err(&c, m),
            };
            let meta = match std::fs::metadata(&resolved) {
                Ok(m) => m,
                Err(e) => return ToolOutcome::err("E_NOT_FOUND", format!("{}: {e}", f.path)),
            };
            if !meta.is_file() {
                return ToolOutcome::err("E_ARGS", format!("{} 不是常规文件", f.path));
            }
            let bytes = match std::fs::read(&resolved) {
                Ok(b) => b,
                Err(e) => return ToolOutcome::err("E_IO", format!("{}: {e}", f.path)),
            };

            // 图片分支
            if let Some(media) = media_type_of(&resolved) {
                if bytes.len() > MAX_IMAGE_BYTES {
                    return ToolOutcome::err("E_TOO_LARGE", "图片超过 3MB 上限");
                }
                let version = crate::util::crockford::version_token(&bytes);
                let b64 = {
                    // 手写 base64 编码（std 标准库没有现成的）
                    const T: &[u8] =
                        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
                    for chunk in bytes.chunks(3) {
                        let b = [
                            chunk[0],
                            *chunk.get(1).unwrap_or(&0),
                            *chunk.get(2).unwrap_or(&0),
                        ];
                        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
                        s.push(T[(n >> 18) as usize & 63] as char);
                        s.push(T[(n >> 12) as usize & 63] as char);
                        s.push(if chunk.len() > 1 {
                            T[(n >> 6) as usize & 63] as char
                        } else {
                            '='
                        });
                        s.push(if chunk.len() > 2 {
                            T[n as usize & 63] as char
                        } else {
                            '='
                        });
                    }
                    s
                };
                images.push(crate::core::types::Content::Image {
                    media_type: media.to_string(),
                    data: b64.clone(),
                });
                out_files.push(json!({
                    "path": f.path, "kind": "image", "media_type": media,
                    "data_url": format!("data:{media};base64,{b64}"), "version": version,
                }));
                continue;
            }

            // 文本分支
            if meta.len() > MAX_TEXT_BYTES && f.start_line.is_none() {
                return ToolOutcome::err(
                    "E_TOO_LARGE",
                    format!("{} 超过 1MB，请用 startLine/endLine 分段读取", f.path),
                );
            }
            let text = read_text_content(&bytes);
            let total_lines = text.split_inclusive('\n').count();
            let (start, end) = match f.start_line {
                Some(n) if n < 0 => {
                    let n = (-n) as usize;
                    let s = total_lines.saturating_sub(n) + 1;
                    (s, f.end_line.map(|e| e as usize).unwrap_or(total_lines))
                }
                Some(n) => (
                    n as usize,
                    f.end_line
                        .map(|e| e as usize)
                        .unwrap_or((n as usize).saturating_add(200).min(total_lines)),
                ),
                None => (1, total_lines.min(2000)),
            };
            let (content, total) = format_lines(&text, start, end);
            let version = crate::util::crockford::version_token(&bytes);
            // 主会话读取预算（[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）：
            // 超限整调用拒绝（原子性，不部分返回）；终结式指引与 E_TOOL_BLOCKED 同风格
            if ctx.rt.is_main_session {
                let end_clamped = end.min(total);
                budget_used += if end_clamped >= start {
                    end_clamped - start + 1
                } else {
                    0
                };
                if budget_used > MAIN_READ_LINE_BUDGET {
                    return ToolOutcome::err(
                        "E_READ_TOO_BROAD",
                        format!(
                            "主会话单次读取超预算（{budget_used} 行 > {MAIN_READ_LINE_BUDGET}）。此限制不因重试或改参数解除：大范围代码调研请派 explore 子代理（task 写明调研问题与文件线索）只回传结论；为编辑而读请用 startLine/endLine 小窗口只读目标区段（窗口读取同样取得 version token，edit 不受影响）。"
                        ),
                    );
                }
            }
            out_files.push(json!({
                "path": f.path, "kind": "text",
                "start_line": start, "end_line": end.min(total), "total_lines": total,
                "truncated": end.min(total) < total,
                "version": version,
                "content": content,
            }));
        }

        let mut out = ToolOutcome::ok(json!({ "files": out_files }));
        out.extra_model_content = images;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_formatting() {
        let text = "alpha\nbeta\ngamma\n";
        let (s, total) = format_lines(text, 2, 3);
        assert_eq!(total, 3);
        assert!(s.contains("     2| beta"));
        assert!(s.contains("     3| gamma"));
    }

    #[test]
    fn utf16_detection() {
        let le: Vec<u8> = {
            let mut v = vec![0xFF, 0xFE];
            for u in "hi 你".encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v
        };
        assert!(looks_utf16(&le));
        assert_eq!(read_text_content(&le), "hi 你");
        assert!(!looks_utf16(b"plain ascii text"));
    }

    #[test]
    fn image_media_type() {
        assert_eq!(media_type_of(Path::new("a/b.PNG")), Some("image/png"));
        assert_eq!(media_type_of(Path::new("a/b.rs")), None);
    }

    /// 构造（core, 主会话 runtime）：走 get_or_create_session（is_main_session = true）。
    fn make_ctx(
        ws: &tempfile::TempDir,
        dd: &tempfile::TempDir,
    ) -> (std::sync::Arc<crate::core::agent::AgentCore>, std::sync::Arc<crate::core::agent::SessionRuntime>) {
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "t",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        (core, rt)
    }

    fn ctx_for(
        core: std::sync::Arc<crate::core::agent::AgentCore>,
        rt: std::sync::Arc<crate::core::agent::SessionRuntime>,
    ) -> ToolCtx {
        ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// 主会话批量读取超预算 → E_READ_TOO_BROAD；同参数子代理豁免通过。
    #[tokio::test]
    async fn main_session_bulk_read_denied_but_sub_allowed() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, main_rt) = make_ctx(&ws, &dd);
        let sub = crate::core::agent::SessionRuntime::new_sub(&main_rt, "sub-1".to_string());
        for name in ["a.txt", "b.txt"] {
            std::fs::write(ws.path().join(name), "line\n".repeat(1500)).unwrap();
        }
        let args = serde_json::json!({"files":[{"path":"a.txt"},{"path":"b.txt"}]});
        let out = ReadTool.run(&ctx_for(core.clone(), main_rt.clone()), args.clone()).await;
        assert!(!out.ok, "{out:?}");
        assert_eq!(out.error.as_ref().unwrap().code, "E_READ_TOO_BROAD");
        assert!(out.error.as_ref().unwrap().message.contains("explore"));
        // 子代理豁免：读代码是其本职
        let out2 = ReadTool.run(&ctx_for(core, sub), args).await;
        assert!(out2.ok, "{out2:?}");
    }

    /// 主会话窗口读取（预算内）通过；超预算单文件整读被拒但窗口仍可用（edit 前置读不受影响）。
    #[tokio::test]
    async fn main_session_windowed_read_within_budget_passes() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, main_rt) = make_ctx(&ws, &dd);
        std::fs::write(ws.path().join("big.txt"), "line\n".repeat(3000)).unwrap();
        let ctx = ctx_for(core, main_rt);
        // 单文件整读：默认窗口 2000 行 = 预算上限，恰好放行（edit 前置读不受影响）
        let out = ReadTool.run(&ctx, serde_json::json!({"files":[{"path":"big.txt"}]})).await;
        assert!(out.ok, "{out:?}");
        // 窗口读取远小于预算
        let out2 = ReadTool
            .run(&ctx, serde_json::json!({"files":[{"path":"big.txt","startLine":1,"endLine":500}]}))
            .await;
        assert!(out2.ok, "{out2:?}");
    }
}
