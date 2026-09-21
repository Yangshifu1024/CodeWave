//! `write_document` 工具：生成新的表格文件。
//!
//! 两条既定要求直接落在这里：
//!
//! 1. **派生值要写成真公式**，而不是把算好的数字焊死。用户改了输入，汇总要跟着变——
//!    这是「交给用户一份活模型」与「交给用户一份死表」的区别。
//! 2. **公式旁边同时写入算好的值**。否则别的工具（以及不看公式的阅读器）读到的是一片空白。

use crate::tools::pathutil;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use umya_spreadsheet::Worksheet;

use super::xlsx;

/// 单次生成的工作表数量上限。
const MAX_SHEETS: usize = 20;
/// 单次生成的总行数上限。
const MAX_TOTAL_ROWS: usize = 50_000;
/// 单列宽度上限（Excel 本身的限制是 255）。
const MAX_COL_WIDTH: f64 = 255.0;

/// 一个工作表的定义。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SheetSpec {
    /// 工作表名。
    name: String,
    /// 表头（加粗显示）。
    #[serde(default)]
    header: Vec<String>,
    /// 数据行：每行是与列对应的值数组。
    #[serde(default)]
    rows: Vec<Vec<Value>>,
    /// 列宽，键是列字母如 `A`。
    #[serde(default)]
    column_widths: std::collections::BTreeMap<String, f64>,
}

/// `write_document` 入参。
///
/// 表格用 `sheets`，Word 文档用 `docx`；由目标文件的扩展名决定用哪一个。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    /// 工作区相对路径（必须尚不存在）。
    path: String,
    /// 工作表列表（表格）。
    #[serde(default)]
    sheets: Vec<SheetSpec>,
    /// 文档内容（Word）。
    #[serde(default)]
    docx: Option<super::write_word::DocxSpec>,
}

/// 目标文件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sheet,
    Word,
}

/// 按扩展名判定；不支持的返回 None。
fn kind_of(path: &str) -> Option<Kind> {
    match Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("xlsx") => Some(Kind::Sheet),
        Some("docx") => Some(Kind::Word),
        _ => None,
    }
}

/// 生成 Word 文档并写入目标路径。
fn write_word(
    ctx: &ToolCtx,
    args: &Args,
    resolved: &std::path::Path,
    spec: &super::write_word::DocxSpec,
) -> ToolOutcome {
    if !args.sheets.is_empty() {
        return ToolOutcome::err("E_ARGS", "sheets 只用于表格；Word 文档请用 docx 字段。");
    }
    let bytes = match super::write_word::generate(spec) {
        Ok(b) => b,
        Err(e) => return ToolOutcome::err("E_ARGS", e),
    };
    if let Err(e) = crate::util::atomic::atomic_write(resolved, &bytes) {
        return ToolOutcome::err("E_IO", format!("写入 {} 失败：{e}", args.path));
    }
    if !ctx.rt.is_task_runtime {
        let owner = ctx
            .rt
            .root_session_id
            .clone()
            .unwrap_or_else(|| ctx.rt.id.clone());
        let canonical = pathutil::canonical_best_effort(resolved)
            .to_string_lossy()
            .into_owned();
        if let Err(e) = ctx.core.store.append_artifact(
            &owner,
            &canonical,
            crate::core::sessions::ArtifactOp::Create,
        ) {
            tracing::warn!("产物登记失败（write_document {}）：{e}", args.path);
        }
    }
    ToolOutcome::ok(json!({
        "path": args.path,
        "blocks": spec.blocks.len(),
        "bytes": bytes.len(),
    }))
}

/// 单元格取值：字符串 / 数值 / 逻辑值 / 公式对象。
///
/// 公式对象形如 `{"formula": "=SUM(B2:B3)", "cached": 270}`；`cached` 可省，
/// 但省了之后不看公式的读取器会读到空白，所以模型应当尽量提供。
fn apply_cell(ws: &mut Worksheet, coord: (u32, u32), v: &Value, bold: bool) {
    let cell = ws.cell_mut(coord);
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                cell.set_value_number(f);
            }
        }
        Value::Bool(b) => {
            cell.set_value_bool(*b);
        }
        Value::String(s) => {
            // 以 = 开头的字符串也当公式处理：模型常直接这么写
            if let Some(rest) = s.strip_prefix('=') {
                cell.set_formula(rest);
            } else {
                cell.set_value_string(s);
            }
        }
        Value::Object(map) => {
            if let Some(f) = map.get("formula").and_then(|x| x.as_str()) {
                cell.set_formula(f.trim_start_matches('='));
                // 算好的值：数值优先，其次文本
                if let Some(cached) = map.get("cached") {
                    if let Some(n) = cached.as_f64() {
                        cell.set_formula_result_number(n);
                    } else if let Some(s) = cached.as_str() {
                        cell.set_formula_result_string(s);
                    } else if let Some(b) = cached.as_bool() {
                        cell.set_formula_result_bool(b);
                    }
                } else {
                    // 没给缓存值时留空，由打开方重算
                    cell.set_formula_result_blank();
                }
            } else if let Some(t) = map.get("text").and_then(|x| x.as_str()) {
                cell.set_value_string(t);
            }
        }
        Value::Null => {}
        _ => {}
    }
    if bold {
        cell.style_mut().font_mut().set_bold(true);
    }
}

/// 把定义里的内容填进工作表（工作表本身由工作簿创建）。
fn fill_sheet(ws: &mut Worksheet, spec: &SheetSpec) -> Result<(), String> {
    let mut row_idx = 1u32;
    if !spec.header.is_empty() {
        for (i, h) in spec.header.iter().enumerate() {
            apply_cell(
                ws,
                ((i + 1) as u32, row_idx),
                &Value::String(h.clone()),
                true,
            );
        }
        row_idx += 1;
    }
    for row in &spec.rows {
        for (i, v) in row.iter().enumerate() {
            apply_cell(ws, ((i + 1) as u32, row_idx), v, false);
        }
        row_idx += 1;
    }

    for (col, width) in &spec.column_widths {
        if xlsx::col_to_index(col).is_none() {
            return Err(format!("列宽里的列标识「{col}」不是合法的列字母"));
        }
        let w = width.clamp(1.0, MAX_COL_WIDTH);
        ws.column_dimension_mut(col).set_width(w);
    }
    Ok(())
}

/// `write_document` 工具：生成新的表格文件。
pub struct WriteDocumentTool;

#[async_trait::async_trait]
impl Tool for WriteDocumentTool {
    fn name(&self) -> &'static str {
        "write_document"
    }

    fn description(&self) -> &'static str {
        "生成新文件：表格（.xlsx）或 Word 文档（.docx）。表格：派生值（汇总、占比等）要写成真公式，并在 formula 单元格里用 cached 给出算好的值，这样用户改了输入汇总会跟着变、不看公式的工具也能读到数字。Word：用 blocks 描述内容，支持标题（自动成为可导航的大纲层级）、段落与表格。目标文件必须尚不存在——要改已有文件请用 edit_document。"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["path"],
  "properties": {
    "path": {"type": "string", "description": "工作区相对路径，必须以 .xlsx 结尾"},
    "docx": {
      "type": "object",
      "description": "Word 文档内容（目标为 .docx 时必填）",
      "additionalProperties": false,
      "required": ["blocks"],
      "properties": {
        "blocks": {
          "type": "array",
          "minItems": 1,
          "description": "内容块，按顺序排列",
          "items": {
            "type": "object",
            "required": ["type"],
            "properties": {
              "type": {"type": "string", "enum": ["heading", "paragraph", "table"]},
              "level": {"type": "integer", "description": "标题层级 1-6（仅 heading）"},
              "text": {"type": "string", "description": "文字内容（heading / paragraph）"},
              "rows": {"type": "array", "description": "表格内容（仅 table）", "items": {"type": "array", "items": {"type": "string"}}},
              "header": {"type": "boolean", "description": "首行是否作为表头加粗（仅 table，默认是）"}
            }
          }
        }
      }
    },
    "sheets": {
      "type": "array",
      "minItems": 1,
      "maxItems": 20,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["name"],
        "properties": {
          "name": {"type": "string", "description": "工作表名（不超过 31 字符）"},
          "header": {"type": "array", "items": {"type": "string"}, "description": "表头，加粗显示"},
          "rows": {
            "type": "array",
            "description": "数据行。单元格可以是字符串、数字、布尔值，或 {\"formula\": \"=SUM(B2:B3)\", \"cached\": 270} 形式的公式对象",
            "items": {"type": "array", "items": {}}
          },
          "columnWidths": {
            "type": "object",
            "description": "列宽，键是列字母如 A",
            "additionalProperties": {"type": "number"}
          }
        }
      }
    }
  }
}"#
    }

    fn kind(&self) -> ToolKind {
        ToolKind::FileWrite
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let kind = match kind_of(&args.path) {
            Some(k) => k,
            None => {
                return ToolOutcome::err(
                    "E_UNSUPPORTED",
                    format!(
                        "{} 不是支持生成的文件类型（只支持 .xlsx 与 .docx）",
                        args.path
                    ),
                );
            }
        };
        if kind == Kind::Sheet {
            if args.sheets.is_empty() {
                return ToolOutcome::err("E_ARGS", "生成表格时 sheets 不能为空");
            }
            if args.sheets.len() > MAX_SHEETS {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!(
                        "一次最多生成 {MAX_SHEETS} 个工作表，本次给了 {}",
                        args.sheets.len()
                    ),
                );
            }
            let total_rows: usize = args
                .sheets
                .iter()
                .map(|s| s.rows.len() + s.header.len())
                .sum();
            if total_rows > MAX_TOTAL_ROWS {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!("总行数 {total_rows} 超过上限 {MAX_TOTAL_ROWS}"),
                );
            }
        } else if args.docx.is_none() {
            return ToolOutcome::err("E_ARGS", "生成 Word 文档时 docx 字段不能为空");
        }

        let roots = ctx.write_roots();
        let resolved = match pathutil::resolve_write(&roots, &args.path) {
            Ok(p) => p,
            Err((c, m)) => return ToolOutcome::err(&c, m),
        };
        if resolved.exists() {
            return ToolOutcome::err(
                "E_EXISTS",
                format!(
                    "{} 已经存在。要修改已有文件请用 edit_document（它能保住原有图表等），或换一个文件名。",
                    args.path
                ),
            );
        }

        if ctx.rt.root_session_id.is_none()
            && let Err(conflicts) =
                crate::tools::claims::claim(&ctx.rt.id, std::slice::from_ref(&resolved))
        {
            return ToolOutcome::err(
                "E_FILE_CLAIMED",
                crate::tools::claims::denial_message(&conflicts),
            );
        }

        if kind == Kind::Word {
            let spec = args.docx.as_ref().expect("上面已校验过");
            return write_word(ctx, &args, &resolved, spec);
        }

        let mut book = umya_spreadsheet::new_file();
        // new_file 自带一个空工作表，先去掉，再按定义逐个加入
        let _ = book.remove_sheet(0);
        let mut used_names: Vec<String> = Vec::new();
        for spec in &args.sheets {
            if spec.name.trim().is_empty() {
                return ToolOutcome::err("E_ARGS", "工作表名不能为空");
            }
            if used_names.iter().any(|n| n == &spec.name) {
                return ToolOutcome::err("E_ARGS", format!("工作表名「{}」重复了", spec.name));
            }
            if spec.name.chars().count() > 31 {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!("工作表名「{}」超过 31 个字符（Excel 的限制）", spec.name),
                );
            }
            let ws = match book.new_sheet(spec.name.clone()) {
                Ok(w) => w,
                Err(e) => {
                    return ToolOutcome::err(
                        "E_ARGS",
                        format!("创建工作表「{}」失败：{e}", spec.name),
                    );
                }
            };
            if let Err(e) = fill_sheet(ws, spec) {
                return ToolOutcome::err("E_ARGS", e);
            }
            used_names.push(spec.name.clone());
        }

        // 先写临时文件再原子搬到目标：中途失败不留半个文件
        let tmp = match tempfile::NamedTempFile::new_in(
            resolved.parent().unwrap_or_else(|| Path::new(".")),
        ) {
            Ok(t) => t,
            Err(e) => return ToolOutcome::err("E_IO", format!("创建临时文件失败：{e}")),
        };
        if let Err(e) = umya_spreadsheet::writer::xlsx::write(&book, tmp.path()) {
            return ToolOutcome::err("E_IO", format!("生成表格失败：{e}"));
        }
        let bytes = match std::fs::read(tmp.path()) {
            Ok(b) => b,
            Err(e) => return ToolOutcome::err("E_IO", format!("读取生成结果失败：{e}")),
        };
        if let Err(e) = crate::util::atomic::atomic_write(&resolved, &bytes) {
            return ToolOutcome::err("E_IO", format!("写入 {} 失败：{e}", args.path));
        }

        // 产物登记（子代理归属主会话；任务 runtime 跳过；失败仅记日志）
        if !ctx.rt.is_task_runtime {
            let owner = ctx
                .rt
                .root_session_id
                .clone()
                .unwrap_or_else(|| ctx.rt.id.clone());
            let canonical = pathutil::canonical_best_effort(&resolved)
                .to_string_lossy()
                .into_owned();
            if let Err(e) = ctx.core.store.append_artifact(
                &owner,
                &canonical,
                crate::core::sessions::ArtifactOp::Create,
            ) {
                tracing::warn!("产物登记失败（write_document {}）：{e}", args.path);
            }
        }

        ToolOutcome::ok(json!({
            "path": args.path,
            "sheets": used_names,
            "bytes": bytes.len(),
            "hint": "派生值已写成公式并带上算好的值：用户改动输入后汇总会跟着变。",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_ctx(
        ws: &tempfile::TempDir,
        dd: &tempfile::TempDir,
    ) -> (
        Arc<crate::core::agent::AgentCore>,
        Arc<crate::core::agent::SessionRuntime>,
    ) {
        let roots = pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
        (core, rt)
    }

    fn ctx_for(
        core: Arc<crate::core::agent::AgentCore>,
        rt: Arc<crate::core::agent::SessionRuntime>,
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

    #[tokio::test]
    async fn generates_sheet_with_header_formula_and_cached_value() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);

        let out = WriteDocumentTool
            .run(
                &ctx,
                json!({
                    "path": "汇总.xlsx",
                    "sheets": [{
                        "name": "月度",
                        "header": ["月份", "金额"],
                        "rows": [
                            ["1月", 120],
                            ["2月", 150],
                            ["合计", {"formula": "=SUM(B2:B3)", "cached": 270}]
                        ],
                        "columnWidths": {"A": 14}
                    }]
                }),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["sheets"][0], "月度");

        // 读回来验证
        let path = ws.path().join("汇总.xlsx");
        let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
        let sheet = book.sheet_by_name("月度").unwrap();
        assert_eq!(sheet.value("A1"), "月份");
        assert_eq!(sheet.value("A2"), "1月");
        assert_eq!(sheet.value("B2"), "120");
        // 派生值必须是真公式，而不是焊死的数字
        let formula = sheet
            .cell("B4")
            .map(|c| c.formula().to_string())
            .unwrap_or_default();
        assert!(
            formula.contains("SUM(B2:B3)"),
            "汇总必须是公式，而不是焊死的数字：{formula:?}"
        );
        // 且带上算好的值，别的工具也能读到
        assert_eq!(sheet.value_number("B4"), Some(270.0));
        // 表头加粗
        let bold = sheet.style("A1").font().map(|f| f.bold()).unwrap_or(false);
        assert!(bold, "表头应加粗");
    }

    #[tokio::test]
    async fn refuses_existing_file_and_points_to_edit() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);
        std::fs::write(ws.path().join("已有.xlsx"), b"x").unwrap();

        let out = WriteDocumentTool
            .run(
                &ctx,
                json!({"path": "已有.xlsx",
                       "sheets": [{"name": "S", "rows": [["a"]]}]}),
            )
            .await;
        assert!(!out.ok);
        let e = out.error.unwrap();
        assert_eq!(e.code, "E_EXISTS");
        assert!(e.message.contains("edit_document"), "{}", e.message);
    }

    #[tokio::test]
    async fn validates_names_extension_and_path() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core.clone(), rt.clone());

        let long_name = "很长的工作表名".repeat(6);
        let out = WriteDocumentTool
            .run(
                &ctx,
                json!({"path": "a.xlsx", "sheets": [{"name": long_name}]}),
            )
            .await;
        assert!(!out.ok);
        assert!(out.error.unwrap().message.contains("31"));

        let dup = WriteDocumentTool
            .run(
                &ctx,
                json!({"path": "b.xlsx",
                       "sheets": [{"name": "S"}, {"name": "S"}]}),
            )
            .await;
        assert!(!dup.ok);
        assert!(dup.error.unwrap().message.contains("重复"));

        let wrong_ext = WriteDocumentTool
            .run(&ctx, json!({"path": "c.csv", "sheets": [{"name": "S"}]}))
            .await;
        assert!(!wrong_ext.ok);
        assert_eq!(wrong_ext.error.unwrap().code, "E_UNSUPPORTED");

        let escape = WriteDocumentTool
            .run(
                &ctx,
                json!({"path": "../../../tmp/x.xlsx", "sheets": [{"name": "S"}]}),
            )
            .await;
        assert!(!escape.ok);
        assert_eq!(escape.error.unwrap().code, "E_PATH_OUTSIDE");
    }

    #[tokio::test]
    async fn multiple_sheets_land_in_one_file() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);

        let out = WriteDocumentTool
            .run(
                &ctx,
                json!({
                    "path": "多表.xlsx",
                    "sheets": [
                        {"name": "第一", "rows": [["a"]]},
                        {"name": "第二", "rows": [["b"]]}
                    ]
                }),
            )
            .await;
        assert!(out.ok, "{out:?}");
        let book = umya_spreadsheet::reader::xlsx::read(ws.path().join("多表.xlsx")).unwrap();
        let names: Vec<&str> = book.sheet_collection().iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["第一", "第二"]);
    }
}
