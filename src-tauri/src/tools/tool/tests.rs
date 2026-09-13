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
        out.extra_model_content.push(Content::Text {
            text: "img".into(),
        });
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
        assert!(
            collect_unknown_fields(&json!({"a": 1}), r#"{"type":"object"}"#).is_empty()
        );
        // schema 不是合法 JSON → 空
        assert!(collect_unknown_fields(&json!({"a": 1}), "{not json").is_empty());
    }
