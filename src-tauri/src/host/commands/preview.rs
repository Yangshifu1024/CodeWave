//! 文档预览取数命令（[docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)）。
//!
//! 界面预览不走「读文本文件」那条通道：Office 与 PDF 是二进制，走那条路只能拿到乱码。
//! 这里提供两个命令：
//!
//! - `preview_document`：复用 `read_document` 工具的实现，返回结构化结果
//!   （表格区域 / 文档段落 / PDF 文字）。**不复用就会有两套解析逻辑**，
//!   界面看到的与模型读到的迟早对不上。
//! - `read_file_chunk`：按分片返回原始字节（base64），供网页阅读器组件自己解析 PDF。
//!   分片是为了避开「一次性把整个文件塞进一条 IPC 消息」——大文件那样做会把内存撑爆。

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
}
