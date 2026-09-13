use crate::core::types::Content;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工具风险分级。分级决定审批行为：ReadOnly 静默放行、FileWrite 走 diff 预览审批、Network 需确认、Interactive 独占批次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// 只读工具：不产生任何副作用（read / grep / list_files 等），无需用户审批
    ReadOnly,
    /// 文件写入类：create / edit / delete 等，ConfirmEach 档下附 diff 预览逐个审批
    FileWrite,
    /// 网络类：web_fetch / http_request 等，触网前需用户确认
    Network,
    /// 交互类工具：必须独占一个批次（ask，以及 P1 的 wait / suggest），不能与其他调用并列
    Interactive,
    /// 元工具：操作 agent 自身状态而非工作区（plan / compact 等）
    Meta,
}

/// 工具执行失败的结构化错误：`code` 是机器可读的稳定错误码（如 `E_FENCE_BLOCKED`），模型据此决策重试或放弃。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    /// 稳定错误码，全大写 `E_*` 形态，进入模型可见的错误文本
    pub code: String,
    /// 面向模型与用户的可读描述
    pub message: String,
}

impl ToolError {
    /// 由错误码与消息构造。
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ToolError {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// 双通道结果：前端收到完整 JSON（本结构），模型收到瘦身后的文本（tools/compact.rs 按工具裁剪）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    /// 是否成功；失败时 `error` 必有值
    pub ok: bool,
    /// 成功时的结构化结果，前端据此渲染工具卡
    #[serde(default)]
    pub data: Value,
    /// 失败时的结构化错误
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
    /// 非致命告警（如宽松解码忽略的未知字段），随结果透出
    #[serde(default)]
    pub warnings: Vec<String>,
    /// 仅注入给模型的多模态附加内容（如 read 注入的图片）；不进前端 outcome JSON。
    #[serde(skip)]
    pub extra_model_content: Vec<Content>,
}

impl ToolOutcome {
    /// 构造成功结果。
    pub fn ok(data: Value) -> Self {
        ToolOutcome {
            ok: true,
            data,
            error: None,
            warnings: Vec::new(),
            extra_model_content: Vec::new(),
        }
    }
    /// 构造失败结果。
    pub fn err(code: &str, message: impl Into<String>) -> Self {
        ToolOutcome {
            ok: false,
            data: Value::Null,
            error: Some(ToolError::new(code, message)),
            warnings: Vec::new(),
            extra_model_content: Vec::new(),
        }
    }
    /// 附加告警（链式消费，追加到已有 warnings 之后）。
    pub fn with_warnings(mut self, w: Vec<String>) -> Self {
        self.warnings.extend(w);
        self
    }
}

/// 工具执行上下文：由编排层注入；工具实现禁止触碰 tauri（分层约束，见 docs/02）。
pub struct ToolCtx {
    /// 全局 agent 核心：配置、审批门、provider 等共享资源
    pub core: std::sync::Arc<crate::core::agent::AgentCore>,
    /// 当前会话运行时：工作区、数据目录、偏好等会话级状态
    pub rt: std::sync::Arc<crate::core::agent::SessionRuntime>,
    /// 本次批次 id：同一次模型回复产出的所有工具调用共享
    pub batch_id: String,
    /// 本调用在批次内的序号
    pub call_index: usize,
    /// 本调用的稳定 key（用于审批流定位与前端工具卡关联）
    pub call_key: String,
    /// 取消令牌：run 被停止时置位，长任务工具应轮询以尽早退出
    pub cancel: tokio_util::sync::CancellationToken,
}

impl ToolCtx {
    /// 工作区根目录：项目会话为项目主目录快照，临时会话为全局数据目录。
    pub fn workspace(&self) -> &std::path::Path {
        &self.rt.workspace
    }

    /// 汇总当前会话的写入许可根：工作区 + 数据目录 + 用户已放行的外部目录（始终允许列表）。
    pub fn write_roots(&self) -> crate::tools::pathutil::WriteRoots {
        let mut r = crate::tools::pathutil::WriteRoots::new(
            self.rt.workspace.clone(),
            self.rt.data_dir.clone(),
        );
        r.extra = self
            .rt
            .extra_roots
            .lock()
            .unwrap()
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        r
    }

    /// 审批总开关是否开启（配置级，与每档权限模式正交）。
    pub fn approval_enabled(&self) -> bool {
        self.core.cfg.read().unwrap().approval.enabled
    }

    /// 创建区之外的写入是否需要确认（审批配置项）。
    pub fn confirm_outside_create(&self) -> bool {
        self.core
            .cfg
            .read()
            .unwrap()
            .approval
            .confirm_outside_create
    }

    /// 当前会话的审批档位（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）。
    pub fn approval_mode(&self) -> crate::core::prefs::ApprovalMode {
        self.rt.prefs.lock().unwrap().approval_mode
    }

    /// 当前会话生效的 fence 策略。approval_enabled 恒为 true：
    /// false 会把高危 Confirm 降级成 Block（更严而非更松，违背档位语义）；FullAccess 的跳过在消费点处理。
    pub fn fence_policy(&self) -> crate::safety::fence::FencePolicy {
        let cfg = self.core.cfg.read().unwrap();
        let mode = self.approval_mode();
        crate::safety::fence::FencePolicy {
            approval_enabled: true,
            confirm_outside_create: cfg.approval.confirm_outside_create,
            confirm_inside_writes: mode.confirm_inside_writes(),
            plan_readonly: mode.plan_readonly(),
        }
    }
}

/// 工具 trait：所有内置工具的统一接口。实现方只做纯函数式执行，审批 / fence / 计账由批次层与安全层负责。
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// 工具名（wire 协议名，全小写下划线，如 `batch_read`）。
    fn name(&self) -> &'static str;
    /// 面向模型的英文描述（进 provider 的 tools 列表；文案调优由专门批次处理）。
    fn description(&self) -> &'static str;
    /// 严格 JSON Schema（additionalProperties:false），进 provider 的 tools 列表
    fn schema(&self) -> &'static str;
    /// 风险分级，决定审批路径。
    fn kind(&self) -> ToolKind;
    /// 执行工具：入参为已宽松解码的 JSON，出参走双通道（前端全量 / 模型瘦身）。
    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome;
    /// ConfirmEach 档下 FileWrite 预审批弹窗的详情预览（如 edit / create 的变更 diff）；
    /// 返回 None 时批次层退化为通用 JSON 展示。预览仅用于展示，真实执行仍以工具自身的结果为准。
    async fn approval_detail(&self, _ctx: &ToolCtx, _args: &Value) -> Option<String> {
        None
    }
}

/// 收集入参中 schema 未定义的顶层未知字段（宽松解码的 warnings 部分，[docs/p0-plan](../../../docs/p0-plan.md) §6.3.5）。
pub fn collect_unknown_fields(args: &Value, schema: &str) -> Vec<String> {
    let (Some(obj), Ok(schema_v)) = (args.as_object(), serde_json::from_str::<Value>(schema))
    else {
        return Vec::new();
    };
    let props = schema_v["properties"].as_object();
    let Some(props) = props else {
        return Vec::new();
    };
    obj.keys()
        .filter(|k| !props.contains_key(*k))
        .map(|k| format!("未知字段 `{k}` 已忽略"))
        .collect()
}

#[cfg(test)]
mod tests;
