//! 文档类命令（[docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)）：预览取数 + 修改回退。
//!
//! **预览**：界面不走「读文本文件」那条通道——Office 与 PDF 是二进制，走那条路只能拿到乱码。
//!
//! 界面预览不走「读文本文件」那条通道：Office 与 PDF 是二进制，走那条路只能拿到乱码。
//! 这里提供两个命令：
//!
//! - `preview_document`：复用 `read_document` 工具的实现，返回结构化结果
//!   （表格区域 / 文档段落 / PDF 文字）。**不复用就会有两套解析逻辑**，
//!   界面看到的与模型读到的迟早对不上。
//! - `read_file_chunk`：按分片返回原始字节（base64），供网页阅读器组件自己解析 PDF。
//!   分片是为了避开「一次性把整个文件塞进一条 IPC 消息」——大文件那样做会把内存撑爆。
//!
//! **回退**：保真修改动的是使用者的真实文件，改错一次代价很高。改之前那份内容存在
//! 会话数据目录里（[`crate::tools::document::backup`]），这里给两个命令把它拿回来。

use super::util::{Core, err};
use super::workspace::session_write_roots;
use base64::Engine as _;
use serde_json::{Value, json};

/// 预览体积上限：与 `read_document` 一致（200MB）。这是一条先按计划写死、
/// 待真实大文件验证的数值，不是已经验证过的安全值。
const MAX_PREVIEW_BYTES: u64 = 200 * 1024 * 1024;
/// 单次分片上限（4MB）：够大到让传输开销可以忽略，又小到不会让单条 IPC 消息过大。
const MAX_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// 表格 / 文档 / PDF 的结构化预览数据（返回体形态与 `read_document` 的工具结果一致）。
#[tauri::command]
pub async fn preview_document(
    core: Core<'_>,
    session_id: String,
    path: String,
    sheet: Option<String>,
    range: Option<String>,
    pages: Option<String>,
) -> Result<Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    let resolved = crate::tools::pathutil::resolve_read(&roots, &path)
        .map_err(|(c, m)| format!("{c}: {m}"))?;
    let out = crate::tools::document::read::read_for_preview(
        &path,
        &resolved,
        sheet.as_deref(),
        range.as_deref(),
        None,
        pages.as_deref(),
    );
    if out.ok {
        Ok(out.data)
    } else {
        Err(outcome_error(&out))
    }
}

/// 把工具失败结果摊成一条 `错误码: 说明` 文本（前端 Alert 直接显示）。
fn outcome_error(out: &crate::tools::ToolOutcome) -> String {
    match &out.error {
        Some(e) => format!("{}: {}", e.code, e.message),
        None => "预览失败，原因未知".into(),
    }
}

/// 分片读取工作区文件（base64 分片）；供 PDF 阅读器组件按需取字节。
#[tauri::command]
pub async fn read_file_chunk(
    core: Core<'_>,
    session_id: String,
    path: String,
    offset: u64,
    length: Option<u64>,
) -> Result<Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    let resolved = crate::tools::pathutil::resolve_read(&roots, &path)
        .map_err(|(c, m)| format!("{c}: {m}"))?;
    chunk_payload(&resolved, offset, length.unwrap_or(MAX_CHUNK_BYTES))
}

/// 分片读取实现（独立成函数便于测试）：根校验由调用方完成，这里只管体积上限与切片。
fn chunk_payload(
    resolved: &std::path::Path,
    offset: u64,
    length: u64,
) -> Result<serde_json::Value, String> {
    let meta = std::fs::metadata(resolved).map_err(err)?;
    if !meta.is_file() {
        return Err("目标不是常规文件".into());
    }
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err(format!(
            "文件为 {}MB，超过预览上限 {}MB，请用外部程序打开",
            meta.len() / 1024 / 1024,
            MAX_PREVIEW_BYTES / 1024 / 1024
        ));
    }
    let total = meta.len();
    if offset > total {
        return Err(format!("读取位置 {offset} 超出文件长度 {total}"));
    }
    let take = length.clamp(1, MAX_CHUNK_BYTES).min(total - offset);
    let mut buf = vec![0u8; take as usize];
    {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut f = std::fs::File::open(resolved).map_err(err)?;
        f.seek(SeekFrom::Start(offset)).map_err(err)?;
        f.read_exact(&mut buf).map_err(err)?;
    }
    Ok(json!({
        "offset": offset,
        "length": take,
        "total": total,
        "eof": offset + take >= total,
        "content": base64::engine::general_purpose::STANDARD.encode(&buf),
    }))
}

// ---------- 修改回退 ----------

/// 列出某个文档可回退的备份，**新的在前**。
///
/// 路径先过读边界校验：能读的文件才能看它的备份，不越界。
#[tauri::command]
pub async fn list_document_backups(
    core: Core<'_>,
    session_id: String,
    path: String,
) -> Result<Vec<crate::tools::document::backup::BackupInfo>, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    let resolved = crate::tools::pathutil::resolve_read(&roots, &path)
        .map_err(|(c, m)| format!("{c}: {m}"))?;
    Ok(crate::tools::document::backup::describe(
        &rt.data_dir,
        &resolved,
    ))
}

/// 把某一份备份还原回原文件位置（原子写），返回回执。
///
/// 两条校验不能省：
/// 1. `path` 过**写**边界校验（要覆盖它，读取权限不够）；
/// 2. `backup_path` 必须落在本会话的备份目录里——否则这个命令就成了「把任意文件内容
///    拷到任意可写位置」的通道，路径一旦能造，它就不是回退而是写文件的旁路。
///
/// 回退前会把**当前内容**也备份一份：「回退」本身也可能点错，点错还得能撒回来。
#[tauri::command]
pub async fn restore_document_backup(
    core: Core<'_>,
    session_id: String,
    path: String,
    backup_path: String,
) -> Result<Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    let target = crate::tools::pathutil::resolve_write(&roots, &path)
        .map_err(|(c, m)| format!("{c}: {m}"))?;
    restore_from_backup(&rt.data_dir, &target, &backup_path)
}

/// 回退实现（独立成函数便于测试）。
fn restore_from_backup(
    data_dir: &std::path::Path,
    target: &std::path::Path,
    backup_path: &str,
) -> Result<Value, String> {
    let dir = crate::tools::document::backup::dir(data_dir);
    let dir = std::fs::canonicalize(&dir).map_err(|e| format!("备份目录不可读：{e}"))?;
    let backup = std::fs::canonicalize(backup_path)
        .map_err(|e| format!("备份文件不存在：{backup_path}（{e}）"))?;
    if !backup.starts_with(&dir) || !backup.is_file() {
        return Err("指定的文件不在本会话的备份目录里，已拒绝".into());
    }
    let bytes = std::fs::read(&backup).map_err(|e| format!("读取备份失败：{e}"))?;

    // 先给「当前内容」留一份，再落盘：两处任一失败都不能让目标文件变半成品
    let current = std::fs::read(target).ok();
    let current_backup = current
        .as_deref()
        .and_then(|b| crate::tools::document::backup::save(data_dir, target, b).ok())
        .map(|p| p.to_string_lossy().into_owned());
    crate::util::atomic::atomic_write(target, &bytes).map_err(err)?;

    Ok(json!({
        "path": target.to_string_lossy(),
        "restoredFrom": backup.to_string_lossy(),
        "currentBackup": current_backup,
        "size": bytes.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分片读取：首片带 eof=false、末片 eof=true、切片以外的位置不越界；
    /// 超上限文件直接拒绝（提示改用外部程序）。
    #[test]
    fn chunk_payload_slices_and_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.bin");
        let data: Vec<u8> = (0u8..255).collect();
        std::fs::write(&f, &data).unwrap();

        let first = chunk_payload(&f, 0, 100).unwrap();
        assert_eq!(first["offset"], json!(0));
        assert_eq!(first["length"], json!(100));
        assert_eq!(first["total"], json!(255));
        assert_eq!(first["eof"], json!(false));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(first["content"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, data[..100].to_vec());

        // 末片自动收缩到剩余长度（请求 100，只剩 55）
        let last = chunk_payload(&f, 200, 100).unwrap();
        assert_eq!(last["length"], json!(55));
        assert_eq!(last["eof"], json!(true));

        // 恰好读完也算 eof
        let exact = chunk_payload(&f, 255, 100).unwrap();
        assert_eq!(exact["length"], json!(0));
        assert_eq!(exact["eof"], json!(true));

        // 越界位置拒绝
        assert!(chunk_payload(&f, 999, 10).is_err());
    }

    /// 分片长度请求值被夹在 [1, 4MB] 区间：0 或负值不会返回空片，超大请求不会一次拉满。
    #[test]
    fn chunk_length_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.bin");
        std::fs::write(&f, vec![7u8; 10]).unwrap();
        assert_eq!(chunk_payload(&f, 0, 0).unwrap()["length"], json!(1));
        assert_eq!(chunk_payload(&f, 0, u64::MAX).unwrap()["length"], json!(10));
    }

    /// 回退往返：备份里的内容是「改动前」的，还原后目标文件回到那个内容，
    /// 并且回退前的「当前内容」也被备份了一份（回退本身能再撒回）。
    #[test]
    fn restore_brings_back_the_backed_up_content() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let target = ws.path().join("表.xlsx");
        std::fs::write(&target, b"modified").unwrap();
        // 备份里存的是改动前的内容
        let bak = crate::tools::document::backup::save(dd.path(), &target, b"original").unwrap();

        let out =
            restore_from_backup(dd.path(), &target, &bak.to_string_lossy()).expect("回退应当成功");
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(out["size"], json!(b"original".len()));
        // 回退前的内容也进了一份备份
        let cur = out["currentBackup"].as_str().unwrap();
        assert_eq!(std::fs::read(cur).unwrap(), b"modified");
        // 回退后清单里至少有两份：刚回退掉的「当前内容」与那份旧备份
        let list = crate::tools::document::backup::describe(dd.path(), &target);
        assert!(list.len() >= 2, "{list:?}");
    }

    /// 备份路径必须落在本会话备份目录里：拿别的文件当「备份」传进来一律拒绝，
    /// 否则这个命令就成了把任意文件内容拷到任意可写位置的通道。
    #[test]
    fn restore_refuses_backup_outside_backup_dir() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let target = ws.path().join("表.xlsx");
        std::fs::write(&target, b"modified").unwrap();
        let _ = crate::tools::document::backup::dir(dd.path());
        // 备份目录必须在（否则报的是另一条错），但这里故意用一个外部文件
        std::fs::create_dir_all(crate::tools::document::backup::dir(dd.path())).unwrap();
        let evil = ws.path().join("evil.bin");
        std::fs::write(&evil, b"arbitrary").unwrap();

        let e = restore_from_backup(dd.path(), &target, &evil.to_string_lossy()).unwrap_err();
        assert!(e.contains("备份目录"), "{e}");
        // 目标文件一个字都不能动
        assert_eq!(std::fs::read(&target).unwrap(), b"modified");

        // 不存在的路径同样是拒绝而不是崩
        assert!(restore_from_backup(dd.path(), &target, "/nope/nope.bak").is_err());
    }
}
