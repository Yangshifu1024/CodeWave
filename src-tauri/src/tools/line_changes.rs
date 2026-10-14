//! 本次成功写入的行级增删统计，与工作区相对 HEAD 的 diff 无关。
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::time::Duration;

#[derive(Serialize)]
pub(crate) struct LineChanges {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

pub(crate) fn count(path: &str, before: &str, after: &str) -> LineChanges {
    let before = before.replace("\r\n", "\n");
    let after = after.replace("\r\n", "\n");
    // similar 超时后仍给出合法 diff，避免大文件的最短编辑搜索阻塞写入。
    let diff = TextDiff::configure()
        .timeout(Duration::from_millis(200))
        .diff_lines(&before, &after);
    let mut result = LineChanges {
        path: path.into(),
        added: 0,
        removed: 0,
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => result.added += 1,
            ChangeTag::Delete => result.removed += 1,
            ChangeTag::Equal => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_real_changes_without_unchanged_context_or_phantom_trailing_line() {
        let changes = count("a", "same\nold\nlast\n", "same\nnew\nextra\nlast\n");
        assert_eq!((changes.added, changes.removed), (2, 1));
        let empty = count("a", "", "");
        assert_eq!((empty.added, empty.removed), (0, 0));
        let new = count("a", "", "one\ntwo\n");
        assert_eq!((new.added, new.removed), (2, 0));
    }

    #[test]
    fn unchanged_and_crlf_equivalent_content_have_no_changes() {
        for (before, after) in [("same\n", "same\n"), ("a\r\nb\r\n", "a\nb\n")] {
            let changes = count("a", before, after);
            assert_eq!((changes.added, changes.removed), (0, 0));
        }
    }
}
