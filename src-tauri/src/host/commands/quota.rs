//! 订阅额度 IPC：只做转调，业务逻辑在 `core::quota`
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。

use super::util::{err, Core};
use crate::core::quota::{self, QuotaSnapshot};

/// 已配置凭证的提供商额度快照。
///
/// `active_base_url` = 当前会话生效模型所属 provider 的 base_url（由前端从设置里取）：
/// 命中的提供商置顶，其余保持固定注册序；未配置凭证的提供商不出现在结果里。
#[tauri::command]
pub async fn quota_snapshots(
    core: Core<'_>,
    active_base_url: Option<String>,
) -> Result<Vec<QuotaSnapshot>, String> {
    let cfg = core.cfg.read().map_err(err)?.clone();
    Ok(quota::snapshots(&cfg, active_base_url.as_deref()).await)
}
