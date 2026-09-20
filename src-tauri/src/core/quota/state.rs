//! 额度状态落盘（全局数据目录下的 `quota.json`）：记录「曾经成功取过数的供应商」。
//!
//! 用途：区分 401/403/404 的两种含义——**从未成功过** = 密钥/套餐被拒（`rejected`，
//! 重试无用），**曾经成功过** = 临时失效（`error`，可重试）。
//!
//! 额度是**账号级**事实：文件固定放全局数据目录（`~/.codewave/quota.json`），
//! 不随项目走（项目数据目录下没有本文件）。
//!
//! 兼容性：结构体级 `#[serde(default)]`——旧文件缺字段透明取默认，绝不因解析失败 panic。
//!
//! 启动期自愈（[`heal`]，见 [docs/quota-from-provider-config](../../../../docs/quota-from-provider-config.md)）：
//! 本文件是「曾成功过」的**唯一持久载体**，写坏后 `load` 只会回退默认，于是所有曾经成功过的
//! 供应商会被误判成「从未成功过」（401/403/404 渲染为「查询被拒」这一误导性结论），且损坏状态
//! 每次启动重复告警、永不收敛。故启动时走一次「要么不管、要么修好」的自愈：
//! - 文件读不到 → 什么都不做（**不创建**：维持「无记录 = 文件不存在」的既有不变式）；
//! - `version` 能按**数值**理解（整数 / 浮点 / 数字字符串）且大于 [`VERSION`] → 完全不碰
//!   （降级运行不得隔离/重写新版数据）；
//! - **文件级形态不识别**（非法 JSON / 截断 / 非 UTF-8 / 顶层非对象 / `verified` 存在但不是数组）
//!   → 移动隔离为 `quota.json.corrupt`（可覆盖、只作证据、不参与任何判据，**不重建**）；日志只记
//!   结构摘要，绝不回显文件内容；
//! - 顶层是对象且 `verified` 是数组（缺失视为空数组）→ **逐元素**解析：单个元素类型错
//!   只丢该元素，同文件里合法元素的「曾成功过」事实**全部保留**；
//! - 可用记录里只清「必然不可用」的（空白 `provider_id` / 非法 RFC3339 时间戳）；`version` 能按
//!   `u32` 原样理解就原样保留（`0` 也不会仅因版本号被写盘），读不成 `u32` 的形态取 [`VERSION`]；
//! - **写盘判据 = 丢了记录，或「盘上内容按 [`QuotaState::load`] 的严格语义读出来 ≠ 保留后的状态」**。
//!   后半句不可省：本模块的分层判定有意比 `load` 宽松（`version` 不参与结构解析、`verified: null`
//!   当空数组），只看「有没有丢记录」会留下「`heal` 判健康、`load` 却整文件回退默认」的形态——
//!   `{"version":"abc","verified":[<合法记录>]}` 与 `{"version":1,"verified":null}` 正是两种：
//!   那种文件每次启动都重复告警，且「曾成功过」全部失效（那些家下次 401/403/404 会被渲染成
//!   「查询被拒」的误导性灰行，正是本批要消灭的结论）。
//!
//! 由此得到本模块的**核心不变式**：`heal` 返回后，文件要么不存在（读不到 / 已被隔离留证），要么
//! [`QuotaState::load`] 成功且读出的状态与 `heal` 保留的状态完全一致。写盘内容就是保留状态的序列化
//! （`u32` + 字符串字段数组），写完 `load` 必然成功——**不变式由构造保证**，不靠枚举「哪些形态会漏修」；
//! 用例 `heal_always_leaves_a_loadable_file_or_none_at_all` 逐一形态把它钉死。
//!
//! 为什么是分层判定而不是整结构 `from_slice::<QuotaState>`：元素级类型错（手改出的
//! `"last_ok_at": null`、`"provider_id": 123`）会让**整文件**判失败，同一文件里合法记录的
//! 「曾成功过」事实随之失效 → 那些家下次 401/403/404 会渲染成「查询被拒（可能非订阅账号）」的
//! 误导性灰行，正是本批要消灭的结论。同 schema 下的元素类型变化不可能是「未来版本的新字段」
//! （[`VerifiedProvider`] 是 `#[serde(default)]`：多出的字段被忽略、少的字段取默认），
//! 所以丢掉的只可能是**必然不可用**的元素。

use crate::util::atomic::atomic_write;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// 状态文件名（位于传入的数据目录下）。
pub const FILE_NAME: &str = "quota.json";

/// 当前 schema 版本。
pub const VERSION: u32 = 1;

/// 损坏文件的隔离后缀：`quota.json` → `quota.json.corrupt`
/// （与 `ui-state.json.corrupt`、`sessions/index.json.corrupt` 同一约定）。
pub const BACKUP_SUFFIX: &str = ".corrupt";

/// 状态文件路径：`<数据目录>/quota.json`。
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// 隔离文件路径：`<数据目录>/quota.json.corrupt`。
pub fn backup_path(dir: &Path) -> PathBuf {
    dir.join(format!("{FILE_NAME}{BACKUP_SUFFIX}"))
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
                // 只记结构化摘要：serde 的 `to_string()` 会把出错处的文本（含长字符串）整段回显
                tracing::warn!(
                    "quota.json 解析失败，回退默认：{}",
                    ShapeFailure::from_error(&e).summary()
                );
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

    /// 启动期自愈的内存侧：删掉「必然不可用」的记录（见 [`record_is_usable`]），返回删除条数。
    /// 只做内存改动，落盘由调用方（[`heal`]）按「丢了记录 **或** 严格语义读不回保留结果」决定。
    fn drop_invalid(&mut self) -> usize {
        let before = self.verified.len();
        self.verified.retain(record_is_usable);
        before - self.verified.len()
    }
}

/// 记录是否「能用」。只认两类必然不可用：
///
/// - `provider_id` trim 后为空——没有任何供应商能匹配到它，留着只会让展示层匹配不上；
/// - `last_ok_at` 不是合法 RFC3339——展示层拿不到时间（空串过滤为 None）。
///
/// 判据**只用** `chrono::DateTime::parse_from_rfc3339`（接受 `Z`、`+08:00` 等偏移与小数秒）：
/// 自己写正则、或只认 `to_rfc3339_opts(.., true)` 生成的 `...Z` 形态，都会把合法记录误删——
/// 本文件是「曾成功过」的唯一持久载体，删错等于把可用供应商永久说成「查询被拒」。
/// 取值先 trim：与 `provider_id` 同一取向（宁可保留，也不因首尾空白误删）。
fn record_is_usable(record: &VerifiedProvider) -> bool {
    !record.provider_id.trim().is_empty()
        && chrono::DateTime::parse_from_rfc3339(record.last_ok_at.trim()).is_ok()
}

/// 启动期自愈结果（`Default` = 什么都没发生，即健康 / 文件不存在 / 跳过）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealOutcome {
    /// 文件损坏、已被隔离为 `quota.json.corrupt`（原文件不再存在，且不重建）
    pub quarantined: bool,
    /// 本次自愈**删掉的记录条数**：元素级解析失败被逐元素丢弃的条数 + 可用记录里
    /// 「必然不可用」被清的条数（空白 `provider_id` / 非法 RFC3339 时间戳）。
    ///
    /// 注意它**不等于**「有没有写盘」：为修复「盘上内容 `load` 读不出来」而做的规范化
    /// （`version` 形态非法 / `verified: null` 等）不删任何记录，那时 `dropped == 0` 却确实写了盘
    /// （见 [`Self::repaired_only`]）；反之 `dropped > 0` 必然要写盘（除非写盘失败，见 [`Self::write_failed`]）。
    pub dropped: usize,
    /// 本次写盘**只为了「让文件能被 [`QuotaState::load`] 读回来」**，一条记录都没删（`dropped == 0`）。
    ///
    /// 起因：本模块的逐元素宽松判定与 `load` 的严格语义不在一条线上——`{"version":"abc"}` /
    /// `{"verified":null}` 这类形态 `load` 会**整文件**回退默认（「曾成功过」全部失效），必须修成
    /// 可读形态。这个字段把「修复可读性」与「清理了坏记录」区分开，避免 `dropped == 0` 被误
    /// 读成「什么都没做」（写盘失败时为 false，另见 [`Self::write_failed`]）。
    pub repaired_only: bool,
    /// 文件 `version` 高于当前 schema，**一字未动**地跳过
    pub skipped_future: bool,
    /// 确有修复（清理或规范化）待写但写盘失败（原文件保留原地，行为退回自愈前）
    pub write_failed: bool,
}

/// 启动期一次性自愈：见模块文档。
///
/// 与 [`commit`] 共用同一把进程内互斥（[`lock_io`]）——自愈的「读—改—写」同样必须串行，
/// 否则会与额度刷新批次互相覆盖（正是「额度静默消失」的老事故）。临界区内只有同步文件 IO，
/// **不跨 `.await`**；任何失败路径都不 panic、不 unwrap，只 `tracing::warn` 并如实回填 [`HealOutcome`]。
/// 健康文件不写盘、不记日志。返回后模块文档里的核心不变式成立：文件要么不存在（读不到 / 已隔离），
/// 要么 [`QuotaState::load`] 成功且等于本函数保留的状态。
pub fn heal(dir: &Path) -> HealOutcome {
    let _guard = lock_io();
    let file = path(dir);
    // 读不到（文件不存在 / 目录不可读 / `dir` 本身不是目录）→ 什么都不做：不创建、不写盘。
    let Ok(bytes) = std::fs::read(&file) else {
        return HealOutcome::default();
    };

    // 降级运行保护：新版 schema 的文件**完全不碰**（隔离会挪走新版数据，清洗写回会把未知字段写没）。
    if future_version(&bytes) {
        tracing::warn!(
            "quota.json 版本高于当前 schema（v{}），跳过启动自愈",
            VERSION
        );
        return HealOutcome {
            skipped_future: true,
            ..HealOutcome::default()
        };
    }

    let ParsedState {
        mut state,
        dropped: unreadable,
    } = match parse_state(&bytes) {
        Ok(parsed) => parsed,
        Err(failure) => {
            // 摘要来自形态枚举（类别 + 行列号），**不回显文件内容**——日志常被用户分享
            let detail = failure.summary();
            if quarantine(dir) {
                tracing::warn!(
                    "quota.json 无法使用（{detail}），已隔离为 {FILE_NAME}{BACKUP_SUFFIX} 并保留原字节"
                );
                return HealOutcome {
                    quarantined: true,
                    ..HealOutcome::default()
                };
            }
            return HealOutcome::default();
        }
    };

    // 严格语义（[`QuotaState::load`] 用的那一条）读出来的状态：`Err` = 这个文件 `load` **读不了**。
    // 判据必须用它，而不是「有没有丢记录」：本模块的分层判定有意比 `load` 宽松，宽松侧修不好的
    // 形态（`version` 形态非法 / `verified: null`）在 `load` 侧是**整文件**回退默认。
    let loadable: Option<QuotaState> = serde_json::from_slice::<QuotaState>(&bytes).ok();
    let dropped = state.drop_invalid() + unreadable;
    // 写盘判据 = 丢了记录 **或** 盘上内容按严格语义读出来与保留结果不一致。
    // 两者都写：`dropped > 0` 已被不等式蕴含（丢过记录则状态必然变了），保留它只为可读性与诊断。
    // 为什么这样就够：写盘内容就是 `state` 的序列化（`u32` + 字符串字段数组），写完 `load`
    // **必然**成功且等于 `state`——不变式由构造保证，后续自愈也必然转 no-op（不 churn）。
    let needs_write = dropped > 0 || loadable.as_ref() != Some(&state);
    if !needs_write {
        return HealOutcome::default();
    }
    if state.save(dir) {
        if dropped > 0 {
            tracing::warn!(
                "quota.json 清理了 {dropped} 条不可用记录（元素类型错 / 空白 provider_id / 非法时间戳）"
            );
            HealOutcome {
                dropped,
                ..HealOutcome::default()
            }
        } else {
            // 一条记录都没删，只是盘上形态 `load` 读不回来（`version` 非法 / `verified: null`）：
            // 重写成可读形态，让「曾成功过」的事实不再是每次启动都失效的空转。
            tracing::warn!(
                "quota.json 形态可用但严格语义读不回来（version / verified 写法异常），已重写为可读形态（未删记录）"
            );
            HealOutcome {
                repaired_only: true,
                ..HealOutcome::default()
            }
        }
    } else {
        // 写盘失败（无写权限 / 磁盘满）：原文件保留原地，下次启动再试，退回自愈前行为。
        if dropped > 0 {
            tracing::warn!("quota.json 有 {dropped} 条不可用记录待清但写入失败，原文件保留原地");
        } else {
            tracing::warn!("quota.json 需要重写为可读形态但写入失败，原文件保留原地");
        }
        HealOutcome {
            dropped,
            write_failed: true,
            ..HealOutcome::default()
        }
    }
}

/// 文件级「形态不识别」的原因：**只描述形态，绝不携带文件内容**（日志卫生，AC-10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShapeFailure {
    /// JSON 本身解析不了：语法错 / 截断 / 空文件 / 非 UTF-8（附 serde 的类别 + 行列号）
    Malformed {
        category: &'static str,
        line: usize,
        column: usize,
    },
    /// 顶层不是 JSON 对象（`[]` / `null` / `5`）
    NotAnObject,
    /// `verified` 存在但不是数组（如 `{"verified": 5}` / `{"verified": "x"}`）
    VerifiedNotAnArray,
}

impl ShapeFailure {
    /// 从 `serde_json::Error` 提炼摘要：**类别 + 行列号**。
    ///
    /// 为什么不用 `Error::to_string()`：serde 对 `Unexpected::Str` 会打印**完整字符串不截断**，
    /// 于是 `{"version": "<很长内容>"}`（结构体解析的 `Data` 错误）会把整段内容写进 warn。
    pub(crate) fn from_error(error: &serde_json::Error) -> Self {
        use serde_json::error::Category;
        Self::Malformed {
            category: match error.classify() {
                Category::Io => "读取/IO 错误",
                Category::Syntax => "JSON 语法错误",
                Category::Data => "字段类型或结构不符",
                Category::Eof => "JSON 截断或空文件",
            },
            line: error.line(),
            column: error.column(),
        }
    }

    /// 供 warn 用的可读摘要（纯结构化，无文件内容）。
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Malformed {
                category,
                line,
                column,
            } => format!("{category}（第 {line} 行 {column} 列）"),
            Self::NotAnObject => "顶层不是 JSON 对象".to_string(),
            Self::VerifiedNotAnArray => "verified 存在但不是数组".to_string(),
        }
    }
}

/// 严格解析的结果：可用状态 + 被逐元素丢弃的条数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedState {
    pub state: QuotaState,
    /// 元素级解析失败被丢弃的条数（当作「必然不可用」删除）
    pub dropped: usize,
}

/// 分层判定（见模块文档）；`Err` = **文件级**形态不识别，交由 [`heal`] 隔离留证。
///
/// **顶层必须是 JSON 对象**：`serde` 的 derive 对结构体同时接受「按字段顺序的数组」形态，
/// 即 `[]` 会被解析成「全默认」（`version` 取默认、`verified` 为空）而悄悄通过——那仍是
/// 形态不识别（AC-1 明列 `[]` 属损坏），所以先宽松取顶层形状、再逐元素解析。
///
/// `version` 只做「能原样保留就保留」处理：顶层 `version` 是合法 `u32` 就**原样留**（缺失 / `0` /
/// `1` 都不会仅因版本号被写盘，满足 AC-14），读不成 `u32` 的形态（`"abc"` / `null` / 浮点 / 负数 /
/// 超 `u32`）取 [`VERSION`]。注意后者**恰恰是 [`QuotaState::load`] 会整文件失败的形态**，取
/// [`VERSION`] 只是为了让写回的字节可读；到底写不写盘由 [`heal`] 的「严格语义读不回保留结果」判据定。
pub(crate) fn parse_state(bytes: &[u8]) -> Result<ParsedState, ShapeFailure> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|e| ShapeFailure::from_error(&e))?;
    let serde_json::Value::Object(fields) = value else {
        return Err(ShapeFailure::NotAnObject);
    };
    let items = match fields.get("verified") {
        // 缺失 = 空数组（与结构体上 `#[serde(default)]` 同语义）→ 不是形态错、不隔离。
        // `null` 也当空数组（值上确实是「没有记录」），但注意它**不是** `load` 的语义：
        // 显式 `null` 不等于字段缺失，`#[serde(default)]` 不生效，`load` 会整文件回退默认——
        // 所以这种文件会由下方写盘判据重写成 `[]`（修可读性，不删记录）。
        None | Some(serde_json::Value::Null) => &[] as &[serde_json::Value],
        Some(serde_json::Value::Array(items)) => items.as_slice(),
        Some(_) => return Err(ShapeFailure::VerifiedNotAnArray),
    };
    // 逐元素解析：单个元素类型错只丢该元素，合法元素全部保留（宁可少丢，不可整文件判死）
    let kept: Vec<VerifiedProvider> = items
        .iter()
        .filter_map(|item| VerifiedProvider::deserialize(item).ok())
        .collect();
    let version = fields
        .get("version")
        .and_then(version_as_u32)
        .unwrap_or(VERSION);
    let dropped = items.len() - kept.len();
    Ok(ParsedState {
        state: QuotaState {
            version,
            verified: kept,
        },
        dropped,
    })
}

/// 顶层 `version` 能原样理解成 `u32` 就取值，否则 `None`（调用方取 [`VERSION`]）。
///
/// 与 [`QuotaState::load`] 对 `u32` 的严格语义同一取向：只认 JSON **整数**（`as_u64` 为 `Some`
/// 且装得进 `u32`）；浮点（`1.0`）、字符串（`"1"`）、`null` 都不算——它们正是 `load` 会失败的形态，
/// 归 [`VERSION`] 是为了写回可读，而不是「反正值一样」。
fn version_as_u32(value: &serde_json::Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}

/// 隔离损坏文件：`quota.json` → `quota.json.corrupt`，成功返回 true（**不重建**原文件）。
///
/// 顺序：先删同名旧备份（备份可覆盖、只作证据、不参与任何判据）→ `rename`；
/// `rename` 失败（跨设备等）退化为 `copy` + `remove_file`；再失败只 warn 并保留原文件——
/// 此时行为与自愈前完全一致，不会更差。全部错误路径不 panic。
fn quarantine(dir: &Path) -> bool {
    let from = path(dir);
    let to = backup_path(dir);
    let _ = std::fs::remove_file(&to);
    if std::fs::rename(&from, &to).is_ok() {
        return true;
    }
    // rename 失败（跳设备等）：退化为复制 + 删原文件（复制失败时绝不动原文件）
    if std::fs::copy(&from, &to).is_ok() && std::fs::remove_file(&from).is_ok() {
        return true;
    }
    // 仍然失败：清掉可能留下的不完整副本，维持「.corrupt 存在 ⟺ 原文件已被移走」；
    // 原文件保留原地（宁可下次再判损坏，也不丢现场），行为与自愈前完全一致。
    let _ = std::fs::remove_file(&to);
    tracing::warn!(
        "quota.json 隔离失败（{} → {}），原文件保留原地",
        from.display(),
        to.display()
    );
    false
}

/// 宽松探 `version`：顶层是合法 JSON 且 `version` 能按**数值**理解且大于当前 [`VERSION`] → true。
///
/// 用独立的宽松解析而非 `QuotaState`（后者形态不认时会失败，那时属「损坏」而不是「未来版本」）。
/// 数值形态宁宽勿严：整数（`2`）、浮点（`2.0` / `2.5`）、数字字符串（`"2"`）、超出 `u64` 的大整数
/// （`99999999999999999999`，serde 当浮点存）都认；漏判会让降级运行把这等新版数据隔离挪走
/// （AC-13 的意图正是「未知/未来 schema 一律不碰」）。非数值形态（缺失 / `null` / `"abc"` /
/// 对象 / 数组）→ 走正常流程（那时若形态确实不识别，仍由 [`parse_state`] 判隔离以留证）。
fn future_version(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| value.get("version").and_then(numeric_version))
        .is_some_and(|version| version > f64::from(VERSION))
}

/// 把 `version` 字段按数值理解（整数 / 浮点 / 数字字符串，字符串先 trim）；其余一律 None。
fn numeric_version(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
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
