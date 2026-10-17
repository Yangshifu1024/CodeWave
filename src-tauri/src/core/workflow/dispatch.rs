//! #2 并行准入：写范围租约的**原子**获取。
//!
//! 复用既有 claims 的路径归一化（`canonical_best_effort` + 组件级祖先判定），
//! **不复制**第二套路径比较（[docs/subagents](../../../../docs/subagents.md)）。
//!
//! 首版的核心判断：**能归因才谈并行**。
//! - 只有结构化写工具（create/edit/delete）能归因到具体文件；
//! - 声明了 shell / MCP 任意写能力的单元，`side_effect_scope = unknown`，一律串行；
//!   **模型承诺「shell 只写某文件」不予采信**；
//! - 公共文件（锁文件 / 公共类型 / 导出入口）只能有一个 owner；
//! - 一次失败**不使依赖自动 ready**。

use crate::core::delivery::types::{PathRef, RunIdentity};
use crate::core::workflow::routing::WorkUnit;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 并发额度（同时持有租约的单元上限）。
pub const MAX_CONCURRENT_UNITS: usize = 4;

/// 写能力能否归因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffectScope {
    /// 可归因：结构化写工具，路径已知。
    Attributable,
    /// 不可归因：shell / MCP 任意写。
    Unknown,
}

impl SideEffectScope {
    pub fn as_str(self) -> &'static str {
        match self {
            SideEffectScope::Attributable => "attributable",
            SideEffectScope::Unknown => "unknown",
        }
    }
}

/// 单元的写能力范围。
pub fn side_effect_scope(unit: &WorkUnit, tool_ids: &[String]) -> SideEffectScope {
    if unit.has_unattributable_writes(tool_ids)
        || unit
            .mutable_resources
            .iter()
            .any(|r| r == "host:serial_without_frozen_plan")
    {
        SideEffectScope::Unknown
    } else {
        SideEffectScope::Attributable
    }
}

/// 准入拒绝。
#[derive(Debug, Clone, PartialEq)]
pub struct Denial {
    /// 稳定码。
    pub code: &'static str,
    /// 说明（含指名冲突方）。
    pub message: String,
}

/// 租约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchLease {
    /// 归属 root run。
    pub root_run_id: String,
    /// 单元 id。
    pub unit_id: String,
    /// attempt。
    pub attempt: u32,
    /// generation：释放时必须匹配，**迟到完成不得释放后继 attempt 的租约**。
    pub generation: u64,
    /// 本租约占用的写范围（归一化后的绝对路径，展示与诊断用）。
    #[serde(default)]
    pub held_paths: Vec<String>,
    #[serde(default)]
    pub exclusive: bool,
    #[serde(default)]
    pub mutable_resources: Vec<String>,
}

// ---------- 进程内租约表 ----------

#[derive(Default)]
struct LeaseTable {
    by_unit: std::collections::BTreeMap<(String, String), DispatchLease>,
    /// **按 root run 分代**：全局单一计数器会让不相关的 run 互相把请求判成过期
    /// （A 准入后 B 的请求就"过期"了），并发下也无法分别重放。
    generation: std::collections::BTreeMap<String, u64>,
    effects: std::collections::BTreeMap<String, Effect>,
}

fn table() -> &'static Mutex<LeaseTable> {
    static T: OnceLock<Mutex<LeaseTable>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(LeaseTable::default()))
}

/// 清空（仅测试用；生产靠 release）。
#[cfg(test)]
pub fn reset_leases() {
    *table().lock().unwrap_or_else(|p| p.into_inner()) = LeaseTable::default();
}

/// 归一化路径（**复用** claims 的同一套：符号链接别名落到同一条目）。
fn key(p: &PathRef, roots: &std::collections::BTreeMap<String, PathBuf>) -> Option<PathBuf> {
    let base = roots.get(&p.root_id)?;
    if p.relative_path.is_empty()
        || std::path::Path::new(&p.relative_path).is_absolute()
        || p.relative_path.split(['/', '\\']).any(|c| c == "..")
    {
        return None;
    }
    let canonical = crate::tools::pathutil::canonical_best_effort(&base.join(&p.relative_path));
    canonical
        .starts_with(crate::tools::pathutil::canonical_best_effort(base))
        .then_some(canonical)
}

/// 候选单元的一次准入尝试。
///
/// 单锁内**全查后全插**：任一候选不满足即整体拒绝，不产生部分准入
/// （部分准入会让"看起来并行、实际半串行"的调度更难排查）。
pub fn admission(
    owner: &RunIdentity,
    root_run_id: &str,
    expected_generation: u64,
    candidates: &[(WorkUnit, Vec<String>)],
    roots: &std::collections::BTreeMap<String, PathBuf>,
    dependency_state: &std::collections::BTreeMap<String, bool>,
    tool_ids_by_unit: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<Vec<DispatchLease>, Vec<Denial>> {
    let mut denials: Vec<Denial> = Vec::new();
    let mut granted: Vec<DispatchLease> = Vec::new();
    let mut taken: Vec<PathBuf> = Vec::new();
    if owner.root_run_id != root_run_id || owner.session_id.is_empty() {
        return Err(vec![Denial {
            code: "identity_mismatch",
            message: "准入身份与root run不匹配".into(),
        }]);
    }
    let ids: BTreeSet<_> = candidates.iter().map(|(u, _)| u.id.as_str()).collect();
    if ids.len() != candidates.len() || ids.contains("") {
        return Err(vec![Denial {
            code: "duplicate_unit",
            message: "工作单元id为空或重复".into(),
        }]);
    }

    // ① 依赖必须**已通过**。partial / failed 不算通过（一次失败不自动解锁）。
    for (unit, _) in candidates {
        for dep in &unit.depends_on {
            let ok = dependency_state.get(dep).copied().unwrap_or(false);
            if !ok {
                denials.push(Denial {
                    code: "dependency_not_accepted",
                    message: format!(
                        "单元 `{}` 的依赖 `{dep}` 尚未被接受：不因失败或部分完成而自动解锁",
                        unit.id
                    ),
                });
            }
        }
    }
    // ② 写能力必须可归因。
    for (unit, _) in candidates {
        let tools = tool_ids_by_unit.get(&unit.id).cloned().unwrap_or_default();
        if side_effect_scope(unit, &tools) == SideEffectScope::Unknown && candidates.len() > 1 {
            denials.push(Denial {
                code: "side_effect_scope_unknown",
                message: format!(
                    "单元 `{}` 的 side_effect_scope=unknown（shell / MCP 任意写）：无法静态归因，一律串行，不采信模型对写入范围的承诺",
                    unit.id
                ),
            });
        }
    }
    // ③ 同一批次内部也不能互相冲突（同批视为同时持有）。
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            if candidates[i].0.conflicts_with(&candidates[j].0) {
                denials.push(Denial {
                    code: "scope_overlap_in_batch",
                    message: format!(
                        "同批单元 `{}` 与 `{}` 写范围重叠：不得同时准入",
                        candidates[i].0.id, candidates[j].0.id
                    ),
                });
            }
            let shared: BTreeSet<&String> = candidates[i]
                .0
                .mutable_resources
                .iter()
                .filter(|r| candidates[j].0.mutable_resources.contains(r))
                .collect();
            if !shared.is_empty() {
                denials.push(Denial {
                    code: "shared_resource_conflict",
                    message: format!(
                        "同批单元 `{}` 与 `{}` 争用共享可变资源 {shared:?}",
                        candidates[i].0.id, candidates[j].0.id
                    ),
                });
            }
        }
    }

    let mut t = table().lock().unwrap_or_else(|p| p.into_inner());
    // generation 必须匹配（按本 root run 计）：过期的准入请求不得占用新 attempt 的租约。
    let cur_gen = *t.generation.get(root_run_id).unwrap_or(&0);
    if cur_gen != expected_generation {
        denials.push(Denial {
            code: "stale_generation",
            message: format!(
                "准入请求基于 generation {expected_generation}，本 run 当前为 {cur_gen}：过期请求不占槽"
            ),
        });
    }
    // ④ 与已持有租约的冲突。
    for (unit, _) in candidates {
        let mut own: Vec<PathBuf> = Vec::new();
        for p in &unit.write_scopes {
            let Some(k) = key(p, roots) else {
                denials.push(Denial {
                    code: "root_not_in_snapshot",
                    message: format!(
                        "单元 `{}` 引用了不在会话快照内的根 `{}`",
                        unit.id, p.root_id
                    ),
                });
                continue;
            };
            own.push(k);
        }
        if side_effect_scope(
            unit,
            tool_ids_by_unit
                .get(&unit.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ) == SideEffectScope::Unknown
        {
            own = roots
                .values()
                .map(|p| crate::tools::pathutil::canonical_best_effort(p))
                .collect();
        }
        if let Some(effect) = t.effects.values().find(|effect| {
            (effect.root_run_id != root_run_id || effect.actor_id != unit.id)
                && effect
                    .paths
                    .iter()
                    .any(|p| own.iter().any(|o| ancestors(p, o)))
        }) {
            denials.push(Denial {
                code: "parent_effect_in_flight",
                message: format!(
                    "单元 `{}` 与 `{}` 的在途副作用冲突，等待执行/后台服务清理",
                    unit.id, effect.actor_id
                ),
            });
            continue;
        }
        let holder = t.by_unit.iter().find(|((run, _), lease)| {
            (run != root_run_id || lease.unit_id != unit.id) && {
                (run == root_run_id
                    && unit
                        .mutable_resources
                        .iter()
                        .any(|r| lease.mutable_resources.contains(r)))
                    || lease
                        .held_paths
                        .iter()
                        .any(|h| own.iter().any(|o| ancestors(h, o)))
            }
        });
        if let Some((_, lease)) = holder {
            denials.push(Denial {
                code: if unit.touches_public_file() {
                    "public_file_single_owner"
                } else {
                    "path_claimed"
                },
                message: format!(
                    "单元 `{}` 的写范围已被 `{}`（generation {}）持有",
                    unit.id, lease.unit_id, lease.generation
                ),
            });
            continue;
        }
        if unit.touches_public_file()
            || t.by_unit.iter().any(|((run, _), lease)| {
                run == root_run_id
                    && lease.unit_id != unit.id
                    && lease.held_paths.iter().any(|p| {
                        super::routing::public_path(&PathRef {
                            root_id: "ws".into(),
                            relative_path: p.clone(),
                        })
                    })
            })
        {
            // 公共文件只能一个 owner：本 run 已有单元持有公共文件时拒绝。
            let public_held = t.by_unit.iter().any(|((run, _), lease)| {
                run.as_str() == root_run_id
                    && lease.unit_id != unit.id
                    && !lease.held_paths.is_empty()
            });
            if public_held {
                denials.push(Denial {
                    code: "public_file_single_owner",
                    message: format!(
                        "单元 `{}` 要写公共文件，但本 run 已有单元在写：公共文件只能一个 owner",
                        unit.id
                    ),
                });
                continue;
            }
        }
        for k in &own {
            if taken.iter().any(|t| ancestors(&t.to_string_lossy(), k)) {
                denials.push(Denial {
                    code: "path_claimed",
                    message: format!("单元 `{}` 的写范围与本批已准入单元重叠", unit.id),
                });
                break;
            }
        }
        granted.push(DispatchLease {
            root_run_id: root_run_id.to_string(),
            unit_id: unit.id.clone(),
            attempt: 0,
            exclusive: side_effect_scope(
                unit,
                tool_ids_by_unit
                    .get(&unit.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            ) == SideEffectScope::Unknown,
            mutable_resources: unit.mutable_resources.clone(),
            generation: cur_gen,
            held_paths: own
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        });
        taken.extend(own);
    }

    if !denials.is_empty() {
        return Err(denials);
    }
    // ⑤ 并发额度：**按本 root run 计**。它是单个工作流同时能跑几个单元的预算，
    // 不是进程级总量——否则一个会话的单元能把另一个会话的额度吃光。
    let live = t
        .by_unit
        .keys()
        .filter(|(r, _)| r.as_str() == root_run_id)
        .count();
    let want = granted.len();
    if live + want > MAX_CONCURRENT_UNITS {
        return Err(vec![Denial {
            code: "concurrency_limit",
            message: format!(
                "并发额度已满（{live}/{}）：本批 {want} 个单元不再准入",
                MAX_CONCURRENT_UNITS
            ),
        }]);
    }
    let lease_gen = cur_gen;
    t.generation.insert(root_run_id.to_string(), cur_gen + 1);
    let out: Vec<DispatchLease> = granted
        .into_iter()
        .map(|mut g| {
            g.generation = lease_gen;
            t.by_unit
                .insert((g.root_run_id.clone(), g.unit_id.clone()), g.clone());
            g
        })
        .collect();
    let _ = owner;
    Ok(out)
}

fn ancestors(a: &str, b: &PathBuf) -> bool {
    let pa = PathBuf::from(a);
    pa.starts_with(b) || b.starts_with(&pa)
}

/// 释放租约。
///
/// `generation` 不匹配 → **拒绝释放**：迟到完成不得释放后继 attempt 的租约。
pub fn release(root_run_id: &str, unit_id: &str, generation: u64) -> Result<(), Denial> {
    let mut t = table().lock().unwrap_or_else(|p| p.into_inner());
    match t
        .by_unit
        .get(&(root_run_id.to_string(), unit_id.to_string()))
    {
        Some(l) if l.generation == generation => {
            t.by_unit
                .remove(&(root_run_id.to_string(), unit_id.to_string()));
            Ok(())
        }
        Some(l) => Err(Denial {
            code: "generation_mismatch",
            message: format!(
                "租约 generation 为 {}，收到 {generation}：迟到完成不得释放后继 attempt 的租约",
                l.generation
            ),
        }),
        None => Err(Denial {
            code: "no_lease",
            message: format!("`{unit_id}` 当前没有持有租约（可能已被释放）"),
        }),
    }
}

/// 由 spawn 侧声明的写范围构造候选单元（`"ws:src/a.rs"` → `PathRef`）。
///
/// 格式错误**不静默丢弃**：无法解析的段会被原样留在一个叫不准的根里，
/// 准入阶段会以 `root_not_in_snapshot` 拒绝，而不是当作"没声明"。
pub fn spawn_unit(unit_id: &str, raw_scopes: &[String]) -> WorkUnit {
    let write_scopes = raw_scopes
        .iter()
        .map(|s| match s.split_once(':') {
            Some((root, rel)) => PathRef {
                root_id: root.to_string(),
                relative_path: rel.to_string(),
            },
            None => PathRef {
                root_id: String::new(),
                relative_path: s.clone(),
            },
        })
        .collect();
    WorkUnit {
        design_input: Default::default(),
        id: unit_id.to_string(),
        requirement_ids: Vec::new(),
        read_scopes: Vec::new(),
        write_scopes,
        depends_on: Vec::new(),
        mutable_resources: Vec::new(),
        role_id: None,
        model_id: None,
        checks: Vec::new(),
        stage_id: None,
    }
}

/// 本 run 的当前 generation（spawn 前读，用于 expected_generation）。
pub fn current_generation(root_run_id: &str) -> u64 {
    table()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .generation
        .get(root_run_id)
        .copied()
        .unwrap_or(0)
}

/// 记录已发放的租约（诊断用；释放仍按 `(run, unit, generation)` 校验）。
pub fn remember_lease(lease: &DispatchLease) {
    let mut t = table().lock().unwrap_or_else(|p| p.into_inner());
    t.by_unit.insert(
        (lease.root_run_id.clone(), lease.unit_id.clone()),
        lease.clone(),
    );
}

/// 当前持有情况（展示 / 诊断）。
pub fn active_leases() -> Vec<DispatchLease> {
    table()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_unit
        .values()
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;

/// Owns a dispatch claim through cancellation, provider failures and panic cleanup.
pub struct LeaseGuard(pub DispatchLease);
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let _ = release(&self.0.root_run_id, &self.0.unit_id, self.0.generation);
    }
}

/// Enforce workflow ownership at the actual file mutation boundary, including parents.
pub fn check_write(
    root_run_id: &str,
    actor_id: &str,
    path: &std::path::Path,
) -> Result<(), String> {
    let path = crate::tools::pathutil::canonical_best_effort(path);
    let t = table().lock().unwrap_or_else(|p| p.into_inner());
    let own = t
        .by_unit
        .get(&(root_run_id.to_owned(), actor_id.to_owned()));
    if let Some(own) = own
        && !own.exclusive
        && !own.held_paths.iter().any(|p| path.starts_with(p))
    {
        return Err("写路径超出工作单元已准入范围".into());
    }
    for lease in t.by_unit.values() {
        if lease.root_run_id != root_run_id || lease.unit_id == actor_id {
            continue;
        }
        if lease.exclusive || lease.held_paths.iter().any(|p| ancestors(p, &path)) {
            return Err(format!(
                "写范围由 {} 持有，等待其清理后再写入",
                lease.unit_id
            ));
        }
    }
    Ok(())
}

/// Arbitrary shell/MCP side effects cannot execute beside another live owner.
pub fn check_unattributed(root_run_id: &str, actor_id: &str) -> Result<(), String> {
    let t = table().lock().unwrap_or_else(|p| p.into_inner());
    if t.by_unit
        .values()
        .any(|l| l.root_run_id == root_run_id && l.unit_id != actor_id)
    {
        return Err("存在活跃工作单元，无法归因的 command/MCP 必须串行执行".into());
    }
    Ok(())
}

/// A real mutation/opaque request remains visible to admission until its future
/// (or background process reaper) is dropped. It uses the same lock as child
/// admission; a snapshot-only check cannot protect the check-to-execute window.
struct Effect {
    root_run_id: String,
    actor_id: String,
    opaque: bool,
    paths: Vec<String>,
}
pub struct EffectGuard(String);
impl Drop for EffectGuard {
    fn drop(&mut self) {
        table()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .effects
            .remove(&self.0);
    }
}

pub fn acquire_effect(
    root_run_id: &str,
    actor_id: &str,
    paths: &[PathBuf],
    opaque: bool,
    roots: &std::collections::BTreeMap<String, PathBuf>,
) -> Result<EffectGuard, String> {
    let paths: Vec<PathBuf> = if opaque {
        roots
            .values()
            .map(|p| crate::tools::pathutil::canonical_best_effort(p))
            .collect()
    } else {
        paths
            .iter()
            .map(|p| crate::tools::pathutil::canonical_best_effort(p))
            .collect()
    };
    let mut t = table().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(own) = t
        .by_unit
        .get(&(root_run_id.to_owned(), actor_id.to_owned()))
        && !own.exclusive
        && (opaque
            || paths
                .iter()
                .any(|p| !own.held_paths.iter().any(|h| p.starts_with(h))))
    {
        return Err("副作用超出工作单元已准入范围".into());
    }
    if let Some(lease) = t.by_unit.values().find(|lease| {
        (lease.root_run_id != root_run_id || lease.unit_id != actor_id)
            && lease
                .held_paths
                .iter()
                .any(|h| paths.iter().any(|p| ancestors(h, p)))
    }) {
        return Err(format!(
            "写范围由 {} 持有，等待其清理后再执行",
            lease.unit_id
        ));
    }
    if let Some(effect) = t.effects.values().find(|effect| {
        (effect.root_run_id != root_run_id || effect.actor_id != actor_id)
            // Existing root-to-root structured edits already serialize at the
            // shared canonical file locks. Record both pending effects so child
            // admission sees them; rejecting here would break queued edits.
            && !(actor_id == crate::core::delivery::types::ROOT_ACTOR
                && effect.actor_id == crate::core::delivery::types::ROOT_ACTOR
                && !opaque && !effect.opaque)
            && effect
                .paths
                .iter()
                .any(|h| paths.iter().any(|p| ancestors(h, p)))
    }) {
        return Err(format!("副作用与 {} 的在途执行冲突", effect.actor_id));
    }
    let id = uuid::Uuid::new_v4().to_string();
    t.effects.insert(
        id.clone(),
        Effect {
            root_run_id: root_run_id.into(),
            actor_id: actor_id.into(),
            opaque,
            paths: paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        },
    );
    Ok(EffectGuard(id))
}
