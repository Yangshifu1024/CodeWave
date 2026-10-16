//! Opt-in Unix native authority + real OS process + real MCP server acceptance.
//! No model/provider configuration or existing keyring account is read.
use super::tests::{node_cfg, wait_ready};
use super::*;
use crate::core::agent::{AgentCore, test_support::make_core};
use crate::core::trust::{TrustBackend, TrustStore};
use crate::host::trust::{SystemTrustBackend, tests::NativeFixture};
use crate::mcp::{
    confirm_launch_review, global_scope, launch_key, launch_version, load_scope,
    prepare_launch_review,
};
use crate::tools::pathutil::WriteRoots;
use serde_json::json;
use std::path::Path;
use std::process::Stdio;

const TEST: &str = "mcp::manager::native_tests::native_mcp_cross_process_revoke";
type Result<T> = std::result::Result<T, String>;

fn core_at(profile: &Path, backend: Arc<dyn TrustBackend>) -> Arc<AgentCore> {
    let mut core = make_core(&WriteRoots {
        workspace: profile.into(),
        data_dir: profile.into(),
        project_dir: None,
        extra: vec![],
    });
    Arc::get_mut(&mut core).unwrap().mcp = Arc::new(McpManager::with_authority(
        16,
        Arc::new(TrustStore::new(profile.into(), backend)),
    ));
    core
}

async fn approve(core: &AgentCore, action: ReviewAction) -> Result<()> {
    let source = global_scope(&core.data_dir);
    let cfg = load_scope(&source).servers.remove(0).1;
    let review = prepare_launch_review(
        core,
        &source,
        None,
        "fixture",
        &launch_version(&cfg),
        action,
    )
    .await
    .map_err(|error| format!("prepare: {error:?}"))?;
    let receipt = confirm_launch_review(core, &source, None, "fixture", &review.id)
        .await
        .map_err(|error| format!("confirm: {error:?}"))?;
    if !receipt.confirmed {
        return Err(format!("review was not confirmed: {:?}", receipt.error));
    }
    Ok(())
}

fn receipt(profile: &Path, phase: &str, nonce: &str) -> Result<()> {
    let data = serde_json::to_vec(&json!({"phase":phase,"nonce":nonce}))
        .map_err(|error| error.to_string())?;
    // Readers never observe a partially written receipt.
    let temporary = profile.join(format!("{phase}.tmp"));
    std::fs::write(&temporary, data).map_err(|error| error.to_string())?;
    std::fs::rename(temporary, profile.join(format!("{phase}.json")))
        .map_err(|error| error.to_string())
}

async fn wait_receipt(profile: &Path, phase: &str, nonce: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(bytes) = std::fs::read(profile.join(format!("{phase}.json"))) {
                let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if value["phase"] != phase || value["nonce"] != nonce {
                    return Err("native child receipt identity mismatch".into());
                }
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| format!("timeout waiting for {phase} receipt"))?
}

async fn process_output(program: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut command = tokio::process::Command::new(program);
    command.args(args).kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| format!("{program} process inspection timed out"))?
        .map_err(|e| e.to_string())
}

async fn node_gone(profile: &Path) -> Result<()> {
    for name in ["original-node.pid", "node.pid"] {
        node_marker_gone(&profile.join(name)).await?;
    }
    Ok(())
}

async fn node_marker_gone(marker: &Path) -> Result<()> {
    let pid = match std::fs::read_to_string(marker) {
        Ok(pid) => pid,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let pid = pid.trim().parse::<u32>().map_err(|e| e.to_string())?;
    if pid <= 1 {
        return Err("invalid fixture Node PID".into());
    }
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if !process_output("kill", &["-0", &pid.to_string()])
                .await?
                .status
                .success()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .map_err(|_| format!("fixture Node {pid} is still alive"))?
}

/// Exceptional fallback only: the marker alone is never authority to kill a PID.
/// Verify the live command's unique nonce, script and process-group leadership.
async fn cleanup_identified_node(profile: &Path, nonce: &str) -> Result<()> {
    if node_gone(profile).await.is_ok() {
        return Ok(());
    }
    for name in ["original-node.pid", "node.pid"] {
        let marker = profile.join(name);
        if node_marker_gone(&marker).await.is_err() {
            cleanup_identified_marker(&marker, nonce).await?;
        }
    }
    node_gone(profile).await
}

async fn cleanup_identified_marker(marker_path: &Path, nonce: &str) -> Result<()> {
    let pid = std::fs::read_to_string(marker_path).map_err(|e| e.to_string())?;
    let pid = pid.trim().parse::<u32>().map_err(|e| e.to_string())?;
    let output = process_output("ps", &["-p", &pid.to_string(), "-o", "pgid=,command="]).await?;
    let identity = String::from_utf8_lossy(&output.stdout);
    let marker = format!("--codewave-native-{nonce}");
    let script = node_cfg().args[0].clone();
    let group = identity.split_whitespace().next();
    if pid <= 1
        || !output.status.success()
        || group != Some(pid.to_string().as_str())
        || !identity.contains(&script)
        || !identity.split_whitespace().any(|part| part == marker)
    {
        return Err(
            "refusing to kill an unverified fixture Node identity; retained diagnostics".into(),
        );
    }
    tokio::time::timeout(
        Duration::from_secs(2),
        crate::mcp::process::kill_tree_by_pid(pid),
    )
    .await
    .map_err(|_| "fixture process-group cleanup timed out".to_owned())?;
    // No SIGKILL against a possibly reused PID: failure retains the profile.
    node_marker_gone(marker_path).await
}

async fn child_behavior(core: Arc<AgentCore>, profile: &Path, nonce: &str) {
    let source = global_scope(profile);
    let cfg = load_scope(&source).servers.remove(0).1;
    let key = launch_key(&core, &source, None, "fixture").unwrap();
    // A fresh OS process has no process-local grant to inherit.
    assert!(core.mcp.current_launch_status(&key, &cfg).await.remembered);
    core.mcp
        .warm(
            "native",
            vec![(key.clone(), cfg.clone())],
            reqwest::Client::new(),
        )
        .await;
    wait_ready(&core.mcp, "native", 1).await;
    let tools = core.mcp.tool_snapshot_for("native").await;
    assert_eq!(tools.len(), 2);
    let echo = tools
        .iter()
        .find(|tool| tool.function_name.ends_with("__echo"))
        .unwrap();
    let binding = core
        .mcp
        .request_binding("native", &echo.function_name)
        .await
        .unwrap();
    let result = core
        .mcp
        .call_bound(
            "native",
            &binding,
            json!({"text":"native-persistent"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.text, "echo: native-persistent");
    let pid = std::fs::read_to_string(profile.join("node.pid")).unwrap();
    std::fs::write(profile.join("original-node.pid"), &pid).unwrap();
    assert_eq!(
        core.mcp.status_for("native").await[0].pid,
        Some(pid.trim().parse().unwrap())
    );
    receipt(profile, "ready", nonce).unwrap();
    wait_receipt(profile, "revoked", nonce).await.unwrap();
    assert!(
        core.mcp
            .call_bound(
                "native",
                &binding,
                json!({"text":"must-not-run"}),
                &CancellationToken::new(),
            )
            .await
            .is_err()
    );
    // This is the production app watcher's method, not the app timer/UI itself.
    core.mcp.revalidate_all_authority().await;
    assert!(core.mcp.tool_snapshot_for("native").await.is_empty());
    assert_eq!(
        core.mcp.status_for("native").await[0].state,
        McpState::Stopped
    );
    node_gone(profile).await.unwrap();
    let calls = std::fs::read_to_string(profile.join("calls.log")).unwrap();
    assert_eq!(calls.lines().count(), 1);
    assert!(calls.ends_with(":native-persistent\n"));
    // Remove the old marker: a relaunch would leave fresh evidence.
    std::fs::remove_file(profile.join("node.pid")).unwrap();
    core.mcp
        .warm(
            "native",
            vec![(key.clone(), cfg.clone())],
            reqwest::Client::new(),
        )
        .await;
    assert!(
        core.mcp
            .ensure(&key, cfg, reqwest::Client::new(), "native")
            .await
            .is_err()
    );
    core.mcp.disconnect_all().await;
    assert!(
        !profile.join("node.pid").exists(),
        "revoked warm launched a new server"
    );
    receipt(profile, "complete", nonce).unwrap();
}

async fn child_main(profile: &Path, nonce: &str) -> Result<()> {
    let core = core_at(profile, Arc::new(SystemTrustBackend));
    let owned_profile = profile.to_owned();
    let owned_nonce = nonce.to_owned();
    let task_core = core.clone();
    let mut task = tokio::spawn(async move {
        child_behavior(task_core, &owned_profile, &owned_nonce).await;
    });
    let result = tokio::select! {
        result = &mut task => result.map_err(|e| format!("child behavior: {e}")),
        _ = async {
            loop {
                if profile.join("abort").exists() { break; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        } => Err("parent requested cooperative abort".into()),
        _ = tokio::time::sleep(Duration::from_secs(45)) => Err("child behavior timed out".into()),
    };
    if !task.is_finished() {
        task.abort();
        let _ = task.await;
    }
    tokio::time::timeout(Duration::from_secs(8), core.mcp.disconnect_all())
        .await
        .map_err(|_| "child disconnect timed out".to_owned())?;
    node_gone(profile).await?;
    receipt(profile, "cleaned", nonce)?;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Real native keyring + OS child + Node; run exactly native_mcp_cross_process_revoke with --ignored; synthetic accounts are removed"]
async fn native_mcp_cross_process_revoke() {
    if let Some(profile) = std::env::var_os("CODEWAVE_NATIVE_MCP_PROFILE") {
        let nonce = std::env::var("CODEWAVE_NATIVE_MCP_NONCE").unwrap();
        child_main(Path::new(&profile), &nonce).await.unwrap();
        return;
    }
    let profile = tempfile::tempdir().unwrap();
    let nonce = uuid::Uuid::new_v4().to_string();
    let backend = Arc::new(NativeFixture::default());
    let core = core_at(profile.path(), backend.clone());
    let mut cfg = node_cfg();
    cfg.args.push(format!("--codewave-native-{nonce}"));
    cfg.env.insert(
        "CODEWAVE_TEST_PID_FILE".into(),
        profile.path().join("node.pid").display().to_string(),
    );
    cfg.env.insert(
        "CODEWAVE_TEST_CALL_LOG".into(),
        profile.path().join("calls.log").display().to_string(),
    );
    std::fs::write(
        global_scope(profile.path()).path,
        json!({"mcpServers":{"fixture":cfg}}).to_string(),
    )
    .unwrap();
    approve(&core, ReviewAction::Remember).await.unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--ignored", "--nocapture"])
        .env("CODEWAVE_NATIVE_MCP_PROFILE", profile.path())
        .env("CODEWAVE_NATIVE_MCP_NONCE", &nonce)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(profile.path().join("child.stdout")).unwrap())
        .stderr(std::fs::File::create(profile.path().join("child.stderr")).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let result: Result<()> = async {
        wait_receipt(profile.path(), "ready", &nonce).await?;
        approve(&core, ReviewAction::Revoke).await?;
        receipt(profile.path(), "revoked", &nonce)?;
        wait_receipt(profile.path(), "complete", &nonce).await?;
        wait_receipt(profile.path(), "cleaned", &nonce).await?;
        Ok(())
    }
    .await;
    // Always request cooperative shutdown before resorting to process killing.
    let _ = std::fs::write(profile.path().join("abort"), b"abort");
    let initial_wait = tokio::time::timeout(Duration::from_secs(12), child.wait()).await;
    let status = match initial_wait {
        Ok(Ok(status)) => status,
        other => {
            let kill = child.start_kill();
            match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => status,
                final_wait => {
                    // A live worker could still spawn Node/read authority. Do not
                    // delete its account or profile. This exceptional intentional
                    // Arc leak suppresses NativeFixture::drop's cleanup too.
                    std::mem::forget(backend);
                    let retained = profile.keep();
                    panic!(
                        "native MCP child not confirmed reaped: initial={other:?}, kill={kill:?}, final={final_wait:?}; profile AND synthetic account retained at {}",
                        retained.display()
                    );
                }
            }
        }
    };
    // Only after the worker is confirmed dead can a final Node sweep exclude
    // late launches. Neither a successful exit nor a disk PID proves cleanup.
    let cleanup = cleanup_identified_node(profile.path(), &nonce).await;
    let credentials = backend.cleanup();
    let success = result.is_ok() && status.success() && cleanup.is_ok() && credentials.is_ok();
    if !success {
        let retained = profile.keep();
        panic!(
            "native MCP acceptance failed: result={result:?}, child={status:?}, node={cleanup:?}, credentials={credentials:?}; diagnostics retained at {}",
            retained.display()
        );
    }
    eprintln!(
        "native MCP: real keyring remembered grant restored in OS child; echo verified; external revoke denied old binding; Node reaped; rewarm denied; synthetic account removal verified"
    );
}
