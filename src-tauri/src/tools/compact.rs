//! 双通道结果：模型侧瘦身（[docs/p0-plan](../../../docs/p0-plan.md) §6.2）。read 不压缩（保留行号）；其余头尾截断。
//!
//! 图片例外（会话 6bca80f4 反复被上游 400 的根因）：read 读到图片时 `outcome.data` 里带
//! `data_url`（整段 base64；实测 944KB 截图 → 1,259,669 字符）。模型侧此前直接照搬这份 data，
//! 那段编码于是作为**普通文本**出网：上游按约 1.6 字符/token 计费（≈79 万 token），而本地估算按
//! 4 字符/token 只算 34 万 —— 占比长期低于自动压缩阈值，上下文一路顶到上游上限并 400。
//! 现在图片走 `extra_model_content` 独立通道（batch.rs 收集进工具消息 → 出网副本层
//! `agent/stream.rs::route_tool_images` 搬进紧随其后的用户消息），模型侧文本只留一行说明；
//! 无图片的 read 行为逐字节不变（不裁剪）。

use crate::tools::ToolKind;
use serde_json::Value;

/// 模型侧保留的头部字节数。
pub const HEAD_BYTES: usize = 4 * 1024;
/// 模型侧保留的尾部字节数。
pub const TAIL_BYTES: usize = 8 * 1024;

/// 把工具结果压缩为模型可读文本：失败只留错误行；read 保持全量；
/// command 已在工具内语义化瘦身故原样透传；其余按 head/tail 字节截断。
/// `vision` = 会话生效模型是否勾选「支持图片输入」：决定图片说明怎么写
///（勾选时图片随后以独立消息送达；未勾选则不发，说明里如实写明模型看不到）。
pub fn compact_for_model(
    kind: ToolKind,
    name: &str,
    outcome: &crate::tools::ToolOutcome,
    vision: bool,
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
    // read / batch_read（兼容别名，同一实现）保持完整（edit 需要对着行号操作）；
    // 图片条目的 base64 例外，见 read_model_text
    if kind == ToolKind::ReadOnly && matches!(name, "read" | "batch_read") {
        return read_model_text(outcome, vision);
    }
    let raw = outcome.data.to_string();
    truncate_head_tail(&raw, HEAD_BYTES, TAIL_BYTES)
}

/// read 的模型侧文本：剥掉图片条目的 `data_url`（整段 base64）并换成一行说明。
/// 无图片时与旧行为逐字节一致（read 不裁剪）；有图片时只改图片条目，
/// path / media_type / version 等字段原样保留。
fn read_model_text(outcome: &crate::tools::ToolOutcome, vision: bool) -> String {
    let has_image = outcome
        .data
        .get("files")
        .and_then(Value::as_array)
        .is_some_and(|fs| {
            fs.iter()
                .any(|f| f.get("kind").and_then(Value::as_str) == Some("image"))
        });
    if !has_image {
        return outcome.data.to_string();
    }
    let note = if vision {
        "[图片数据不在此文本中：图片随下一条图片消息发送]"
    } else {
        "[图片数据不在此文本中：当前模型未开启图片输入，无法查看图片内容]"
    };
    let mut data = outcome.data.clone();
    if let Some(files) = data.get_mut("files").and_then(Value::as_array_mut) {
        for f in files.iter_mut() {
            if f.get("kind").and_then(Value::as_str) != Some("image") {
                continue;
            }
            if let Some(obj) = f.as_object_mut() {
                obj.remove("data_url");
                obj.insert("image_note".into(), Value::String(note.into()));
            }
        }
    }
    data.to_string()
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
        let s = compact_for_model(ToolKind::ReadOnly, "read", &out, true);
        assert_eq!(s.len(), out.data.to_string().len());
    }

    /// 回归钉（会话 6bca80f4）：读图结果的模型侧文本绝不能再带上那段 base64。
    #[test]
    fn read_image_result_drops_base64_for_model() {
        let out = ToolOutcome::ok(serde_json::json!({
            "files": [{
                "path": "a.png",
                "kind": "image",
                "media_type": "image/png",
                "data_url": format!("data:image/png;base64,{}", "A".repeat(4000)),
                "version": "V1",
            }]
        }));
        let with_vision = compact_for_model(ToolKind::ReadOnly, "read", &out, true);
        assert!(!with_vision.contains("base64,"), "{with_vision}");
        assert!(!with_vision.contains("AAAA"), "base64 本体不得进模型侧文本");
        assert!(with_vision.contains("a.png"), "路径仍要给模型");
        assert!(with_vision.contains("image/png"), "格式仍要给模型");
        assert!(
            with_vision.len() < 500,
            "剥掉 base64 后文本必须很小：{} 字节",
            with_vision.len()
        );
        assert!(
            with_vision.contains("图片随下一条图片消息发送"),
            "{with_vision}"
        );

        // 未勾选「支持图片输入」：说明要如实写明模型看不到图
        let no_vision = compact_for_model(ToolKind::ReadOnly, "read", &out, false);
        assert!(!no_vision.contains("base64,"), "{no_vision}");
        assert!(no_vision.contains("未开启图片输入"), "{no_vision}");

        // 前端通道不受影响：data_url 仍在（聊天里的图片预览靠它）
        assert!(out.data["files"][0]["data_url"].is_string());

        // batch_read 是 read 的兼容别名（同一实现、同样产 data_url）：必须走同一条剥图逻辑，
        // 否则那段 base64 会被头尾截断后当文本发出去（JSON 腰斩 + 白烧 token）
        let alias = compact_for_model(ToolKind::ReadOnly, "batch_read", &out, true);
        assert!(!alias.contains("base64,"), "{alias}");
        assert!(alias.len() < 500, "{alias}");
    }

    /// 无图片的 read 仍是原样全文（多文件混合读取时不能因出现图片而整体裁剪）。
    #[test]
    fn read_with_image_keeps_text_files_intact() {
        let big = "y".repeat(30_000);
        let out = ToolOutcome::ok(serde_json::json!({
            "files": [
                {"path": "a.txt", "kind": "text", "content": big},
                {"path": "b.png", "kind": "image", "media_type": "image/png", "data_url": "data:image/png;base64,ZZZZ"},
            ]
        }));
        let s = compact_for_model(ToolKind::ReadOnly, "read", &out, true);
        assert!(s.contains(&"y".repeat(30_000)), "文本文件内容不得被裁剪");
        assert!(!s.contains("ZZZZ"));
    }

    #[test]
    fn command_passes_through_and_keeps_output_on_error() {
        // command 在工具内已语义化瘦身：成功原样透传不二次切割；非零退出时 data 仍携带 output
        let out =
            ToolOutcome::ok(serde_json::json!({"output": "y".repeat(50_000), "exit_code": 0}));
        let s = compact_for_model(ToolKind::ReadOnly, "command", &out, true);
        assert_eq!(s, out.data.to_string());

        let err = ToolOutcome {
            ok: false,
            data: serde_json::json!({"exit_code": 3, "output": "build failed here"}),
            error: Some(crate::tools::ToolError::new("E_EXIT_CODE", "命令退出码 3")),
            warnings: Vec::new(),
            extra_model_content: Vec::new(),
        };
        let s = compact_for_model(ToolKind::ReadOnly, "command", &err, true);
        assert!(s.contains("[error E_EXIT_CODE: 命令退出码 3]"));
        assert!(
            s.contains("build failed here"),
            "非零退出的输出必须保留给模型"
        );
    }

    #[test]
    fn other_tools_still_truncated() {
        let out = ToolOutcome::ok(serde_json::json!({"output": "y".repeat(50_000)}));
        let s = compact_for_model(ToolKind::ReadOnly, "service", &out, true);
        assert!(s.len() < 20_000);
        assert!(s.contains("已截断"));
    }

    #[test]
    fn error_mapping() {
        let out = ToolOutcome::err("E_PATH_OUTSIDE", "越界");
        assert_eq!(
            compact_for_model(ToolKind::FileWrite, "edit", &out, true),
            "[error E_PATH_OUTSIDE: 越界]"
        );
    }
}
