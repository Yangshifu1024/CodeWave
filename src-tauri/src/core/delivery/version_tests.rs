//! #15 范围版本与新鲜度的失败/边界夹具。
//!
//! 覆盖任务文档列出的八类场景：同 HEAD 不同脏内容、新增 untracked 纳入范围、无关目录
//! 变化不影响明确范围、锁文件改变失效、同长度同 mtime 内容变化仍失效、检查期间改动、
//! 越界 symlink 拒绝、跨平台相对路径归一。

use super::*;
use crate::core::delivery::types::{PathRef, RootIdentity, RootKind, VerificationScope};
use std::collections::BTreeMap;

fn root_id() -> String {
    "root_ws".to_string()
}

fn resolver(dir: &Path) -> RootResolver {
    let mut m = BTreeMap::new();
    m.insert(root_id(), dir.to_path_buf());
    m
}

fn root_identity(dir: &Path) -> RootIdentity {
    RootIdentity {
        root_id: root_id(),
        kind: RootKind::Workspace,
        absolute: dir.to_string_lossy().into_owned(),
    }
}

fn p(rel: &str) -> PathRef {
    PathRef {
        root_id: root_id(),
        relative_path: rel.to_string(),
    }
}

fn scope_for(dir: &Path, files: &[&str]) -> VerificationScope {
    VerificationScope {
        roots: vec![root_identity(dir)],
        inputs: files.iter().map(|f| p(f)).collect(),
        outputs: Vec::new(),
        dependency_files: Vec::new(),
        environment_keys: Vec::new(),
    }
}

fn opts() -> CaptureOptions {
    CaptureOptions::default()
}

#[test]
fn same_length_same_mtime_content_change_still_invalidates() {
    // mtime-only 判定的经典漏网：长度与 mtime 都不变，只有内容变了。
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, b"AAAA").unwrap();
    let scope = scope_for(dir.path(), &["a.txt"]);

    let before = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    // 只改内容，不改长度。
    std::fs::write(&f, b"BBBB").unwrap();
    let after = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();

    assert_ne!(before.digest, after.digest, "同长度内容变化必须被检出");
    assert!(matches!(
        compare_scope(&before, &after),
        Freshness::Stale { .. }
    ));
}

#[test]
fn identical_content_is_fresh_and_digest_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"same").unwrap();
    let scope = scope_for(dir.path(), &["a.txt"]);
    let r = resolver(dir.path());

    let a = capture_scope(&r, &scope, &opts()).unwrap();
    let b = capture_scope(&r, &scope, &opts()).unwrap();
    assert_eq!(a.digest, b.digest, "同一内容必须得到同一摘要");
    assert_eq!(compare_scope(&a, &b), Freshness::Fresh);
}

#[test]
fn unrelated_file_change_does_not_invalidate_declared_scope() {
    // 明确范围之外的改动不该让无关证据失效。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scoped.txt"), b"x").unwrap();
    std::fs::write(dir.path().join("other.txt"), b"y").unwrap();
    let scope = scope_for(dir.path(), &["scoped.txt"]);

    let before = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    std::fs::write(dir.path().join("other.txt"), b"z").unwrap();
    let after = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();

    assert_eq!(compare_scope(&before, &after), Freshness::Fresh);
}

#[test]
fn dependency_file_change_invalidates() {
    // 锁文件属于声明范围：它变了就该重验。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), b"v1").unwrap();
    let scope = VerificationScope {
        roots: vec![root_identity(dir.path())],
        inputs: vec![p("a.txt")],
        outputs: Vec::new(),
        dependency_files: vec![p("Cargo.lock")],
        environment_keys: Vec::new(),
    };
    let before = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), b"v2").unwrap();
    let after = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();

    assert!(matches!(
        compare_scope(&before, &after),
        Freshness::Stale { .. }
    ));
}

#[test]
fn untracked_new_file_in_scope_changes_digest() {
    // 首次采集时不存在、二次存在 → 记为 Missing→File，不靠 git 跟踪状态。
    let dir = tempfile::tempdir().unwrap();
    let scope = scope_for(dir.path(), &["new.txt"]);

    // 首次采集时文件还不存在（等价于 untracked 新增前）。
    let missing = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    assert_eq!(
        missing.entry(&root_id(), "new.txt").unwrap().kind,
        EntryKind::Missing
    );
    assert!(missing.is_valid(), "确定的缺失是完整观测，不是读不到");

    std::fs::write(dir.path().join("new.txt"), b"v1").unwrap();
    let present = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    assert_ne!(missing.digest, present.digest);
    assert_eq!(
        present.entry(&root_id(), "new.txt").unwrap().kind,
        EntryKind::File
    );
}

#[test]
fn deletion_is_explicit_missing_not_unreadable() {
    // 删除要能被单独识别（#12 的删除声明依赖它），不能塌成"读不到"。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gone.txt"), b"x").unwrap();
    let scope = scope_for(dir.path(), &["gone.txt"]);
    let before = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    let after = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();

    let e = after.entry(&root_id(), "gone.txt").unwrap();
    assert_eq!(e.kind, EntryKind::Missing);
    assert!(e.readable, "缺失是确定事实，不是读不到");
    assert!(matches!(
        compare_scope(&before, &after),
        Freshness::Stale { .. }
    ));
}

#[test]
fn symlink_escaping_read_roots_is_rejected_not_followed() {
    // 越界 symlink 不能被"读到了"当成内容。
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"outside").unwrap();
    let link = dir.path().join("escape.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), &link).unwrap();
    #[cfg(unix)]
    {
        let scope = scope_for(dir.path(), &["escape.txt"]);
        let v = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
        let e = v.entry(&root_id(), "escape.txt").unwrap();
        assert_eq!(e.kind, EntryKind::Symlink);
        assert!(!e.readable, "越界 symlink 必须判为不可读");
        assert!(e.sha256.is_none(), "越界 symlink 不得带回内容摘要");
        assert_eq!(v.completeness, VersionCompleteness::Incomplete);
    }
}

#[test]
fn unreadable_side_of_comparison_never_counts_as_fresh() {
    // 采集不完整 → Unknown。**不允许**因为两边都没读到就判"相同"。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let scope = scope_for(dir.path(), &["a.txt"]);
    let good = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();

    let mut broken = good.clone();
    broken.completeness = VersionCompleteness::Incomplete;
    assert!(
        matches!(compare_scope(&broken, &good), Freshness::Unknown { .. }),
        "记录侧不完整不得判 fresh"
    );
    assert!(
        matches!(compare_scope(&good, &broken), Freshness::Unknown { .. }),
        "当前侧不完整不得判 fresh"
    );
}

#[test]
fn unknown_root_is_incomplete_not_empty_hash() {
    let dir = tempfile::tempdir().unwrap();
    let scope = VerificationScope {
        roots: vec![root_identity(dir.path())],
        inputs: vec![PathRef {
            root_id: "not_a_root".into(),
            relative_path: "a.txt".into(),
        }],
        outputs: Vec::new(),
        dependency_files: Vec::new(),
        environment_keys: Vec::new(),
    };
    let v = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    assert_eq!(v.completeness, VersionCompleteness::Incomplete);
    assert!(!v.is_valid());
}

#[test]
fn path_traversal_in_scope_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let scope = scope_for(dir.path(), &["../outside.txt"]);
    let v = capture_scope(&resolver(dir.path()), &scope, &opts()).unwrap();
    assert_eq!(v.completeness, VersionCompleteness::Incomplete);
    assert!(v.entries.is_empty(), "越界路径不得产生条目");
}

#[test]
fn relative_path_normalization_is_cross_platform_stable() {
    // Windows 的 `\` 与 macOS 的 `/` 不能让同一份内容算出两个 digest：落盘的
    // relative_path 必须先归一。解析仍按各平台的文件系统语义走（Unix 上 `\` 是
    // 普通文件名字符，不是分隔符），所以这里断言归一结果而不是两个不同路径的摘要相等。
    assert_eq!(normalize_rel("./a.txt"), "a.txt");
    assert_eq!(normalize_rel(".\\a.txt"), "a.txt");
    assert_eq!(normalize_rel("dir\\sub\\f.txt"), "dir/sub/f.txt");
    assert_eq!(normalize_rel("dir/sub/f.txt"), "dir/sub/f.txt");

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let v = capture_scope(
        &resolver(dir.path()),
        &scope_for(dir.path(), &["./a.txt"]),
        &opts(),
    )
    .unwrap();
    assert_eq!(v.entry(&root_id(), "a.txt").unwrap().relative_path, "a.txt");
    assert_eq!(v.digest, {
        let v2 = capture_scope(
            &resolver(dir.path()),
            &scope_for(dir.path(), &["a.txt"]),
            &opts(),
        )
        .unwrap();
        v2.digest
    });
}

#[test]
fn digest_covers_toolchain_facts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let base = VerificationScope {
        roots: vec![root_identity(dir.path())],
        inputs: vec![p("a.txt")],
        outputs: Vec::new(),
        dependency_files: Vec::new(),
        environment_keys: Vec::new(),
    };
    let with_env = VerificationScope {
        environment_keys: vec!["definitely_not_a_real_tool".into()],
        ..base.clone()
    };
    let r = resolver(dir.path());
    let a = capture_scope(&r, &base, &opts()).unwrap();
    let b = capture_scope(&r, &with_env, &opts()).unwrap();
    assert_ne!(a.digest, b.digest, "工具链事实属于范围摘要");
    assert_eq!(b.toolchain.len(), 1);
    assert_eq!(b.toolchain[0].value, "unknown");
}

#[test]
fn entries_are_sorted_so_enum_order_cannot_change_digest() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["z.txt", "a.txt", "m.txt"] {
        std::fs::write(dir.path().join(n), n.as_bytes()).unwrap();
    }
    let mut scope = scope_for(dir.path(), &["z.txt", "a.txt", "m.txt"]);
    scope.inputs.reverse();
    let r = resolver(dir.path());
    let v = capture_scope(&r, &scope, &opts()).unwrap();
    let keys: Vec<&str> = v.entries.iter().map(|e| e.relative_path.as_str()).collect();
    assert_eq!(keys, vec!["a.txt", "m.txt", "z.txt"]);
}

#[test]
fn cancelled_capture_returns_error_without_empty_digest() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let scope = scope_for(dir.path(), &["a.txt"]);
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let o = CaptureOptions {
        cancel: Some(cancel),
    };
    let err = capture_scope(&resolver(dir.path()), &scope, &o).unwrap_err();
    assert_eq!(err.code, "E_CAPTURE_CANCELLED");
}

#[test]
fn declared_directory_detects_member_content_and_membership_changes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a"), b"old").unwrap();
    let roots = std::collections::BTreeMap::from([("ws".into(), dir.path().to_owned())]);
    let scope = VerificationScope {
        inputs: vec![PathRef {
            root_id: "ws".into(),
            relative_path: "src".into(),
        }],
        ..Default::default()
    };
    let first = capture_scope(&roots, &scope, &Default::default()).unwrap();
    std::fs::write(dir.path().join("src/a"), b"new").unwrap();
    let second = capture_scope(&roots, &scope, &Default::default()).unwrap();
    assert!(!compare_scope(&first, &second).is_fresh());
    std::fs::write(dir.path().join("src/b"), b"added").unwrap();
    assert!(
        !compare_scope(
            &second,
            &capture_scope(&roots, &scope, &Default::default()).unwrap()
        )
        .is_fresh()
    );
    let mut environment = second.clone();
    environment.toolchain.push(VersionFact {
        key: "node".into(),
        value: "different".into(),
    });
    assert!(!compare_scope(&second, &environment).is_fresh());
}

#[test]
fn output_generation_does_not_change_test_inputs_but_later_input_edits_do() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source"), b"input").unwrap();
    let mut scope = scope_for(dir.path(), &["source"]);
    scope.outputs = vec![p("report")];
    let roots = resolver(dir.path());
    let before = capture_scope(&roots, &scope, &Default::default()).unwrap();
    std::fs::write(dir.path().join("report"), b"generated").unwrap();
    let after = capture_scope(&roots, &scope, &Default::default()).unwrap();
    assert!(compare_execution_inputs(&before, &after, &scope).is_fresh());
    std::fs::write(dir.path().join("source"), b"edited").unwrap();
    let later = capture_scope(&roots, &scope, &Default::default()).unwrap();
    assert!(!compare_execution_inputs(&after, &later, &scope).is_fresh());
}

#[test]
fn recursive_inputs_ignore_real_host_control_state_but_include_project_task_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join(".codewave");
    let data = dir.path().join("profile");
    std::fs::create_dir_all(project.join("tasks")).unwrap();
    std::fs::create_dir_all(data.join("coord")).unwrap();
    std::fs::write(project.join("tasks/plan.md"), b"plan").unwrap();
    let mut roots = resolver(dir.path());
    roots.insert("workspace".into(), dir.path().to_path_buf());
    roots.insert("project_dir".into(), project.clone());
    roots.insert("data_dir".into(), data.clone());
    let scope = scope_for(dir.path(), &["."]);
    let before = capture_scope(&roots, &scope, &Default::default()).unwrap();
    std::fs::write(data.join("coord/lease.json"), b"control").unwrap();
    let after = capture_scope(&roots, &scope, &Default::default()).unwrap();
    assert!(compare_scope(&before, &after).is_fresh());
    std::fs::write(project.join("tasks/plan.md"), b"changed plan").unwrap();
    let changed = capture_scope(&roots, &scope, &Default::default()).unwrap();
    assert!(!compare_scope(&after, &changed).is_fresh());
}
