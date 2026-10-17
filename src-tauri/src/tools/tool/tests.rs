use super::*;
use crate::core::types::Content;
use serde_json::json;

#[test]
fn outcome_ok_serialization_shape() {
    let out = ToolOutcome::ok(json!({"n": 1}));
    let v: Value = serde_json::to_value(&out).unwrap();
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["data"], json!({"n": 1}));
    assert_eq!(v["warnings"], json!([]));
    assert!(v.get("error").is_none(), "error omitted when None");
    // extra_model_content 只进模型通道，永不序列化到前端 JSON
    let mut out = out;
    out.extra_model_content
        .push(Content::Text { text: "img".into() });
    let v: Value = serde_json::to_value(&out).unwrap();
    assert!(v.get("extra_model_content").is_none());
}

#[test]
fn outcome_err_serialization_shape() {
    let out = ToolOutcome::err("E_ARGS", "bad input");
    assert!(!out.ok);
    assert_eq!(out.data, Value::Null);
    let v: Value = serde_json::to_value(&out).unwrap();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["error"]["code"], "E_ARGS");
    assert_eq!(v["error"]["message"], "bad input");
    assert_eq!(v["data"], Value::Null);
}

#[test]
fn outcome_roundtrip_and_warning_accumulation() {
    let out = ToolOutcome::ok(json!("x")).with_warnings(vec!["w1".into()]);
    let out = out.with_warnings(vec!["w2".into()]);
    assert_eq!(out.warnings, vec!["w1".to_string(), "w2".to_string()]);
    let text = serde_json::to_string(&out).unwrap();
    let back: ToolOutcome = serde_json::from_str(&text).unwrap();
    assert_eq!(back.warnings, out.warnings);
    assert_eq!(back.data, out.data);
    assert!(back.error.is_none());
}

#[test]
fn tool_error_new_builds_fields() {
    let e = ToolError::new("E_IO", "boom");
    assert_eq!(e.code, "E_IO");
    assert_eq!(e.message, "boom");
}

#[test]
fn unknown_field_collection() {
    let schema = r#"{"properties":{"path":{},"content":{}}}"#;
    let args = json!({"path": "a", "content": "b", "extra": 1, "junk": true});
    let mut unknown = collect_unknown_fields(&args, schema);
    unknown.sort();
    assert_eq!(unknown.len(), 2);
    assert!(unknown.iter().all(|u| u.contains("已忽略")));
    assert!(unknown.iter().any(|u| u.contains("junk")));
    assert!(unknown.iter().any(|u| u.contains("extra")));

    // 全部字段已知 → 空
    assert!(collect_unknown_fields(&json!({"path": "a"}), schema).is_empty());
    // 非对象入参 → 空（无可报告项）
    assert!(collect_unknown_fields(&json!([1, 2]), schema).is_empty());
    assert!(collect_unknown_fields(&Value::Null, schema).is_empty());
    // schema 无 properties → 空
    assert!(collect_unknown_fields(&json!({"a": 1}), r#"{"type":"object"}"#).is_empty());
    // schema 不是合法 JSON → 空
    assert!(collect_unknown_fields(&json!({"a": 1}), "{not json").is_empty());
}

// ── diff 通道（#36 AC03）───────────────────────────────────────────

/// `diffs` 为空时**不出现**在 outcome JSON 里。
///
/// 前端工具卡与旧 sidecar 都会原样收下这个 JSON；凭空多一个空数组等于给每条
/// 工具结果都加了一份噪声，而这条噪声对没有任何消费者。
#[test]
fn 空_diffs_不出现在序列化结果里() {
    let out = ToolOutcome::ok(json!({"n": 1}));
    let v = serde_json::to_value(&out).unwrap();
    assert!(v.get("diffs").is_none(), "不该出现：{v}");
    // 反向：旧 JSON（无 diffs 字段）必须能解析回来——向前兼容的底线
    let back: ToolOutcome = serde_json::from_value(json!({"ok": true, "data": {"n": 1}})).unwrap();
    assert!(back.diffs.is_empty(), "缺字段必须解析成空，不能报错");
}

/// diff 记录的三条基本语义：绝对路径、新文件 `old_text=None`、两侧全文。
#[test]
fn push_diff_记录路径与两侧全文() {
    let mut out = ToolOutcome::ok(json!({}));
    out.push_diff("/ws/a.txt", Some("old\n".into()), "new\n".into());
    assert_eq!(out.diffs.len(), 1);
    let d = &out.diffs[0];
    assert_eq!(d.path, "/ws/a.txt");
    assert_eq!(d.old_text.as_deref(), Some("old\n"));
    assert_eq!(d.new_text, "new\n");
}

/// 「存在的空文件」不等于「新文件」：`Some("")` 必须原样保留。
///
/// 把它并成 `None` 的话，客户端会把一次覆盖空文件显示成一次新建——
/// 而新建与覆盖在很多编辑器里是两种完全不同的操作语义。
#[test]
fn 旧内容为空串时不得降级成_none() {
    let mut out = ToolOutcome::ok(json!({}));
    out.push_diff("/ws/a.txt", Some(String::new()), "x".into());
    assert_eq!(out.diffs[0].old_text, Some(String::new()));
}

/// 单侧超预算：**整条不记**，不留半截。
///
/// 断言落在「一条都没有」上而不是「被截到 64 KiB」——截半截的 diff 会被
/// 客户端当成"文件就这么长"，那比不显示 diff 更糟。
#[test]
fn 单侧超预算时整条不记() {
    for (old, new, why) in [
        (None, "x".repeat(DIFF_SIDE_BUDGET_BYTES + 1), "新内容超限"),
        (
            Some("x".repeat(DIFF_SIDE_BUDGET_BYTES + 1)),
            "y".into(),
            "旧内容超限",
        ),
    ] {
        let mut out = ToolOutcome::ok(json!({}));
        out.push_diff("/ws/big.txt", old, new);
        assert!(out.diffs.is_empty(), "{why} 时不得留下半条 diff");
    }
    // 恰好等于上限要留下（判据是 `>`，不是 `>=`）
    let mut out = ToolOutcome::ok(json!({}));
    let exact = "x".repeat(DIFF_SIDE_BUDGET_BYTES);
    out.push_diff("/ws/edge.txt", None, exact.clone());
    assert_eq!(out.diffs.len(), 1, "恰好等于上限应保留");
    assert_eq!(out.diffs[0].new_text.len(), DIFF_SIDE_BUDGET_BYTES);
}

/// 单次调用的总预算：多文件编辑累加超限后，后续条目被跳过，已记的不受影响。
#[test]
fn 多次调用累计超总预算时后续条目跳过() {
    let half = "x".repeat(DIFF_TOTAL_BUDGET_BYTES / 2);
    let mut out = ToolOutcome::ok(json!({}));
    out.push_diff("/ws/1.txt", None, half.clone());
    out.push_diff("/ws/2.txt", None, half.clone());
    assert_eq!(out.diffs.len(), 2, "两条各半预算都该留下");
    // 第三条再加就超了：整条跳过，前两条不动
    out.push_diff("/ws/3.txt", None, half);
    assert_eq!(out.diffs.len(), 2, "超总预算的条目必须被跳过");
    assert_eq!(out.diffs[1].path, "/ws/2.txt");
}

/// diff **绝不进 `data`**：`compact_for_model` 只序列化 `data`，
/// 走 `data` 等于把整份文件正文塞进模型上下文（[docs/tools-and-safety](../../../docs/tools-and-safety.md)）。
#[test]
fn diff_不进_data_因而不进模型通道() {
    let mut out = ToolOutcome::ok(json!({"path": "a.txt"}));
    out.push_diff("/ws/a.txt", Some("old".into()), "new".into());
    assert!(
        out.data.get("diffs").is_none(),
        "diff 不得出现在 data：{}",
        out.data
    );
    let model_text = crate::tools::compact::compact_for_model(
        crate::tools::ToolKind::FileWrite,
        "edit",
        &out,
        true,
    );
    assert!(
        !model_text.contains("diffs") && !model_text.contains("/ws/a.txt"),
        "模型侧文本里不该出现 diff 内容：{model_text}"
    );
}
