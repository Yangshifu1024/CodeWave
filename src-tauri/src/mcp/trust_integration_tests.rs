use super::tests::{node_cfg, wait_ready};
use super::*;
use serde_json::json;
use std::path::Path;

fn marked_config(path: &Path) -> McpServerConfig {
    let mut cfg = node_cfg();
    cfg.env
        .insert("CODEWAVE_TEST_PID_FILE".into(), path.display().to_string());
    cfg
}

async fn wait_stopped(manager: &McpManager, session: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let statuses = manager.status_for(session).await;
            if !statuses.is_empty()
                && statuses
                    .iter()
                    .all(|s| s.state == McpState::Stopped && s.pid.is_none() && s.tools == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unapproved connection did not settle without launching");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_entry_requires_launch_approval_and_probe_does_not_grant_it() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let cfg = marked_config(&pid_file);
    let key = PoolKey::project("trust-entry", "fixture").with_source(&dir.path().join("mcp.json"));
    let manager = Arc::new(McpManager::default());
    let http = reqwest::Client::new();
    let visible = vec![(key.clone(), cfg.clone())];

    manager.warm("s", visible.clone(), http.clone()).await;
    wait_stopped(&manager, "s").await;
    manager
        .connect_named(
            "s",
            visible.clone(),
            std::slice::from_ref(&key.name),
            http.clone(),
        )
        .await;
    wait_stopped(&manager, "s").await;
    // An altered configuration forces reconcile to queue connection work too.
    let mut changed = cfg.clone();
    changed
        .env
        .insert("FIXTURE_VERSION".into(), "changed".into());
    manager
        .reconcile(
            vec![("s".into(), vec![(key.clone(), changed.clone())])],
            http.clone(),
        )
        .await;
    wait_stopped(&manager, "s").await;
    assert!(
        manager
            .ensure(&key, changed, http.clone(), "s")
            .await
            .is_err()
    );
    assert!(
        manager
            .test_connection(&key, cfg.clone(), "", http.clone())
            .await
            .is_err()
    );
    assert!(manager.tool_snapshot_for("s").await.is_empty());
    assert!(
        !pid_file.exists(),
        "an unapproved entry point launched the server"
    );

    let challenge = manager.prepare_probe(&key, &cfg).unwrap();
    assert_eq!(
        manager
            .test_connection(&key, cfg.clone(), &challenge.id, http.clone())
            .await
            .unwrap(),
        2
    );
    assert!(
        pid_file.exists(),
        "approved probe did not actually execute Node"
    );
    assert!(!manager.launch_status(&key, &cfg).trusted);
    assert!(manager.tool_snapshot_for("s").await.is_empty());
    assert!(
        manager
            .test_connection(&key, cfg.clone(), &challenge.id, http.clone())
            .await
            .is_err()
    );
    std::fs::remove_file(&pid_file).unwrap();
    manager.warm("s", visible.clone(), http.clone()).await;
    wait_stopped(&manager, "s").await;
    assert!(
        !pid_file.exists(),
        "probe approval leaked into formal launch rights"
    );

    assert!(
        manager
            .set_launch_trust(&key, &cfg, "stale-version", true)
            .await
            .is_err()
    );
    assert!(!pid_file.exists());
    manager
        .set_launch_trust(&key, &cfg, &super::super::launch_version(&cfg), true)
        .await
        .unwrap();
    manager.warm("s", visible, http).await;
    wait_ready(&manager, "s", 1).await;
    let tools = manager.tool_snapshot_for("s").await;
    assert_eq!(tools.len(), 2);
    let echo = tools
        .iter()
        .find(|tool| tool.function_name.ends_with("__echo"))
        .expect("real server echo tool");
    let binding = manager
        .request_binding("s", &echo.function_name)
        .await
        .unwrap();
    let result = manager
        .call_bound(
            "s",
            &binding,
            json!({"text": "approved"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.text, "echo: approved");
    manager.disconnect_all().await;
}

#[cfg(unix)]
async fn wait_pid_file(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(path)
                && pid.trim().parse::<u32>().is_ok()
            {
                break pid.trim().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real Node process never wrote its PID")
}

#[cfg(unix)]
async fn wait_process_gone(pid: &str) {
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                tokio::process::Command::new("kill")
                    .args(["-0", pid])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            )
            .await
            .expect("PID check timed out")
            .expect("kill -0 unavailable");
            if !status.success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("revoked server process remained alive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_removes_tools_rejects_old_binding_and_prevents_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let calls = dir.path().join("calls");
    let mut cfg = marked_config(&pid_file);
    cfg.env
        .insert("CODEWAVE_TEST_CALL_LOG".into(), calls.display().to_string());
    let key = PoolKey::global("trust-revoke").with_source(&dir.path().join("mcp.json"));
    let version = super::super::launch_version(&cfg);
    let manager = Arc::new(McpManager::default());
    manager
        .set_launch_trust(&key, &cfg, &version, true)
        .await
        .unwrap();
    manager
        .warm(
            "s",
            vec![(key.clone(), cfg.clone())],
            reqwest::Client::new(),
        )
        .await;
    wait_ready(&manager, "s", 1).await;
    let original_pid = std::fs::read_to_string(&pid_file).unwrap();
    let tools = manager.tool_snapshot_for("s").await;
    let echo = tools
        .iter()
        .find(|tool| tool.function_name.ends_with("__echo"))
        .unwrap();
    let binding = manager
        .request_binding("s", &echo.function_name)
        .await
        .unwrap();
    manager
        .set_launch_trust(&key, &cfg, &version, false)
        .await
        .unwrap();
    assert!(manager.tool_snapshot_for("s").await.is_empty());
    assert!(
        manager
            .call_bound(
                "s",
                &binding,
                json!({"text": "must-not-run"}),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(!calls.exists(), "stale dispatch reached the server");
    #[cfg(unix)]
    wait_process_gone(original_pid.trim()).await;
    // Windows still exercises revoke/no-relaunch; this is not a process-reaping assertion there.
    std::fs::remove_file(&pid_file).unwrap();
    manager
        .warm(
            "s",
            vec![(key.clone(), cfg.clone())],
            reqwest::Client::new(),
        )
        .await;
    wait_stopped(&manager, "s").await;
    assert!(!pid_file.exists());
    assert!(!manager.launch_status(&key, &cfg).trusted);
    #[cfg(not(unix))]
    let _ = original_pid;
    manager.disconnect_all().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_interrupts_formal_and_probe_initialization_without_late_ready() {
    for probe in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut cfg = marked_config(&pid_file);
        cfg.env
            .insert("CODEWAVE_TEST_INIT_DELAY_MS".into(), "30000".into());
        let key = PoolKey::global("trust-initializing").with_source(&dir.path().join("mcp.json"));
        let version = super::super::launch_version(&cfg);
        let manager = Arc::new(McpManager::default());
        let task = if probe {
            let challenge = manager.prepare_probe(&key, &cfg).unwrap();
            let manager = manager.clone();
            let key = key.clone();
            let cfg = cfg.clone();
            tokio::spawn(async move {
                manager
                    .test_connection(&key, cfg, &challenge.id, reqwest::Client::new())
                    .await
            })
        } else {
            manager
                .set_launch_trust(&key, &cfg, &version, true)
                .await
                .unwrap();
            manager
                .register_session("s", &[(key.clone(), cfg.clone())])
                .await;
            let manager = manager.clone();
            let key = key.clone();
            let cfg = cfg.clone();
            tokio::spawn(
                async move { manager.ensure(&key, cfg, reqwest::Client::new(), "s").await },
            )
        };
        let pid = wait_pid_file(&pid_file).await;
        manager
            .set_launch_trust(&key, &cfg, &version, false)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .expect("revocation did not interrupt initialization")
                .unwrap()
                .is_err()
        );
        wait_process_gone(&pid).await;
        assert!(manager.tool_snapshot_for("s").await.is_empty());
        assert!(
            manager
                .status_for("s")
                .await
                .iter()
                .all(|status| status.state == McpState::Stopped && status.pid.is_none())
        );
        assert!(!manager.launch_status(&key, &cfg).trusted);
        manager.disconnect_all().await;
    }
}
