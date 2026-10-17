//! #54 交付边车存储夹具：CAS 索引、孤儿记录、幂等、崩溃对账、保存失败。
//!
//! 这些用例直接打真实文件系统（隔离 tempfile），断言的是**磁盘与索引状态**，
//! 不是返回值字符串——"保存失败被吞成通过"正是它们要挡住的。

use super::*;
use crate::core::delivery::types::{DeliveryResult, EndedKind, RunIdentity};

fn sid() -> String {
    "sess1".to_string()
}

fn owner() -> RunIdentity {
    RunIdentity::root("sess1", "run1")
}

fn store(dir: &Path) -> DeliveryStore {
    DeliveryStore::new(dir.join("sessions"))
}

fn delivery(id: &str) -> DeliveryResult {
    DeliveryResult {
        schema: SCHEMA,
        id: id.into(),
        contract_revision: 1,
        owner: owner(),
        task_id: "task1".into(),
        ended: EndedKind::Report,
        completed: vec!["r1".into()],
        incomplete: Vec::new(),
        changes: Vec::new(),
        evidence: Vec::new(),
        blockers: Vec::new(),
        submitted_at: "0".into(),
    }
}

#[test]
fn record_is_not_readable_until_index_commits() {
    // 核心纪律：落盘 ≠ 被采纳。索引没提交前，读路径必须拿不到。
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let w = s
        .write_record(
            &sid(),
            &owner(),
            "op1",
            RecordBody::Delivery(Box::new(delivery("d1"))),
            Some("task1"),
        )
        .unwrap();
    assert!(w.persisted);
    assert!(s.load_record(&sid(), &w.record_id).is_some());
    // 一旦从索引移除（模拟索引提交丢失），记录立刻不可采纳。
    let mut idx = s.load_index(&sid());
    idx.records.clear();
    std::fs::write(
        dir.path().join("sessions/sess1.delivery/index.json"),
        serde_json::to_vec(&idx).unwrap(),
    )
    .unwrap();
    assert!(
        s.load_record(&sid(), &w.record_id).is_none(),
        "未入索引的孤儿记录不得被采纳"
    );
}

#[test]
fn same_operation_same_content_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let a = s
        .write_record(
            &sid(),
            &owner(),
            "op1",
            RecordBody::Delivery(Box::new(delivery("d1"))),
            Some("task1"),
        )
        .unwrap();
    let b = s
        .write_record(
            &sid(),
            &owner(),
            "op1",
            RecordBody::Delivery(Box::new(delivery("d1"))),
            Some("task1"),
        )
        .unwrap();
    assert_eq!(a.record_id, b.record_id);
    assert_eq!(
        s.load_index(&sid()).records.len(),
        1,
        "重复请求不产生第二条记录"
    );
}

#[test]
fn same_operation_different_content_is_rejected() {
    // 同 operation_id 不同内容必须报冲突，不能静默覆盖已提交事实。
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.write_record(
        &sid(),
        &owner(),
        "op1",
        RecordBody::Delivery(Box::new(delivery("d1"))),
        Some("task1"),
    )
    .unwrap();
    let mut other = delivery("d1");
    other.completed = vec!["r9".into()];
    let err = s
        .write_record(
            &sid(),
            &owner(),
            "op1",
            RecordBody::Delivery(Box::new(other)),
            Some("task1"),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E_OPERATION_CONFLICT");
}

#[test]
fn cross_session_write_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let other = RunIdentity::root("sess2", "run1");
    let err = s
        .write_record(
            &sid(),
            &other,
            "op1",
            RecordBody::Delivery(Box::new(delivery("d1"))),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), "E_DELIVERY_OWNER");
}

#[test]
fn save_failure_never_reports_persisted() {
    // 磁盘满 / 目录不可写时：如实报保存失败，绝不返回 persisted=true。
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    // 用一个文件占住 records 目录的位置，原子写必然失败。
    let d = s.delivery_dir(&sid());
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("records"), b"blocked").unwrap();
    let err = s
        .write_record(
            &sid(),
            &owner(),
            "op1",
            RecordBody::Delivery(Box::new(delivery("d1"))),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), "E_DELIVERY_SAVE");
}

#[test]
fn contract_revision_cas_rejects_stale_write() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let c1 = sample_contract(1);
    s.save_contract(&sid(), &owner(), "task1", None, &c1, "c_op1")
        .unwrap();

    // 用过期修订写：必须被拒。
    let c_stale = sample_contract(1);
    let err = s
        .save_contract(&sid(), &owner(), "task1", Some(0), &c_stale, "c_op2")
        .unwrap_err();
    assert_eq!(err.code(), "E_CONTRACT_STALE");

    // 正确修订：成功且修订递增。
    let c2 = sample_contract(2);
    let r = s
        .save_contract(&sid(), &owner(), "task1", Some(1), &c2, "c_op3")
        .unwrap();
    assert_eq!(r.revision, 2);
    assert_eq!(
        s.load_contract(&sid(), "task1").unwrap().revision,
        2,
        "读回必须是新修订"
    );
}

#[test]
fn contract_file_out_of_sync_with_index_is_not_loaded() {
    // 磁盘上被替换过的契约文件与索引条目不一致 → 视为未提交，不读取。
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_contract(&sid(), &owner(), "task1", None, &sample_contract(1), "c1")
        .unwrap();
    let path = s.contract_path(&sid(), "task1", 1);
    std::fs::write(&path, serde_json::to_vec(&sample_contract(9)).unwrap()).unwrap();
    assert!(
        s.load_contract(&sid(), "task1").is_none(),
        "文件与索引不一致时不得当作有效契约"
    );
}

#[test]
fn orphan_records_are_reported_and_never_adopted() {
    // 崩溃在「写记录」与「提交索引」之间：journal 有、索引没有 → 孤儿。
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.write_record(
        &sid(),
        &owner(),
        "op1",
        RecordBody::Delivery(Box::new(delivery("d1"))),
        Some("task1"),
    )
    .unwrap();
    let idx = s.load_index(&sid());
    let rec_id = idx.records.keys().next().unwrap().clone();
    // 手工抹掉索引条目，模拟索引提交丢失。
    let mut broken = idx.clone();
    broken.records.clear();
    std::fs::write(
        dir.path().join("sessions/sess1.delivery/index.json"),
        serde_json::to_vec(&broken).unwrap(),
    )
    .unwrap();

    let report = s.reconcile(&sid());
    assert!(report.orphans.contains(&"op1".to_string()));
    assert!(s.load_record(&sid(), &rec_id).is_none());
    // 孤儿文件仍在磁盘上（只诊断，不删唯一证据）。
    assert!(
        dir.path()
            .join("sessions/sess1.delivery/records")
            .join(format!("{rec_id}.json"))
            .exists()
    );
}

#[test]
fn reconcile_recovers_operation_whose_index_commit_landed() {
    // journal 落后于索引：记录已在索引 → 恢复为已提交。
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.write_record(
        &sid(),
        &owner(),
        "op1",
        RecordBody::Delivery(Box::new(delivery("d1"))),
        Some("task1"),
    )
    .unwrap();
    // 把 journal 改回未提交。
    let j = dir.path().join("sessions/sess1.delivery/journal/op1.json");
    let mut op: OperationEntry = serde_json::from_slice(&std::fs::read(&j).unwrap()).unwrap();
    op.committed = false;
    std::fs::write(&j, serde_json::to_vec(&op).unwrap()).unwrap();

    let report = s.reconcile(&sid());
    assert!(report.recovered.contains(&"op1".to_string()));
    assert!(report.orphans.is_empty());
}

#[test]
fn output_edge_is_written_with_budget_and_truncation_is_visible() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let payload = vec![b'x'; 1024];
    let (rel, truncated) = s.write_output(&sid(), "rcpt1", &payload).unwrap();
    assert!(!truncated);
    assert_eq!(rel, "outputs/rcpt1.log");
    assert_eq!(
        std::fs::read(dir.path().join("sessions/sess1.delivery").join(&rel))
            .unwrap()
            .len(),
        1024
    );
}

#[test]
fn output_truncation_is_reported_when_over_budget() {
    // 注入小阈值验证截断可见（生产恒用 8 MiB 常量）。
    let dir = tempfile::tempdir().unwrap();
    let s = DeliveryStore::new(dir.path().join("sessions")).with_budget_limits(1024, 4096, 1024);
    let payload = vec![b'x'; 2048];
    let (rel, truncated) = s.write_output(&sid(), "rcpt1", &payload).unwrap();
    assert!(truncated, "超预算必须显式标记，不能静默丢");
    assert_eq!(
        std::fs::read(dir.path().join("sessions/sess1.delivery").join(&rel))
            .unwrap()
            .len(),
        1024,
        "落盘长度应等于单份上限"
    );
}

#[test]
fn hard_fuse_stops_new_evidence_but_keeps_existing() {
    // 硬线：只停新写入，绝不删既有数据。
    let dir = tempfile::tempdir().unwrap();
    let s = DeliveryStore::new(dir.path().join("sessions")).with_budget_limits(1, 4, 64);
    // 先写一份超预算的，让占用超过硬线。
    let _ = s.write_output(&sid(), "first", &[b'a'; 64]);
    let err = s.write_output(&sid(), "second", b"b").unwrap_err();
    assert_eq!(err.code(), "E_DELIVERY_FUSE");
    assert!(
        dir.path()
            .join("sessions/sess1.delivery/outputs/first.log")
            .exists(),
        "硬线不得删除既有证据"
    );
}

#[test]
fn index_bump_is_monotonic_across_writes() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let r1 = s
        .write_record(
            &sid(),
            &owner(),
            "op1",
            RecordBody::Delivery(Box::new(delivery("d1"))),
            None,
        )
        .unwrap();
    let r2 = s
        .write_record(
            &sid(),
            &owner(),
            "op2",
            RecordBody::Delivery(Box::new(delivery("d2"))),
            None,
        )
        .unwrap();
    assert!(r2.revision > r1.revision);
}

#[test]
fn whole_task_cancel_is_indexed_and_reversible_visibility() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_contract(&sid(), &owner(), "task1", None, &sample_contract(1), "c1")
        .unwrap();
    assert!(!s.whole_task_cancelled(&sid(), "task1"));
    s.cancel_whole_task(&sid(), "task1", "x1").unwrap();
    assert!(s.whole_task_cancelled(&sid(), "task1"));
    // 取消只影响整体归约，契约本体与原始需求保留。
    assert!(s.load_contract(&sid(), "task1").is_some());
}

#[test]
fn session_dir_removal_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_contract(&sid(), &owner(), "task1", None, &sample_contract(1), "c1")
        .unwrap();
    s.remove_session_dir(&sid()).unwrap();
    assert!(!s.delivery_dir(&sid()).exists());
    // 重复删除不报错（删除路径要可重入）。
    s.remove_session_dir(&sid()).unwrap();
}

fn sample_contract(revision: u64) -> crate::core::delivery::types::TaskContract {
    crate::core::delivery::types::TaskContract {
        schema: SCHEMA,
        task_id: "task1".into(),
        revision,
        owner: owner(),
        requirements: vec![crate::core::delivery::types::Requirement {
            id: "r1".into(),
            title: "做一件事".into(),
            required: true,
            check_ids: vec![],
            artifact_ids: vec![],
        }],
        artifacts: Vec::new(),
        checks: Vec::new(),
        review_policy: Default::default(),
        scope_changes: Vec::new(),
    }
}

#[test]
fn multiple_task_contracts_and_old_revisions_remain_readable() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_contract(
        &sid(),
        &owner(),
        "task1",
        None,
        &sample_contract(1),
        "first",
    )
    .unwrap();
    let mut second = sample_contract(1);
    second.task_id = "task2".into();
    s.save_contract(&sid(), &owner(), "task2", None, &second, "second")
        .unwrap();
    assert_eq!(s.load_contract(&sid(), "task1").unwrap().task_id, "task1");
    assert_eq!(s.load_contract(&sid(), "task2").unwrap().task_id, "task2");
    s.ensure_readable(&sid()).unwrap();
    std::fs::write(s.index_path(&sid()), b"broken").unwrap();
    assert!(s.ensure_readable(&sid()).is_err());
}
#[test]
fn record_ids_preserve_distinct_operation_identity() {
    assert_ne!(
        host_id("call:a", "execution"),
        host_id("call-a", "execution")
    );
    assert_ne!(
        host_id(&format!("{}a", "x".repeat(100)), "execution"),
        host_id(&format!("{}b", "x".repeat(100)), "execution")
    );
}

#[test]
fn phase_changes_cannot_overwrite_an_immutable_record_or_output() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let original = store
        .write_record(
            &sid(),
            &owner(),
            "same",
            RecordBody::Delivery(Box::new(delivery("first"))),
            None,
        )
        .unwrap();
    store
        .write_record(
            &sid(),
            &owner(),
            "same",
            RecordBody::RepairBudget(Box::new(RepairBudgetState {
                task_id: "task1".into(),
                budget: Default::default(),
            })),
            None,
        )
        .unwrap();
    let mut changed = delivery("changed");
    changed.completed = vec!["forged".into()];
    assert!(
        store
            .write_record(
                &sid(),
                &owner(),
                "same",
                RecordBody::Delivery(Box::new(changed)),
                None
            )
            .is_err()
    );
    let back = store.load_record(&sid(), &original.record_id).unwrap();
    assert!(matches!(back.body,RecordBody::Delivery(d) if d.id=="first"));
    let (path, _) = store.write_output(&sid(), "output", b"original").unwrap();
    assert!(
        store
            .write_output(&sid(), "output", b"replacement")
            .is_err()
    );
    assert_eq!(
        std::fs::read(store.delivery_dir(&sid()).join(path)).unwrap(),
        b"original"
    );
}
