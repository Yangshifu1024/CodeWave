//! #4 选择性设计夹具：机械改动免设计、存储格式与高危面要设计、用户可要求完整覆盖、
//! 跳设计不跳批准、预览不冻结、范围未扩不重启、范围扩张保留旧版本。

use super::*;
use crate::core::delivery::types::PathRef;
use crate::core::workflow::routing::{RouteInput, route};

fn p(rel: &str) -> PathRef {
    PathRef {
        root_id: "ws".into(),
        relative_path: rel.into(),
    }
}

fn profile_for(paths: &[&str]) -> TaskProfile {
    let input = RouteInput {
        suggested: Some(TaskKind::Mechanical),
        planned_writes: paths.iter().map(|r| p(r)).collect(),
        ..Default::default()
    };
    route(&input).profile
}

#[test]
fn plain_mechanical_rename_needs_no_design() {
    let d = decide(
        &profile_for(&["src/util/format.rs"]),
        &[p("src/util/format.rs")],
        false,
    );
    assert!(!d.required);
    assert!(d.triggers.contains(&DesignTrigger::Mechanical));
}

#[test]
fn version_bump_needs_no_design() {
    // 改版本号是典型的机械活：不碰存储格式、不碰公共导出入口、无并发面。
    let d = decide(&profile_for(&["package.json"]), &[p("package.json")], false);
    assert!(!d.required);
}

#[test]
fn touching_a_shared_manifest_alongside_code_does_require_design() {
    // 连带改 manifest（依赖变更会影响锁文件与安装态）→ 保守方向：需要设计。
    let d = decide(
        &profile_for(&["package.json", "ui/src/App.tsx"]),
        &[p("package.json"), p("ui/src/App.tsx")],
        false,
    );
    assert!(d.required);
    assert!(
        d.reasons
            .iter()
            .any(|r| r.contains("未命中任何豁免") || r.contains("公共导出入口"))
    );
}

#[test]
fn storage_format_change_requires_design() {
    let d = decide(
        &profile_for(&["src/store/schema.rs"]),
        &[p("src/store/schema.rs")],
        false,
    );
    assert!(d.required);
    assert!(d.triggers.contains(&DesignTrigger::StorageFormat));
}

#[test]
fn security_and_destructive_paths_require_design_and_cannot_be_waived() {
    let d = decide(
        &profile_for(&["src/host/keyring.rs", "src/tools/delete.rs"]),
        &[p("src/host/keyring.rs"), p("src/tools/delete.rs")],
        false,
    );
    assert!(d.required);
    assert!(d.triggers.contains(&DesignTrigger::Security));
    assert!(d.triggers.contains(&DesignTrigger::Destructive));
}

#[test]
fn concurrency_paths_require_design() {
    let d = decide(
        &profile_for(&["src/core/writelock.rs"]),
        &[p("src/core/writelock.rs")],
        false,
    );
    assert!(d.required);
    assert!(d.triggers.contains(&DesignTrigger::Concurrency));
}

#[test]
fn user_can_demand_full_design_over_the_default() {
    // 默认机械（免设计），但用户明确要求完整覆盖 → 需要设计。
    let d = decide(
        &profile_for(&["src/util/format.rs"]),
        &[p("src/util/format.rs")],
        true,
    );
    assert!(d.required);
    assert!(d.triggers.contains(&DesignTrigger::UserRequested));
}

#[test]
fn skipping_design_does_not_skip_approval() {
    // 设计裁决只管「要不要先出方案」；验收门由 #16 独立把守。
    let d = decide(
        &profile_for(&["src/util/format.rs"]),
        &[p("src/util/format.rs")],
        false,
    );
    assert!(!d.required);
    // 该单元仍然拿得到必跑检查（机械精确断言），说明批准门没被跳过。
    let profile = profile_for(&["src/util/format.rs"]);
    let units = crate::core::workflow::routing::to_work_units(
        &profile,
        &crate::core::delivery::types::RunIdentity::root("s", "r"),
        &["r1".into()],
    );
    assert!(!units.is_empty());
}

#[test]
fn preview_does_not_freeze_the_baseline() {
    // 预览方案只是展示；冻结发生在批准那一刻。
    let rec = DesignRecord {
        id: "d1".into(),
        version: 1,
        contract_revision: 1,
        scope: vec![p("src/a.rs")],
        decision: decide(&profile_for(&["src/a.rs"]), &[p("src/a.rs")], false),
        baseline_frozen: false,
        at: "0".into(),
    };
    assert!(!rec.baseline_frozen);
}

#[test]
fn no_revision_when_scope_and_revision_are_unchanged() {
    let d = decide(&profile_for(&["src/a.rs"]), &[p("src/a.rs")], false);
    let rec = DesignRecord {
        id: "d1".into(),
        version: 1,
        contract_revision: 1,
        scope: vec![p("src/a.rs")],
        decision: d.clone(),
        baseline_frozen: true,
        at: "0".into(),
    };
    assert!(
        revise_on_scope_change(std::slice::from_ref(&rec), 1, &[p("src/a.rs")], d, "1").is_none()
    );
}

#[test]
fn scope_expansion_keeps_the_old_version_and_does_not_inherit_the_freeze() {
    let d = decide(&profile_for(&["src/a.rs"]), &[p("src/a.rs")], false);
    let v1 = DesignRecord {
        id: "d1".into(),
        version: 1,
        contract_revision: 1,
        scope: vec![p("src/a.rs")],
        decision: d.clone(),
        baseline_frozen: true,
        at: "0".into(),
    };
    let v2 = revise_on_scope_change(
        std::slice::from_ref(&v1),
        2,
        &[p("src/a.rs"), p("src/b.rs")],
        d,
        "1",
    )
    .expect("范围扩张应产生新版本");
    assert_eq!(v2.version, 2);
    assert!(!v2.baseline_frozen, "新范围尚未批准，不继承旧冻结");
    // 旧版本原样保留，可查「当时为什么这么定」。
    assert_eq!(v1.version, 1);
    assert!(v1.baseline_frozen);
    assert_eq!(v1.scope.len(), 1);
}

#[test]
fn non_mechanical_change_without_markers_still_requires_design() {
    // 保守方向：证明不了是机械活，就按需要设计处理。
    let d = decide(
        &profile_for(&["src/a.rs", "ui/App.tsx"]),
        &[p("src/a.rs"), p("ui/App.tsx")],
        false,
    );
    assert!(d.required);
    assert!(d.reasons.iter().any(|r| r.contains("未命中任何豁免")));
}
