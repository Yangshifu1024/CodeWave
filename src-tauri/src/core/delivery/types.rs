//! 交付核心模型 v1（任务 #54 拥有，[docs/tools-and-safety](../../../../docs/tools-and-safety.md)）。
//!
//! 这里是 S1 全部新类型的**唯一**定义处：`ExecutionReceipt`（#14 签发）、`ScopeVersion`
//! （#15 计算）、`DeliveryResult`（#17 提交）、`AcceptanceSnapshot`（#54 求值）都复用本文件，
//! 禁止任何任务另起第二份 receipt / 版本 / 完成状态定义。
//!
//! 三条贯穿全文件的底线（[共同契约 §2](../../../../docs/tools-and-safety.md)）：
//!
//! 1. **证据、完成、权限三者分离**：`completed` / `report` / 进程退出 0 / wait 命中都不等于验收
//!    通过；验收通过也不赋予任何读写、网络、MCP、Git 权限。
//! 2. **身份只能由宿主补齐**：普通工具 JSON 不能指定自己的签发者身份，`RunIdentity` 由
//!    宿主从真实调用上下文构造后传入，不从模型入参反解。
//! 3. **没采集就是没有**：`before`/`after` 为 `None` 表示未采集、采集失败为
//!    `Some(completeness=incomplete)`，二者都**不能**满足版本验证——禁止用空哈希冒充"没变"。

use serde::{Deserialize, Serialize};

/// 交付模型 schema 版本；任何字段语义变更都要升版并保留旧记录可读。
pub const SCHEMA: u32 = 1;

/// 单个契约的规模上限（[#54 任务文档](../../../../docs/tools-and-safety.md)）：超过即拒绝创建，
/// **不静默截断**——被截断的"通过"比没有通过更危险。
pub const MAX_REQUIREMENTS: usize = 100;
pub const MAX_CHECKS: usize = 100;
pub const MAX_ARTIFACTS: usize = 100;

/// 单份原始输出的落盘上限（共同契约 §4）：超限截断落盘并在回执显式标记，不静默丢弃。
pub const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
/// 单会话证据总量软线：触顶照常写、只提示。
pub const SESSION_SOFT_WARN_BYTES: u64 = 256 * 1024 * 1024;
/// 单会话证据总量硬线：**只停写、绝不删既有数据**（唯一副本不可回收）。
pub const SESSION_HARD_FUSE_BYTES: u64 = 1024 * 1024 * 1024;
/// 给模型的输出摘要上限（共同契约 §4）：超限带截断标记并附完整边车 ID。
pub const MAX_SUMMARY_BYTES: usize = 16 * 1024;

// ---------- 身份 ----------

/// 统一关联键（共同契约 §2）。`actor_id` 代表实际 runtime 身份，角色字符串只是用途，
/// 不能替代身份——"以 reviewer 角色自居"不构成独立审查。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdentity {
    /// 所属会话。
    pub session_id: String,
    /// 根 run 标识：根 run 开始时分配，子代理继承。
    pub root_run_id: String,
    /// 实际执行身份（`root` / 子代理 sub_id / 宿主校验器 id 等）。
    ///
    /// serde default：**老快照可能缺这两个执行身份字段**。缺省按根 run 身份读回，
    /// 而不是整份记录读不出来——读不回就等于「旧会话显示不出历史」。
    #[serde(default = "default_actor_id")]
    pub actor_id: String,
    /// 该身份本次执行的 run 标识。
    #[serde(default = "default_actor_run_id")]
    pub actor_run_id: String,
    /// 子代理身份；根 run 为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_id: Option<String>,
}

fn default_actor_id() -> String {
    "root".into()
}

fn default_actor_run_id() -> String {
    "unknown".into()
}

impl RunIdentity {
    /// 根 run 便捷构造。
    pub fn root(session_id: impl Into<String>, root_run_id: impl Into<String>) -> Self {
        let root_run_id = root_run_id.into();
        RunIdentity {
            session_id: session_id.into(),
            actor_run_id: root_run_id.clone(),
            root_run_id,
            actor_id: ROOT_ACTOR.into(),
            sub_id: None,
        }
    }

    /// 是否与另一身份共享同一 actor（[#10](../../../../docs/tools-and-safety.md) 独立审查判据之一）。
    pub fn same_actor(&self, other: &RunIdentity) -> bool {
        self.actor_id == other.actor_id
    }
}

/// 根身份的稳定 actor 名。与 `sub_id` 分离，使"同一次 run 里的两个不同子代理"彼此可区分，
/// 而"同一个子代理冒充审查自己"仍被判为同 actor。
pub const ROOT_ACTOR: &str = "root";

// ---------- 根与路径引用 ----------

/// 规范化根的类别（从 `SessionMeta.roots` 快照派生，不回查可变化注册表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// 会话工作区（项目主目录；临时会话为全局数据目录）。
    Workspace,
    /// 全局数据目录。
    DataDir,
    /// 项目托管目录 `<主目录>/.codewave/`。
    ProjectDir,
    /// 用户「始终允许」的项目外目录。
    Extra,
}

/// 一个可引用的逻辑根。`root_id` 是稳定句柄，绝对路径只在宿主受控解析——
/// 报告自带的绝对路径不被信任（[#12](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootIdentity {
    /// 稳定根 id（写入契约与回执，可在同一会话内解析回真实路径）。
    pub root_id: String,
    /// 根类别。
    pub kind: RootKind,
    /// 该根的绝对路径快照（宿主在创建契约时从会话快照填入，模型不能覆盖）。
    pub absolute: String,
}

/// 契约内引用的路径：`root_id + 相对路径`。**不使用裸绝对路径**，避免把机器布局写进契约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRef {
    /// 所属根。
    pub root_id: String,
    /// 根内相对路径（须能过 `pathutil::safe_join`）。
    pub relative_path: String,
}

// ---------- 执行命令 ----------

/// 命令规格：**请求**与**已解析事实**的显式 tagged union（共同契约 §4）。
///
/// 只有 [`Resolved`](Self::Resolved) 可真正执行；未准入请求只能落到 `Requested`，
/// 拒绝记录用 `Requested` 的有界脱敏内容，**不得**回填未经解释器/cwd 解析的假 program/argv。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum CommandSpec {
    /// 模型/上层请求的原始形态（尚未解析，未必会发生）。
    Requested {
        /// 原始命令文本（有界、脱敏后存储）。
        command_text: String,
        /// 模型自称的解释器提示（仅参考，不作事实）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell_hint: Option<String>,
    },
    /// 宿主解析后的真实执行形态。
    Resolved {
        /// 实际启动的可执行程序（shell 调用时为解释器本身）。
        program: String,
        /// 实际 argv（shell 调用时为解释器参数 + `-c` 脚本，不从脚本文本反推子命令）。
        argv: Vec<String>,
        /// 实际 shell 家族（`bash` / `powershell` / `cmd` / 无）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell_family: Option<String>,
        /// 规范化命令文本（展示与匹配用）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell_command: Option<String>,
        /// 生效超时（毫秒）。
        timeout_ms: u64,
        /// 执行所用配置指纹。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config_digest: Option<String>,
    },
}

impl CommandSpec {
    /// 是否为可执行的已解析形态。
    pub fn is_resolved(&self) -> bool {
        matches!(self, CommandSpec::Resolved { .. })
    }
}

// ---------- 证据 ----------

/// 执行终态（共同契约 §3）。`exit_code` **只在** [`Exited`](Self::Exited) 时有值——
/// spawn 失败、取消、超时都没有退出码，捏造 `exit_code: 0` 是最典型的假完成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// 进程真实启动并退出。
    Exited,
    /// 进程未能启动（程序不存在、权限拒绝等）。
    SpawnFailed,
    /// 超时终止。
    TimedOut,
    /// 被取消终止。
    Cancelled,
    /// 在准入阶段被拒（fence / 审批 / 保护目录），从未启动。
    Denied,
    /// 执行期间进程中断（重启、宿主退出）。
    Interrupted,
}

impl ExecutionOutcome {
    /// 是否真的启动过进程。
    pub fn executed(self) -> bool {
        matches!(self, ExecutionOutcome::Exited | ExecutionOutcome::TimedOut)
    }
    /// 是否产生可信退出码。
    pub fn has_exit_code(self) -> bool {
        matches!(self, ExecutionOutcome::Exited)
    }
    /// wire 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionOutcome::Exited => "exited",
            ExecutionOutcome::SpawnFailed => "spawn_failed",
            ExecutionOutcome::TimedOut => "timed_out",
            ExecutionOutcome::Cancelled => "cancelled",
            ExecutionOutcome::Denied => "denied",
            ExecutionOutcome::Interrupted => "interrupted",
        }
    }
}

/// 回执签发方。只有宿主可签发，模型同形 JSON 无效（共同契约 §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptIssuer {
    /// 宿主真实执行链（命令工具 / postcheck 外部命令）。
    Host,
    /// 注册在宿主的内置校验器。
    BuiltinValidator,
}

/// 宿主执行意向（[#14](../../../../docs/tools-and-safety.md)）：请求已受理但结果未定。
///
/// 保存的是**有界脱敏请求**，`requested_cwd` 绝不可当作实际 cwd 使用；重启后未决 intent
/// 一律标 `interrupted`——凭 intent 不能说"命令已执行"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIntent {
    /// schema 版本。
    pub schema: u32,
    /// 与最终回执共用的操作 id。
    pub operation_id: String,
    /// 发起身份（宿主补齐）。
    pub owner: RunIdentity,
    /// 关联的预登记检查 id（普通命令为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    /// 有界脱敏请求命令。
    pub requested_command: String,
    /// 请求 cwd（有界脱敏，仅作诊断）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_cwd: Option<String>,
    /// 请求超时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_timeout_ms: Option<u64>,
    /// 创建时刻（RFC3339）。
    pub created_at: String,
}

/// Exact contract version authorized by the host at evidence collection time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractBinding {
    pub task_id: String,
    pub revision: u64,
}
impl ContractBinding {
    pub fn of(contract: &TaskContract) -> Self {
        Self {
            task_id: contract.task_id.clone(),
            revision: contract.revision,
        }
    }
    pub fn matches(&self, contract: &TaskContract) -> bool {
        self.task_id == contract.task_id && self.revision == contract.revision
    }
}

/// 宿主签发的不可伪造执行回执（[#14](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ContractBinding>,
    /// schema 版本。
    pub schema: u32,
    /// 回执 id（宿主生成的 UUID，路径不拼用户输入）。
    pub id: String,
    /// 执行身份。
    pub owner: RunIdentity,
    /// 关联检查（普通命令为 `None`；不接受命令文本自称的检查身份）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    /// 请求形态（被拒时只有这个）。
    pub requested: CommandSpec,
    /// 已解析真实形态（只有成功解析才 `Some`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<CommandSpec>,
    /// 真实工作目录引用（只有成功解析才 `Some`；拒绝可为 `None`，不用默认目录补值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathRef>,
    /// 开始时刻。
    pub started_at: String,
    /// 结束时刻。
    pub finished_at: String,
    /// 执行终态。
    pub outcome: ExecutionOutcome,
    /// 真实退出码（仅 `Exited`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 终止信号（Unix 平台）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// 完整输出的边车引用（`outputs/<uuid>.log`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    /// 输出是否因预算被截断（截断时验收依赖完整输出者必须保持未证明）。
    #[serde(default)]
    pub output_truncated: bool,
    /// 执行前范围版本（`None` = 未采集）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<crate::core::delivery::version::ScopeVersion>,
    /// 执行后范围版本（`None` = 未采集）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<crate::core::delivery::version::ScopeVersion>,
    /// 签发方。
    pub issuer: ReceiptIssuer,
    /// 拒绝/失败的具体原因（`denied` 时必填，标明是 fence / 审批 / cwd 越界……）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 证据引用类型（共同契约 §3）。截图、DOM 观察、内置校验、人工确认走这里，
/// **不能**塞进 `ExecutionReceipt` 填空 command 冒充一次执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// 宿主真实命令执行。
    Execution,
    /// 注册在宿主���置校验器产出。
    Validation,
    /// 浏览器截图 / DOM 观察（[S4](#)）。
    BrowserCapture,
    /// 产物本体（文件哈希、大小、类型）。
    Artifact,
    /// 独立审查结论。
    Review,
    /// 真实用户交互应答。
    UserDecision,
}

/// 指向受保护类型化记录的一条证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// 记录类型。
    pub kind: EvidenceKind,
    /// 宿主生成的记录 id。
    pub record_id: String,
    /// 记录归属身份（宿主校验其与 owner 关系）。
    pub owner: RunIdentity,
    /// 记录绑定的范围版本（供失效判定；可空表示该证据不声明版本范围）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_version: Option<String>,
}

/// 内置校验记录（共同契约 §3）：**不含 command / exit_code**。
///
/// 它证明"注册在宿主的校验器读了这批输入并给出结论"，而不是"某条命令退出了 0"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinValidationRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ContractBinding>,
    /// 记录 id。
    pub id: String,
    /// 执行身份。
    pub owner: RunIdentity,
    /// 校验器 id。
    pub validator_id: String,
    /// 校验器版本（算法升级即换版，旧记录不能被新版判据追认）。
    pub validator_version: String,
    /// 关联检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    /// 输入引用（被校验对象的范围版本 / 记录 id）。
    pub input_refs: Vec<String>,
    /// 被校验内容的范围版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_version: Option<String>,
    /// 开始时刻。
    pub started_at: String,
    /// 结束时刻。
    pub finished_at: String,
    /// 结论。无法读取输入 = `Failed` 并带 `reason`，不静默通过。
    pub outcome: ValidationOutcome,
    /// 发现项（结构化）。
    pub findings: Vec<ValidationFinding>,
    /// 输出边车引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
}

/// 内置校验结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationOutcome {
    /// 通过。
    Passed,
    /// 失败。
    Failed,
    /// 被取消。
    Cancelled,
    /// 执行被中断。
    Interrupted,
}

/// 校验发现项严重度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    /// 提示。
    Info,
    /// 警告。
    Warning,
    /// 错误（阻断）。
    Error,
}

/// 一条校验发现。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationFinding {
    /// 严重度。
    pub severity: FindingSeverity,
    /// 可定位位置（文件 + 相对路径 + 行列，均可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// 描述。
    pub message: String,
}

/// 真实用户决定（共同契约 §4）：只能由宿主交互应答生成。
///
/// 「已人工验证」与「豁免」是**两种不同决定**：`confirmed` 是 manual 检查的证据，
/// `waived` 是范围决定，UI 必须按真实类型展示，不能混为一个"用户说完成了"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserDecision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ContractBinding>,
    /// 记录 id。
    pub id: String,
    /// 回答身份。
    pub owner: RunIdentity,
    /// 覆盖的验收项 / 需求 id。
    pub item_ids: Vec<String>,
    /// 决定内容。
    pub decision: UserDecisionKind,
    /// 决定时绑定的范围版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_version: Option<String>,
    /// 触发的交互 id（对应 ask/审批的真实交互轮次）。
    pub prompt_id: String,
    /// 应答时刻。
    pub answered_at: String,
}

/// 用户决定的两类语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserDecisionKind {
    /// Explicitly cancel the named requirement/check; never inferred from verification.
    Cancelled,
    /// 用户确认「我已人工验证」。
    Confirmed,
    /// 用户明确豁免某范围（豁免只能引用明确用户决定，模型声明/预算耗尽不能自动豁免）。
    Waived,
}

// ---------- 检查 ----------

/// 检查种类（共同契约 §3，首版五类；未知类型拒绝创建契约，不静默忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// 外部命令退出码。
    Command,
    /// 产物存在 / 内容约束。
    Artifact,
    /// 宿主注册的结构化校验器。
    Structured,
    /// 独立审查。
    Review,
    /// 人工确认。
    Manual,
}

/// 检查期望。与 `CheckKind` 一一对应，错配在契约创建时即拒绝。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckExpectation {
    /// 真实退出码落在允许集合内才算满足（任意退出 0 只满足**这一个** CheckSpec）。
    Command {
        /// 允许的退出码；缺省 `[0]`。
        #[serde(default = "default_allowed_exit")]
        allowed_exit_codes: Vec<i32>,
    },
    /// 指定产物 id 全部满足其 ArtifactRequirement。
    Artifact {
        /// 产物 id 列表。
        artifact_ids: Vec<String>,
    },
    /// 宿主注册校验器跑通。
    Structured {
        /// 校验器 id。
        validator_id: String,
        /// 校验器版本（可选；缺省用当前注册版本）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        validator_version: Option<String>,
    },
    /// 独立审查结论无阻断项。
    Review {
        /// 必须覆盖的维度（缺省七维）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<Vec<String>>,
    },
    /// 真实用户确认。
    Manual {
        /// 提示文本（展示给用户，不代替真实交互）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
}

fn default_allowed_exit() -> Vec<i32> {
    vec![0]
}

impl CheckExpectation {
    /// 与 `kind` 是否匹配。
    pub fn matches_kind(&self, kind: CheckKind) -> bool {
        matches!(
            (self, kind),
            (CheckExpectation::Command { .. }, CheckKind::Command)
                | (CheckExpectation::Artifact { .. }, CheckKind::Artifact)
                | (CheckExpectation::Structured { .. }, CheckKind::Structured)
                | (CheckExpectation::Review { .. }, CheckKind::Review)
                | (CheckExpectation::Manual { .. }, CheckKind::Manual)
        )
    }
    /// 期望种类 wire 串。
    pub fn kind_str(&self) -> &'static str {
        match self {
            CheckExpectation::Command { .. } => "command",
            CheckExpectation::Artifact { .. } => "artifact",
            CheckExpectation::Structured { .. } => "structured",
            CheckExpectation::Review { .. } => "review",
            CheckExpectation::Manual { .. } => "manual",
        }
    }
}

/// 验证范围（共同契约 §5）。**显式**列出来的东西才参与版本计算——
/// 不能自动把整个用户主目录纳入，也不能靠 watcher 猜测变更。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationScope {
    /// 参与本次验证的根。
    #[serde(default)]
    pub roots: Vec<RootIdentity>,
    /// 输入（只读检查期间不应变化；写入检查需单独观察前后版本）。
    #[serde(default)]
    pub inputs: Vec<PathRef>,
    /// 产物（写入检查的输出，绑定结束时版本）。
    #[serde(default)]
    pub outputs: Vec<PathRef>,
    /// 依赖文件（锁文件等）。
    #[serde(default)]
    pub dependency_files: Vec<PathRef>,
    /// 需要记录版本的环境工具链键（如 `rustc` / `node`）。不保存完整 env。
    #[serde(default)]
    pub environment_keys: Vec<String>,
}

/// 单条检查。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckSpec {
    /// Actual command directory; omitted means the session workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathRef>,
    /// Require an independently assigned verifier's host-signed execution evidence.
    #[serde(default)]
    pub independent_verifier: bool,
    /// 契约内唯一 id。
    pub id: String,
    /// 检查种类。
    pub kind: CheckKind,
    /// 命令形态（仅 `CheckKind::Command` 需要）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandSpec>,
    /// 验证范围。
    #[serde(default)]
    pub scope: VerificationScope,
    /// 是否必需（非必需检查不阻止整体通过，但仍记录事实）。
    #[serde(default = "default_true")]
    pub required: bool,
    /// 期望。
    pub expectation: CheckExpectation,
}

fn default_true() -> bool {
    true
}

// ---------- 需求与产物 ----------

/// 单条需求。引用闭包由宿主校验：只能指向本契约内的 check / artifact id。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    /// 契约内唯一 id。
    pub id: String,
    /// 标题（面向用户）。
    pub title: String,
    /// 是否必需。
    #[serde(default = "default_true")]
    pub required: bool,
    /// 满足本需求所需的检查 id（全部满足才 passed）。
    #[serde(default)]
    pub check_ids: Vec<String>,
    /// 满足本需求所需的产物 id。
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

/// 产物类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// 文本文件。
    Text,
    /// 二进制（以哈希 / 大小 / 文档检查引用呈现，不强行文本 diff）。
    Binary,
    /// 目录（只验证存在与枚举，不逐文件内容）。
    Directory,
    /// 文档（Office / PDF，交给文档检查）。
    Document,
    /// 其他。
    Other,
}

/// 产物内容约束（首版固定几种，**不执行任意表达式**）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactConstraint {
    /// 存在即可。
    Exists,
    /// 媒体类型约束。
    MediaType {
        /// 期望媒体类型。
        media_type: String,
    },
    /// 最小字节数。
    MinBytes {
        /// 下限（含）。
        bytes: u64,
    },
    /// 最大字节数。
    MaxBytes {
        /// 上限（含）。
        bytes: u64,
    },
    /// 引用一条结构化检查（由宿主校验器判定内容）。
    StructuredCheck {
        /// 校验器 id。
        validator_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        validator_version: Option<String>,
    },
}

/// 产物要求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRequirement {
    /// 契约内唯一 id。
    pub id: String,
    /// 目标路径引用。
    pub path: PathRef,
    /// 类别。
    pub kind: ArtifactKind,
    /// 内容约束。
    #[serde(default)]
    pub constraints: Vec<ArtifactConstraint>,
    /// 是否必需。
    #[serde(default = "default_true")]
    pub required: bool,
}

/// 审查策略（[#10](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewPolicy {
    /// 是否要求审查。
    #[serde(default)]
    pub required: bool,
    /// 是否必须由**独立身份**审查（同一执行者/run 冒充审查会被宿主拒绝）。
    #[serde(default = "default_true")]
    pub independent: bool,
    /// 覆盖维度；缺省七维。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Vec<String>>,
    /// 阻断级严重度（缺省 `error`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_severities: Option<Vec<FindingSeverity>>,
}

impl ReviewPolicy {
    /// Explicit policy dimensions cannot be weakened by an individual check.
    pub fn required_dimensions(&self, check: Option<&[String]>) -> Vec<String> {
        let mut dimensions = self.dimensions.clone().unwrap_or_else(|| {
            check
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| REVIEW_DIMENSIONS.iter().map(|d| (*d).into()).collect())
        });
        if let Some(check) = check {
            for dimension in check {
                if !dimensions.contains(dimension) {
                    dimensions.push(dimension.clone());
                }
            }
        }
        dimensions
    }

    pub fn is_blocking(&self, severity: FindingSeverity) -> bool {
        self.blocking_severities
            .as_deref()
            .unwrap_or(&[FindingSeverity::Error])
            .contains(&severity)
    }
}

impl Default for ReviewPolicy {
    fn default() -> Self {
        ReviewPolicy {
            required: false,
            independent: true,
            dimensions: None,
            blocking_severities: None,
        }
    }
}

/// 七维审查维度缺省值（与既有七维审查一致）。
pub const REVIEW_DIMENSIONS: [&str; 7] = [
    "correctness",
    "security",
    "performance",
    "maintainability",
    "readability",
    "test_coverage",
    "best_practices",
];

// ---------- 契约 ----------

/// 用户范围变更（[#54](../../../../docs/tools-and-safety.md)）：只有明确的用户决定
/// 才能把必需项取消或豁免——模型不能自行删项、不能自己声明豁免。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeChange {
    /// 变更 id。
    pub id: String,
    /// 目标（需求 id / 产物 id / 检查 id）。
    pub target_id: String,
    /// 变更类型。
    pub kind: ScopeChangeKind,
    /// 原因（展示）。
    pub reason: String,
    /// 依据的真实用户决定 id（`None` 即非法来源）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_decision_id: Option<String>,
    /// 发生时刻。
    pub at: String,
}

/// 范围变更类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeChangeKind {
    /// 取消（移出当前有效需求集；原始需求记录保留）。
    Cancelled,
    /// 豁免（不再阻断整体通过，但摘要必须显示 waived 数量）。
    Waived,
    /// 解除豁免 / 恢复。
    Restored,
    /// 需求 / 产物 / 检查的新增（走正常引用闭包校验）。
    Added,
}

/// 任务契约。创建后 `revision = 1`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskContract {
    /// schema 版本。
    pub schema: u32,
    /// 任务 id（宿主生成）。
    pub task_id: String,
    /// 修订号；任何修订 +1，旧回执 / 旧验收结果不再满足新契约。
    pub revision: u64,
    /// 契约归属身份（宿主补齐，模型不能指定）。
    pub owner: RunIdentity,
    /// 需求。
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    /// 产物。
    #[serde(default)]
    pub artifacts: Vec<ArtifactRequirement>,
    /// 检查。
    #[serde(default)]
    pub checks: Vec<CheckSpec>,
    /// 审查策略。
    #[serde(default)]
    pub review_policy: ReviewPolicy,
    /// 范围变更历史。
    #[serde(default)]
    pub scope_changes: Vec<ScopeChange>,
}

impl TaskContract {
    /// 按 id 查检查。
    pub fn check(&self, id: &str) -> Option<&CheckSpec> {
        self.checks.iter().find(|c| c.id == id)
    }
    /// 按 id 查需求。
    pub fn requirement(&self, id: &str) -> Option<&Requirement> {
        self.requirements.iter().find(|r| r.id == id)
    }
    /// 按 id 查产物。
    pub fn artifact(&self, id: &str) -> Option<&ArtifactRequirement> {
        self.artifacts.iter().find(|a| a.id == id)
    }
    /// 某项当前是否被取消（存在 Cancelled 且其后无 Restored 的范围变更）。
    pub fn is_cancelled(&self, target_id: &str) -> bool {
        let mut cancelled = false;
        for sc in self
            .scope_changes
            .iter()
            .filter(|sc| sc.target_id == target_id)
        {
            match sc.kind {
                ScopeChangeKind::Cancelled => cancelled = true,
                ScopeChangeKind::Restored => cancelled = false,
                _ => {}
            }
        }
        cancelled
    }
    /// 某项当前是否被豁免。
    pub fn is_waived(&self, target_id: &str) -> bool {
        let mut waived = false;
        for sc in self
            .scope_changes
            .iter()
            .filter(|sc| sc.target_id == target_id)
        {
            match sc.kind {
                ScopeChangeKind::Waived => waived = true,
                ScopeChangeKind::Restored => waived = false,
                _ => {}
            }
        }
        waived
    }
}

// ---------- 独立审查（#9 / #10 / #18 共享同一份记录模型） ----------

/// 审查结论。**角色字符串不是身份**：`reviewer.actor_id` 必须与被审对象不同，
/// 否则宿主直接拒绝该证据（[#10](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewResult {
    /// schema 版本。
    pub schema: u32,
    /// 记录 id（宿主生成）。
    pub id: String,
    /// 产出者身份（提交审查报告的 runtime）。
    pub owner: RunIdentity,
    /// **实际审查身份**；与被审对象同 actor 时宿主拒绝采纳。
    pub reviewer: RunIdentity,
    /// 被审任务 id。
    pub task_id: String,
    /// Exact registered review check; legacy unbound records remain display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    /// 审查时针对的契约修订（修订升级后旧审查不再适用）。
    pub contract_revision: u64,
    /// 逐维结论。
    #[serde(default)]
    pub dimensions: Vec<ReviewDimensionResult>,
    /// 总体判定。
    pub verdict: ReviewVerdict,
    /// 审查绑定的范围版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_version: Option<String>,
    /// 审查时刻。
    pub reviewed_at: String,
}

/// 单个维度的审查结论。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewDimensionResult {
    /// Explicit coverage state. Missing legacy state remains unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<super::responsibility::DimensionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stable review findings retained for a subsequent review round.
    #[serde(default)]
    pub review_findings: Vec<super::responsibility::ReviewFinding>,
    /// 维度名（见 [`REVIEW_DIMENSIONS`]）。
    pub dimension: String,
    /// 发现项。
    #[serde(default)]
    pub findings: Vec<ValidationFinding>,
}

/// 审查总判定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewVerdict {
    /// 是否批准（无阻断项）。
    pub approved: bool,
    /// 阻断级发现数（`approved` 的真实依据，不能只信布尔）。
    pub blocking_count: usize,
}

/// 契约草稿：**模型只能提出**，归属身份由宿主补齐。
///
/// 与 [`TaskContract`] 的区别是它没有 `task_id` / `revision` / `owner` / `scope_changes`——
/// 这四项宿主赋值，模型填不了也填不进来（填了也不作数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractDraft {
    /// 需求。
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    /// 产物。
    #[serde(default)]
    pub artifacts: Vec<ArtifactRequirement>,
    /// 检查。
    #[serde(default)]
    pub checks: Vec<CheckSpec>,
    /// 审查策略。
    #[serde(default)]
    pub review_policy: ReviewPolicy,
    /// 契约声明的根（宿主会与真实快照核对，不一致即拒绝）。
    #[serde(default)]
    pub roots: Vec<RootIdentity>,
    /// 范围变更（豁免 / 取消 / 新增）。
    ///
    /// 这一项**必须**可由草稿携带：`review_waiver_without_routing` 要求"机械任务豁免
    /// 独立审查"时留下路由依据，而依据的唯一载体就是 `ScopeChange`。草稿带不了它，
    /// 那条校验就永远不可满足——非审查类契约一条都建不出来。
    #[serde(default)]
    pub scope_changes: Vec<ScopeChange>,
}

impl ContractDraft {
    /// 由草稿 + 宿主身份构造正式契约（`revision = 1`）。
    pub fn into_contract(self, task_id: String, owner: RunIdentity) -> TaskContract {
        TaskContract {
            schema: SCHEMA,
            task_id,
            revision: 1,
            owner,
            requirements: self.requirements,
            artifacts: self.artifacts,
            checks: self.checks,
            review_policy: self.review_policy,
            scope_changes: self.scope_changes,
        }
    }
}

// ---------- 交付结果 ----------

/// 子代理收尾形态（复述 [tools::subagent::SubEnded](../../../../docs/subagents.md) 的四象限，
/// 落盘为不可变记录需要可序列化形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndedKind {
    /// `report`
    Report,
    /// `partial`
    Partial,
    /// `budget`
    Budget,
    /// `no_report`
    NoReport,
}

/// 未完成项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncompleteItem {
    /// 未完成的需求 / 产物 id。
    pub target_id: String,
    /// 原因。
    pub reason: String,
}

/// 阻塞原因。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blocker {
    /// 阻塞目标 id。
    pub target_id: String,
    /// 阻塞类别（如 `missing_dependency` / `permission_denied` / `budget_exhausted`）。
    pub code: String,
    /// 说明。
    pub message: String,
}

/// 声明的变更（[#12](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeRef {
    /// 变更路径（`root_id + 相对路径`）。
    pub path: PathRef,
    /// 变更前内容哈希（不存在则为 `None`——创建/新增）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    /// 变更后内容哈希。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    /// 变更类别。
    pub kind: ChangeKind,
}

/// 变更类别。rename 首版按 delete + create 处理（不做 rename 推断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// 新增。
    Created,
    /// 修改。
    Modified,
    /// 删除。
    Deleted,
}

/// 交付结果（[#17](../../../../docs/tools-and-safety.md)）。owner / id / ended 由**宿主**赋值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryResult {
    /// schema 版本。
    pub schema: u32,
    /// 交付 id（宿主生成）。
    pub id: String,
    /// 对应契约修订（草稿提交时快照的 revision；旧 revision 的验收结果不满足新契约）。
    pub contract_revision: u64,
    /// 归属身份（宿主补齐）。
    pub owner: RunIdentity,
    /// 任务 id（宿主从调用上下文解析）。
    pub task_id: String,
    /// 子代理收尾形态。
    pub ended: EndedKind,
    /// 完成的需求 / 产物 id。
    #[serde(default)]
    pub completed: Vec<String>,
    /// 未完成项。
    #[serde(default)]
    pub incomplete: Vec<IncompleteItem>,
    /// 声明的变更。
    #[serde(default)]
    pub changes: Vec<ChangeRef>,
    /// 证据 id（宿主侧 EvidenceRef 的记录 id 列表）。
    #[serde(default)]
    pub evidence: Vec<String>,
    /// 阻塞。
    #[serde(default)]
    pub blockers: Vec<Blocker>,
    /// 提交时刻。
    pub submitted_at: String,
}

// ---------- 验收 ----------

/// 单项验收状态。**没有以 null 代替未执行**的约定（共同契约 §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceStatus {
    /// 从未检查。
    Pending,
    /// 通过：全部必需检查与产物成立。
    Passed,
    /// 失败：至少一项明确失败。
    Failed,
    /// 阻塞：等待人工 / 资源 / 平台能力。
    Blocked,
    /// 证据有效但相关文件已变（[#15](../../../../docs/tools-and-safety.md)）。
    Stale,
    /// 用户明确豁免。
    Waived,
    /// 用户明确取消。
    Cancelled,
}

/// 整体验收状态归约。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceStatusKind {
    /// 整个任务被取消。
    Cancelled,
    /// 无法继续。
    Blocked,
    /// 有明确失败。
    Failed,
    /// 证据已失效。
    Stale,
    /// 尚未完成全部检查。
    Pending,
    /// 全部必需项通过（可能含合法 waived，摘要必须同时显示 waived 数量）。
    Passed,
}

/// 逐项验收条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceItem {
    /// 需求 id。
    pub requirement_id: String,
    /// 状态。
    pub status: AcceptanceStatus,
    /// 支撑证据。
    #[serde(default)]
    pub evidence_refs: Vec<EvidenceRef>,
    /// 未执行 / 未满足原因。
    #[serde(default)]
    pub reasons: Vec<Reason>,
    /// 判定所依据的当前范围版本摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluated_version: Option<String>,
}

/// 结构化原因。位置保存在这里，**不能把错误码文本当状态真值**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reason {
    /// 稳定原因码（如 `check_failed` / `evidence_stale` / `no_evidence`）。
    pub code: String,
    /// 可定位描述。
    pub message: String,
    /// 相关 id（检查 / 产物 / 证据）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

/// 验收快照（只读投影）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceSnapshot {
    /// 任务 id。
    pub task_id: String,
    /// 契约修订。
    pub contract_revision: u64,
    /// 快照自身修订（每次求值 +1；旧 revision 不覆盖当前值）。
    pub revision: u64,
    /// 逐项。
    #[serde(default)]
    pub items: Vec<AcceptanceItem>,
    /// 整体状态。
    pub overall: AcceptanceStatusKind,
    /// 合法豁免数量（`overall = passed` 且该值 > 0 时摘要必须提示）。
    #[serde(default)]
    pub waived_count: usize,
    /// 判定时的当前范围版本摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluated_version: Option<String>,
    /// 整体级原因。
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

impl AcceptanceSnapshot {
    /// 归约整体状态（[#54](../../../../docs/tools-and-safety.md) 明确优先级）：
    /// cancelled（整个任务被取消）→ blocked → failed → stale → pending → passed。
    pub fn reduce_overall(
        items: &[AcceptanceItem],
        whole_task_cancelled: bool,
    ) -> AcceptanceStatusKind {
        if whole_task_cancelled {
            return AcceptanceStatusKind::Cancelled;
        }
        let active: Vec<&AcceptanceItem> = items
            .iter()
            .filter(|i| i.status != AcceptanceStatus::Cancelled)
            .collect();
        if active.iter().any(|i| i.status == AcceptanceStatus::Blocked) {
            return AcceptanceStatusKind::Blocked;
        }
        if active.iter().any(|i| i.status == AcceptanceStatus::Failed) {
            return AcceptanceStatusKind::Failed;
        }
        if active.iter().any(|i| i.status == AcceptanceStatus::Stale) {
            return AcceptanceStatusKind::Stale;
        }
        if active.iter().any(|i| i.status == AcceptanceStatus::Pending) {
            return AcceptanceStatusKind::Pending;
        }
        AcceptanceStatusKind::Passed
    }
}

/// 结构化提交完整性（[#17](../../../../docs/tools-and-safety.md)）。与 `SubEnded` 四象限**独立**——
/// 不通过重命名 ended 混淆"跑完了"与"交付完整"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionState {
    /// 字段完整、引用合法。
    Valid,
    /// 提交了但字段缺失 / 引用非法 / 状态矛盾。
    Invalid,
    /// 未提交结构化结果（仅文本收尾）。
    Unsubmitted,
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
