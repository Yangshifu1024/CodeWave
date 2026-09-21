//! `edit_document` 工具：保真修改已有的 `.xlsx` / `.xlsm`。
//!
//! 全程只做「解开压缩包 → 只替换目标工作表的内部文件 → 其余原样搬运重打包」，
//! 不把整份文件读成模型再重建（重建会丢图表、数据透视表、迷你图）。
//!
//! 写入链路按「先备份、再落盘」的顺序：动的是用户的真实文件，改错一次代价很高，
//! 所以每次都先留一份可回退的原件，再原子替换。

use crate::tools::pathutil;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::sheet_edit::CellValue;
use super::{backup, formula, patch, sheet_edit, workbook};

/// 单次调用允许修改的单元格数上限（防止一次改动过大、审批卡片无法阅读）。
const MAX_EDITS: usize = 200;
/// 文件大小上限（与共识一致：保真修改 50MB）。
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// 一处单元格修改。
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    /// 工作表名（与文件里显示的完全一致）。
    pub sheet: String,
    /// 单元格坐标，如 `B3`。
    pub cell: String,
    /// 写入文本。
    #[serde(default)]
    pub text: Option<String>,
    /// 写入数值。
    #[serde(default)]
    pub number: Option<f64>,
    /// 写入公式（前导 `=` 可有可无）。
    #[serde(default)]
    pub formula: Option<String>,
    /// 写入逻辑值。
    #[serde(default, rename = "bool")]
    pub boolean: Option<bool>,
}

impl Edit {
    /// 取出要写入的值；四个字段必须恰好给一个。
    fn value(&self) -> Result<CellValue, String> {
        let given = [
            self.text.is_some(),
            self.number.is_some(),
            self.formula.is_some(),
            self.boolean.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        if given != 1 {
            return Err(format!(
                "{}!{}：text / number / formula / bool 必须恰好给一个（当前给了 {given} 个）",
                self.sheet, self.cell
            ));
        }
        Ok(
            match (&self.text, self.number, &self.formula, self.boolean) {
                (Some(t), ..) => CellValue::Text(t.clone()),
                (_, Some(n), ..) => CellValue::Number(n),
                (_, _, Some(f), _) => CellValue::Formula {
                    text: f.clone(),
                    // 缓存值在这里开不出来：要等拿到工作簿其它单元格才谈得上「算」。
                    // 计划阶段会把能算的填上，见 edit.rs 的 plan()。
                    cached: None,
                },
                (_, _, _, Some(b)) => CellValue::Bool(b),
                _ => unreachable!("上面已保证恰好一个"),
            },
        )
    }

    /// 新值的人类可读描述（审批卡片与结果回执用）。
    fn describe(&self) -> String {
        match self.value() {
            Ok(CellValue::Text(t)) => t,
            Ok(CellValue::Number(n)) => sheet_edit::format_number_for_display(n),
            Ok(CellValue::Formula { text: f, .. }) => {
                if f.trim_start().starts_with('=') {
                    f.clone()
                } else {
                    format!("={f}")
                }
            }
            Ok(CellValue::Bool(b)) => if b { "TRUE" } else { "FALSE" }.to_string(),
            Err(e) => format!("（{e}）"),
        }
    }
}

/// `edit_document` 入参。
///
/// 表格用 `edits`（单元格级），Word 文档用 `textEdits`（文字替换）。
/// 两者互斥，由文件类型决定用哪一个。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 工作区相对路径。
    pub path: String,
    /// 要修改的单元格列表（表格）。
    #[serde(default)]
    pub edits: Vec<Edit>,
    /// 要替换的文字（Word 文档）。
    #[serde(default)]
    pub text_edits: Vec<super::edit_word::TextEdit>,
}

/// 文件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 新版表格（单元格级修改）。
    Sheet,
    /// Word 文档（文字替换）。
    Word,
}

/// 按扩展名判定文件类型；不支持的返回 None。
fn kind_of(path: &str) -> Option<Kind> {
    match Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("xlsx") | Some("xlsm") => Some(Kind::Sheet),
        Some("docx") => Some(Kind::Word),
        _ => None,
    }
}

/// 按类型校验参数里的修改列表，并把数量上限一并检查。
fn check_edits(kind: Kind, args: &Args) -> Result<(), String> {
    match kind {
        Kind::Sheet => {
            if !args.text_edits.is_empty() {
                return Err("textEdits 只用于 Word 文档；表格请用 edits 按单元格修改。".to_string());
            }
            if args.edits.is_empty() {
                return Err("edits 不能为空".to_string());
            }
            if args.edits.len() > MAX_EDITS {
                return Err(format!(
                    "一次最多修改 {MAX_EDITS} 处，本次给了 {} 处",
                    args.edits.len()
                ));
            }
        }
        Kind::Word => {
            if !args.edits.is_empty() {
                return Err("edits 只用于表格；Word 文档请用 textEdits 做文字替换。".to_string());
            }
            if args.text_edits.is_empty() {
                return Err("textEdits 不能为空".to_string());
            }
            if args.text_edits.len() > MAX_EDITS {
                return Err(format!(
                    "一次最多替换 {MAX_EDITS} 处，本次给了 {} 处",
                    args.text_edits.len()
                ));
            }
        }
    }
    Ok(())
}

/// Word 文档的修改：只改正文里的文字，其余内部文件与非文字节点全部保留。
fn run_word(ctx: &ToolCtx, args: &Args, resolved: &std::path::Path) -> ToolOutcome {
    if let Err(e) = super::edit_word::preview(
        &match super::patch::read_entry(resolved, super::docx::DOCUMENT_ENTRY) {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    return ToolOutcome::err("E_PARSE", format!("{} 的正文无法解析", args.path));
                }
            },
            Ok(None) => {
                return ToolOutcome::err("E_PARSE", format!("{} 里找不到正文内容", args.path));
            }
            Err(e) => return ToolOutcome::err("E_PARSE", e),
        },
        &args.text_edits,
    ) {
        return ToolOutcome::err("E_EDIT_FAILED", e);
    }

    // 真正执行：读出正文 → 逐处替换 → 其余内部文件原样搬运重打包
    let document_xml = match super::patch::read_entry(resolved, super::docx::DOCUMENT_ENTRY) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return ToolOutcome::err("E_PARSE", format!("{} 的正文无法解析", args.path)),
        },
        Ok(None) => return ToolOutcome::err("E_PARSE", format!("{} 里找不到正文内容", args.path)),
        Err(e) => return ToolOutcome::err("E_PARSE", e),
    };
    let (patched_xml, applied) =
        match super::edit_word::apply_text_edits(&document_xml, &args.text_edits) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("E_EDIT_FAILED", e),
        };

    let work = match tempfile::tempdir() {
        Ok(w) => w,
        Err(e) => return ToolOutcome::err("E_IO", format!("创建临时目录失败：{e}")),
    };
    let out_path = work.path().join("patched.docx");
    let reps = vec![super::patch::Replacement {
        name: super::docx::DOCUMENT_ENTRY.to_string(),
        bytes: patched_xml.into_bytes(),
    }];
    if let Err(e) = super::patch::apply(resolved, &out_path, &reps) {
        return ToolOutcome::err("E_PATCH_FAILED", e);
    }
    let patched = match std::fs::read(&out_path) {
        Ok(b) => b,
        Err(e) => return ToolOutcome::err("E_IO", format!("读取修改结果失败：{e}")),
    };

    finish_write(ctx, args, resolved, &patched, &applied)
}

/// 写入公共收尾：备份 → 原子落盘 → 产物登记 → 回执。
fn finish_write(
    ctx: &ToolCtx,
    args: &Args,
    resolved: &std::path::Path,
    patched: &[u8],
    applied: &[super::edit_word::Applied],
) -> ToolOutcome {
    let original = std::fs::read(resolved).ok();
    let backup_path = match &original {
        Some(bytes) => match super::backup::save(&ctx.rt.data_dir, resolved, bytes) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("修改前备份失败（{}）：{e}", args.path);
                None
            }
        },
        None => None,
    };
    if let Err(e) = crate::util::atomic::atomic_write(resolved, patched) {
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
            crate::core::sessions::ArtifactOp::Edit,
        ) {
            tracing::warn!("产物登记失败（edit_document {}）：{e}", args.path);
        }
    }
    let mut out = ToolOutcome::ok(json!({
        "path": args.path,
        "changed": applied.iter().map(|a| a.count).sum::<usize>(),
        "changes": super::edit_word::changes_json(applied),
        "backup": backup_path.map(|p| p.to_string_lossy().into_owned()),
        "hint": "其余内容（图表、页眉页脚、图片、批注、样式）已原样保留。",
    }));
    out.warnings
        .push("本次改动已保留原件备份；如需回退请告知。".to_string());
    out
}

/// 一处已完成定位的修改，供审批卡片与执行共用（避免两处逻辑漂移）。
#[derive(Debug, Clone)]
struct Planned {
    sheet: String,
    cell: String,
    old: Option<String>,
    new_text: String,
}

/// 载入工作簿：读工作簿声明、关系文件、目标工作表 XML。
struct Loaded {
    path: PathBuf,
    sheets: Vec<workbook::SheetRef>,
    /// 工作表文件路径 → XML 文本
    xml: BTreeMap<String, String>,
    /// 共享字符串表（没有这份内部文件时为空）
    shared: Vec<String>,
}

/// 读入工作簿与本次要用到的工作表内容。
fn load(roots: &pathutil::WriteRoots, raw_path: &str, edits: &[Edit]) -> Result<Loaded, String> {
    let path = pathutil::resolve_read(roots, raw_path).map_err(|(_, m)| m)?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("{raw_path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{raw_path} 不是常规文件"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{raw_path} 为 {}MB，超过修改上限 {}MB",
            meta.len() / 1024 / 1024,
            MAX_FILE_BYTES / 1024 / 1024
        ));
    }

    let read = |name: &str| -> Result<Option<String>, String> {
        match patch::read_entry(&path, name) {
            Ok(Some(bytes)) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| format!("{name} 不是有效的文本内容（文件可能已损坏）")),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    };

    let wb_xml = read(workbook::WORKBOOK_ENTRY)?
        .ok_or_else(|| format!("{raw_path} 里找不到工作簿声明，可能不是有效的表格文件"))?;
    let rels_xml = read(workbook::WORKBOOK_RELS_ENTRY)?.unwrap_or_default();
    let sheets = workbook::list_sheets(&wb_xml, &rels_xml);
    if sheets.is_empty() {
        return Err(format!("{raw_path} 里没有工作表"));
    }
    let shared = read(workbook::SHARED_STRINGS_ENTRY)?
        .map(|x| workbook::parse_shared_strings(&x))
        .unwrap_or_default();

    // 只加载本次真正要改的工作表
    let mut xml = BTreeMap::new();
    for e in edits {
        let sheet = workbook::find_sheet(&sheets, &e.sheet).ok_or_else(|| {
            let names: Vec<&str> = sheets.iter().map(|s| s.name.as_str()).collect();
            format!(
                "没有名为「{}」的工作表。这份文件里的工作表：{}",
                e.sheet,
                names.join("、")
            )
        })?;
        if sheet.entry.is_empty() {
            return Err(format!(
                "工作表「{}」在文件里找不到对应内容（关系声明缺失），出于安全考虑中止修改",
                e.sheet
            ));
        }
        if !xml.contains_key(&sheet.entry) {
            let content = read(&sheet.entry)?
                .ok_or_else(|| format!("读取工作表「{}」的内容失败", e.sheet))?;
            xml.insert(sheet.entry.clone(), content);
        }
    }

    // 公式可能引用别的工作表（`=SUM(明细!B2:B3)`）：那些表也要载入，
    // 否则求值时读不到单元格，只能一律判「算不出来」——白丢一次填缓存值的机会。
    let referenced: Vec<String> = edits
        .iter()
        .filter_map(|e| e.formula.as_deref())
        .flat_map(formula::referenced_sheets)
        .collect();
    for name in referenced {
        let Some(sheet) = workbook::find_sheet(&sheets, &name) else {
            // 引用了不存在的工作表：不求值（求值那边也会因读不到而放弃）
            continue;
        };
        if sheet.entry.is_empty() || xml.contains_key(&sheet.entry) {
            continue;
        }
        if let Some(content) = read(&sheet.entry)? {
            xml.insert(sheet.entry.clone(), content);
        }
    }

    Ok(Loaded {
        path,
        sheets,
        xml,
        shared,
    })
}

/// 试跑结果：改动清单 + 每个工作表文件的新内容 + 是否需要给工作簿打「打开时重算」标记。
type PlanResult = (Vec<Planned>, BTreeMap<String, String>, bool);

/// 试运行：在内存里把全部修改应用一遍。
///
/// 审批卡片与真正执行都走这里——两处共用同一套逻辑，避免「卡片上显示的」与「实际改的」不一致。
fn plan(loaded: &Loaded, edits: &[Edit]) -> Result<PlanResult, String> {
    let mut planned = Vec::new();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut needs_recalc = false;
    for e in edits {
        let sheet = workbook::find_sheet(&loaded.sheets, &e.sheet)
            .ok_or_else(|| format!("没有名为「{}」的工作表", e.sheet))?;
        let current = out
            .get(&sheet.entry)
            .or_else(|| loaded.xml.get(&sheet.entry))
            .ok_or_else(|| format!("工作表「{}」的内容未载入", e.sheet))?;
        let mut value = e.value()?;
        let mut note = String::new();
        if let CellValue::Formula { text, .. } = &value {
            // 前面几处修改已经生效，所以取值时当前表的「演变中」内容是准确的；
            // 跨表引用查的是载入时的快照（不追迹跨表依赖，算不出来就不算）。
            let entry_now = sheet.entry.clone();
            let snapshot = |want: Option<&str>, col: u32, row: u32| {
                let entry = match want {
                    None => Some(entry_now.clone()),
                    Some(name) => {
                        workbook::find_sheet(&loaded.sheets, name).map(|s| s.entry.clone())
                    }
                }?;
                if entry.is_empty() {
                    return None;
                }
                let xml = out.get(&entry).or_else(|| loaded.xml.get(&entry))?;
                Some(sheet_edit::peek_typed(
                    xml,
                    &format!("{}{}", sheet_edit::col_name(col), row),
                ))
            };
            match formula::evaluate(text, &snapshot) {
                formula::Computed::Value(n) => {
                    note = format!("（已算出 {n}）");
                    value = CellValue::Formula {
                        text: text.clone(),
                        cached: Some(n),
                    };
                }
                formula::Computed::NeedsRecalc => {
                    note = "（打开时重算）".to_string();
                    needs_recalc = true;
                }
            }
        }
        let old = sheet_edit::peek_cell(
            current,
            &e.cell,
            if loaded.shared.is_empty() {
                None
            } else {
                Some(&loaded.shared)
            },
        );
        let updated = sheet_edit::set_cell(current, &e.cell, &value)
            .map_err(|err| format!("{}!{}：{err}", e.sheet, e.cell))?;
        planned.push(Planned {
            sheet: e.sheet.clone(),
            cell: e.cell.to_ascii_uppercase(),
            old,
            new_text: format!("{}{note}", e.describe()),
        });
        out.insert(sheet.entry.clone(), updated);
    }
    Ok((planned, out, needs_recalc))
}

/// 渲染单元格级改动清单（审批卡片正文）。
fn render_plan(path: &str, planned: &[Planned]) -> String {
    let mut lines = vec![format!("将修改 {path}：")];
    for p in planned {
        let old = p.old.as_deref().unwrap_or("（空）");
        lines.push(format!("  {}!{}：{old} → {}", p.sheet, p.cell, p.new_text));
    }
    lines.join("\n")
}

/// 在工作簿声明上打「打开时重算」标记（`<calcPr fullCalcOnLoad="1"/>`）。
///
/// 什么时候需要它：改完的公式我们算不出来（语法不在白名单、引用了别的公式……），
/// 于是单元格里只写了公式没写值。Excel 看到已有公式就会按标记重算整本，
/// 使用者打开时看到的是正确数字，而不是一片空白。
///
/// 只能改这一个元素：`calcPr` 在 schema 里有固定位置（在 `sheets` / `definedNames` 之后，
/// 在 `pivotCaches` / `extLst` 之前），插错位置 Excel 会报「文件已损坏」。
fn mark_full_recalc(workbook_xml: &str) -> String {
    if let Some(at) = workbook_xml.find("<calcPr") {
        let Some(gt) = workbook_xml[at..].find('>').map(|e| at + e) else {
            return workbook_xml.to_string();
        };
        let tag = &workbook_xml[at..gt];
        if tag.contains("fullCalcOnLoad") {
            // 已经有这个属性：把值改写成 1（可能原来是 0）
            let updated = replace_attr_value(tag, "fullCalcOnLoad", "1");
            return format!("{}{}{}", &workbook_xml[..at], updated, &workbook_xml[gt..]);
        }
        let self_closing = tag.ends_with('/');
        let head = tag.trim_end_matches('/').trim_end();
        let updated = if self_closing {
            format!("{head} fullCalcOnLoad=\"1\"/>")
        } else {
            format!("{head} fullCalcOnLoad=\"1\"")
        };
        return format!("{}{}{}", &workbook_xml[..at], updated, &workbook_xml[gt..]);
    }
    // 没有 calcPr：插在它该在的位置。找第一个「比 calcPr 靠后」的元素，插到它前面；
    // 都没找到就插在 </workbook> 之前。
    const AFTER: [&str; 9] = [
        "<oleSize",
        "<customWorkbookViews",
        "<pivotCaches",
        "<smartTagPr",
        "<smartTagTypes",
        "<webPublishing",
        "<fileRecoveryPr",
        "<webPublishObjects",
        "<extLst",
    ];
    let insert_at = AFTER
        .iter()
        .filter_map(|t| workbook_xml.find(t))
        .min()
        .or_else(|| workbook_xml.find("</workbook>"))
        .unwrap_or(workbook_xml.len());
    format!(
        "{}<calcPr fullCalcOnLoad=\"1\"/>{}",
        &workbook_xml[..insert_at],
        &workbook_xml[insert_at..]
    )
}

/// 把一个属性（含引号）的值换成 `value`；属性不存在时原样返回。
fn replace_attr_value(tag: &str, name: &str, value: &str) -> String {
    let needle = format!("{name}=");
    let Some(at) = tag.find(&needle) else {
        return tag.to_string();
    };
    let rest = &tag[at + needle.len()..];
    let Some(quote) = rest.chars().next() else {
        return tag.to_string();
    };
    if quote != '"' && quote != '\'' {
        return tag.to_string();
    }
    let Some(len) = rest[1..].find(quote) else {
        return tag.to_string();
    };
    format!(
        "{}{}{}{}",
        &tag[..at],
        needle,
        format_args!("{quote}{value}{quote}"),
        &rest[1 + len + 1..]
    )
}

/// 生成修改后的文件内容：把每个工作表的新 XML 替换进去，其余内部文件原样搬运。
/// `recalc` 为真时同时把工作簿声明换成带「打开时重算」标记的版本。
fn build_patched(
    loaded: &Loaded,
    sheet_xml: &BTreeMap<String, String>,
    recalc: bool,
) -> Result<Vec<u8>, String> {
    let work = tempfile::tempdir().map_err(|e| format!("创建临时目录失败：{e}"))?;
    let out = work.path().join("patched.xlsx");
    let mut reps: Vec<patch::Replacement> = sheet_xml
        .iter()
        .map(|(name, xml)| patch::Replacement {
            name: name.clone(),
            bytes: xml.as_bytes().to_vec(),
        })
        .collect();
    if recalc && let Some(wb) = patch::read_entry(&loaded.path, workbook::WORKBOOK_ENTRY)? {
        let text =
            String::from_utf8(wb).map_err(|_| "工作簿声明不是文本，无法标记重算".to_string())?;
        let marked = mark_full_recalc(&text);
        if marked != text {
            reps.push(patch::Replacement {
                name: workbook::WORKBOOK_ENTRY.to_string(),
                bytes: marked.into_bytes(),
            });
        }
    }
    patch::apply(&loaded.path, &out, &reps)?;
    std::fs::read(&out).map_err(|e| format!("读取修改结果失败：{e}"))
}

/// `edit_document` 工具：保真修改表格文件。
pub struct EditDocumentTool;

#[async_trait::async_trait]
impl Tool for EditDocumentTool {
    fn name(&self) -> &'static str {
        "edit_document"
    }

    fn description(&self) -> &'static str {
        "保真修改已有的表格（.xlsx/.xlsm）或 Word 文档（.docx），文件里的图表、条件格式、数据透视表、页眉页脚、批注等一律原样保留。表格：用 edits 按单元格改（sheet + cell + 四选一的 text/number/formula/bool）。写公式时，能算的（纯数值四则运算与 SUM/AVERAGE/COUNT/MIN/MAX）会把算好的值一并写上，不看公式的读取方也能立刻拿到数字；算不出来的不编数字，改成让 Excel 打开时重算。Word：用 textEdits 做文字替换（find + replace，默认只替换唯一命中，命中多处会报错要求补充上下文）。一次最多 200 处。增删行列不在支持范围内。"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["path"],
  "properties": {
    "path": {"type": "string", "description": "工作区相对路径"},
    "edits": {
      "type": "array",
      "minItems": 1,
      "maxItems": 200,
      "description": "要修改的单元格列表",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["sheet", "cell"],
        "properties": {
          "sheet": {"type": "string", "description": "工作表名"},
          "cell": {"type": "string", "description": "单元格坐标，如 B3"},
          "text": {"type": "string", "description": "写入文本"},
          "number": {"type": "number", "description": "写入数值"},
          "formula": {"type": "string", "description": "写入公式，如 =SUM(B2:B9)"},
          "bool": {"type": "boolean", "description": "写入逻辑值"}
        }
      }
    },
    "textEdits": {
      "type": "array",
      "minItems": 1,
      "maxItems": 200,
      "description": "Word 文档的文字替换列表（表格请用 edits）",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["find"],
        "properties": {
          "find": {"type": "string", "description": "要查找的文字"},
          "replace": {"type": "string", "description": "替换成什么；留空表示删除"},
          "all": {"type": "boolean", "description": "是否替换全部命中，默认否（命中多处会报错）"}
        }
      }
    }
  }
}"#
    }

    fn kind(&self) -> ToolKind {
        ToolKind::FileWrite
    }

    async fn approval_detail(&self, ctx: &ToolCtx, args: &Value) -> Option<String> {
        let args: Args = serde_json::from_value(args.clone()).ok()?;
        match kind_of(&args.path)? {
            Kind::Sheet => {
                let roots = ctx.write_roots();
                let loaded = load(&roots, &args.path, &args.edits).ok()?;
                let (planned, _, _) = plan(&loaded, &args.edits).ok()?;
                Some(render_plan(&args.path, &planned))
            }
            Kind::Word => {
                let roots = ctx.write_roots();
                let resolved = pathutil::resolve_read(&roots, &args.path).ok()?;
                let xml =
                    super::patch::read_entry(&resolved, super::docx::DOCUMENT_ENTRY).ok()??;
                let xml = String::from_utf8(xml).ok()?;
                let applied = super::edit_word::preview(&xml, &args.text_edits).ok()?;
                Some(super::edit_word::render_plan(&args.path, &applied))
            }
        }
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        // 先过路径边界：越界是最该优先报出来的问题，不能被「格式不支持」掩盖
        let roots = ctx.write_roots();
        let resolved = match pathutil::resolve_read(&roots, &args.path) {
            Ok(p) => p,
            Err((c, m)) => return ToolOutcome::err(&c, m),
        };
        let kind = match kind_of(&args.path) {
            Some(k) => k,
            None => {
                return ToolOutcome::err(
                    "E_UNSUPPORTED",
                    format!(
                        "{} 不是支持修改的格式（只支持 .xlsx/.xlsm/.docx）。旧格式请先用对应软件另存为新格式。",
                        args.path
                    ),
                );
            }
        };
        if let Err(e) = check_edits(kind, &args) {
            return ToolOutcome::err("E_ARGS", e);
        }
        if kind == Kind::Word {
            if ctx.rt.root_session_id.is_none()
                && let Err(conflicts) =
                    crate::tools::claims::claim(&ctx.rt.id, std::slice::from_ref(&resolved))
            {
                return ToolOutcome::err(
                    "E_FILE_CLAIMED",
                    crate::tools::claims::denial_message(&conflicts),
                );
            }
            return run_word(ctx, &args, &resolved);
        }

        let roots = ctx.write_roots();
        let loaded = match load(&roots, &args.path, &args.edits) {
            Ok(l) => l,
            Err(e) => return ToolOutcome::err("E_DOCUMENT", e),
        };

        // 子代理并发写同一文件时拒绝（与其它写工具一致）
        if ctx.rt.root_session_id.is_none()
            && let Err(conflicts) =
                crate::tools::claims::claim(&ctx.rt.id, std::slice::from_ref(&loaded.path))
        {
            return ToolOutcome::err(
                "E_FILE_CLAIMED",
                crate::tools::claims::denial_message(&conflicts),
            );
        }

        let (planned, sheet_xml, needs_recalc) = match plan(&loaded, &args.edits) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("E_EDIT_FAILED", e),
        };
        let patched = match build_patched(&loaded, &sheet_xml, needs_recalc) {
            Ok(b) => b,
            Err(e) => return ToolOutcome::err("E_PATCH_FAILED", e),
        };

        // 备份 → 原子落盘 → 产物登记 → 回执
        let original = std::fs::read(&loaded.path).ok();
        let backup_path = match &original {
            Some(bytes) => match backup::save(&ctx.rt.data_dir, &loaded.path, bytes) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("修改前备份失败（{}）：{e}", args.path);
                    None
                }
            },
            None => None,
        };

        if let Err(e) = crate::util::atomic::atomic_write(&loaded.path, &patched) {
            return ToolOutcome::err("E_IO", format!("写入 {} 失败：{e}", args.path));
        }

        // 产物登记（子代理归属主会话；任务 runtime 跳过；失败仅记日志）
        if !ctx.rt.is_task_runtime {
            let owner = ctx
                .rt
                .root_session_id
                .clone()
                .unwrap_or_else(|| ctx.rt.id.clone());
            let canonical = pathutil::canonical_best_effort(&loaded.path)
                .to_string_lossy()
                .into_owned();
            if let Err(e) = ctx.core.store.append_artifact(
                &owner,
                &canonical,
                crate::core::sessions::ArtifactOp::Edit,
            ) {
                tracing::warn!("产物登记失败（edit_document {}）：{e}", args.path);
            }
        }

        let changes: Vec<Value> = planned
            .iter()
            .map(|p| {
                json!({
                    "sheet": p.sheet, "cell": p.cell,
                    "old": p.old, "new": p.new_text,
                })
            })
            .collect();
        let mut out = ToolOutcome::ok(json!({
            "path": args.path,
            "changed": changes.len(),
            "changes": changes,
            "backup": backup_path.map(|p| p.to_string_lossy().into_owned()),
            "hint": "其余内容（图表、条件格式、数据透视表等）已原样保留。公式单元格没有算好的值，Excel 打开时会自动重算。",
        }));
        out.warnings
            .push("本次改动已保留原件备份；如需回退请告知。".to_string());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use umya_spreadsheet::Worksheet;

    /// 造一份带样式与公式的样本表格，并返回其路径。
    fn write_sample(dir: &Path) -> PathBuf {
        let mut book = umya_spreadsheet::new_file();
        {
            let ws: &mut Worksheet = book.sheet_by_name_mut("Sheet1").unwrap();
            let data = [["月份", "金额"], ["1月", "120"], ["2月", "150"]];
            for (r, row) in data.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    ws.cell_mut(((c + 1) as u32, (r + 1) as u32)).set_value(*v);
                }
            }
        }
        let path = dir.join("sample.xlsx");
        umya_spreadsheet::writer::xlsx::write(&book, &path).unwrap();
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

    /// 读出某个工作表某个坐标的值（用于断言修改结果）。
    fn cell_of(path: &Path, sheet: &str, entry: &str, coord: &str) -> String {
        let book = umya_spreadsheet::reader::xlsx::read(path).unwrap();
        let _ = sheet;
        let _ = entry;
        book.sheet_by_name(sheet).unwrap().value(coord)
    }

    #[tokio::test]
    async fn edits_cells_and_keeps_other_content() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);

        let out = EditDocumentTool
            .run(
                &ctx,
                json!({
                    "path": "sample.xlsx",
                    "edits": [
                        {"sheet": "Sheet1", "cell": "B2", "number": 999.0},
                        {"sheet": "Sheet1", "cell": "A3", "text": "改过的月份"},
                        {"sheet": "Sheet1", "cell": "C1", "formula": "=SUM(B2:B3)"}
                    ]
                }),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["changed"], 3);
        // 改动生效
        let p = ws.path().join("sample.xlsx");
        assert_eq!(cell_of(&p, "Sheet1", "", "B2"), "999");
        assert_eq!(cell_of(&p, "Sheet1", "", "A3"), "改过的月份");
        // 未被改动的单元格保持原值
        assert_eq!(cell_of(&p, "Sheet1", "", "B3"), "150");
        assert_eq!(cell_of(&p, "Sheet1", "", "A1"), "月份");
        // 返回里带了改动清单与备份路径
        assert!(out.data["changes"].as_array().unwrap().len() == 3);
        assert!(out.data["backup"].as_str().unwrap().ends_with(".bak"));
    }

    /// 能算的公式：把算好的值一并写上（不看公式的读取方也能立刻拿到数字），
    /// 并且**不**需要给工作簿打「打开时重算」标记。
    #[tokio::test]
    async fn simple_formula_gets_a_cached_value() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);
        let target = ws.path().join("sample.xlsx");

        let out = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx", "edits": [
                    {"sheet": "Sheet1", "cell": "B2", "number": 999.0},
                    // 引用同一个批次里刚改过的单元格：应当看到改后的 999，而不是原值
                    {"sheet": "Sheet1", "cell": "C2", "formula": "=SUM(B2:B3)"}
                ]}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        // 缓存值真的落到文件里了；1149 = 999 + 150，正是「看到同批次刚改过的值」的证据
        // （若用了载入时的旧快照就是 120 + 150 = 270）。
        assert_eq!(cell_of(&target, "Sheet1", "", "C2"), "1149");
        // 审批/回执里写明算出了多少
        let changes = out.data["changes"].as_array().unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c["new"].as_str().unwrap_or("").contains("已算出 1149")),
            "{changes:?}"
        );
        // 都算得出来，就不该打重算标记（打了会让 Excel 每次打开都全量重算）
        let wb = String::from_utf8(
            patch::read_entry(&target, workbook::WORKBOOK_ENTRY)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(!wb.contains("fullCalcOnLoad"), "{wb}");
    }

    /// 算不出来的公式：只写公式、不编数字，同时给工作簿打「打开时重算」标记；
    /// 文件必须仍然能被正常读取（标记插错位置会被 Excel 当成文件损坏）。
    #[tokio::test]
    async fn complex_formula_marks_the_workbook_for_recalc() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);
        let target = ws.path().join("sample.xlsx");

        let out = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx", "edits": [
                    {"sheet": "Sheet1", "cell": "C3", "formula": "=VLOOKUP(1,A1:B3,2)"}
                ]}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        // 回执里说清楚了会由 Excel 重算，而不是假装有值
        let changes = out.data["changes"].as_array().unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c["new"].as_str().unwrap_or("").contains("打开时重算")),
            "{changes:?}"
        );
        // 工作表里只有公式、没有编出来的值
        let sheet = String::from_utf8(
            patch::read_entry(&target, "xl/worksheets/sheet1.xml")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(sheet.contains("<f>VLOOKUP(1,A1:B3,2)</f>"), "{sheet}");
        assert!(!sheet.contains("<f>VLOOKUP(1,A1:B3,2)</f><v>"), "{sheet}");
        // 工作簿声明上有重算标记，而且文件仍能被读（标记没插错位置）
        let wb = String::from_utf8(
            patch::read_entry(&target, workbook::WORKBOOK_ENTRY)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(wb.contains("fullCalcOnLoad=\"1\""), "{wb}");
        assert_eq!(cell_of(&target, "Sheet1", "", "A1"), "月份");
        assert_eq!(cell_of(&target, "Sheet1", "", "B3"), "150");
    }

    /// 「打开时重算」标记只在真需要时才进工作簿：已有的 `<calcPr>` 就地改属性，
    /// 不新增元素（同一份文件上多次改公式不应把 calcPr 越加越多）。
    #[test]
    fn recalc_marker_is_idempotent_and_keeps_schema_order() {
        let wb = r#"<?xml version="1.0"?><workbook><sheets><sheet name="S" sheetId="1"/></sheets><definedNames/><calcPr calcId="0"/><pivotCaches/></workbook>"#;
        let once = mark_full_recalc(wb);
        let twice = mark_full_recalc(&once);
        assert_eq!(once, twice, "重复标记不得叠加");
        assert!(
            once.contains(r#"<calcPr calcId="0" fullCalcOnLoad="1"/>"#),
            "{once}"
        );
        assert_eq!(once.matches("<calcPr").count(), 1, "{once}");

        // 没有 calcPr：插在 pivotCaches / extLst 这些「靠后」的元素之前（schema 有固定顺序）
        let plain = r#"<workbook><sheets/><pivotCaches/><extLst/></workbook>"#;
        let marked = mark_full_recalc(plain);
        let at = marked.find("<calcPr").unwrap();
        assert!(at < marked.find("<pivotCaches").unwrap(), "{marked}");
        assert!(
            marked.contains(r#"<calcPr fullCalcOnLoad="1"/>"#),
            "{marked}"
        );

        // 连 </workbook> 都没有的怪文件：插在末尾，不 panic
        assert!(mark_full_recalc("<workbook>").contains("<calcPr"));
    }

    #[tokio::test]
    async fn approval_detail_shows_old_and_new_values() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core, rt);

        let detail = EditDocumentTool
            .approval_detail(
                &ctx,
                &json!({
                    "path": "sample.xlsx",
                    "edits": [{"sheet": "Sheet1", "cell": "B2", "number": 999.0}]
                }),
            )
            .await
            .expect("应给出单元格级改动清单");
        assert!(detail.contains("Sheet1!B2"), "{detail}");
        assert!(detail.contains("→ 999"), "{detail}");
        // 旧值应能读出（样本里 B2 是共享字符串「120」）
        assert!(detail.contains("120"), "旧值缺失：{detail}");
    }

    #[tokio::test]
    async fn backup_holds_the_content_before_edit() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let target = ws.path().join("sample.xlsx");
        let before = std::fs::read(&target).unwrap();
        let ctx = ctx_for(core, rt);

        let out = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx",
                       "edits": [{"sheet": "Sheet1", "cell": "B2", "number": 1.0}]}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        let backup_path = out.data["backup"].as_str().unwrap();
        assert_eq!(
            std::fs::read(backup_path).unwrap(),
            before,
            "备份必须是改动前的内容"
        );
        assert_ne!(std::fs::read(&target).unwrap(), before, "目标文件应已变化");
    }

    #[tokio::test]
    async fn rejects_wrong_value_arity_and_bad_sheet() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        write_sample(ws.path());
        let ctx = ctx_for(core.clone(), rt.clone());

        // 同时给 text 与 number
        let both = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx",
                       "edits": [{"sheet": "Sheet1", "cell": "B2", "text": "x", "number": 1.0}]}),
            )
            .await;
        assert!(!both.ok);
        assert!(both.error.unwrap().message.contains("恰好给一个"));

        // 一个都没给
        let none = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx", "edits": [{"sheet": "Sheet1", "cell": "B2"}]}),
            )
            .await;
        assert!(!none.ok);
        assert!(none.error.unwrap().message.contains("恰好给一个"));

        // 工作表名不存在：要列出可用工作表
        let bad_sheet = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "sample.xlsx",
                       "edits": [{"sheet": "不存在", "cell": "B2", "number": 1.0}]}),
            )
            .await;
        assert!(!bad_sheet.ok);
        assert!(bad_sheet.error.unwrap().message.contains("Sheet1"));
    }

    #[tokio::test]
    async fn rejects_unsupported_extension_and_oversize_count() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);

        std::fs::write(ws.path().join("old.xls"), b"x").unwrap();
        let bad = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "old.xls", "edits": [{"sheet": "Sheet1", "cell": "A1", "text": "x"}]}),
            )
            .await;
        assert!(!bad.ok);
        assert_eq!(bad.error.unwrap().code, "E_UNSUPPORTED");

        let many: Vec<Value> = (0..MAX_EDITS + 1)
            .map(|i| json!({"sheet": "Sheet1", "cell": format!("A{}", i + 1), "text": "x"}))
            .collect();
        let too_many = EditDocumentTool
            .run(&ctx, json!({"path": "sample.xlsx", "edits": many}))
            .await;
        assert!(!too_many.ok);
        assert_eq!(too_many.error.unwrap().code, "E_ARGS");
    }

    #[tokio::test]
    async fn path_outside_workspace_is_refused() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let (core, rt) = make_ctx(&ws, &dd);
        let ctx = ctx_for(core, rt);
        let out = EditDocumentTool
            .run(
                &ctx,
                json!({"path": "../../../etc/hosts",
                       "edits": [{"sheet": "S", "cell": "A1", "text": "x"}]}),
            )
            .await;
        assert!(!out.ok);
        assert_eq!(out.error.unwrap().code, "E_PATH_OUTSIDE");
    }
}
