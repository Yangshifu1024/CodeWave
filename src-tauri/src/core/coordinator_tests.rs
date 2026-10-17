//! #36 协调夹具：租约代次、迟到释放、事务全生效或全不生效、恢复只读、跨会话隔离。

use super::*;

fn c() -> Arc<InProcessCoordinator> {
    InProcessCoordinator::new()
}

fn req(resource: &str, run: &str, attempt: u32) -> AcquireRequest {
    AcquireRequest {
        session_id: "s1".into(),
        root_run_id: run.into(),
        resource: resource.into(),
        attempt,
        expected_generation: None,
    }
}

#[test]
fn a_resource_has_exactly_one_holder() {
    let co = c();
    co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    let err = co.acquire(&req("shared/a.rs", "run2", 0)).unwrap_err();
    assert_eq!(err.code, "resource_held");
}

#[test]
fn the_same_holder_reacquiring_is_idempotent() {
    let co = c();
    let a = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    let b = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_late_release_does_not_free_the_successor_lease() {
    let co = c();
    let first = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    co.release(&first).unwrap();
    let second = co.acquire(&req("shared/a.rs", "run1", 1)).unwrap();
    assert_ne!(first.generation, second.generation);
    // 迟到的第一次释放 → 拒绝。
    let err = co.release(&first).unwrap_err();
    assert_eq!(err.code, "generation_mismatch");
    // 后继租约仍在。
    assert_eq!(co.recover("s1").unwrap().len(), 1);
}

#[test]
fn a_transaction_is_all_or_nothing() {
    let co = c();
    let lease = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    let bad = SharedTransaction {
        lease: lease.clone(),
        writes: vec![
            SharedWrite {
                path: "/p/a.rs".into(),
                contents: "x".into(),
            },
            SharedWrite {
                path: "  ".into(),
                contents: "y".into(),
            },
        ],
    };
    let err = co.commit(&bad).unwrap_err();
    assert_eq!(err.code, "invalid_write");
    // 一条都没落：部分生效无法回滚，等于没有隔离。
    assert_eq!(co.committed_count(), 0);
}

#[test]
fn a_valid_transaction_applies_every_write() {
    let co = c();
    let lease = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    let tx = SharedTransaction {
        lease,
        writes: vec![
            SharedWrite {
                path: "/p/a.rs".into(),
                contents: "x".into(),
            },
            SharedWrite {
                path: "/p/b.rs".into(),
                contents: "y".into(),
            },
        ],
    };
    let r = co.commit(&tx).unwrap();
    assert_eq!(r.applied, 2);
    assert_eq!(co.committed_count(), 1);
}

#[test]
fn a_commit_on_a_stale_lease_applies_nothing() {
    let co = c();
    let lease = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    co.release(&lease).unwrap();
    let err = co
        .commit(&SharedTransaction {
            lease,
            writes: vec![SharedWrite {
                path: "/p/a.rs".into(),
                contents: "x".into(),
            }],
        })
        .unwrap_err();
    assert_eq!(err.code, "stale_lease");
    assert_eq!(co.committed_count(), 0);
}

#[test]
fn a_stale_acquire_request_is_refused() {
    let co = c();
    co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    let mut bad = req("shared/b.rs", "run1", 0);
    bad.expected_generation = Some(999);
    let err = co.acquire(&bad).unwrap_err();
    assert_eq!(err.code, "stale_generation");
}

#[test]
fn recovery_returns_only_display_state_and_never_replays_work() {
    let co = c();
    let lease = co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    let lease2 = co.acquire(&req("shared/b.rs", "run1", 0)).unwrap();
    let back = co.recover("s1").unwrap();
    assert_eq!(back.len(), 2);
    assert!(back.contains(&lease) && back.contains(&lease2));
    // 恢复**不新增**任何提交：没有工具被重放。
    assert_eq!(co.committed_count(), 0);
}

#[test]
fn recovery_is_scoped_to_the_session() {
    let co = c();
    co.acquire(&req("shared/a.rs", "run1", 0)).unwrap();
    assert!(co.recover("别的会话").unwrap().is_empty());
}

#[test]
fn the_adapter_is_explicitly_marked_as_non_cross_process() {
    // C0 未实接前，这个适配器**不得**用于真实共享目录；类型名与文档都要能让人一眼看出来。
    let src = include_str!("coordinator.rs");
    assert!(
        src.contains("不具备跨进程语义"),
        "单进程适配必须自带不得用于真实共享目录的声明"
    );
}
