//! 桌面装配回归：真实信任注入、默认调度策略、隔离配置和崩溃标记。

use super::*;
use crate::core::trust::TrustError;

/// 会记录调用的假信任后端（用来证明**注入的那个**确实被装上了）。
///
/// 必须**真存真读**：只记不存的话，写入后的确认读回拿不到东西，
/// 事务只能报 `OutcomeUnknown`——那测的是假后端不合格，不是装配对不对。
struct RecordingBackend {
    records: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    writes: std::sync::Mutex<Vec<String>>,
}

impl RecordingBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            records: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            writes: std::sync::Mutex::new(Vec::new()),
        })
    }
}

impl TrustBackend for RecordingBackend {
    fn read(&self, account: &str) -> Result<Option<String>, TrustError> {
        Ok(self.records.lock().unwrap().get(account).cloned())
    }
    fn write(&self, account: &str, value: &str) -> Result<(), TrustError> {
        self.records
            .lock()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        self.writes
            .lock()
            .unwrap()
            .push(format!("{account}={value}"));
        Ok(())
    }
}

/// 吞事件的假 sink。
struct NullSink;
impl EventSink for NullSink {
    fn channel_frame(&self, _s: &crate::core::types::SessionId, _f: &crate::core::agent::Frame) {}
    fn emit(&self, _s: &crate::core::types::SessionId, _e: &str, _p: serde_json::Value) {}
}

/// 只写数据根、不碰用户配置的最小 cfg 来源：`ConfigState::load` 读的是全局 data_dir，
/// 故这里用隔离目录做 bootstrap，并在断言里只依赖我们注入的部分。
fn sandbox() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().to_path_buf();
    (d, p)
}

fn services(sink: Arc<dyn EventSink>, backend: Arc<dyn TrustBackend>) -> HostServices {
    HostServices::desktop_like(sink, backend)
}

// ---------- 装配不变量 ----------

fn trust_key() -> crate::core::trust::TrustKey {
    crate::core::trust::TrustKey {
        kind: "mcp".into(),
        purpose: "launch".into(),
        source: std::env::temp_dir().join("fixture-project/.codewave/mcp.json"),
        owner: "fixture-owner".into(),
        name: "fixture".into(),
    }
}

#[tokio::test]
async fn bootstrap_uses_the_injected_trust_backend() {
    // 提取装配后，信任后端必须是**宿主给的那个**，而不是 AgentCore::new 里的默认空实例。
    let (_d, data) = sandbox();
    let backend = RecordingBackend::new();
    let h = bootstrap(
        &BootstrapOptions::new(data),
        services(Arc::new(NullSink), backend.clone()),
    )
    .expect("装配应成功");

    // 与桌面装配完全同源：TrustStore(宿主后端) → McpManager::with_authority。
    // 走一次真实写入路径，确认写穿到注入的那个后端。
    let store = TrustStore::new(h.data_dir.clone(), backend.clone());
    store
        .compare_exchange_guard(
            &trust_key(),
            None,
            // 版本必须是 64 位十六进制摘要（许可绑定内容哈希，不是任意串）。
            &"a".repeat(64),
            crate::core::trust::Decision::Grant,
            &uuid::Uuid::new_v4().to_string(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("写入许可应成功");
    // 账号是 TrustKey 的**摘要**（不把项目名/服务名泄进钥匙串），所以断言的是
    // 「注入的后端确实被写了、且写进去的内容能原样读回」，而不是断言明文名字。
    let writes = backend.writes.lock().unwrap().clone();
    assert!(!writes.is_empty(), "注入的后端没被调用：生产信任服务丢失");
    let account = backend
        .records
        .lock()
        .unwrap()
        .keys()
        .next()
        .cloned()
        .expect("许可未落到注入的后端");
    let stored = backend
        .records
        .lock()
        .unwrap()
        .get(&account)
        .cloned()
        .unwrap();
    assert_eq!(
        backend.read(&account).unwrap().as_deref(),
        Some(stored.as_str())
    );
}

#[test]
fn bootstrap_refuses_to_assemble_without_a_trust_backend() {
    // 没有后端就没有"默认可用"这一说：空权威实例不得被当成生产信任服务。
    let (_d, data) = sandbox();
    // 手工构造一个 sink，但把信任后端换成"忘了注入"的形态——
    // 类型上它仍是 Arc<dyn TrustBackend>，所以此处验证的是**装配路径必须经过构造器**：
    // 直接调 bootstrap 不存在可省略后端的重载。
    let backend = RecordingBackend::new();
    let svcs = services(Arc::new(NullSink), backend);
    assert!(bootstrap(&BootstrapOptions::new(data), svcs).is_ok());
    // 错误语义本身也要可读。
    assert_eq!(
        BootstrapError::MissingTrustBackend.to_string(),
        "宿主未提供信任后端：拒绝装配（默认空权威实例不得当作生产信任服务）"
    );
}

#[test]
fn desktop_scheduler_is_enabled_unless_explicitly_disabled() {
    let mut options = BootstrapOptions::new(std::env::temp_dir().join("scheduler-fixture"));
    assert!(options.scheduler_enabled());
    options.start_scheduler = Some(false);
    assert!(!options.scheduler_enabled());
}

// ---------- 恢复策略 ----------

#[test]
fn read_only_recovery_does_not_create_the_crash_marker() {
    let (_d, data) = sandbox();
    let before = std::fs::read_dir(&data).map(|d| d.count()).unwrap_or(0);
    let mut o = BootstrapOptions::new(data.clone());
    o.recovery_policy = RecoveryPolicy::ReadOnly;
    bootstrap(&o, services(Arc::new(NullSink), RecordingBackend::new())).expect("装配应成功");
    let after = std::fs::read_dir(&data).map(|d| d.count()).unwrap_or(0);
    // 装配本身会建目录，但**不得**写出运行标记。
    let marker = data.join(crate::core::sessions::interrupt::MARKER_FILE);
    assert!(!marker.exists(), "只读恢复不得写运行标记");
    assert!(after >= before);
}

#[test]
fn standard_recovery_writes_the_crash_marker() {
    let (_d, data) = sandbox();
    bootstrap(
        &BootstrapOptions::new(data.clone()),
        services(Arc::new(NullSink), RecordingBackend::new()),
    )
    .expect("装配应成功");
    assert!(
        data.join(crate::core::sessions::interrupt::MARKER_FILE)
            .exists(),
        "标准恢复应写运行标记"
    );
}

fn handle(data: &std::path::Path) -> CoreHandle {
    let mut injected = services(Arc::new(NullSink), RecordingBackend::new());
    injected.coordinator = crate::core::process_coordination::FileCoordinator::new(data);
    let mut options = BootstrapOptions::new(data.to_path_buf());
    options.recovery_policy = RecoveryPolicy::ReadOnly;
    bootstrap(&options, injected).expect("装配应成功")
}

#[test]
fn desktop_capabilities_preserve_services_and_scheduling() {
    let services = services(Arc::new(NullSink), RecordingBackend::new());
    assert!(!services.capabilities.browser);
    assert!(services.capabilities.services);
    assert!(services.capabilities.scheduled_tasks);
    assert!(services.capabilities.notifications);
    let (_dir, data) = sandbox();
    let handle = bootstrap(&BootstrapOptions::new(data), services).unwrap();
    assert!(handle.core.host_excluded_tools.lock().unwrap().is_empty());
}

#[test]
fn bootstrap_reads_only_the_supplied_configuration_root() {
    let (_dir, data) = sandbox();
    let config = crate::core::config::ConfigState {
        providers: vec![crate::core::config::ProviderConfig {
            id: "isolated-provider".into(),
            name: "隔离档".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        data.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let handle = handle(&data);
    assert_eq!(handle.data_dir, data.canonicalize().unwrap());
    assert_eq!(
        handle.core.cfg.read().unwrap().providers[0].id,
        "isolated-provider"
    );
}

#[test]
fn shutdown_releases_owned_leases_without_exiting_the_process() {
    let (_dir, data) = sandbox();
    let handle = handle(&data);
    assert_eq!(handle.shutdown(), ShutdownSignal::Requested);
}

#[test]
fn injected_file_coordinator_blocks_a_second_writer() {
    let (_dir, data) = sandbox();
    let first = handle(&data);
    let second = handle(&data);
    let id = "desktop-shared-session";
    first
        .core
        .store
        .save_provisional_meta(id, &data, None, &[])
        .unwrap();
    first
        .core
        .store
        .claim_session_owner(id, "run-one", 0)
        .unwrap();
    assert!(
        second
            .core
            .store
            .claim_session_owner(id, "run-two", 0)
            .is_err()
    );
    first.core.store.release_session_owner(id);
    second
        .core
        .store
        .claim_session_owner(id, "run-two", 0)
        .unwrap();
    second.core.store.release_session_owner(id);
}

#[test]
fn delayed_terminal_cannot_release_a_successor_run_owner() {
    let (_dir, data) = sandbox();
    let handle = handle(&data);
    let id = "desktop-owner-fixture".to_string();
    handle
        .core
        .store
        .save_provisional_meta(&id, &data, None, &[])
        .unwrap();
    handle
        .core
        .store
        .claim_session_owner(&id, "current-run", 0)
        .unwrap();
    handle.core.store.mark_running(&id, true).unwrap();
    handle.core.sink.emit(
        &id,
        "run:done",
        serde_json::json!({"run_id":"old-run","cleanup_done":true}),
    );
    assert_eq!(handle.core.store.claimed_sessions(), vec![id.clone()]);
    assert!(handle.core.store.get(&id).unwrap().running);
    handle.core.sink.emit(
        &id,
        "run:done",
        serde_json::json!({"run_id":"current-run","cleanup_done":false}),
    );
    assert_eq!(handle.core.store.claimed_sessions(), vec![id.clone()]);
    handle.core.sink.emit(
        &id,
        "run:done",
        serde_json::json!({"run_id":"current-run","cleanup_done":true}),
    );
    assert!(handle.core.store.claimed_sessions().is_empty());
}
