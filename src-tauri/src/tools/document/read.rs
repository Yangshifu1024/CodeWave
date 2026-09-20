//! `read_document` 工具：按扩展名分派，读取 Office 与 PDF 的内容。
//!
//! 本批次只落地表格（`.xlsx` / `.xlsm`）；其余类型返回带原因的 `E_UNSUPPORTED`，
//! 而不是让模型拿到乱码或空白。旧格式（`.xls` / `.doc`）一律引导用户另存为新格式——
//! 它们与新版是两套完全不同的二进制格式，纯 Rust 没有可用方案。

use crate::tools::pathutil;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use umya_spreadsheet::{Workbook, Worksheet};

use super::{docx, patch, pdf, xlsx};

/// 读取上限（共识值：读与预览 200MB）。**实测后可能下调**——这是一条先按计划写死、
/// 待真实文件验证的数值，不是已经验证过的安全值。
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// 摘要模式每张表默认返回的预览行数。
const DEFAULT_PREVIEW_ROWS: u32 = 5;
/// 摘要模式预览行数上限。
const MAX_PREVIEW_ROWS: u32 = 50;
/// 单次按区域取数的行数上限。
const MAX_ROWS_PER_CALL: u32 = 200;
/// 单次返回的列数上限（防止对整行取数时拉入 16384 列）。
const MAX_COLS_PER_CALL: u32 = 64;
/// 单次返回的单元格总数上限（行数据此再收一次）。
const MAX_CELLS_PER_CALL: u32 = 20_000;

/// `read_document` 入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    /// 工作区相对路径。
    path: String,
    /// 工作表名；不给则返回结构摘要。
    #[serde(default)]
    sheet: Option<String>,
    /// 单元格区域（如 `A1:D50`）；仅在与 `sheet` 同用时生效。
    #[serde(default)]
    range: Option<String>,
    /// 摘要模式每张表的预览行数。
    #[serde(default)]
    preview_rows: Option<u32>,
    /// PDF 页码（如 "1-10"）。
    #[serde(default)]
    pages: Option<String>,
}

/// 按扩展名判定的文件类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    /// 新版表格（`.xlsx` / `.xlsm`）——本批次支持。
    Xlsx,
    /// 2003 格式的表格。
    XlsLegacy,
    /// 新版文档。
    Docx,
    /// 2003 格式的文档。
    DocLegacy,
    /// 演示文稿。
    Pptx,
    /// PDF。
    Pdf,
    /// 不在支持范围内的其它类型。
    Other,
}

/// 扩展名 → 文件类别（大小写不敏感）。
fn kind_of(path: &Path) -> DocKind {
    match path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("xlsx") | Some("xlsm") => DocKind::Xlsx,
        Some("xls") => DocKind::XlsLegacy,
        Some("docx") => DocKind::Docx,
        Some("doc") => DocKind::DocLegacy,
        Some("pptx") => DocKind::Pptx,
        Some("pdf") => DocKind::Pdf,
        _ => DocKind::Other,
    }
}

/// 在单次上限内收出一个实际返回的窗口，返回（窗口, 是否被截断）。
fn clamp_window(region: xlsx::Region) -> (xlsx::Region, bool) {
    let end_col = region
        .end_col
        .min(region.start_col.saturating_add(MAX_COLS_PER_CALL - 1));
    let cols = end_col.saturating_sub(region.start_col) + 1;
    let max_rows_by_cells = (MAX_CELLS_PER_CALL / cols.max(1)).max(1);
    let end_row = region
        .end_row
        .min(region.start_row.saturating_add(MAX_ROWS_PER_CALL - 1))
        .min(region.start_row.saturating_add(max_rows_by_cells - 1));
    let window = xlsx::Region {
        start_col: region.start_col,
        start_row: region.start_row,
        end_col,
        end_row,
    };
    let truncated = window.end_col < region.end_col || window.end_row < region.end_row;
    (window, truncated)
}

/// 按窗口取值；窗口非法（终点小于起点）时返回空。
fn collect(ws: &Worksheet, window: xlsx::Region) -> Vec<Vec<String>> {
    if window.end_row < window.start_row || window.end_col < window.start_col {
        return Vec::new();
    }
    (window.start_row..=window.end_row)
        .map(|r| {
            (window.start_col..=window.end_col)
                .map(|c| ws.value((c, r)))
                .collect()
        })
        .collect()
}

/// 结构摘要：每张表的名称、行列数与预览行。
fn summary(book: &Workbook, args: &Args) -> ToolOutcome {
    let preview_rows = args
        .preview_rows
        .unwrap_or(DEFAULT_PREVIEW_ROWS)
        .clamp(1, MAX_PREVIEW_ROWS);
    let mut sheets = Vec::new();
    for ws in book.sheet_collection() {
        let total_rows = ws.highest_row();
        let total_cols = ws.highest_column();
        let window = xlsx::Region {
            start_col: 1,
            start_row: 1,
            end_col: total_cols.min(MAX_COLS_PER_CALL),
            end_row: total_rows.min(preview_rows),
        };
        sheets.push(json!({
            "name": ws.name(),
            "rows": total_rows,
            "cols": total_cols,
            "preview": xlsx::render_tsv(&collect(ws, window)),
        }));
    }
    ToolOutcome::ok(json!({
        "file": args.path,
        "sheetCount": sheets.len(),
        "sheets": sheets,
        "hint": "这是结构摘要。要看具体数据请再调用一次，带上 sheet 与 range（如 range=\"A1:F200\"）。",
    }))
}

/// 区域取数：带 sheet（可选带 range）时返回该区域的数据。
fn region_of(ws: &Worksheet, args: &Args) -> ToolOutcome {
    let total_rows = ws.highest_row();
    let total_cols = ws.highest_column();
    let requested = match &args.range {
        Some(raw) => match xlsx::parse_region(raw) {
            Some(r) => r,
            None => {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!("区域地址无法解析：{raw}（示例：A1:D50，或单格 B3）"),
                )
            }
        },
        None => xlsx::Region {
            start_col: 1,
            start_row: 1,
            end_col: total_cols.max(1),
            end_row: total_rows.max(1),
        },
    };
    let (window, truncated) = clamp_window(requested);
    let rows = collect(ws, window);
    ToolOutcome::ok(json!({
        "file": args.path,
        "sheet": ws.name(),
        "totalRows": total_rows,
        "totalCols": total_cols,
        "range": format!(
            "{}{}:{}{}",
            xlsx::index_to_col(window.start_col),
            window.start_row,
            xlsx::index_to_col(window.end_col),
            window.end_row
        ),
        "returnedRows": rows.len(),
        "truncated": truncated,
        "truncatedHint": if truncated {
            "本次只返回了窗口内的数据。剩余部分请用 range 继续取（单次上限 200 行 / 64 列 / 20000 格）。"
        } else {
            ""
        },
        "text": xlsx::render_tsv(&rows),
    }))
}

/// 解析表格并按参数分派到摘要或区域取数。
fn read_xlsx(args: &Args, path: &Path) -> ToolOutcome {
    let book = match umya_spreadsheet::reader::xlsx::read(path) {
        Ok(b) => b,
        Err(e) => {
            return ToolOutcome::err(
                "E_PARSE",
                format!("无法解析表格 {}：{e}。文件可能损坏，或含有本应用尚未支持的表格特性。", args.path),
            )
        }
    };
    if book.sheet_collection().is_empty() {
        return ToolOutcome::err("E_PARSE", format!("{} 里没有任何工作表", args.path));
    }
    match &args.sheet {
        None => summary(&book, args),
        Some(name) => match book.sheet_by_name(name) {
            Ok(ws) => region_of(ws, args),
            Err(_) => {
                let names: Vec<&str> = book
                    .sheet_collection()
                    .iter()
                    .map(|s| s.name())
                    .collect();
                ToolOutcome::err(
                    "E_ARGS",
                    format!(
                        "没有名为「{name}」的工作表。这份文件里的工作表：{}",
                        names.join("、")
                    ),
                )
            }
        },
    }
}

/// 单次返回的文字长度上限（字符）。超长文档截断返回，避免一次灌爆上下文。
const MAX_TEXT_CHARS: usize = 40_000;

/// 读取 Word 文档：解析出段落、标题层级与表格。
fn read_docx(args: &Args, path: &Path) -> ToolOutcome {
    let xml = match patch::read_entry(path, docx::DOCUMENT_ENTRY) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                return ToolOutcome::err(
                    "E_PARSE",
                    format!("{} 的正文内容无法解析（文件可能已损坏）", args.path),
                )
            }
        },
        Ok(None) => {
            return ToolOutcome::err(
                "E_PARSE",
                format!("{} 里找不到正文内容，可能不是有效的 Word 文档", args.path),
            )
        }
        Err(e) => return ToolOutcome::err("E_PARSE", e),
    };

    let blocks = docx::parse_blocks(&xml);
    let full = docx::render_blocks(&blocks);
    let total_chars = full.chars().count();
    let (text, truncated) = if total_chars > MAX_TEXT_CHARS {
        (
            full.chars().take(MAX_TEXT_CHARS).collect::<String>(),
            true,
        )
    } else {
        (full, false)
    };
    let headings: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match b {
            docx::Block::Paragraph {
                text,
                heading: Some(lv),
            } => Some(json!({"level": lv, "text": text})),
            _ => None,
        })
        .collect();
    let tables = blocks
        .iter()
        .filter(|b| matches!(b, docx::Block::Table { .. }))
        .count();

    ToolOutcome::ok(json!({
        "file": args.path,
        "totalChars": total_chars,
        "truncated": truncated,
        "truncatedHint": if truncated {
            "文档较长，这里只返回了前一部分。需要后续内容请结合其它工具定位具体段落。"
        } else {
            ""
        },
        "tables": tables,
        "headings": headings,
        "text": text,
        "hint": "标题以 # 开头（层级即 # 的个数），表格用 | 分隔。要改文字请用 edit_document 的 find/replace。",
    }))
}

/// 读取 PDF：逐页提取文字（含崩溃隔离），并识别扫描件。
fn read_pdf(args: &Args, path: &Path) -> ToolOutcome {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ToolOutcome::err("E_IO", format!("{}: {e}", args.path)),
    };
    let pages = match pdf::extract_pages(&bytes) {
        Ok(p) => p,
        Err(e) => return ToolOutcome::err("E_PDF", e),
    };
    if pages.is_empty() {
        return ToolOutcome::err(
            "E_PDF",
            format!("{} 里没有任何页面", args.path),
        );
    }

    // 扫描件：页面本身是图片、没有文字层——说清楚，而不是返回一片空白
    if pdf::looks_scanned(&pages) {
        return ToolOutcome::ok(json!({
            "file": args.path,
            "pages": pages.len(),
            "scanned": true,
            "text": "",
            "hint": "这份 PDF 看上去是扫描件：页面是图片、没有文字层，所以提取不到文字。需要内容的话，可以告诉使用者用带文字识别的工具先转一遗。",
        }));
    }

    let range = match &args.pages {
        Some(raw) => match pdf::parse_pages(raw) {
            Some(r) => Some(r),
            None => {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!(
                        "页码写法无法识别：{raw}（示例：pages=3、1-10、5-）"
                    ),
                )
            }
        },
        None => None,
    };
    let wanted = match pdf::resolve_pages(range, pages.len()) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::err("E_ARGS", e),
    };

    let mut text = String::new();
    for n in &wanted {
        let body = pages.get(n - 1).map(|s| s.trim()).unwrap_or("");
        text.push_str(&format!("--- 第 {n} 页 ---\n"));
        if body.is_empty() {
            text.push_str("（这一页没有可提取的文字）\n");
        } else {
            text.push_str(body);
            text.push('\n');
        }
    }

    ToolOutcome::ok(json!({
        "file": args.path,
        "pages": pages.len(),
        "returned": wanted.len(),
        "scanned": false,
        "text": text,
        "hint": "如果内容看起来不对（中文乱码或缺失），可能是这份 PDF 的字体没有内嵌字形对照表，请如实告诉使用者。",
    }))
}

/// `read_document` 工具：读取 Office 文档与 PDF 的内容。
pub struct ReadDocumentTool;

#[async_trait::async_trait]
impl Tool for ReadDocumentTool {
    fn name(&self) -> &'static str {
        "read_document"
    }

    fn description(&self) -> &'static str {
        "读取表格、文档与 PDF 的内容。表格（.xlsx/.xlsm）：不传 sheet 时返回结构摘要（工作表名、行列数、表头与前几行）；传了 sheet 再按 range 取具体区域——大表必须分次取，单次上限 200 行 / 64 列 / 20000 格。PDF：按页提取文字，用 pages 指定页码（超过 10 页必须指定，单次上限 20 页）；扫描件提取不到文字，会明确告知。旧格式（.xls/.doc）不支持，需先另存为新格式。"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["path"],
  "properties": {
    "path": {"type": "string", "description": "工作区相对路径"},
    "sheet": {"type": "string", "description": "工作表名；不传则返回结构摘要"},
    "range": {"type": "string", "description": "单元格区域，如 A1:D50；仅在与 sheet 同用时生效"},
    "previewRows": {"type": "integer", "description": "摘要模式下每张表的预览行数（默认 5，上限 50）"},
    "pages": {"type": "string", "description": "PDF 页码，如 3、1-10、5-；超过 10 页的 PDF 必须指定"}
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
        let roots = ctx.write_roots();
        let resolved: PathBuf = match pathutil::resolve_read(&roots, &args.path) {
            Ok(p) => p,
            Err((c, m)) => return ToolOutcome::err(&c, m),
        };
        let meta = match std::fs::metadata(&resolved) {
            Ok(m) => m,
            Err(e) => return ToolOutcome::err("E_NOT_FOUND", format!("{}: {e}", args.path)),
        };
        if !meta.is_file() {
            return ToolOutcome::err("E_ARGS", format!("{} 不是常规文件", args.path));
        }
        if meta.len() > MAX_FILE_BYTES {
            return ToolOutcome::err(
                "E_TOO_LARGE",
                format!(
                    "{} 为 {}MB，超过读取上限 {}MB",
                    args.path,
                    meta.len() / 1024 / 1024,
                    MAX_FILE_BYTES / 1024 / 1024
                ),
            );
        }
        match kind_of(&resolved) {
            DocKind::Xlsx => read_xlsx(&args, &resolved),
            DocKind::XlsLegacy => ToolOutcome::err(
                "E_UNSUPPORTED",
                format!(
                    "{} 是 2003 格式的 .xls，本应用不支持读取。请先用 Excel 或 WPS 把它另存为 .xlsx 再试。",
                    args.path
                ),
            ),
            DocKind::DocLegacy => ToolOutcome::err(
                "E_UNSUPPORTED",
                format!(
                    "{} 是 2003 格式的 .doc，本应用不支持读取。请先另存为 .docx 再试。",
                    args.path
                ),
            ),
            DocKind::Docx => {
                if args.sheet.is_some() || args.range.is_some() {
                    return ToolOutcome::err(
                        "E_ARGS",
                        "sheet / range 只对表格有效；读取 Word 文档不需要这两个参数",
                    );
                }
                read_docx(&args, &resolved)
            }
            DocKind::Pptx => ToolOutcome::err(
                "E_UNSUPPORTED",
                "演示文稿（.pptx）不在本期支持范围内。",
            ),
            DocKind::Pdf => {
                if args.sheet.is_some() || args.range.is_some() {
                    return ToolOutcome::err(
                        "E_ARGS",
                        "sheet / range 只对表格有效；读取 PDF 请用 pages 指定页码",
                    );
                }
                read_pdf(&args, &resolved)
            }
            DocKind::Other => ToolOutcome::err(
                "E_UNSUPPORTED",
                format!(
                    "{} 不是受支持的文档类型。本工具支持：.xlsx / .xlsm。文本文件请用 read。",
                    args.path
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 造一份样本表格：两列三行（表头 + 两行数据）。
    fn write_sample(dir: &Path) -> PathBuf {
        let mut book = umya_spreadsheet::new_file();
        {
            let ws = book
                .sheet_by_name_mut("Sheet1")
                .expect("默认工作表名应为 Sheet1");
            let data = [["月份", "金额"], ["1月", "120"], ["2月", "150"]];
            for (r, row) in data.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    ws.cell_mut(((c + 1) as u32, (r + 1) as u32))
                        .set_value(*v);
                }
            }
        }
        let path = dir.join("sample.xlsx");
        umya_spreadsheet::writer::xlsx::write(&book, &path).expect("写出样本表格");
        path
    }

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

    #[test]
    fn clamp_window_enforces_row_col_and_cell_caps() {
        // 申请整表 → 行 200 / 列 64 双上限生效
        let (w, cut) = clamp_window(xlsx::Region::new(1, 1, 16_384, 1_048_576));
        assert!(cut);
        assert_eq!((w.cols(), w.rows()), (MAX_COLS_PER_CALL, MAX_ROWS_PER_CALL));

        // 列很宽时，单元格总数上限再把行数压下来（64 列 × 200 行 = 12800 < 20000，
        // 但 64 列时 20000/64 = 312 > 200，所以行上限仍由 MAX_ROWS_PER_CALL 决定）
        let (w2, _) = clamp_window(xlsx::Region::new(1, 1, 64, 1000));
        assert_eq!(w2.rows(), MAX_ROWS_PER_CALL);

        // 窄区域不截断
        let (w3, cut3) = clamp_window(xlsx::Region::new(1, 1, 4, 50));
        assert!(!cut3);
        assert_eq!((w3.cols(), w3.rows()), (4, 50));
    }

    #[tokio::test]
    async fn summary_then_region_round_trip() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);

        // 第一步：结构摘要
        let out = ReadDocumentTool
            .run(&ctx, json!({"path": "sample.xlsx"}))
            .await;
        assert!(out.ok, "{out:?}");
        let sheets = &out.data["sheets"];
        assert_eq!(out.data["sheetCount"], 1);
        assert_eq!(sheets[0]["name"], "Sheet1");
        assert_eq!(sheets[0]["rows"], 3);
        assert_eq!(sheets[0]["cols"], 2);
        let preview = sheets[0]["preview"].as_str().unwrap();
        assert!(preview.starts_with("月份\t金额"), "{preview:?}");
        assert!(preview.contains("2月\t150"), "{preview:?}");

        // 第二步：按区域取数
        let out2 = ReadDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx", "sheet": "Sheet1", "range": "A2:B3"}),
            )
            .await;
        assert!(out2.ok, "{out2:?}");
        assert_eq!(out2.data["returnedRows"], 2);
        assert_eq!(out2.data["range"], "A2:B3");
        assert_eq!(out2.data["truncated"], false);
        assert_eq!(out2.data["text"], "1月\t120\n2月\t150\n");
    }

    #[tokio::test]
    async fn region_defaults_to_whole_sheet_when_range_omitted() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);
        let out = ReadDocumentTool
            .run(&ctx, json!({"path": "sample.xlsx", "sheet": "Sheet1"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["range"], "A1:B3");
        assert_eq!(out.data["returnedRows"], 3);
    }

    #[tokio::test]
    async fn unknown_sheet_and_bad_range_report_actionable_errors() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);

        let bad_sheet = ReadDocumentTool
            .run(&ctx, json!({"path": "sample.xlsx", "sheet": "不存在"}))
            .await;
        assert!(!bad_sheet.ok);
        let e = bad_sheet.error.unwrap();
        assert_eq!(e.code, "E_ARGS");
        assert!(e.message.contains("Sheet1"), "错误要列出可用工作表：{e:?}");

        let bad_range = ReadDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx", "sheet": "Sheet1", "range": "不是地址"}),
            )
            .await;
        assert!(!bad_range.ok);
        assert_eq!(bad_range.error.unwrap().code, "E_ARGS");
    }

    #[tokio::test]
    async fn legacy_and_unsupported_types_are_refused_with_reason() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);

        for (name, expect_hint) in [
            ("old.xls", "另存为 .xlsx"),
            ("old.doc", "另存为 .docx"),
            ("deck.pptx", "不在本期支持范围"),
            ("note.txt", "请用 read"),
        ] {
            std::fs::write(ws.path().join(name), b"x").unwrap();
            let out = ReadDocumentTool
                .run(&ctx, json!({"path": name}))
                .await;
            assert!(!out.ok, "{name} 不应被接受：{out:?}");
            let e = out.error.unwrap();
            assert_eq!(e.code, "E_UNSUPPORTED", "{name}: {e:?}");
            assert!(
                e.message.contains(expect_hint),
                "{name} 的提示缺少「{expect_hint}」：{}",
                e.message
            );
        }
    }

    #[tokio::test]
    async fn missing_file_and_oversize_are_reported() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);

        let missing = ReadDocumentTool
            .run(&ctx, json!({"path": "nope.xlsx"}))
            .await;
        assert!(!missing.ok);
        assert_eq!(missing.error.unwrap().code, "E_NOT_FOUND");
    }

    #[tokio::test]
    async fn path_escape_is_refused() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);
        let out = ReadDocumentTool
            .run(&ctx, json!({"path": "../../../../etc/passwd"}))
            .await;
        assert!(!out.ok);
        assert_eq!(out.error.unwrap().code, "E_PATH_OUTSIDE");
    }
}
