use crate::core::config::ConfigState;
use crate::core::sessions::SessionStore;
use crate::core::types::SessionId;
use std::path::PathBuf;
use std::sync::Arc;
    use super::*;

    /// 测试用空事件汇：吞掉所有帧与事件，供各测试模块构造 AgentCore 复用。
    pub struct NoopSink;
    impl EventSink for NoopSink {
        fn channel_frame(&self, _s: &SessionId, _f: &Frame) {}
        fn emit(&self, _s: &SessionId, _e: &str, _p: serde_json::Value) {}
    }

    /// 构造带默认 provider/model 的测试 AgentCore（data_dir 指向调用方给定的目录）。
    pub fn make_core(roots: &crate::tools::pathutil::WriteRoots) -> Arc<AgentCore> {
        let mut cfg = ConfigState::default();
        cfg.providers.push(crate::core::config::ProviderConfig {
            models: vec![crate::core::config::ProviderModel::default()],
            ..Default::default()
        });
        cfg.active_model_id = Some(cfg.providers[0].models[0].id.clone());
        let store = Arc::new(SessionStore::new(roots.data_dir.clone()));
        let core = AgentCore::new(
            cfg,
            Arc::new(NoopSink),
            store,
            reqwest::Client::new(),
            roots.data_dir.clone(),
        );
        Arc::new(core)
    }

    /// 显式指定 workspace/data_dir/extra_roots 的测试 runtime（host 层多根用例，评审 C1 回归）。
    pub fn make_runtime(
        workspace: PathBuf,
        data_dir: PathBuf,
        extra_roots: Vec<String>,
    ) -> Arc<SessionRuntime> {
        let mut rt = SessionRuntime::new("rt-test".to_string(), workspace, data_dir);
        if let Some(r) = Arc::get_mut(&mut rt) {
            *r.extra_roots.lock().unwrap() = extra_roots;
            // make_runtime 语义 = 主会话 runtime（checkpoint 相关测试保持主会话行为）
            r.is_main_session = true;
        }
        rt
    }
