//! 会话历史里的图片外置存储（[docs/session-history-limits](../../../../docs/session-history-limits.md)）：
//! 历史文件（gzip 后 8MB 上限）里只留 `image_blob` 引用，base64 原文落到
//! `sessions/<owner>.imgblob/<blob>`。
//!
//! 为什么图片要外置：它是唯一能顶到 8MB 上限的载荷——base64 近似不可压缩（gzip 收效甚微），
//! 且在 token 估算里只按 1600 token/张计（[`crate::util::token_est`]），几乎不受 trim 约束。
//! 落盘副本换成引用之后，历史文件恒定在几百 KB 量级、上限几乎不可达，图片也不再被剥。
//! **内存模型与 wire 零改动**：转换只发生在落盘 DTO（[`super::persist`]）里。
//!
//! 设计要点：
//! - **内容寻址**：blob id = `sha256(base64 原文)` 的十六进制前 [`BLOB_ID_LEN`] 位 → 同图天然去重、写幂等；
//! - **失败不阻断保存**：写失败 / 单图超 [`MAX_DATA_CHARS`] 时返回 `None`，调用方退回内联（`image_inline`）；
//! - **归属随会话**：目录 `sessions/<owner>.imgblob/`（照 `sessions/<owner>.toolres/` 范式：路径由编号拼出、
//!   随会话级联删除、不进右栏「文件」面板），不做跨会话共享；
//! - **GC 只在历史写成功之后调用**（调用方纪律）：只删本会话目录内、本次引用集合之外的文件——
//!   历史没落盘却已删 blob 是不可逆的数据丢失。

use crate::core::sessions::SessionStore;
use crate::core::sessions::cleanup::is_safe_session_id;
use sha2::{Digest, Sha256};

/// blob id 长度（sha256 十六进制前 32 位 = 128 bit，碰撞概率可忽略，文件名也足够短）。
const BLOB_ID_LEN: usize = 32;

/// 单个 blob 的 base64 原文长度上限（≈5MB 原图对应的 base64 长度）：
/// 超过就保持内联（写了也会把历史推回上限，不如老实内联让降级阶梯去裁决）。
pub const MAX_DATA_CHARS: usize = 7_000_000;

/// blob id：`sha256(base64 原文)` 的十六进制前 [`BLOB_ID_LEN`] 位（内容寻址）。
pub fn blob_id(data: &str) -> String {
    let digest = Sha256::digest(data.as_bytes());
    let mut out = String::with_capacity(BLOB_ID_LEN);
    for b in digest.iter().take(BLOB_ID_LEN / 2) {
        let _ = std::fmt::write(&mut out, format_args!("{b:02x}"));
    }
    out
}

/// blob 名合法性：恰好 [`BLOB_ID_LEN`] 位小写十六进制。
///
/// 读 / GC 两条路径都过这道白名单：blob 名来自历史 JSON（磁盘上的普通数据），
/// 一条含 `/`、`\` 或 `..` 的脏名绝不能参与路径拼接；同时它也让 GC 只认自己写出的文件，
/// 不会顺手删掉目录里的临时文件（`atomic_write` 的中转文件、Windows 的 `.bak`）。
fn is_valid_blob_name(name: &str) -> bool {
    name.len() == BLOB_ID_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 写一份图片 base64，返回 blob id（内容寻址：已存在直接返回，幂等）。
///
/// 返回 `None` = 调用方保持内联，可能原因：单图超 [`MAX_DATA_CHARS`]、owner 编号非法、
/// 目录创建失败、写盘失败。**绝不抛错、绝不阻断历史保存**（图片只是历史的一部分）。
pub fn write(store: &SessionStore, owner: &str, data: &str) -> Option<String> {
    if data.len() > MAX_DATA_CHARS {
        tracing::warn!(
            "图片 base64 超过 {}MB 对应的长度上限，保持内联（{owner}）",
            MAX_DATA_CHARS / 1_000_000
        );
        return None;
    }
    if !is_safe_session_id(owner) {
        tracing::warn!("会话编号非法（{owner}），图片保持内联");
        return None;
    }
    let dir = store.image_blobs_dir(owner);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("图片 blob 目录创建失败（{}）：{e}", dir.display());
        return None;
    }
    let id = blob_id(data);
    let path = dir.join(&id);
    // 内容寻址 → 同内容同 id：已存在就是幂等命中，不重写（也避开 Windows 覆盖的 .bak 中转）
    if path.exists() {
        return Some(id);
    }
    match crate::util::atomic::atomic_write(&path, data.as_bytes()) {
        Ok(()) => Some(id),
        Err(e) => {
            tracing::warn!("图片 blob 写入失败（{}）：{e}", path.display());
            None
        }
    }
}

/// 读一份图片 base64（文件缺失 / 非法 blob 名 / 非法 owner / 非 UTF-8 → `None`）。
pub fn read(store: &SessionStore, owner: &str, blob: &str) -> Option<String> {
    if !is_valid_blob_name(blob) || !is_safe_session_id(owner) {
        return None;
    }
    let bytes = std::fs::read(store.image_blobs_dir(owner).join(blob)).ok()?;
    String::from_utf8(bytes).ok()
}

/// 回收本会话目录内**未被本次历史引用**的 blob（历史写成功之后才可调用）。
///
/// 失败只记 warn，不返回错误、不阻断保存——回收是尽力而为，漏删只是多占磁盘。
/// 只删「名字是合法 blob id」且不在 `referenced` 里的普通文件：目录里的其它东西
///（中转文件、`.bak`、用户手放的文件）一概不碰。
pub fn gc(store: &SessionStore, owner: &str, referenced: &[String]) {
    if !is_safe_session_id(owner) {
        return;
    }
    let dir = store.image_blobs_dir(owner);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_valid_blob_name(&name) || referenced.iter().any(|r| r == &name) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("图片 blob 回收失败（{}）：{e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (SessionStore::new(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn blob_id_is_content_addressed() {
        let a = blob_id("AAAA");
        assert_eq!(a.len(), BLOB_ID_LEN);
        assert!(is_valid_blob_name(&a), "生成的 id 必须是合法 blob 名：{a}");
        // 同内容同 id、不同内容不同 id
        assert_eq!(a, blob_id("AAAA"));
        assert_ne!(a, blob_id("AAAB"));
    }

    #[test]
    fn write_is_idempotent_and_dedups_by_content() {
        let (store, _dir) = store();
        let id1 = write(&store, "s1", "AAAA").expect("写入应成功");
        let id2 = write(&store, "s1", "AAAA").expect("重复写入应命中同一 id");
        assert_eq!(id1, id2);
        // 目录里只有一个文件（幂等：不重复写）
        let entries = std::fs::read_dir(store.image_blobs_dir("s1"))
            .unwrap()
            .count();
        assert_eq!(entries, 1);
        assert_eq!(read(&store, "s1", &id1).as_deref(), Some("AAAA"));
        // 不同内容 → 另一个 blob（同目录共存）
        let id3 = write(&store, "s1", "BBBB").unwrap();
        assert_ne!(id3, id1);
        assert_eq!(read(&store, "s1", &id3).as_deref(), Some("BBBB"));
        // owner 隔离：另一个会话读不到
        assert!(read(&store, "s2", &id1).is_none());
    }

    #[test]
    fn read_missing_or_illegal_blob_returns_none() {
        let (store, _dir) = store();
        let id = write(&store, "s1", "AAAA").unwrap();
        std::fs::remove_file(store.image_blobs_dir("s1").join(&id)).unwrap();
        assert!(read(&store, "s1", &id).is_none(), "blob 缺失 → None");

        // 非法 blob 名不参与路径拼接（越界尝试一律 None，不 panic）
        for bad in [
            "../../../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "",
            "0123456789abcdef0123456789abcde",   // 短一位
            "0123456789abcdef0123456789abcdeff", // 长一位
            "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",  // 非十六进制
            "0123456789ABCDEF0123456789ABCDEF",  // 大写（生成侧恒小写）
        ] {
            assert!(
                read(&store, "s1", bad).is_none(),
                "非法 blob 名必须拒绝：{bad}"
            );
        }
        // 非法 owner 同样拒绝
        assert!(read(&store, "../evil", &id).is_none());
        assert!(write(&store, "../evil", "AAAA").is_none());
    }

    #[test]
    fn oversized_data_stays_inline() {
        let (store, _dir) = store();
        let big = "A".repeat(MAX_DATA_CHARS + 1);
        assert!(write(&store, "s1", &big).is_none(), "超限应保持内联");
    }

    #[test]
    fn gc_deletes_only_unreferenced_blobs() {
        let (store, _dir) = store();
        let keep = write(&store, "s1", "AAAA").unwrap();
        let drop = write(&store, "s1", "BBBB").unwrap();
        // 目录里的异物（非 blob 名）绝不被 GC 删掉
        let foreign = store.image_blobs_dir("s1").join("stray.txt");
        std::fs::write(&foreign, b"x").unwrap();
        gc(&store, "s1", std::slice::from_ref(&keep));
        assert!(read(&store, "s1", &keep).is_some(), "被引用的 blob 不得删");
        assert!(
            read(&store, "s1", &drop).is_none(),
            "未引用的 blob 应被回收"
        );
        assert!(foreign.exists(), "异物不在 GC 范围内");
        // 引用集合为空 = 历史里已无图片 → 清空（只清 blob）
        gc(&store, "s1", &[]);
        assert!(read(&store, "s1", &keep).is_none());
        assert!(foreign.exists());
        // 目录不存在时 GC 静默返回（不 panic、不报错）
        gc(&store, "s-never-written", &[]);
    }
}
