//! 双通道结果：模型侧瘦身（[docs/p0-plan](../../../docs/p0-plan.md) §6.2）。read 不压缩（保留行号）；其余头尾截断。

use crate::tools::ToolKind;

/// 模型侧保留的头部字节数。
pub const HEAD_BYTES: usize = 4 * 1024;
/// 模型侧保留的尾部字节数。
pub const TAIL_BYTES: usize = 8 * 1024;

/// 把工具结果压缩为模型可读文本：失败只留错误行；read 保持全量；
/// command 已在工具内语义化瘦身故原样透传；其余按 head/tail 字节截断。
pub fn compact_for_model(
    kind: ToolKind,
    name: &str,
    outcome: &crate::tools::ToolOutcome,
) -> String {
    // command（ally 移植）：工具内已做过语义化瘦身（尾部 + 信号行 + spill 路径），
    // 原样透传不再做字节级切割；且失败时 data 仍携带 output/exit_code——
    // 此前非零退出只回错误行，整个构建失败输出全部丢失。
    if name == "command" {
        let mut s = String::new();
        if let Some(e) = &outcome.error {
            s.push_str(&format!("[error {}: {}]\n", e.code, e.message));
        }
        if !outcome.data.is_null() {
            s.push_str(&outcome.data.to_string());
        }
        return s;
    }
    if !outcome.ok {
        if let Some(e) = &outcome.error {
            return format!("[error {}: {}]", e.code, e.message);
        }
        return "[error] unknown failure".into();
    }
    // read 保持完整（edit 需要对着行号操作）
    if kind == ToolKind::ReadOnly && name == "read" {
        return outcome.data.to_string();
    }
    let raw = outcome.data.to_string();
    truncate_head_tail(&raw, HEAD_BYTES, TAIL_BYTES)
}

/// 通用头尾截断：超长时保留前 head 字节 + 后 tail 字节，中间以截断提示衔接（UTF-8 边界宽松解码）。
pub fn truncate_head_tail(s: &str, head: usize, tail: usize) -> String {
    let b = s.as_bytes();
    if b.len() <= head + tail {
        return s.to_string();
    }
    let h = String::from_utf8_lossy(&b[..head]).into_owned();
    let t = String::from_utf8_lossy(&b[b.len() - tail..]).into_owned();
    format!("{h}\n…[已截断 {} 字节]…\n{t}", b.len() - head - tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolOutcome;

    #[test]
    fn read_not_compacted() {
        let out = ToolOutcome::ok(serde_json::json!({"content": "x".repeat(20_000)}));
        let s = compact_for_model(ToolKind::ReadOnly, "read", &out);
        assert_eq!(s.len(), out.data.to_string().len());
    }

    #[test]
    fn command_passes_through_and_keeps_output_on_error() {
        // command 在工具内已语义化瘦身：成功原样透传不二次切割；非零退出时 data 仍携带 output
        let out =
            ToolOutcome::ok(serde_json::json!({"output": "y".repeat(50_000), "exit_code": 0}));
        let s = compact_for_model(ToolKind::ReadOnly, "command", &out);
        assert_eq!(s, out.data.to_string());

        let err = ToolOutcome {
            ok: false,
            data: serde_json::json!({"exit_code": 3, "output": "build failed here"}),
            error: Some(crate::tools::ToolError::new("E_EXIT_CODE", "命令退出码 3")),
            warnings: Vec::new(),
            extra_model_content: Vec::new(),
        };
        let s = compact_for_model(ToolKind::ReadOnly, "command", &err);
        assert!(s.contains("[error E_EXIT_CODE: 命令退出码 3]"));
        assert!(
            s.contains("build failed here"),
            "非零退出的输出必须保留给模型"
        );
    }

    #[test]
    fn other_tools_still_truncated() {
        let out = ToolOutcome::ok(serde_json::json!({"output": "y".repeat(50_000)}));
        let s = compact_for_model(ToolKind::ReadOnly, "service", &out);
        assert!(s.len() < 20_000);
        assert!(s.contains("已截断"));
    }

    #[test]
    fn error_mapping() {
        let out = ToolOutcome::err("E_PATH_OUTSIDE", "越界");
        assert_eq!(
            compact_for_model(ToolKind::FileWrite, "edit", &out),
            "[error E_PATH_OUTSIDE: 越界]"
        );
    }
}
