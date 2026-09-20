//! 保真修改的可行性验证：用真实样本文件走一遍「解开 → 改一个单元格 → 打包」，
//! 逐条对比内部文件——除目标之外，每一条都必须逐字节不变。
//!
//! 这是整个方案里唯一「机制上可行、但结果是否被 Excel 接受未经验证」的环节，所以单独立一个
//! 验证用例。样本取自 umya-spreadsheet 自带的测试文件（含图表、条件格式、数据透视表的真实文件）；
//! 找不到样本时跳过，不影响常规测试。

use super::{patch, sheet_edit};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// 在 cargo 的本地缓存里找 umya 的测试样本目录。
fn sample_dir() -> Option<PathBuf> {
    let root = dirs::home_dir()?.join(".cargo/registry/src");
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let candidate = e.path().join("umya-spreadsheet-3.1.0/tests/test_files");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// 读出压缩包里每条内部文件的原始（压缩态）字节，用于逐条比对。
fn raw_entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let f = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f)).unwrap();
    let names: Vec<String> = archive.file_names().map(|s| s.to_string()).collect();
    let mut out = BTreeMap::new();
    for name in names {
        let mut e = archive.by_name(&name).unwrap();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        out.insert(name, buf);
    }
    out
}

/// 单个样本的验证结果。
struct CaseResult {
    file: String,
    entries: usize,
    copied: usize,
    /// 未通过对比的条目（条目名丢失/新增/内容变化）
    problems: Vec<String>,
    /// 目标单元格是否真的被改了
    target_changed: bool,
    /// 各阶段耗时（毫秒）：读工作表、改单元格、重打包、对比
    ms_read: u128,
    ms_edit: u128,
    ms_pack: u128,
    ms_diff: u128,
}

impl CaseResult {
    fn passed(&self) -> bool {
        self.problems.is_empty() && self.target_changed
    }
}

/// 对一份样本跑一遍补丁并逐条对比。
fn run_case(dir: &Path, file: &str, sheet_entry: &str, coord: &str) -> Option<CaseResult> {
    let src = dir.join(file);
    let work = tempfile::tempdir().ok()?;
    let dst = work.path().join("patched.xlsx");

    let t0 = std::time::Instant::now();
    let xml = String::from_utf8(patch::read_entry(&src, sheet_entry).ok()??).ok()?;
    let ms_read = t0.elapsed().as_millis();
    eprintln!("   [读工作表] {ms_read}ms，{} 字节", xml.len());

    let t1 = std::time::Instant::now();
    let edited = sheet_edit::set_cell(
        &xml,
        coord,
        &sheet_edit::CellValue::Text("patch-验证值".into()),
    )
    .ok()?;
    let ms_edit = t1.elapsed().as_millis();
    eprintln!("   [改单元格] {ms_edit}ms，变成 {} 字节", edited.len());

    let t2 = std::time::Instant::now();
    let report = patch::apply(
        &src,
        &dst,
        &[patch::Replacement {
            name: sheet_entry.to_string(),
            bytes: edited.into_bytes(),
        }],
    )
    .ok()?;
    let ms_pack = t2.elapsed().as_millis();
    eprintln!(
        "   [重打包] {ms_pack}ms，条目 {} 搬运 {}",
        report.entries_total, report.copied
    );

    let t3 = std::time::Instant::now();
    let before = raw_entries(&src);
    let after = raw_entries(&dst);
    let ms_diff = t3.elapsed().as_millis();
    eprintln!("   [逐条对比] {ms_diff}ms，{} 条", before.len());

    let mut problems: Vec<String> = Vec::new();
    // 条目清单必须一致：不能丢，也不能多
    for name in before.keys() {
        if !after.contains_key(name) {
            problems.push(format!("丢失条目 {name}"));
        }
    }
    for name in after.keys() {
        if !before.contains_key(name) {
            problems.push(format!("多出条目 {name}"));
        }
    }
    // 除目标条目外，逐字节比对
    for (name, bytes) in &before {
        if name == sheet_entry {
            continue;
        }
        if after.get(name) != Some(bytes) {
            problems.push(format!("内容变化 {name}"));
        }
    }
    let target_changed = after.get(sheet_entry) != before.get(sheet_entry);

    Some(CaseResult {
        file: file.to_string(),
        entries: report.entries_total,
        copied: report.copied,
        problems,
        target_changed,
        ms_read,
        ms_edit,
        ms_pack,
        ms_diff,
    })
}

#[test]
fn byte_patch_preserves_every_other_part() {
    let Some(dir) = sample_dir() else {
        eprintln!("跳过：未找到 umya 测试样本目录");
        return;
    };
    // 覆盖不同类型：多图表、数据透视表、条件格式、批注、大文件、第三方产物
    let cases: &[&str] = &[
        "aaa.xlsx",
        "issue_281.xlsx",
        "issue_298.xlsx",
        "64450.xlsx",
        "table.xlsx",
        "wps_comment.xlsx",
        "aaa_large.xlsx",
        "openpyxl.xlsx",
        "google.xlsx",
        "libre2.xlsx",
        "issue_216.xlsx",
    ];

    let mut lines = Vec::new();
    let mut failures = Vec::new();
    let mut ran = 0usize;

    for file in cases {
        if !dir.join(file).exists() {
            continue;
        }
        let size = std::fs::metadata(dir.join(file))
            .map(|m| m.len())
            .unwrap_or(0);
        eprintln!("→ 开始处理 {file}（{size} 字节）");
        // 统一用 A1：有些文件里第 1 行整行为空，这条也会顺带验证「新建空行」这条路
        match run_case(&dir, file, "xl/worksheets/sheet1.xml", "A1") {
            Some(r) => {
                ran += 1;
                lines.push(format!(
                    "{:<20} 条目 {:>3}｜搬运 {:>3}｜读 {:>5}ms 改 {:>6}ms 包 {:>5}ms 比 {:>5}ms｜{}",
                    r.file,
                    r.entries,
                    r.copied,
                    r.ms_read,
                    r.ms_edit,
                    r.ms_pack,
                    r.ms_diff,
                    if r.passed() {
                        "✅ 其余条目逐字节不变，目标已改".to_string()
                    } else {
                        format!("❌ {}", r.problems.join("；"))
                    }
                ));
                if !r.passed() {
                    failures.push(format!("{}: {:?}", r.file, r.problems));
                }
            }
            None => lines.push(format!("{file:<20} ⚠️ 用例无法执行（结构不符合预期）")),
        }
    }

    eprintln!(
        "\n===== 保真修改可行性验证（样本 {ran} 份）=====\n{}\n",
        lines.join("\n")
    );
    assert!(ran > 0, "一份样本都没跑起来，验证无效");
    assert!(failures.is_empty(), "存在保真差异：{failures:#?}");
}

/// 把一份带图表、条件格式、数据透视表的真实样本改一个格子后写出来，
/// 供人工用真 Excel 打开确认（「会不会弹修复提示」「图还在不在」自动化测不出来）。
///
/// 仅当设置了环境变量 `CODEWAVE_VERIFY_OUT`（输出目录）时执行。
#[test]
fn emit_sample_for_manual_verification() {
    let Some(dir) = sample_dir() else {
        eprintln!("跳过：未找到样本目录");
        return;
    };
    let Ok(out_dir) = std::env::var("CODEWAVE_VERIFY_OUT") else {
        eprintln!("跳过：未设置 CODEWAVE_VERIFY_OUT");
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();

    // aaa.xlsx：11 个图表 + 条件格式 + 数据透视表，最能暴露「重建路线会丢东西」的问题
    for (file, coord, out_name) in [
        ("aaa.xlsx", "A1", "改动后-带图表与数据透视表.xlsx"),
        ("issue_216.xlsx", "A1", "改动后-75个内部文件.xlsx"),
    ] {
        let src = dir.join(file);
        if !src.exists() {
            continue;
        }
        let dst = out_dir.join(out_name);
        let xml = String::from_utf8(
            patch::read_entry(&src, "xl/worksheets/sheet1.xml")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let edited = sheet_edit::set_cell(
            &xml,
            coord,
            &sheet_edit::CellValue::Text("本单元格由 CodeWave 改动".into()),
        )
        .unwrap();
        patch::apply(
            &src,
            &dst,
            &[patch::Replacement {
                name: "xl/worksheets/sheet1.xml".into(),
                bytes: edited.into_bytes(),
            }],
        )
        .unwrap();
        // 原件也拷一份，方便对照
        std::fs::copy(&src, out_dir.join(format!("原始-{file}"))).unwrap();
        eprintln!("已写出 {}（对照原件 原始-{file}）", dst.display());
    }
}
