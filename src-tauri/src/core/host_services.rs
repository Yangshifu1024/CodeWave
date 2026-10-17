//! 桌面核心装配。凭据、信任后端、协调器与事件汇均由宿主注入。
//!
//! 会话运行、询问与通知继续使用桌面原有 IPC 和 TauriSink 路径；本模块只负责
//! 初始化核心、恢复会话状态并安装 MCP 与指令资源的真实信任服务。

use crate::core::agent::{AgentCore, EventSink};
use crate::core::coordinator::{Coordinator, InProcessCoordinator};
use crate::core::process_coordination::{LeaseStore, ProcessGuardian};
use crate::core::sessions::SessionStore;
use crate::core::trust::{TrustBackend, TrustStore};
use crate::mcp::McpManager;
use std::path::PathBuf;
use std::sync::Arc;

/// 崩溃恢复策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPolicy {
    /// 正常恢复：把上次遗留的 running 标中断并建本次标记（桌面默认）。
    Standard,
    /// 隔离测试可跳过崩溃标记恢复，不影响桌面默认策略。
    ReadOnly,
}

/// 装配参数。`data_dir` 是**唯一的路径来源**。
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    /// 数据根。
    pub data_dir: PathBuf,
    /// 是否启动调度器（`None` = 桌面默认启用）。
    pub start_scheduler: Option<bool>,
    /// 恢复策略。
    pub recovery_policy: RecoveryPolicy,
}

impl BootstrapOptions {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        BootstrapOptions {
            data_dir: data_dir.into(),
            start_scheduler: None,
            recovery_policy: RecoveryPolicy::Standard,
        }
    }

    /// 实际是否启动调度器。
    pub fn scheduler_enabled(&self) -> bool {
        self.start_scheduler.unwrap_or(true)
    }
}

/// 宿主能力实际支持状态。
///
/// 浏览器之类的能力要看真实实现。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostCapabilities {
    pub browser: bool,
    pub services: bool,
    pub scheduled_tasks: bool,
    pub notifications: bool,
}

/// 宿主服务集合。**全部是 trait 对象，不含 Tauri 类型**，
/// 且**不允许模型 / 普通工具构造替换**。
pub struct HostServices {
    /// 事件汇。
    pub sink: Arc<dyn EventSink>,
    pub credentials: Arc<dyn crate::core::config_commit::CredentialStore>,
    /// 信任后端（**必填**，见模块文档的生产注入纪律）。
    pub trust_backend: Arc<dyn TrustBackend>,
    /// 协调器（跨进程共享资源）。
    pub coordinator: Arc<dyn Coordinator>,
    /// 能力实况。
    pub capabilities: HostCapabilities,
}

impl HostServices {
    /// 桌面等无浏览器宿主的能力实况。
    pub fn desktop_like(sink: Arc<dyn EventSink>, trust_backend: Arc<dyn TrustBackend>) -> Self {
        HostServices {
            sink,
            credentials: Arc::new(crate::host::keyring::SystemCredentialStore),
            trust_backend,
            coordinator: InProcessCoordinator::new(),
            capabilities: HostCapabilities {
                browser: false,
                services: true,
                scheduled_tasks: true,
                notifications: true,
            },
        }
    }
}

/// 装配失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapError {
    /// 没给信任后端。**不允许**退回默认空权威实例。
    MissingTrustBackend,
    /// 装配过程中的 IO 失败。
    Io(String),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootstrapError::MissingTrustBackend => write!(
                f,
                "宿主未提供信任后端：拒绝装配（默认空权威实例不得当作生产信任服务）"
            ),
            BootstrapError::Io(m) => write!(f, "装配失败：{m}"),
        }
    }
}

/// 核心句柄。
pub struct CoreHandle {
    /// 底层核心。
    pub core: Arc<AgentCore>,
    /// 数据根（**只读**，供消费者定位边车）。
    pub data_dir: PathBuf,
    /// 是否启动调度器。
    pub scheduler_enabled: bool,
    /// 本进程持有的根租约（#36-C0 guardian）。
    ///
    /// 存在的意义是"这个进程在管这些子树"成为**可证明的事实**：`shutdown()`
    /// 统一回收，退出前把自身租约留在盘上会让下一个进程一直看到 busy。
    pub guardian: Arc<ProcessGuardian>,
}

impl CoreHandle {
    /// 请求关闭（消费者随后自行退出）。
    ///
    /// 这里只回收本进程持有的租约，不终止桌面宿主进程。
    pub fn shutdown(&self) -> ShutdownSignal {
        // 回收自身根租约。失败只告警不 panic：残留租约按"崩溃保守 busy"处理，
        // 由另一个 guardian 显式回收，比在这里 panic 把整个关闭流程打断要好。
        for e in self.guardian.release_all() {
            tracing::warn!("guardian 释放根租约失败：{e}");
        }
        ShutdownSignal::Requested
    }
}

/// 装配。
///
/// 与 `lib.rs` 的桌面装配**同源**：信任后端经 `TrustStore` → `McpManager::with_authority`
/// 注入，绝不沿用 `AgentCore::new` 里的默认空权威实例。
pub fn bootstrap(
    opts: &BootstrapOptions,
    services: HostServices,
) -> Result<CoreHandle, BootstrapError> {
    let data_dir = crate::tools::pathutil::canonical_best_effort(&opts.data_dir);
    std::fs::create_dir_all(&data_dir).map_err(|e| BootstrapError::Io(e.to_string()))?;

    // 只有 Standard 恢复上次崩溃状态并写本次运行标记。
    if opts.recovery_policy == RecoveryPolicy::Standard {
        crate::core::sessions::interrupt::recover_after_crash(
            &SessionStore::new(data_dir.clone()),
            &data_dir,
        );
        if let Err(e) = crate::core::sessions::interrupt::create_marker(&data_dir) {
            tracing::warn!("运行标记写入失败（本次崩溃检测退化）：{e}");
        }
    }

    // 配置与会话始终从同一个宿主数据根读取；隔离测试不得回读真实全局配置。
    let cfg = crate::core::config::ConfigState::load_from(&data_dir);
    let client = crate::provider::proxy::build_client(&cfg);
    let store = Arc::new(
        SessionStore::new(data_dir.clone()).with_coordinator(services.coordinator.clone()),
    );
    store.purge_non_session_entries();

    let watched: Arc<dyn EventSink> =
        Arc::new(crate::core::sessions::interrupt::InterruptWatchSink::new(
            services.sink.clone(),
            store.clone(),
        ));
    let mut agent = AgentCore::new_with_credentials(
        cfg,
        watched,
        store,
        client,
        data_dir.clone(),
        services.credentials.clone(),
    );
    agent.instructions =
        crate::core::instruction_resources::InstructionTrust::with_store(Arc::new(
            TrustStore::new(data_dir.clone(), services.trust_backend.clone()),
        ));
    // **生产信任注入**：用宿主给的后端构造权威，而不是默认空实例。
    let manager = McpManager::with_authority(
        crate::mcp::DEFAULT_MAX_SERVERS,
        Arc::new(TrustStore::new(
            data_dir.clone(),
            services.trust_backend.clone(),
        )),
    );
    #[cfg(not(test))]
    let manager = manager.with_process_guardian(
        crate::core::process_coordination::guardian::GuardianClient::launch_rootless(&data_dir)
            .map_err(|error| BootstrapError::Io(error.to_string()))?,
    );
    agent.mcp = Arc::new(manager);
    {
        let mut excluded = agent.host_excluded_tools.lock().unwrap();
        if !services.capabilities.services {
            excluded.push("service".into());
        }
        if !services.capabilities.scheduled_tasks {
            excluded.push("scheduled_task".into());
        }
    }
    let core = Arc::new(agent);
    let guardian = ProcessGuardian::new(&LeaseStore::new(&data_dir.join("coord")));
    Ok(CoreHandle {
        core,
        data_dir,
        scheduler_enabled: opts.scheduler_enabled(),
        guardian,
    })
}

/// 关闭信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    Requested,
}

#[cfg(test)]
#[path = "host_services_tests.rs"]
mod tests;
