//! 额度状态落盘（全局数据目录下的 `quota.json`）：记录「曾经成功取过数的供应商」。
//!
//! 用途：区分 401/403/404 的两种含义——**从未成功过** = 密钥/套餐被拒（`rejected`，
//! 重试无用），**曾经成功过** = 临时失效（`error`，可重试）。
//!
//! 额度是**账号级**事实：文件固定放全局数据目录（`~/.codewave/quota.json`），
//! 不随项目走（项目数据目录下没有本文件）。
//!
//! 兼容性：结构体级 `#[serde(default)]`——旧文件缺字段透明取默认，绝不因解析失败 panic。

use crate::util::atomic::atomic_write;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// 状态文件名（位于传入的数据目录下）。
pub const FILE_NAME: &str = "quota.json";

/// 当前 schema 版本。
pub const VERSION: u32 = 1;

/// 状态文件路径：`<数据目录>/quota.json`。
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// 一家「曾经成功取数」的供应商。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VerifiedProvider {
    /// CodeWave 供应商 id（`config.providers[].id`）
    pub provider_id: String,
    /// 最近一次成功取数的时刻（RFC3339 秒）
    pub last_ok_at: String,
}

/// 额度状态文件根结构（`version` + 已成功过的供应商列表）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotaState {
    pub version: u32,
    pub verified: Vec<VerifiedProvider>,
}

impl Default for QuotaState {
    fn default() -> Self {
        Self {
            version: VERSION,
            verified: Vec::new(),
        }
    }
}

impl QuotaState {
    /// 读盘：文件不存在 / 内容损坏 / 形态不识别一律回退默认值（绝不 panic）。
    pub fn load(dir: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path(dir)) else {
            return Self::default();
        };
        match serde_json::from_slice::<QuotaState>(&bytes) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!("quota.json 解析失败，回退默认：{e}");
                Self::default()
            }
        }
    }

    /// 原子写盘（与 config.json 同一写法）；失败只记日志，不阻断额度展示。
    pub fn save(&self, dir: &Path) -> bool {
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => match atomic_write(&path(dir), &bytes) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("quota.json 写入失败：{e}");
                    false
                }
            },
            Err(e) => {
                tracing::warn!("quota.json 序列化失败：{e}");
                false
            }
        }
    }

    /// 该供应商是否曾经成功取过数（401/403/404 → `rejected` / `error` 的判据）。
    pub fn has_succeeded(&self, provider_id: &str) -> bool {
        self.verified.iter().any(|v| v.provider_id == provider_id)
    }

    /// 记一次成功：同 id 覆盖时间戳，不产生重复行。
    pub fn record_ok(&mut self, provider_id: &str, now: &str) {
        match self
            .verified
            .iter_mut()
            .find(|v| v.provider_id == provider_id)
        {
            Some(existing) => existing.last_ok_at = now.to_string(),
            None => self.verified.push(VerifiedProvider {
                provider_id: provider_id.to_string(),
                last_ok_at: now.to_string(),
            }),
        }
    }

    /// 按当前 providers 清理孤儿条目（已删除的供应商）。
    /// 返回是否有变更——上层据此判「本批次是否需要写盘」（清孤儿也需落盘）。
    pub fn retain(&mut self, ids: &[String]) -> bool {
        let before = self.verified.len();
        self.verified.retain(|v| ids.contains(&v.provider_id));
        before != self.verified.len()
    }
}

/// 进程内互斥：`quota.json` 的「读—改—写」必须串行。
///
/// 起因：额度刷新是「先并发取数、批次末统一落盘」。两次刷新并发（手动连点刷新 +
/// 5 分钟定时器 / 前端 debounce 重拉）时会各自读到同一份旧状态再各自写回，
/// **后写覆盖前写**——丢掉某家的 `last_ok_at` 后，该家下次 401/403/404 就会被判成
/// 「从未成功过」而灰掉，正是本批要消灭的「额度静默消失」观感。
///
/// **绝不跨 `.await` 持锁**：临界区里只有同步文件 IO（load → 合并 → save），取数与
/// 落盘彻底分离，因此既不会阻塞别家的网络请求，也没有死锁风险。
fn io_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 取临界区守卫；中毒（持锁期 panic）不连带后续批次失效，直接取回内部值。
fn lock_io() -> MutexGuard<'static, ()> {
    io_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 批次末的原子提交：**在临界区内重新读盘**再合并本批成功者，最后一次写盘。
///
/// 为什么必须重新读盘：批次开始时读到的是「取数前」的状态，取数期间可能有另一批次
/// 落盘；直接写回内存状态就会覆盖对方的记录。这里是增量合并：
/// 重新 load → 清孤儿（`retain`）→ 对本批成功者 `record_ok` → save。
///
/// - `ids` = 当前配置的 provider id（用于清孤儿）；`ok_ids` = 本批取数成功者；
/// - `now` = 本批取数时刻（RFC3339 秒）；
/// - 无成功者且无孤儿 → **不写盘**（目录里不出现文件），返回 false。
pub fn commit(dir: &Path, ids: &[String], ok_ids: &[String], now: &str) -> bool {
    let _guard = lock_io();
    let mut latest = QuotaState::load(dir);
    let orphans_removed = latest.retain(ids);
    for id in ok_ids {
        latest.record_ok(id, now);
    }
    if ok_ids.is_empty() && !orphans_removed {
        return false;
    }
    latest.save(dir)
}
