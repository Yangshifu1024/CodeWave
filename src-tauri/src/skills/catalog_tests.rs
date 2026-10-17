use super::catalog::SkillCatalogSnapshot;
use super::*;
use crate::resources::ResourceSource;

fn install(root: &Path, relative: &str, name: &str, body: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {body}\n---\n{body}\n"),
    )
    .unwrap();
    path
}

#[test]
fn seven_sources_remain_auditable_while_disabled_winner_never_falls_back() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    for (base, path, body) in [
        (
            home.path(),
            ".claude/skills/fixture/SKILL.md",
            "user-claude",
        ),
        (
            home.path(),
            ".agents/skills/fixture/SKILL.md",
            "user-agents",
        ),
        (
            ws.path(),
            ".claude/skills/fixture/SKILL.md",
            "workspace-claude",
        ),
        (
            ws.path(),
            ".agents/skills/fixture/SKILL.md",
            "workspace-agents",
        ),
        (dd.path(), "skills/fixture/SKILL.md", "global"),
        (pd.path(), "skills/fixture/SKILL.md", "project"),
    ] {
        install(base, path, "repo-index", body);
    }
    let build = |disabled: &[String]| {
        SkillCatalogSnapshot::build(
            Some(home.path()),
            ws.path(),
            dd.path(),
            disabled,
            Some(pd.path()),
        )
    };
    let snapshot = build(&[]);
    assert_eq!(snapshot.effective["repo-index"].body, "project\n");
    let entry = snapshot
        .audit
        .entries
        .iter()
        .find(|entry| entry.name == "repo-index")
        .unwrap();
    assert_eq!(
        entry
            .candidates
            .iter()
            .map(|candidate| candidate.meta.resource.as_ref().unwrap().source)
            .collect::<Vec<_>>(),
        [
            ResourceSource::ProjectManaged,
            ResourceSource::GlobalManaged,
            ResourceSource::WorkspaceAgents,
            ResourceSource::WorkspaceClaude,
            ResourceSource::UserAgents,
            ResourceSource::UserClaude,
            ResourceSource::Builtin,
        ]
    );
    assert_eq!(
        entry
            .candidates
            .iter()
            .filter(|candidate| candidate.selected)
            .count(),
        1
    );
    assert_eq!(
        entry.candidates[0].meta,
        snapshot.effective["repo-index"].meta
    );
    let disabled = build(&["repo-index".into(), "catalog-deleted-disabled".into()]);
    assert!(!disabled.effective.contains_key("repo-index"));
    let entry = disabled
        .audit
        .entries
        .iter()
        .find(|entry| entry.name == "repo-index")
        .unwrap();
    assert!(entry.disabled);
    assert_eq!(entry.candidates.len(), 7);
    assert!(
        entry.candidates[0].selected,
        "disabling a name must retain its source winner"
    );
    let orphan = disabled
        .audit
        .entries
        .iter()
        .find(|entry| entry.name == "catalog-deleted-disabled")
        .unwrap();
    assert!(orphan.disabled && orphan.candidates.is_empty());
}

#[test]
fn audit_get_and_list_share_cached_bytes_until_explicit_reload() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let path = install(
        dd.path(),
        "skills/catalog-cache/SKILL.md",
        "catalog-cache",
        "before",
    );
    let index = SkillIndex {
        home: None,
        cache: Mutex::new(None),
        ttl: Duration::from_secs(3600),
    };
    let audit = index.audit(ws.path(), dd.path(), &[], None);
    let cached = audit
        .entries
        .iter()
        .find(|entry| entry.name == "catalog-cache")
        .unwrap()
        .candidates[0]
        .meta
        .clone();
    // New raw bytes normalize to the same body, but must produce a new version after reload.
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replace('\n', "\r\n")).unwrap();
    let get = index
        .get(ws.path(), dd.path(), &[], None, "catalog-cache")
        .unwrap();
    assert_eq!(get.meta, cached);
    assert_eq!(
        index
            .list(ws.path(), dd.path(), &[], None)
            .into_iter()
            .find(|meta| meta.name == "catalog-cache")
            .unwrap(),
        cached
    );
    index.invalidate();
    let reloaded = index
        .get(ws.path(), dd.path(), &[], None, "catalog-cache")
        .unwrap();
    assert_eq!(reloaded.body, get.body);
    assert_ne!(
        reloaded.meta.resource.as_ref().unwrap().version,
        cached.resource.as_ref().unwrap().version
    );
    let audit = index.audit(ws.path(), dd.path(), &[], None);
    assert_eq!(
        audit
            .entries
            .iter()
            .find(|entry| entry.name == "catalog-cache")
            .unwrap()
            .candidates[0]
            .meta,
        reloaded.meta
    );
}

#[test]
fn delete_requires_current_identity_and_bytes_then_reveals_lower_source() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let pd = tempfile::tempdir().unwrap();
    let global = install(
        dd.path(),
        "skills/catalog-cas/SKILL.md",
        "catalog-cas",
        "global",
    );
    let project = install(
        pd.path(),
        "skills/catalog-cas/SKILL.md",
        "catalog-cas",
        "project",
    );
    let index = SkillIndex::new();
    let get = || {
        index
            .get(ws.path(), dd.path(), &[], Some(pd.path()), "catalog-cas")
            .unwrap()
    };
    let old = get().meta.resource.unwrap();
    std::fs::write(&project, "---\nname: catalog-cas\n---\nchanged\n").unwrap();
    assert!(
        index
            .delete_skill(
                ws.path(),
                dd.path(),
                Some(pd.path()),
                "catalog-cas",
                &old.identity,
                &old.version
            )
            .is_err()
    );
    assert!(project.exists() && global.exists());
    let current = get().meta.resource.unwrap();
    let lower =
        SkillCatalogSnapshot::build(None, ws.path(), dd.path(), &[], None).effective["catalog-cas"]
            .meta
            .resource
            .clone()
            .unwrap();
    assert!(
        index
            .delete_skill(
                ws.path(),
                dd.path(),
                Some(pd.path()),
                "catalog-cas",
                &lower.identity,
                &current.version
            )
            .is_err()
    );
    assert!(project.exists() && global.exists());
    index
        .delete_skill(
            ws.path(),
            dd.path(),
            Some(pd.path()),
            "catalog-cas",
            &current.identity,
            &current.version,
        )
        .unwrap();
    assert!(!project.parent().unwrap().exists());
    assert_eq!(get().body, "global\n");
    assert!(
        index
            .delete_skill(
                ws.path(),
                dd.path(),
                Some(pd.path()),
                "catalog-cas",
                &current.identity,
                &current.version
            )
            .is_err()
    );
    assert!(
        global.exists(),
        "stale project approval must not delete the revealed global skill"
    );
}

#[test]
fn root_skill_deletion_preserves_managed_root_and_other_contents() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let root_file = install(dd.path(), "skills/SKILL.md", "catalog-root", "root-file");
    let sibling = install(
        dd.path(),
        "skills/catalog-sibling/SKILL.md",
        "catalog-sibling",
        "sibling",
    );
    let keep = dd.path().join("skills/keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    let index = SkillIndex::new();
    let resource = index
        .get(ws.path(), dd.path(), &[], None, "catalog-root")
        .unwrap()
        .meta
        .resource
        .unwrap();
    index
        .delete_skill(
            ws.path(),
            dd.path(),
            None,
            "catalog-root",
            &resource.identity,
            &resource.version,
        )
        .unwrap();
    assert!(!root_file.exists());
    assert!(dd.path().join("skills").is_dir() && sibling.exists() && keep.exists());
    assert!(
        index
            .get(ws.path(), dd.path(), &[], None, "catalog-root")
            .is_none()
    );
    assert!(
        index
            .get(ws.path(), dd.path(), &[], None, "catalog-sibling")
            .is_some()
    );
}

#[test]
fn equal_precedence_conflicts_are_stable_and_invalid_candidates_are_diagnosed() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let later = install(
        dd.path(),
        "skills/z/SKILL.md",
        "catalog-collision",
        "z-winner",
    );
    install(
        dd.path(),
        "skills/a/SKILL.md",
        "catalog-collision",
        "a-shadow",
    );
    let invalid = dd.path().join("skills/broken.md");
    std::fs::write(&invalid, b"---\nname: [broken\n---\nbody").unwrap();
    let non_utf8 = dd.path().join("skills/binary.md");
    std::fs::write(&non_utf8, [0xff, 0xfe]).unwrap();
    for _ in 0..2 {
        let snapshot = SkillCatalogSnapshot::build(None, ws.path(), dd.path(), &[], None);
        assert_eq!(snapshot.effective["catalog-collision"].body, "z-winner\n");
        let entry = snapshot
            .audit
            .entries
            .iter()
            .find(|entry| entry.name == "catalog-collision")
            .unwrap();
        assert_eq!(entry.candidates.len(), 2);
        for path in [&later, &invalid, &non_utf8] {
            assert!(
                snapshot
                    .audit
                    .issues
                    .iter()
                    // read_dir uses native separators; fixture paths may contain
                    // '/' on Windows. Compare paths rather than their spelling.
                    .any(|issue| Path::new(&issue.path) == path.as_path()
                        && !issue.message.is_empty()),
                "missing diagnostic for {}",
                path.display()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn physical_aliases_are_deduplicated_and_linked_entries_cannot_be_deleted() {
    use std::os::unix::fs::symlink;
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    install(
        ws.path(),
        ".claude/skills/catalog-physical/SKILL.md",
        "catalog-physical",
        "physical",
    );
    std::fs::create_dir_all(ws.path().join(".agents")).unwrap();
    symlink(
        ws.path().join(".claude/skills"),
        ws.path().join(".agents/skills"),
    )
    .unwrap();
    std::fs::create_dir_all(dd.path().join("skills")).unwrap();
    let directory_target = install(
        outside.path(),
        "directory/SKILL.md",
        "catalog-link-directory",
        "outside-directory",
    );
    let file_target = install(
        outside.path(),
        "file.md",
        "catalog-link-file",
        "outside-file",
    );
    symlink(
        directory_target.parent().unwrap(),
        dd.path().join("skills/directory"),
    )
    .unwrap();
    symlink(&file_target, dd.path().join("skills/file.md")).unwrap();
    let broken = dd.path().join("skills/broken-link.md");
    symlink(outside.path().join("missing.md"), &broken).unwrap();
    let snapshot = SkillCatalogSnapshot::build(None, ws.path(), dd.path(), &[], None);
    assert!(
        snapshot
            .audit
            .issues
            .iter()
            .any(|issue| issue.path == broken.display().to_string() && !issue.message.is_empty()),
        "broken skill links must be diagnosed"
    );
    let physical = snapshot
        .audit
        .entries
        .iter()
        .find(|entry| entry.name == "catalog-physical")
        .unwrap();
    assert_eq!(physical.candidates.len(), 1);
    assert_eq!(
        physical.candidates[0]
            .meta
            .resource
            .as_ref()
            .unwrap()
            .source,
        ResourceSource::WorkspaceAgents
    );
    let index = SkillIndex::new();
    for name in ["catalog-link-directory", "catalog-link-file"] {
        let meta = &snapshot.effective[name].meta;
        assert!(!meta.deletable);
        let resource = meta.resource.as_ref().unwrap();
        assert!(
            index
                .delete_skill(
                    ws.path(),
                    dd.path(),
                    None,
                    name,
                    &resource.identity,
                    &resource.version
                )
                .is_err()
        );
    }
    assert_eq!(
        std::fs::read_to_string(directory_target).unwrap(),
        "---\nname: catalog-link-directory\ndescription: outside-directory\n---\noutside-directory\n"
    );
    assert!(file_target.exists());
}

#[cfg(unix)]
#[test]
fn linked_managed_root_is_loadable_but_never_authorizes_deletion_of_external_files() {
    use std::os::unix::fs::symlink;
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root_file = install(outside.path(), "SKILL.md", "catalog-linked-root", "root");
    let nested = install(
        outside.path(),
        "nested/SKILL.md",
        "catalog-linked-nested",
        "nested",
    );
    let untouched = outside.path().join("keep.txt");
    std::fs::write(&untouched, "outside must remain").unwrap();
    let originals: Vec<_> = [&root_file, &nested, &untouched]
        .into_iter()
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect();
    symlink(outside.path(), dd.path().join("skills")).unwrap();
    let snapshot = SkillCatalogSnapshot::build(None, ws.path(), dd.path(), &[], None);
    let index = SkillIndex::new();
    for name in ["catalog-linked-root", "catalog-linked-nested"] {
        let skill = &snapshot.effective[name];
        assert!(!skill.meta.deletable);
        assert!(index.get(ws.path(), dd.path(), &[], None, name).is_some());
        let resource = skill.meta.resource.as_ref().unwrap();
        assert!(
            index
                .delete_skill(
                    ws.path(),
                    dd.path(),
                    None,
                    name,
                    &resource.identity,
                    &resource.version
                )
                .is_err()
        );
    }
    assert!(outside.path().is_dir());
    for (path, bytes) in originals {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert!(
        std::fs::symlink_metadata(dd.path().join("skills"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn nameless_physical_skill_keeps_only_its_highest_precedence_alias_name() {
    use std::os::unix::fs::symlink;
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let original = ws
        .path()
        .join(".claude/skills/catalog-lower-alias/SKILL.md");
    std::fs::create_dir_all(original.parent().unwrap()).unwrap();
    std::fs::write(&original, "# Plain skill\nNo frontmatter.\n").unwrap();
    let higher = ws.path().join(".agents/skills/catalog-higher-alias");
    std::fs::create_dir_all(higher.parent().unwrap()).unwrap();
    symlink(original.parent().unwrap(), &higher).unwrap();
    let snapshot = SkillCatalogSnapshot::build(None, ws.path(), dd.path(), &[], None);
    assert!(!snapshot.effective.contains_key("catalog-lower-alias"));
    assert!(
        !snapshot
            .audit
            .entries
            .iter()
            .any(|entry| entry.name == "catalog-lower-alias")
    );
    let selected = &snapshot.effective["catalog-higher-alias"];
    assert_eq!(selected.body, "# Plain skill\nNo frontmatter.\n");
    let resource = selected.meta.resource.as_ref().unwrap();
    assert_eq!(resource.source, ResourceSource::WorkspaceAgents);
    assert_eq!(
        snapshot
            .audit
            .entries
            .iter()
            .flat_map(|entry| &entry.candidates)
            .filter(
                |candidate| candidate.meta.resource.as_ref().unwrap().identity == resource.identity
            )
            .count(),
        1
    );
    let index = SkillIndex::new();
    assert!(
        index
            .get(ws.path(), dd.path(), &[], None, "catalog-lower-alias")
            .is_none()
    );
    assert!(
        index
            .get(ws.path(), dd.path(), &[], None, "catalog-higher-alias")
            .is_some()
    );
}
