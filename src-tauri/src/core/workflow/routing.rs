//! #1 任务分类与工作单元所有权路由。
//!
//! 这一层回答「**这件事该怎么拆、谁能动哪些文件**」。它不生产 todo，也不自己定检查——
//! 输出是 [`TaskProfile`] 与 [`WorkUnit`]，作为 #54 `TaskContract` 的**派发输入**。
//!
//! 三条不可让步的规则（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：
//!
//! 1. **模型建议、宿主核验**。分类由模型提议，但 `kind` 由宿主按结构化图与范围判定；
//!    模型自称 `parallel_surfaces` 不能让它并行。
//! 2. **无法证明独立 → 默认串行**。证明不了不重叠，就按重叠处理。
//! 3. **规模不是升级理由**。仓库大、文件多、一次失败本身都**不构成**升级完整流程的条件；
//!    否则每个大仓库里的普通问答都会被塞进工作流。

use crate::core::delivery::types::{PathRef, RunIdentity, TaskContract};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 任务形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskKind {
    /// 问答：不产生写包，也不派发写单元。
    Conversation,
    /// 机械改动：单一明确步骤，无设计分叉（改名 / 格式化 / 版本号）。
    Mechanical,
    /// 单面改动：一个 owner 独占全部写范围。
    SingleSurface,
    /// 多面但有真实依赖：串行。
    #[default]
    SerialSurfaces,
    /// 多面且已证明独立：并行（**仍须经 #2 准入**）。
    ParallelSurfaces,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Conversation => "conversation",
            TaskKind::Mechanical => "mechanical",
            TaskKind::SingleSurface => "single_surface",
            TaskKind::SerialSurfaces => "serial_surfaces",
            TaskKind::ParallelSurfaces => "parallel_surfaces",
        }
    }

    /// 是否会产生写单元。`conversation` 不产生写包。
    pub fn writes(self) -> bool {
        !matches!(self, TaskKind::Conversation)
    }

    /// 是否可能并行。**声明并行不等于获准并行**——见 [`RouteDecision::allows_parallel`]。
    pub fn may_parallel(self) -> bool {
        matches!(self, TaskKind::ParallelSurfaces)
    }
}

/// 一个工作面（一组同属主的文件范围）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    /// 稳定 id。
    pub id: String,
    /// 写范围（`root_id + 相对路径`，**不用自然语言**）。
    #[serde(default)]
    pub write_scopes: Vec<PathRef>,
    /// 只读范围。
    #[serde(default)]
    pub read_scopes: Vec<PathRef>,
}

impl Surface {
    /// 写范围是否与 `other` 重叠（同路径、祖先/子孙，或共享锁文件语义路径）。
    ///
    /// 组件级 `starts_with` 蕴含相等：兄弟面认领目录时其下文件同样算重叠。
    pub fn conflicts_with(&self, other: &Surface) -> bool {
        self.write_scopes
            .iter()
            .any(|a| other.write_scopes.iter().any(|b| path_overlap(a, b)))
    }
}

/// 两个路径引用是否重叠（必须同根；相对路径按组件前缀判定）。
pub fn path_overlap(a: &PathRef, b: &PathRef) -> bool {
    if a.root_id != b.root_id {
        return false;
    }
    let pa = segments(&a.relative_path);
    let pb = segments(&b.relative_path);
    let n = pa.len().min(pb.len());
    pa[..n] == pb[..n]
}

fn segments(p: &str) -> Vec<&str> {
    p.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// 工作单元：派发的最小粒度。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkUnit {
    #[serde(default)]
    pub design_input: super::design::DesignInput,
    /// 稳定 id。
    pub id: String,
    /// 覆盖的需求（→ #54 契约）。
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    /// 读范围。
    #[serde(default)]
    pub read_scopes: Vec<PathRef>,
    /// 写范围。
    #[serde(default)]
    pub write_scopes: Vec<PathRef>,
    /// 依赖的其它单元 id（**依赖未通过不会自动变 ready**，见 #2）。
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 会变的共享可变资源（数据库 / 端口 / 锁文件）。
    #[serde(default)]
    pub mutable_resources: Vec<String>,
    /// 角色 id（#9 职责分离）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    /// 模型 id（spawn 快照）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// 绑定的检查（→ #3 策略生成的 CheckSpec id）。
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub stage_id: Option<String>,
}

impl WorkUnit {
    /// 写范围是否与 `other` 重叠。
    pub fn conflicts_with(&self, other: &WorkUnit) -> bool {
        self.write_scopes
            .iter()
            .any(|a| other.write_scopes.iter().any(|b| path_overlap(a, b)))
    }

    /// 是否声明了 shell / MCP 任意写能力。
    ///
    /// 首版里只有**可归因的结构化写工具**能并行：模型承诺「shell 只写某文件」不予采信，
    /// 写能力无法静态归因的单元一律标 `side_effect_scope = unknown` 并串行（见 #2）。
    pub fn has_unattributable_writes(&self, tool_ids: &[String]) -> bool {
        tool_ids.iter().any(|t| {
            let t = t.to_ascii_lowercase();
            t == "command"
                || t == "service"
                || t == "http_request"
                || t == "bash"
                || t == "shell"
                || t.starts_with("mcp:")
                || t.starts_with("mcp_")
        })
    }

    /// 写范围是否触到公共文件（锁文件 / 公共类型 / 导出入口）。
    ///
    /// 公共文件**只能有一个 owner**：任何并行方案里第二个要写它都必须串行。
    pub fn touches_public_file(&self) -> bool {
        self.write_scopes.iter().any(public_path)
    }
}

/// 公共文件判定（相对路径 basename / 末段）。
pub fn public_path(p: &PathRef) -> bool {
    const PUBLIC: &[&str] = &[
        "cargo.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "package.json",
        "cargo.toml",
        "tsconfig.json",
        "go.mod",
        "go.sum",
        "types.rs",
        "types.ts",
        "lib.rs",
        "mod.rs",
        "index.ts",
    ];
    let last = segments(&p.relative_path).last().copied().unwrap_or("");
    last.is_empty() || PUBLIC.contains(&last.to_ascii_lowercase().as_str())
}

/// 任务分类（宿主核验后的结论）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskProfile {
    /// 分类。
    pub kind: TaskKind,
    /// 工作面。
    #[serde(default)]
    pub surfaces: Vec<Surface>,
    /// 各面属主（surface id → owner actor）。
    #[serde(default)]
    pub owners: BTreeMap<String, String>,
    /// 单元间依赖（unit id → 依赖的 unit id）。
    #[serde(default)]
    pub depends_on: BTreeMap<String, Vec<String>>,
    /// 共享外部资源。
    #[serde(default)]
    pub shared_resources: Vec<String>,
    /// **为什么**是这个分类（逐条可核对，不是「模型说的」）。
    #[serde(default)]
    pub reasons: Vec<String>,
    /// 用户覆盖记录（真实交互才写，模型不能自称）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_record: Option<ProfileOverride>,
}

/// 用户覆盖：必须带真实 actor 与理由。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileOverride {
    /// 覆盖后的分类。
    pub kind: TaskKind,
    /// 真实交互的用户 actor。
    pub actor_id: String,
    /// 覆盖时的契约修订。
    pub contract_revision: u64,
    /// 理由（展示与审计）。
    pub reason: String,
    /// 时刻。
    pub at: String,
}

/// 宿主核验结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecision {
    /// 核验后的分类。
    pub profile: TaskProfile,
    /// 是否允许并行。**`profile.kind` 是并行不等于这里为真**。
    pub allows_parallel: bool,
    /// 降级原因（`None` = 未降级）。
    pub downgraded_because: Option<String>,
}

impl RouteDecision {
    pub fn allows_parallel(&self) -> bool {
        self.allows_parallel
    }
}

/// 分类输入：模型建议 + 宿主掌握的真实范围。
#[derive(Debug, Clone, Default)]
pub struct RouteInput {
    /// 模型建议的分类（**仅是建议**，宿主可否决）。
    pub suggested: Option<TaskKind>,
    /// 用户第一句诉求（用于识别问答）。
    pub user_text: String,
    /// 计划写到的文件（相对路径 + 根）。
    pub planned_writes: Vec<PathRef>,
    /// 计划读到的文件。
    pub planned_reads: Vec<PathRef>,
    /// 各写范围预期的属主（surface id → actor）。
    pub owner_hints: BTreeMap<String, String>,
    /// 单元间依赖（unit id → 依赖）。
    pub unit_depends_on: BTreeMap<String, Vec<String>>,
    /// 共享外部资源。
    pub shared_resources: Vec<String>,
}

/// 宿主核验：把模型建议收敛成有依据的 [`TaskProfile`]。
///
/// 降级方向只有一个：**更保守**。模型建议并行而宿主证明不了独立 → 串行；
/// 建议串行而确实单面 → 可以收敛到 `single_surface`（省掉不必要的流程）。
pub fn route(input: &RouteInput) -> RouteDecision {
    let mut reasons: Vec<String> = Vec::new();
    // ① 没有任何写范围 → 问答，不产生写包。
    if input.planned_writes.is_empty() {
        reasons.push("无写范围：按问答处理，不产生写工作单元".into());
        return RouteDecision {
            profile: TaskProfile {
                kind: TaskKind::Conversation,
                surfaces: Vec::new(),
                owners: BTreeMap::new(),
                depends_on: BTreeMap::new(),
                shared_resources: input.shared_resources.clone(),
                reasons,
                override_record: None,
            },
            allows_parallel: false,
            downgraded_because: None,
        };
    }

    // ② 机械改动：单一明确步骤、无设计分叉、无读依赖。
    //    判据是**形状**（一个写范围 + 不读别的），不是「简单」这种形容词。
    if input.suggested == Some(TaskKind::Mechanical)
        && input.planned_writes.len() == 1
        && input.planned_reads.is_empty()
    {
        reasons.push(format!(
            "单写范围 `{}` 且无读依赖：机械改动，无需设计分叉",
            input.planned_writes[0].relative_path
        ));
        return RouteDecision {
            profile: TaskProfile {
                kind: TaskKind::Mechanical,
                surfaces: vec![Surface {
                    id: "s1".into(),
                    write_scopes: input.planned_writes.clone(),
                    read_scopes: Vec::new(),
                }],
                owners: BTreeMap::from([("s1".into(), String::new())]),
                depends_on: BTreeMap::new(),
                shared_resources: input.shared_resources.clone(),
                reasons,
                override_record: None,
            },
            allows_parallel: false,
            downgraded_because: None,
        };
    }

    // ③ 能否证明各面独立：无重叠写范围、**不在同一顶层组件**、无共享可变资源、无真实依赖。
    //
    //    「路径不相交」**不等于**「语义独立」：同一模块里的两个文件可以互相引用类型、
    //    共享私有 helper，两个前端文件也可能都经由同一个 barrel 导出。只有落到
    //    不同顶层组件（`src-tauri/` 与 `ui/`）才谈得上证明得了独立——否则按串行处理。
    let surfaces = build_surfaces(input);
    let overlap = overlapping_pairs(&surfaces);
    let same_component = same_top_component_pairs(&surfaces);
    let read_write_dependency = surfaces.iter().enumerate().any(|(i, a)| {
        surfaces.iter().skip(i + 1).any(|b| {
            a.write_scopes
                .iter()
                .any(|w| b.read_scopes.iter().any(|r| path_overlap(w, r)))
                || b.write_scopes
                    .iter()
                    .any(|w| a.read_scopes.iter().any(|r| path_overlap(w, r)))
        })
    });
    let public_touched = input.planned_writes.iter().any(public_path) && surfaces.len() > 1;
    let depends = !input.unit_depends_on.is_empty();
    let independent = overlap.is_empty()
        && same_component.is_empty()
        && !depends
        && !public_touched
        && !read_write_dependency
        && input.shared_resources.is_empty();

    if surfaces.len() <= 1 {
        reasons.push("只有一个工作面：单面改动，由一个 owner 独占写范围".into());
        return RouteDecision {
            profile: TaskProfile {
                kind: TaskKind::SingleSurface,
                surfaces,
                owners: input.owner_hints.clone(),
                depends_on: BTreeMap::new(),
                shared_resources: input.shared_resources.clone(),
                reasons,
                override_record: None,
            },
            allows_parallel: false,
            downgraded_because: None,
        };
    }

    if independent {
        reasons.push(format!(
            "共 {} 个工作面，分别落在不同顶层组件、写范围互不重叠、无共享可变资源、无真实依赖：可并行（仍须经并行准入）",
            surfaces.len()
        ));
        return RouteDecision {
            profile: TaskProfile {
                kind: TaskKind::ParallelSurfaces,
                surfaces,
                owners: input.owner_hints.clone(),
                depends_on: BTreeMap::new(),
                shared_resources: input.shared_resources.clone(),
                reasons,
                override_record: None,
            },
            // **声明并行 ≠ 获准并行**：还要过 #2 的原子准入。
            allows_parallel: true,
            downgraded_because: None,
        };
    }

    // ④ 证明不了独立（或确实重叠 / 有依赖）→ 串行。
    let mut because = Vec::new();
    if !overlap.is_empty() {
        reasons.push(format!("写范围重叠：{overlap:?}"));
        because.push("写范围重叠".to_string());
    }
    if !same_component.is_empty() {
        reasons.push(format!(
            "多个面落在同一顶层组件 {same_component:?}：同组件内路径不相交也证明不了语义独立"
        ));
        because.push("同一顶层组件内改动".to_string());
    }
    if public_touched {
        reasons.push(
            "多个面都要动公共文件（锁文件 / 公共类型 / 导出入口）：公共文件只能一个 owner".into(),
        );
        because.push("公共文件多属主".into());
    }
    if depends {
        reasons.push(format!("存在单元间依赖：{:?}", input.unit_depends_on));
        because.push("存在真实依赖".into());
    }
    if !input.shared_resources.is_empty() {
        reasons.push(format!("涉及共享外部资源 {:?}", input.shared_resources));
        because.push("共享外部资源状态未知".into());
    }
    let downgraded = input.suggested == Some(TaskKind::ParallelSurfaces);
    if downgraded {
        reasons.push("模型建议并行，但宿主无法证明独立：按串行处理".into());
    }
    RouteDecision {
        profile: TaskProfile {
            kind: TaskKind::SerialSurfaces,
            surfaces,
            owners: input.owner_hints.clone(),
            depends_on: input.unit_depends_on.clone(),
            shared_resources: input.shared_resources.clone(),
            reasons,
            override_record: None,
        },
        allows_parallel: false,
        downgraded_because: Some(because.join("；")),
    }
}

/// 顶层组件标识：路径第一段。`src/a.rs` 与 `ui/b.tsx` 分属不同组件；
/// `src/a.rs` 与 `src/b.rs` 同属 `src`。
fn top_component(p: &PathRef) -> &str {
    segments(&p.relative_path).first().copied().unwrap_or("")
}

/// 落在同一顶层组件的面对（"同组件 = 语义可能耦合"）。
fn same_top_component_pairs(surfaces: &[Surface]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in surfaces.iter().enumerate() {
        for b in surfaces.iter().skip(i + 1) {
            if a.write_scopes.iter().any(|x| {
                b.write_scopes
                    .iter()
                    .any(|y| x.root_id == y.root_id && top_component(x) == top_component(y))
            }) {
                out.push((a.id.clone(), b.id.clone()));
            }
        }
    }
    out
}

/// 重叠的面对（展示用）。
fn overlapping_pairs(surfaces: &[Surface]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in surfaces.iter().enumerate() {
        for b in surfaces.iter().skip(i + 1) {
            if a.conflicts_with(b) {
                out.push((a.id.clone(), b.id.clone()));
            }
        }
    }
    out
}

/// 按写入路径聚成工作面：互不重叠的路径各自成面（隐含并行候选），
/// 重叠的路径并入同一面（同一属主）。
fn build_surfaces(input: &RouteInput) -> Vec<Surface> {
    let mut surfaces: Vec<Surface> = Vec::new();
    for p in &input.planned_writes {
        match surfaces
            .iter_mut()
            .find(|s| s.write_scopes.iter().any(|a| path_overlap(a, p)))
        {
            Some(s) => s.write_scopes.push(p.clone()),
            None => surfaces.push(Surface {
                id: format!("s{}", surfaces.len() + 1),
                write_scopes: vec![p.clone()],
                read_scopes: Vec::new(),
            }),
        }
    }
    for s in surfaces.iter_mut() {
        s.read_scopes = input
            .planned_reads
            .iter()
            .filter(|r| !s.write_scopes.iter().any(|w| path_overlap(w, r)))
            .cloned()
            .collect();
    }
    surfaces
}

/// 应用用户覆盖。
///
/// 覆盖**只改分类与理由**，不解除任何冲突判定：`allows_parallel` 仍要重新算，
/// 覆盖成并行而实际重叠照样串行（否则「改个分类就能并发写同一文件」成了后门）。
pub fn apply_override(
    decision: &mut RouteDecision,
    over: ProfileOverride,
    contract: &TaskContract,
    owner: &RunIdentity,
) -> Result<(), String> {
    if over.contract_revision != contract.revision {
        return Err(format!(
            "覆盖基于契约修订 {}，当前为 {}：过期修订一律拒绝",
            over.contract_revision, contract.revision
        ));
    }
    if over.actor_id.trim().is_empty() {
        return Err("覆盖必须记录真实用户 actor".into());
    }
    if over.reason.trim().is_empty() {
        return Err("覆盖必须写明理由".into());
    }
    let _ = owner;
    let previous = decision.profile.kind.as_str();
    let wanted = over.kind;
    decision.profile.kind = wanted;
    decision.profile.reasons.push(format!(
        "用户覆盖：{previous} → {}（理由：{}）",
        wanted.as_str(),
        over.reason
    ));
    decision.profile.override_record = Some(over);
    // 覆盖不能凭空制造并行：并行仍取决于 #2 准入，这里只放开「声明」层面。
    decision.allows_parallel = wanted.may_parallel() && decision.allows_parallel;
    if wanted.may_parallel() && !decision.allows_parallel {
        decision.downgraded_because =
            Some("用户要求并行，但写范围确实重叠：冲突判定不因覆盖解除".into());
    }
    Ok(())
}

/// 工作面 → 工作单元（一个属主一个单元；`conversation` 无单元）。
pub fn to_work_units(
    profile: &TaskProfile,
    owner: &RunIdentity,
    requirement_ids: &[String],
) -> Vec<WorkUnit> {
    if !profile.kind.writes() {
        return Vec::new();
    }
    let _ = owner;
    profile
        .surfaces
        .iter()
        .map(|s| WorkUnit {
            design_input: Default::default(),
            id: format!("unit_{}", s.id),
            requirement_ids: requirement_ids.to_vec(),
            read_scopes: s.read_scopes.clone(),
            write_scopes: s.write_scopes.clone(),
            depends_on: profile.depends_on.get(&s.id).cloned().unwrap_or_default(),
            mutable_resources: profile.shared_resources.clone(),
            role_id: profile.owners.get(&s.id).cloned(),
            model_id: None,
            checks: Vec::new(),
            stage_id: None,
        })
        .collect()
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
