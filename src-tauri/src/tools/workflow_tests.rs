use super::*;
use crate::core::agent::test_support::make_core;
#[tokio::test]
async fn workflow_confirmation_preserves_all_permission_modes() {
    for mode in [
        crate::core::prefs::ApprovalMode::ConfirmEach,
        crate::core::prefs::ApprovalMode::Plan,
        crate::core::prefs::ApprovalMode::FullAccess,
    ] {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("workspace");
        let data = d.path().join("profile");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let core = make_core(&crate::tools::pathutil::WriteRoots {
            workspace: ws.clone(),
            data_dir: data,
            project_dir: None,
            extra: vec![],
        });
        let rt = core.get_or_create_session(
            "workflow-test",
            ws,
            Some("project1".into()),
            vec![],
            None,
            vec![],
        );
        let mut prefs = rt.prefs();
        prefs.approval_mode = mode;
        rt.set_prefs(prefs);
        let ctx = ToolCtx {
            core,
            rt: rt.clone(),
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            provider_call_id: "workflow-save".into(),
            cancel: Default::default(),
        };
        let answer = async {
            let id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if let Some(id) = rt.asks.lock().unwrap().keys().next().cloned() {
                        break id;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(rt.resolve_ask(
                &id,
                json!({"answers":{"workflow_change":{"selections":["apply_workflow"]}}})
            ));
        };
        let args = json!({"action":"save_config","expectedRevision":0,"definition":{"schema":1,"id":"main","nodes":[{"id":"build","title":"build"}]}});
        let (result, ()) = tokio::join!(WorkflowTool.run(&ctx, args), answer);
        assert!(result.ok, "{result:?}");
        assert_eq!(rt.prefs().approval_mode, mode);
        assert!(rt.asks.lock().unwrap().is_empty());
    }
}
