//! 会话级运行偏好（Composer 工具条批次，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）：审批档位 + 会话模型 + 思考力度。
//! 纯内存：随 SessionRuntime 存活；重启后回落全局默认（已知限制，
//! [docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md) §limitations）。

use crate::core::config::ConfigState;
use serde::{Deserialize, Serialize};

/// 五档审批模式（Composer 下拉，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md) 安全语义表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ApprovalMode {
    /// 逐项确认：文件写入（edit/create/delete 及工作区内 shell 写）前弹审批
    ConfirmEach,
    /// 自动编辑：工作区内写入直通，fence 高危命令仍需确认
    AutoEdit,
    /// 计划模式（默认档，[docs/thinking-scroll-fix](../../../docs/thinking-scroll-fix.md)）：只调研与出方案，不做修改；方案获批后再执行。
    #[default]
    Plan,
    /// 目标模式：先澄清目标与验收标准（澄清期只读），登记后按**账本**（允许触碰的路径与程序）
    /// 执行最小改动；工作区内写入直通（与自动编辑档同），越界由驱动层硬拦而非弹审批。
    Goal,
    /// 完全放行：跳过审批弹窗与 fence 确认；灾难级命令仍被直接拦截
    FullAccess,
}

impl ApprovalMode {
    /// 全局 approval.enabled → 新会话初始档位（默认 = Plan，[docs/thinking-scroll-fix](../../../docs/thinking-scroll-fix.md)；
    /// enabled=false ≈ 跳过确认，同样映射为 FullAccess）。
    pub fn from_global(enabled: bool) -> Self {
        if enabled {
            ApprovalMode::Plan
        } else {
            ApprovalMode::FullAccess
        }
    }

    /// fence 是否把工作区内写目标升级为 Confirm（ConfirmEach / Plan 两档）。
    /// 目标档不在此列：执行期的范围控制由账本在驱动层做（越界即拒，不弹审批）。
    pub fn confirm_inside_writes(self) -> bool {
        matches!(self, ApprovalMode::ConfirmEach | ApprovalMode::Plan)
    }

    /// 计划模式：shell 只读命令白名单直通，白名单外一律 Confirm。
    /// 目标档为 false：澄清期的只读由驱动层排除写工具实现，不走 fence（fence 只管命令形态）。
    pub fn plan_readonly(self) -> bool {
        self == ApprovalMode::Plan
    }
}

/// 思考力度四档（"default" = None，回落模型配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

impl EffortLevel {
    /// 解析模型配置里的自由字符串（未知值视为未设置）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" | "minimal" => Some(EffortLevel::Low),
            "medium" => Some(EffortLevel::Medium),
            "high" => Some(EffortLevel::High),
            "max" => Some(EffortLevel::Max),
            _ => None,
        }
    }

    /// OpenAI 系协议的力度字符串（Max 是非标准枚举，显式降级为 high）。
    pub fn to_openai(self) -> &'static str {
        match self {
            EffortLevel::Low => "low",
            EffortLevel::Medium => "medium",
            EffortLevel::High | EffortLevel::Max => "high",
        }
    }

    /// Anthropic thinking 预算占用 max_tokens 的比例。
    pub fn anthropic_ratio(self) -> f32 {
        match self {
            EffortLevel::Low => 0.2,
            EffortLevel::Medium => 0.4,
            EffortLevel::High => 0.6,
            EffortLevel::Max => 0.8,
        }
    }
}

/// 会话运行偏好（前端 Tab.prefs 的后端镜像，整体替换语义）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionPrefs {
    /// 审批档位（会话内 Shift+Tab 循环切换）
    pub approval_mode: ApprovalMode,
    /// None = 跟随全局 active 模型
    #[serde(default)]
    pub model_id: Option<String>,
    /// None = 跟随模型配置的 reasoning_effort
    #[serde(default)]
    pub reasoning_effort: Option<EffortLevel>,
}

impl SessionPrefs {
    /// 全局配置 → 新会话初始偏好。
    pub fn from_config(cfg: &ConfigState) -> Self {
        SessionPrefs {
            approval_mode: ApprovalMode::from_global(cfg.approval.enabled),
            model_id: None,
            reasoning_effort: None,
        }
    }
}

/// 用户消息携带的图片附件（start_chat 入参；base64 编码）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageIn {
    /// MIME 类型（如 image/png）
    pub mime: String,
    /// base64 图片数据
    pub data: String,
}

/// 生效模型解析：会话覆盖优先；悬空/未设置回落全局 active。
/// 返回摊平形态（provider + model，[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）；调用方可持有返回值。
pub fn effective_model(
    cfg: &ConfigState,
    prefs: &SessionPrefs,
) -> Option<crate::core::config::ModelConfig> {
    if let Some(id) = &prefs.model_id {
        if let Some(m) = cfg.find_model(id) {
            return Some(m);
        }
    }
    cfg.active_model()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_mode_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&ApprovalMode::ConfirmEach).unwrap(),
            r#""confirm_each""#
        );
        assert_eq!(
            serde_json::to_string(&ApprovalMode::FullAccess).unwrap(),
            r#""full_access""#
        );
        let m: ApprovalMode = serde_json::from_str(r#""plan""#).unwrap();
        assert_eq!(m, ApprovalMode::Plan);
    }

    /// 目标档：wire 值为 `goal`，往返一致；默认档仍是 Plan（新增变体不得挪动默认值）。
    #[test]
    fn goal_variant_serde_roundtrip() {
        assert_eq!(
            serde_json::to_string(&ApprovalMode::Goal).unwrap(),
            r#""goal""#
        );
        let m: ApprovalMode = serde_json::from_str(r#""goal""#).unwrap();
        assert_eq!(m, ApprovalMode::Goal);
        assert_eq!(ApprovalMode::default(), ApprovalMode::Plan);
    }

    /// 语义矩阵：目标档 = 工作区内写直通（不升级 Confirm）+ 不走 plan 只读 fence。
    #[test]
    fn approval_mode_semantics_matrix() {
        let cases = [
            (ApprovalMode::ConfirmEach, true, false),
            (ApprovalMode::AutoEdit, false, false),
            (ApprovalMode::Plan, true, true),
            (ApprovalMode::Goal, false, false),
            (ApprovalMode::FullAccess, false, false),
        ];
        for (mode, confirm_inside, plan_readonly) in cases {
            assert_eq!(
                mode.confirm_inside_writes(),
                confirm_inside,
                "{mode:?} confirm_inside_writes"
            );
            assert_eq!(
                mode.plan_readonly(),
                plan_readonly,
                "{mode:?} plan_readonly"
            );
        }
    }

    #[test]
    fn from_global_maps_enabled() {
        assert_eq!(ApprovalMode::from_global(true), ApprovalMode::Plan);
        assert_eq!(ApprovalMode::from_global(false), ApprovalMode::FullAccess);
    }

    #[test]
    fn effort_parse_and_openai_downgrade() {
        assert_eq!(EffortLevel::parse("low"), Some(EffortLevel::Low));
        assert_eq!(EffortLevel::parse("minimal"), Some(EffortLevel::Low));
        assert_eq!(EffortLevel::parse("MAX"), Some(EffortLevel::Max));
        assert_eq!(EffortLevel::parse("bogus"), None);
        assert_eq!(EffortLevel::Max.to_openai(), "high");
        assert_eq!(EffortLevel::Medium.to_openai(), "medium");
    }

    #[test]
    fn prefs_serde_roundtrip() {
        let p = SessionPrefs::default();
        let back: SessionPrefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.approval_mode, ApprovalMode::Plan);
        assert!(back.model_id.is_none() && back.reasoning_effort.is_none());
        // 完整形态解析
        let full: SessionPrefs = serde_json::from_str(
            r#"{"approval_mode":"plan","model_id":"m1","reasoning_effort":"max"}"#,
        )
        .unwrap();
        assert_eq!(full.approval_mode, ApprovalMode::Plan);
        assert_eq!(full.model_id.as_deref(), Some("m1"));
        assert_eq!(full.reasoning_effort, Some(EffortLevel::Max));
    }

    /// 前档快照**刻意不放 prefs**：prefs 是前端整体替换写的事实源（`updatePrefs` 走
    /// `{ ...prev, ...patch }`），把纯后端运行时状态塞进来会被前端补丁冲掉。
    /// 它现在住在 `SessionRuntime::goal_prev_mode`；这里钉死 wire 字段集防回归。
    #[test]
    fn prefs_wire_fields_are_user_preferences_only() {
        let json = serde_json::to_string(&SessionPrefs::default()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 3, "{json}");
        for k in ["approval_mode", "model_id", "reasoning_effort"] {
            assert!(obj.contains_key(k), "缺 {k}: {json}");
        }
        assert!(!json.contains("goal_prev_mode"), "{json}");
    }

    #[test]
    fn effective_model_falls_back_on_dangling_id() {
        let mut cfg = ConfigState::default();
        cfg.providers.push(crate::core::config::ProviderConfig {
            id: "p1".into(),
            models: vec![crate::core::config::ProviderModel {
                id: "m1".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        cfg.active_model_id = Some("m1".into());
        let prefs = SessionPrefs {
            model_id: Some("gone".into()),
            ..Default::default()
        };
        assert_eq!(effective_model(&cfg, &prefs).unwrap().id, "m1");
        let prefs = SessionPrefs {
            model_id: Some("m1".into()),
            ..Default::default()
        };
        assert_eq!(effective_model(&cfg, &prefs).unwrap().id, "m1");
    }
}
