//! #12 声明与真实改动核对的夹具：虚构路径、目录冒充文件、越界 / 符号链接、
//! 旧版本 hash、删除声明反向验证、同 basename 不同 roots、真实未修改输入不得冒充本轮改动。

use super::*;
use crate::core::delivery::types::{
    ArtifactConstraint, ArtifactKind, DeliveryResult, EndedKind, RootIdentity, RootKind,
    RunIdentity, SCHEMA, ScopeChange, ScopeChangeKind, TaskContract,
};
use std::collections::BTreeMap;

fn roots_a(dir: &std::path::Path) -> RootResolver {
    let mut m = BTreeMap::new();
    m.insert("ws".to_string(), dir.to_path_buf());
    m
}

fn roots_two(a: &std::path::Path, b: &std::path::Path) -> RootResolver {
    let mut m = BTreeMap::new();
    m.insert("ws".to_string(), a.to_path_buf());
    m.insert("extra".to_string(), b.to_path_buf());
    m
}

fn p(root: &str, rel: &str) -> PathRef {
    PathRef {
        root_id: root.into(),
        relative_path: rel.into(),
    }
}

fn art(id: &str, root: &str, rel: &str, kind: ArtifactKind) -> ArtifactRequirement {
    ArtifactRequirement {
        id: id.into(),
        path: p(root, rel),
        kind,
        constraints: Vec::new(),
        required: true,
    }
}

fn change(
    root: &str,
    rel: &str,
    kind: ChangeKind,
    before: Option<&str>,
    after: Option<&str>,
) -> ChangeRef {
    ChangeRef {
        path: p(root, rel),
        before_hash: before.map(str::to_string),
        after_hash: after.map(str::to_string),
        kind,
    }
}

fn hash_of(bytes: &[u8]) -> String {
    crate::core::delivery::version::digest_of_bytes(bytes)
}

// ---------- 虚构路径 ----------

#[test]
fn fabricated_path_fails_because_it_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    let r = roots_a(dir.path());
    let out = validate_artifact(&r, &art("a1", "ws", "never-written.rs", ArtifactKind::Text));
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "artifact_missing");
}

#[test]
fn unknown_root_is_rejected_before_resolution() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let r = roots_a(dir.path());
    // root_id 指向未登记的根：拒绝解析，不因为"路径长得像"就放行。
    let out = validate_artifact(&r, &art("a1", "not_a_root", "a.txt", ArtifactKind::Text));
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "E_EVIDENCE_INVALID");
}

#[test]
fn path_traversal_escaping_the_root_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
    let r = roots_a(dir.path());
    let out = validate_artifact(&r, &art("a1", "ws", "../secret.txt", ArtifactKind::Text));
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "E_EVIDENCE_INVALID");
}

// ---------- 目录冒充文件 ----------

#[test]
fn directory_cannot_masquerade_as_a_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("out.txt")).unwrap();
    let r = roots_a(dir.path());
    let out = validate_artifact(&r, &art("a1", "ws", "out.txt", ArtifactKind::Text));
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "artifact_kind_mismatch");
}

#[test]
fn file_cannot_masquerade_as_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("build"), b"not a dir").unwrap();
    let r = roots_a(dir.path());
    let out = validate_artifact(&r, &art("a1", "ws", "build", ArtifactKind::Directory));
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "artifact_kind_mismatch");
}

// ---------- 符号链接 ----------

#[test]
#[cfg(unix)]
fn symlink_out_of_scope_is_not_readable() {
    // 越界链接读不到内容 → 判不成立（不是"链接存在就算交付"）。
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("real.rs"), b"real").unwrap();
    std::os::unix::fs::symlink(outside.path().join("real.rs"), dir.path().join("link.rs")).unwrap();
    let r = roots_a(dir.path());
    let out = validate_artifact(&r, &art("a1", "ws", "link.rs", ArtifactKind::Text));
    assert!(!out.satisfied, "越界 symlink 不得算有效产物");
}

// ---------- 内容约束 ----------

#[test]
fn media_type_and_size_constraints_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
    let r = roots_a(dir.path());
    let mut req = art("a1", "ws", "a.txt", ArtifactKind::Text);
    req.constraints = vec![ArtifactConstraint::MediaType {
        media_type: "application/pdf".into(),
    }];
    assert!(!validate_artifact(&r, &req).satisfied);
    req.constraints = vec![
        ArtifactConstraint::MediaType {
            media_type: "text/plain".into(),
        },
        ArtifactConstraint::MinBytes { bytes: 100 },
    ];
    let out = validate_artifact(&r, &req);
    assert_eq!(out.reason.unwrap().code, "artifact_too_small");
    req.constraints = vec![ArtifactConstraint::MinBytes { bytes: 5 }];
    assert!(validate_artifact(&r, &req).satisfied);
}

// ---------- 变更声明 ----------

#[test]
fn created_change_requires_matching_current_hash() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("new.rs");
    std::fs::write(&f, b"v1").unwrap();
    let r = roots_a(dir.path());
    // 声明的哈希过期（旧版本）→ 不成立
    let stale = change(
        "ws",
        "new.rs",
        ChangeKind::Created,
        None,
        Some(&hash_of(b"v0")),
    );
    assert!(!validate_change(&r, &stale).satisfied);
    // 与当前内容一致 → 成立
    let ok = change(
        "ws",
        "new.rs",
        ChangeKind::Created,
        None,
        Some(&hash_of(b"v1")),
    );
    assert!(validate_change(&r, &ok).satisfied);
}

#[test]
fn deleted_change_is_verified_inversely() {
    // 删除声明证明的是「基线存在 + 当前不存在」，不是「文件还在」。
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("gone.rs");
    std::fs::write(&f, b"was here").unwrap();
    let r = roots_a(dir.path());
    let baseline = hash_of(b"was here");
    // 文件仍在时不能声称已删除
    let still = change("ws", "gone.rs", ChangeKind::Deleted, Some(&baseline), None);
    let out = validate_change(&r, &still);
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "change_still_present");

    std::fs::remove_file(&f).unwrap();
    let gone = change("ws", "gone.rs", ChangeKind::Deleted, Some(&baseline), None);
    assert!(
        validate_change(&r, &gone).satisfied,
        "确实删掉后删除声明应成立"
    );
}

#[test]
fn deleted_change_without_baseline_is_rejected() {
    // 缺基线就无法证明"它曾经存在"——照"存在即满足"写会让删除声明永远失败或永远通过。
    let dir = tempfile::tempdir().unwrap();
    let r = roots_a(dir.path());
    let c = change("ws", "never.rs", ChangeKind::Deleted, None, None);
    let out = validate_change(&r, &c);
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "change_missing_baseline");
}

#[test]
fn unmodified_input_cannot_claim_to_be_this_rounds_change() {
    // 真实未修改的输入不得冒充本轮变更：前后摘要相同却声明 modified。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("untouched.rs"), b"same").unwrap();
    let r = roots_a(dir.path());
    let h = hash_of(b"same");
    let c = change(
        "ws",
        "untouched.rs",
        ChangeKind::Modified,
        Some(&h),
        Some(&h),
    );
    let out = validate_change(&r, &c);
    assert!(!out.satisfied);
    assert_eq!(out.reason.unwrap().code, "change_not_modified");
}

#[test]
fn rename_is_treated_as_delete_plus_create() {
    // 首版不做 rename 推断：声明 renamed 不被接受，必须拆成两条。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.rs"), b"data").unwrap();
    let r = roots_a(dir.path());
    let c = change(
        "ws",
        "new.rs",
        ChangeKind::Created,
        Some(&hash_of(b"data")),
        Some(&hash_of(b"data")),
    );
    // new.rs 尚不存在 → created 声明不成立（rename 没真发生）
    assert!(!validate_change(&r, &c).satisfied);
}

#[test]
fn same_basename_in_different_roots_does_not_confuse_validation() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("mod.rs"), b"AAA").unwrap();
    std::fs::write(b.join("mod.rs"), b"BBB").unwrap();
    let r = roots_two(&a, &b);

    let in_a = change(
        "ws",
        "mod.rs",
        ChangeKind::Created,
        None,
        Some(&hash_of(b"AAA")),
    );
    let in_b = change(
        "extra",
        "mod.rs",
        ChangeKind::Created,
        None,
        Some(&hash_of(b"AAA")),
    );
    assert!(validate_change(&r, &in_a).satisfied);
    assert!(
        !validate_change(&r, &in_b).satisfied,
        "另一根下同名文件的哈希不得被拿来满足本根声明"
    );
}

// ---------- 汇总 ----------

#[test]
fn cancelled_requirements_are_skipped_not_failed() {
    // 用户取消的范围只减少当前有效集，不参与核对失败。
    let dir = tempfile::tempdir().unwrap();
    let r = roots_a(dir.path());
    let c = TaskContract {
        schema: SCHEMA,
        task_id: "t".into(),
        revision: 1,
        owner: RunIdentity::root("s", "r"),
        requirements: Vec::new(),
        artifacts: vec![art("a1", "ws", "missing.txt", ArtifactKind::Text)],
        checks: Vec::new(),
        review_policy: Default::default(),
        scope_changes: vec![ScopeChange {
            id: "sc".into(),
            target_id: "a1".into(),
            kind: ScopeChangeKind::Cancelled,
            reason: "用户取消".into(),
            user_decision_id: Some("d".into()),
            at: "0".into(),
        }],
    };
    let delivery = DeliveryResult {
        schema: SCHEMA,
        id: "d".into(),
        contract_revision: 1,
        owner: RunIdentity::root("s", "r"),
        task_id: "t".into(),
        ended: EndedKind::Report,
        completed: Vec::new(),
        incomplete: Vec::new(),
        changes: Vec::new(),
        evidence: Vec::new(),
        blockers: Vec::new(),
        submitted_at: "0".into(),
    };
    let report = validate_claims(&c, &delivery, &r);
    assert!(report.all_satisfied());
    // 原始需求记录保留在契约里，没有被删除。
    assert_eq!(c.artifacts.len(), 1);
}

#[test]
fn root_kind_is_carried_for_host_resolution() {
    // 根类别只用于展示与解析，不影响存在性判定。
    let dir = tempfile::tempdir().unwrap();
    let r = roots_a(dir.path());
    let root = RootIdentity {
        root_id: "ws".into(),
        kind: RootKind::Workspace,
        absolute: dir.path().to_string_lossy().into_owned(),
    };
    assert!(r.contains_key(&root.root_id));
    assert_eq!(root.kind, RootKind::Workspace);
}
