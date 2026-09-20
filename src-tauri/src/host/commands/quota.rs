//! 订阅额度 IPC：只做校验 + 转调，业务逻辑在 `core::quota`
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。

use super::util::{Core, err};
use crate::core::quota::{self, QuotaSnapshot};

/// 全部已配置供应商的额度快照（数据源 = `config.providers`，每行带行态）。
///
/// `active_provider_id` = 当前会话生效模型所属供应商 id（前端从设置里取）：命中者置顶，
/// 其余可查询类按配置序、`unsupported` 类殿后。
/// 状态文件 `quota.json` 读写在**全局数据目录**（额度是账号级事实，不随项目走）。
#[tauri::command]
pub async fn quota_snapshots(
    core: Core<'_>,
    active_provider_id: Option<String>,
) -> Result<Vec<QuotaSnapshot>, String> {
    let cfg = core.cfg.read().map_err(err)?.clone();
    Ok(quota::snapshots(&cfg, active_provider_id.as_deref(), &core.data_dir).await)
}
