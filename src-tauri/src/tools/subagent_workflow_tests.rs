use super::*;
use crate::core::delivery::{DeliveryService, types::*};
use crate::core::workflow::{routing::WorkUnit, service};
use std::{collections::BTreeSet, sync::Arc};
struct Server {
    _listener: Arc<tokio::net::TcpListener>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[tokio::test]
async fn builtin_work_package_keeps_tools_and_performs_a_real_read() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("workspace");
    let data = dir.path().join("profile");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(ws.join("README.md"), "production-read-sentinel").unwrap();
    let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let requests = captured.clone();
    let listen = listener.clone();
    let read_path = ws.join("README.md").to_string_lossy().into_owned();
    let task = tokio::spawn(async move {
        for index in 0..6 {
            let (mut socket, _) = listen.accept().await.unwrap();
            let mut bytes = vec![];
            let mut buf = [0u8; 4096];
            let body_start = loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..body_start]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            while bytes.len() < body_start + length {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&buf[..n]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
            requests.lock().unwrap().push(request);
            let chunk = if index == 0 {
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"read1","type":"function","function":{"name":"read","arguments":json!({"files":[{"path":read_path}]}).to_string()}}]},"finish_reason":"tool_calls"}]})
            } else {
                json!({"choices":[{"delta":{"content":"<report>read complete</report>"},"finish_reason":"stop"}]})
            };
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    let _server = Server {
        _listener: listener,
        task,
    };
    let core = crate::core::agent::test_support::make_core(
        &crate::tools::pathutil::WriteRoots::new(ws.clone(), data.clone()),
    );
    *core.client.write().unwrap() = reqwest::Client::builder().no_proxy().build().unwrap();
    {
        let mut cfg = core.cfg.write().unwrap();
        cfg.providers[0].base_url = format!("http://127.0.0.1:{port}/v1");
        cfg.providers[0].keys = vec!["isolated-test".into()];
    }
    let session = uuid::Uuid::new_v4().to_string();
    let mut rt = crate::core::agent::test_support::make_runtime(ws.clone(), data.clone(), vec![]);
    {
        let inner = Arc::get_mut(&mut rt).unwrap();
        inner.id = session.clone();
        inner.root_run_id = Some("production-test-root".into());
    }
    let mut prefs = rt.prefs();
    prefs.approval_mode = crate::core::prefs::ApprovalMode::FullAccess;
    rt.set_prefs(prefs);
    rt.prepare_actor_run("production-test-root");
    let owner = DeliveryService::identity_of(&rt);
    let mut contract:TaskContract=serde_json::from_value(json!({"schema":1,"task_id":"task1","revision":1,"owner":owner,"requirements":[{"id":"r1","title":"read","required":true}],"artifacts":[{"id":"a1","path":{"root_id":"workspace","relative_path":"README.md"},"kind":"text","required":true}]})).unwrap();
    let roots = DeliveryService::roots_from_runtime(&rt);
    let prepared = service::prepare(
        &mut contract,
        &roots,
        &data,
        "temporary",
        None,
        &BTreeSet::new(),
    )
    .unwrap();
    service::persist(&data, &prepared).unwrap();
    core.delivery
        .store()
        .save_contract(&session, &owner, "task1", None, &contract, "publish")
        .unwrap();
    let unit = WorkUnit {
        design_input: crate::core::workflow::design::DesignInput {
            documentation_only: true,
            ..Default::default()
        },
        id: "docs".into(),
        requirement_ids: vec!["r1".into()],
        read_scopes: vec![],
        write_scopes: vec![PathRef {
            root_id: "workspace".into(),
            relative_path: "README.md".into(),
        }],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: None,
    };
    service::plan_units(&data, &owner, &contract, vec![unit], &roots).unwrap();
    *rt.delivery_task_id.lock().unwrap() = Some("task1".into());
    let ctx = ToolCtx {
        core,
        rt,
        batch_id: "b".into(),
        call_index: 0,
        call_key: "b:0".into(),
        provider_call_id: "subcall".into(),
        cancel: Default::default(),
    };
    let out=tokio::time::timeout(std::time::Duration::from_secs(15),SubagentTool.run(&ctx,json!({"task":"Read README.md and report","role":"backend-dev","maxSteps":3,"contractTaskId":"task1","unitId":"docs","writeScopes":["workspace:README.md"]}))).await.expect("child timeout");
    assert!(out.ok, "{out:?}");
    let requests = captured.lock().unwrap();
    assert!(
        requests.len() >= 2,
        "read must produce a second actual provider request"
    );
    let names: Vec<_> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"read"));
    assert!(names.contains(&"edit"));
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("production-read-sentinel")
    );
}
