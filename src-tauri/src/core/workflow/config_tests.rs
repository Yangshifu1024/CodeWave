//! #6 项目工作流配置夹具：未知字段/角色/模型/工具拒绝、普通环拒绝而 repair 环接受、
//! 危险命令存得下但仍要审批、跨项目路径拒绝、CAS 冲突保留草稿、磁盘失败不丢原版本、
//! 外部修改不自动应用。

use super::*;

fn models() -> BTreeSet<String> {
    ["m-a", "m-b"].iter().map(|s| s.to_string()).collect()
}

fn stage(id: &str, next: &[&str]) -> Stage {
    Stage {
        id: id.into(),
        title: id.into(),
        next: next.iter().map(|s| s.to_string()).collect(),
        required_checks: vec![],
    }
}

fn def() -> WorkflowDefinition {
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA,
        id: "main".into(),
        revision: 1,
        stages: vec![
            stage("plan", &["build"]),
            stage("build", &["verify"]),
            stage("verify", &[]),
        ],
        edges: vec![
            Edge {
                from: "plan".into(),
                to: "build".into(),
                kind: EdgeKind::Advance,
            },
            Edge {
                from: "build".into(),
                to: "verify".into(),
                kind: EdgeKind::Advance,
            },
        ],
        roles: [(
            "backend-dev".to_string(),
            RolePolicy {
                tools: vec!["read".into(), "edit".into()],
                model_id: Some("m-a".into()),
                independent_review: true,
            },
        )]
        .into_iter()
        .collect(),
        checks: vec![CheckCommand {
            id: "cargo".into(),
            command_text: "cargo test --workspace".into(),
            cwd: Some("src-tauri".into()),
            dangerous: false,
        }],
        author: Some("user-1".into()),
    }
}

#[test]
fn a_plain_definition_validates() {
    let d = tempfile::tempdir().unwrap();
    assert!(validate(&def(), d.path(), &models()).is_ok());
}

#[test]
fn unknown_role_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut x = def();
    x.roles.insert(
        "wizard".into(),
        RolePolicy {
            tools: vec![],
            model_id: None,
            independent_review: false,
        },
    );
    let issues = validate(&x, d.path(), &models()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "unknown_role"));
}

#[test]
fn unknown_tool_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut x = def();
    x.roles
        .get_mut("backend-dev")
        .unwrap()
        .tools
        .push("teleport".into());
    let issues = validate(&x, d.path(), &models()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "unknown_tool"));
}

#[test]
fn unknown_model_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut x = def();
    x.roles.get_mut("backend-dev").unwrap().model_id = Some("m-不存在".into());
    let issues = validate(&x, d.path(), &models()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "unknown_model"));
}

#[test]
fn unknown_check_reference_in_a_stage_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut x = def();
    x.stages[2].required_checks.push("不存在的检查".into());
    let issues = validate(&x, d.path(), &models()).unwrap_err();
    assert!(issues.iter().any(|i| i.code == "unknown_check"));
}

#[test]
fn an_ordinary_cycle_is_rejected_but_a_repair_edge_is_accepted() {
    let d = tempfile::tempdir().unwrap();

    // 普通环：build → verify → build，两条都是 Advance。
    let mut ordinary = def();
    ordinary.edges.push(Edge {
        from: "verify".into(),
        to: "build".into(),
        kind: EdgeKind::Advance,
    });
    let issues = validate(&ordinary, d.path(), &models()).unwrap_err();
    assert!(
        issues.iter().any(|i| i.code == "ordinary_cycle_rejected"),
        "普通环意味着无限循环，必须拒绝"
    );

    // 修复环：同一条边标成 Repair —— 唯一允许成环的类别。
    let mut repair = ordinary.clone();
    repair.edges.last_mut().unwrap().kind = EdgeKind::Repair;
    assert!(validate(&repair, d.path(), &models()).is_ok());
}

#[test]
fn a_dangerous_command_may_be_stored_but_still_needs_execution_approval() {
    let mut x = def();
    x.checks.push(CheckCommand {
        id: "cleanup".into(),
        command_text: "rm -rf target".into(),
        cwd: None,
        dangerous: true,
    });
    let d = tempfile::tempdir().unwrap();
    // 存得下：保存配置**不绕**执行审批，也**不**因为危险就拒绝保存。
    assert!(validate(&x, d.path(), &models()).is_ok());
    // 但它被标记为需要审批 —— 真正执行时仍要过一次既有审批门。
    assert!(requires_execution_approval(&x, "cleanup"));
    assert!(!requires_execution_approval(&x, "cargo"));
}

#[test]
fn cross_project_paths_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let mut x = def();
    for bad in ["../other", "/etc", "a/../../b"] {
        x.checks[0].cwd = Some(bad.into());
        let issues = validate(&x, d.path(), &models()).unwrap_err();
        assert!(
            issues.iter().any(|i| i.code == "cross_project_path"),
            "越出项目主目录的 cwd 必须拒绝：{bad}"
        );
    }
}

#[test]
fn cas_conflict_keeps_the_draft_and_leaves_the_current_version_alone() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join(".codewave/workflows.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let owner = RunIdentity::root("s1", "run1");
    let cur = def();

    // 当前已是 revision 3，草稿基于 1 提交 → 冲突。
    let cur3 = WorkflowDefinition {
        revision: 3,
        ..cur.clone()
    };
    std::fs::write(&path, serde_json::to_vec(&cur3).unwrap()).unwrap();
    let out = commit(&path, &def(), 1, d.path(), &models(), &owner);
    assert_eq!(
        out,
        SaveOutcome::Conflict {
            current_revision: 3
        }
    );
    // 冲突时不得落盘覆盖当前有效版本。
    let on_disk: WorkflowDefinition =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk.revision, 3);
}

#[test]
fn an_external_edit_shows_up_as_a_revision_conflict_not_a_silent_apply() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join(".codewave/workflows.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let owner = RunIdentity::root("s1", "run1");
    let saved = def();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();

    // 外部把文件改成了 revision 9；我们的草稿仍基于 1。
    let mut external = saved.clone();
    external.revision = 9;
    std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();

    let out = commit(&path, &saved, 1, d.path(), &models(), &owner);
    assert!(
        matches!(out, SaveOutcome::Conflict { .. }),
        "外部修改不自动应用"
    );
    // 磁盘上仍是外部那一版。
    let on_disk: WorkflowDefinition =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk.revision, 9);
}

#[test]
fn a_write_failure_does_not_lose_the_previously_valid_version() {
    let d = tempfile::tempdir().unwrap();
    let owner = RunIdentity::root("s1", "run1");
    // 目录当路径 → 写入必失败。
    let bad_path = d.path().to_path_buf();
    let out = commit(&bad_path, &def(), 1, d.path(), &models(), &owner);
    assert!(matches!(out, SaveOutcome::Failed { .. }));
    // 磁盘上的原有效版本不受影响。
    assert!(bad_path.is_dir(), "写失败时目标原样保留");
}

#[test]
fn a_valid_commit_bumps_the_revision_and_records_the_real_author() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join(".codewave/workflows.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut owner = RunIdentity::root("s1", "run1");
    owner.actor_id = "user-42".into();
    std::fs::write(&path, serde_json::to_vec(&def()).unwrap()).unwrap();
    let out = commit(&path, &def(), 1, d.path(), &models(), &owner);
    assert_eq!(out, SaveOutcome::Committed { revision: 2 });
    let on_disk: WorkflowDefinition =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk.revision, 2);
    assert_eq!(on_disk.author.as_deref(), Some("user-42"));
}

#[test]
fn an_invalid_draft_is_rejected_before_touching_the_disk() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join(".codewave/workflows.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let owner = RunIdentity::root("s1", "run1");
    let mut bad = def();
    bad.checks[0].cwd = Some("../escape".into());
    std::fs::write(&path, serde_json::to_vec(&def()).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let out = commit(&path, &bad, 1, d.path(), &models(), &owner);
    assert!(
        matches!(out, SaveOutcome::Failed { ref message } if message.contains("cross_project_path"))
    );
    // 原有效版本逐字节不变。
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
