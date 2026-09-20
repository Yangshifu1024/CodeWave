//! 保真修改的压缩包层：把 Office 文件当压缩包处理。
//!
//! 「解开 → 只替换目标内部文件 → 其余原样搬运重打包」。**关键在「原样搬运」**：
//! 未修改的条目用压缩包库的原样复制接口，连解压都不做，压缩数据与校验码逐字节保留。
//!
//! 为什么不整份解析重建：重建路线会把「解析器没读懂的内容」一并丢掉——图表、数据透视表、
//! 迷你图、切片器、外部数据连接都会凭空消失。这是格式与重建路线共同决定的，不是实现质量差异。

use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 要替换的一个内部文件：zip 条目名 + 新内容。
#[derive(Debug, Clone)]
pub struct Replacement {
    /// zip 内的条目名，如 `xl/worksheets/sheet1.xml`。
    pub name: String,
    /// 替换后的完整内容。
    pub bytes: Vec<u8>,
}

/// 一次修改的结果统计，供调用方与测试断言「动了几个、搬了几个」。
#[derive(Debug, Clone)]
pub struct PatchReport {
    /// 原文件里的条目总数。
    pub entries_total: usize,
    /// 被替换的条目名（顺序与原文件一致）。
    pub replaced: Vec<String>,
    /// 原样搬运的条目数。
    pub copied: usize,
}

/// 读出一个内部文件的内容（解压）；不存在返回 None。
pub fn read_entry(src: &Path, name: &str) -> Result<Option<Vec<u8>>, String> {
    let f = File::open(src).map_err(|e| format!("打开 {} 失败：{e}", src.display()))?;
    let mut archive =
        ZipArchive::new(BufReader::new(f)).map_err(|e| format!("不是有效的压缩包结构：{e}"))?;
    let mut entry = match archive.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(format!("读取内部文件 {name} 失败：{e}")),
    };
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("解压内部文件 {name} 失败：{e}"))?;
    Ok(Some(buf))
}

/// 列出全部内部文件名（按原文件顺序），供诊断与测试对比清单用。
pub fn list_entries(src: &Path) -> Result<Vec<String>, String> {
    let f = File::open(src).map_err(|e| format!("打开 {} 失败：{e}", src.display()))?;
    let archive =
        ZipArchive::new(BufReader::new(f)).map_err(|e| format!("不是有效的压缩包结构：{e}"))?;
    Ok(archive.file_names().map(|s| s.to_string()).collect())
}

/// 把 `src` 重新打包成 `dst`，其中 `replacements` 里的条目换成新内容，其余原样搬运。
///
/// 三条硬约束，任何一条不满足即整体失败（不产出半个文件）：
/// 1. 要替换的条目必须存在于原文件——否则说明我们对文件结构的假设错了，宁可报错不要瞎改；
/// 2. 带密码的内部条目一律拒绝（结构不同，这条路走不通）；
/// 3. 条目顺序与原文件保持一致（Excel 不在意，但改动最小化便于逐字节比对）。
pub fn apply(src: &Path, dst: &Path, replacements: &[Replacement]) -> Result<PatchReport, String> {
    let src_file = File::open(src).map_err(|e| format!("打开 {} 失败：{e}", src.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(src_file))
        .map_err(|e| format!("{} 不是有效的压缩包结构：{e}", src.display()))?;
    let entries_total = archive.len();

    // 约束 1：替换目标必须存在
    for r in replacements {
        if archive.index_for_name(&r.name).is_none() {
            return Err(format!(
                "原文件里没有名为 {} 的内部文件，出于安全考虑中止修改（文件结构可能与预期不同）",
                r.name
            ));
        }
    }

    let dst_file = File::create(dst).map_err(|e| format!("创建 {} 失败：{e}", dst.display()))?;
    let mut writer = ZipWriter::new(BufWriter::new(dst_file));
    // 新写入的条目统一用 deflate —— 与 Excel 自己产出的文件一致
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut replaced = Vec::new();
    let mut copied = 0usize;

    for i in 0..entries_total {
        let entry = archive
            .by_index_raw(i)
            .map_err(|e| format!("读取第 {} 个内部条目失败：{e}", i + 1))?;
        let name = entry.name().to_string();

        // 约束 2：加密条目直接拒绝
        if entry.encrypted() {
            return Err(format!(
                "文件包含受密码保护的内部条目（{name}），本应用不支持处理带密码的文件"
            ));
        }

        match replacements.iter().find(|r| r.name == name) {
            Some(r) => {
                // 目标条目：写新内容。先释放原条目借用，再取新字节。
                drop(entry);
                let bytes = r.bytes.clone();
                writer
                    .start_file(name.clone(), options)
                    .map_err(|e| format!("写入 {name} 失败：{e}"))?;
                std::io::Write::write_all(&mut writer, &bytes)
                    .map_err(|e| format!("写入 {name} 内容失败：{e}"))?;
                replaced.push(name);
            }
            None => {
                // 其余条目：原样搬运（不解压、不重压缩）
                writer
                    .raw_copy_file(entry)
                    .map_err(|e| format!("搬运 {name} 失败：{e}"))?;
                copied += 1;
            }
        }
    }

    writer
        .finish()
        .map_err(|e| format!("封包 {} 失败：{e}", dst.display()))?;

    Ok(PatchReport {
        entries_total,
        replaced,
        copied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 造一个简单压缩包当夹具。
    fn make_zip(path: &Path, entries: &[(&str, &str)]) {
        let f = File::create(path).unwrap();
        let mut w = ZipWriter::new(BufWriter::new(f));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, body) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn untouched_entries_are_copied_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.zip");
        let dst = dir.path().join("b.zip");
        make_zip(
            &src,
            &[
                ("keep.txt", "原始内容"),
                ("change.txt", "旧内容"),
                ("last.bin", "尾巴"),
            ],
        );

        let rep = vec![Replacement {
            name: "change.txt".into(),
            bytes: "新内容".as_bytes().to_vec(),
        }];
        let report = apply(&src, &dst, &rep).unwrap();

        assert_eq!(report.entries_total, 3);
        assert_eq!(report.replaced, vec!["change.txt".to_string()]);
        assert_eq!(report.copied, 2);

        // 清单顺序一致
        assert_eq!(list_entries(&src).unwrap(), list_entries(&dst).unwrap());
        // 目标条目换了内容
        assert_eq!(
            read_entry(&dst, "change.txt").unwrap().unwrap(),
            "新内容".as_bytes()
        );
        // 未动条目内容不变
        assert_eq!(
            read_entry(&dst, "keep.txt").unwrap().unwrap(),
            "原始内容".as_bytes()
        );
    }

    #[test]
    fn raw_bytes_of_untouched_entries_are_identical() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.zip");
        let dst = dir.path().join("b.zip");
        // 内容够长，保证压缩后确实产生差异（短内容压缩后可能退化为 Stored）
        let long = "重复内容".repeat(500);
        make_zip(&src, &[("keep.txt", &long), ("change.txt", "旧")]);

        apply(
            &src,
            &dst,
            &[Replacement {
                name: "change.txt".into(),
                bytes: b"new".to_vec(),
            }],
        )
        .unwrap();

        // 逐字节比对「未动条目」的原始压缩数据：这才是「原样搬运」的强证据
        let raw_of = |path: &Path, name: &str| -> Vec<u8> {
            let f = File::open(path).unwrap();
            let mut a = ZipArchive::new(BufReader::new(f)).unwrap();
            let mut e = a.by_name(name).unwrap();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            buf
        };
        assert_eq!(raw_of(&src, "keep.txt"), raw_of(&dst, "keep.txt"));
    }

    #[test]
    fn missing_target_is_refused_instead_of_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.zip");
        let dst = dir.path().join("b.zip");
        make_zip(&src, &[("only.txt", "x")]);
        let err = apply(
            &src,
            &dst,
            &[Replacement {
                name: "不存在.xml".into(),
                bytes: vec![],
            }],
        )
        .unwrap_err();
        assert!(err.contains("没有名为"), "{err}");
        assert!(!dst.exists(), "失败时不得留下半个文件");
    }

    #[test]
    fn corrupted_input_reports_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("bad.zip");
        std::fs::write(&src, "这不是压缩包".as_bytes()).unwrap();
        let err = apply(&src, &dir.path().join("out.zip"), &[]).unwrap_err();
        assert!(err.contains("压缩包结构"), "{err}");
    }

    #[test]
    fn read_entry_returns_none_for_absent_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.zip");
        make_zip(&src, &[("a.txt", "1")]);
        assert!(read_entry(&src, "a.txt").unwrap().is_some());
        assert!(read_entry(&src, "nope.txt").unwrap().is_none());
    }
}
