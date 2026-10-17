//! #3 任务验证策略：改动面 → 建议检查。
//!
//! 这一层回答「**改了这东西，该用什么证明它成了**」。它**生成建议**，不代替执行：
//! 执行仍经既有工具与审批（[docs/tools-and-safety](../../../../docs/tools-and-safety.md)）。
//!
//! 四条硬规则：
//!
//! 1. **命令模板来自宿主内置或经信任的项目配置**，**不从 README 任意命令直接执行**。
//!    项目自己写的 `.agents/workflows/*.md` 里的一句 `rm -rf` 不是检查模板。
//! 2. **跑不了就是没验过**。缺工具链 / 平台 / 权限 → `blocked` + 原因 `unverified`，
//!    既不新增顶层 `unverified` 状态，也不把无关检查补成通过。
//! 3. **不能互相替代**：lint 通过顶不掉行为测试，静态检查顶不掉人工看界面。
//! 4. **平台缺口单列**。没有 Windows 机器就不能标 Windows 检查通过；
//!    `required = true` 而本机跑不了 → 保持未验证。

use crate::core::delivery::types::{
    CheckExpectation, CheckKind, CheckSpec, CommandSpec, PathRef, VerificationScope,
};
use crate::core::workflow::routing::WorkUnit;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 改动面类别（宿主从**真实变更**与工作单元范围归纳，不采信模型自述）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSurface {
    /// 后端（Rust）。
    Backend,
    /// 前端（TS/TSX/CSS）。
    Frontend,
    /// 文档。
    Docs,
    /// 纯机械（精确断言，不跑整套）。
    Mechanical,
}

impl ChangeSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeSurface::Backend => "backend",
            ChangeSurface::Frontend => "frontend",
            ChangeSurface::Docs => "docs",
            ChangeSurface::Mechanical => "mechanical",
        }
    }
}

/// 从真实写范围归纳改动面。
///
/// 判据是**文件所在位置与扩展名**，不是「看起来像不像前端」：
/// 同一个包里 `.rs` 与 `.ts` 混放时，逐文件归面并集。
pub fn surfaces_of(unit: &WorkUnit) -> BTreeSet<ChangeSurface> {
    let mut out = BTreeSet::new();
    for p in &unit.write_scopes {
        out.insert(surface_of_path(p));
    }
    if out.is_empty() {
        out.insert(ChangeSurface::Mechanical);
    }
    out
}

fn surface_of_path(p: &PathRef) -> ChangeSurface {
    let rel = p.relative_path.to_ascii_lowercase();
    let head = rel.split('/').next().unwrap_or("");
    if rel.ends_with(".md") || rel.ends_with(".mdx") || head == "docs" {
        return ChangeSurface::Docs;
    }
    if rel.ends_with(".rs") || rel.ends_with(".go") || rel.ends_with(".py") {
        return ChangeSurface::Backend;
    }
    if rel.ends_with(".ts")
        || rel.ends_with(".tsx")
        || rel.ends_with(".jsx")
        || rel.ends_with(".css")
        || rel.ends_with(".vue")
    {
        return ChangeSurface::Frontend;
    }
    ChangeSurface::Mechanical
}

/// 检查模板（**宿主内置**；项目配置只能覆盖允许的字段，见 #6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckTemplate {
    /// 模板 id。
    pub id: String,
    /// 适用面。
    pub surface: ChangeSurface,
    /// 命令文本（**仅宿主内置表里的值可信**）。
    pub command_text: String,
    /// 工作目录相对项目主目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 需要人工执行 / 人工判读（如 GUI 走查）。
    #[serde(default)]
    pub manual: bool,
    /// 适用平台；空 = 全平台。
    #[serde(default)]
    pub platforms: Vec<String>,
    /// 是否必需。
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

/// 宿主内置模板表。**这是首版唯一的模板来源**。
pub fn builtin_templates() -> Vec<CheckTemplate> {
    vec![
        CheckTemplate {
            id: "backend_test".into(),
            surface: ChangeSurface::Backend,
            command_text: "cargo test --workspace".into(),
            cwd: Some("src-tauri".into()),
            manual: false,
            platforms: vec![],
            required: true,
        },
        CheckTemplate {
            id: "frontend_test".into(),
            surface: ChangeSurface::Frontend,
            command_text: "pnpm --dir ui test".into(),
            cwd: None,
            manual: false,
            platforms: vec![],
            required: true,
        },
        CheckTemplate {
            id: "frontend_build".into(),
            surface: ChangeSurface::Frontend,
            command_text: "pnpm --dir ui build".into(),
            cwd: None,
            manual: false,
            platforms: vec![],
            required: true,
        },
        CheckTemplate {
            id: "frontend_lint".into(),
            surface: ChangeSurface::Frontend,
            command_text: "pnpm --dir ui run lint".into(),
            cwd: None,
            manual: false,
            platforms: vec![],
            // lint **顶不掉** test/build：三者都在建议里，required 也都为真。
            required: true,
        },
        CheckTemplate {
            id: "docs_linkcheck".into(),
            surface: ChangeSurface::Docs,
            command_text: "人工核对 Markdown 相对链接、章节锚点和失效引用；运行 git diff --check"
                .into(),
            cwd: None,
            manual: true,
            platforms: vec![],
            required: true,
        },
        CheckTemplate {
            id: "frontend_manual_gui".into(),
            surface: ChangeSurface::Frontend,
            command_text: "人工走查界面".into(),
            cwd: None,
            // 界面改动不做自动点验：交分步手动清单，宿主不代跑也不代判。
            manual: true,
            platforms: vec![],
            required: true,
        },
        CheckTemplate {
            id: "prepr_full_gate".into(),
            surface: ChangeSurface::Backend,
            command_text: "pnpm prepr".into(),
            cwd: None,
            manual: false,
            platforms: vec![],
            // PR 完整门禁作为**单独**检查：局部 --only / --skip-soft 不算全门禁。
            required: true,
        },
    ]
}

/// 跨层改动的检查并集（去重、按模板 id 稳定排序）。
fn union_for(surfaces: &BTreeSet<ChangeSurface>) -> Vec<CheckTemplate> {
    // 顺序由 `builtin_templates()` 的声明顺序决定（稳定、可预期）；
    // 面 → 模板 id 的映射是固定的宿主表，不是模型给的。
    let order: [(&str, ChangeSurface); 7] = [
        ("backend_test", ChangeSurface::Backend),
        ("prepr_full_gate", ChangeSurface::Backend),
        ("frontend_test", ChangeSurface::Frontend),
        ("frontend_build", ChangeSurface::Frontend),
        ("frontend_lint", ChangeSurface::Frontend),
        ("frontend_manual_gui", ChangeSurface::Frontend),
        ("docs_linkcheck", ChangeSurface::Docs),
    ];
    let all = builtin_templates();
    order
        .iter()
        .filter(|(_, s)| surfaces.contains(s))
        .filter_map(|(id, _)| all.iter().find(|t| t.id == *id).cloned())
        .collect()
}

/// 工作单元 → 建议检查（`CheckSpec`，可直接进 #54 契约）。
pub fn suggest_checks(unit: &WorkUnit) -> Vec<CheckSpec> {
    let surfaces = surfaces_of(unit);
    // 纯机械改动复用精确断言：不为一次改名跑整套后端测试。
    if surfaces.len() == 1 && surfaces.contains(&ChangeSurface::Mechanical) {
        return vec![CheckSpec {
            id: "mechanical_exact".into(),
            cwd: None,
            independent_verifier: false,
            kind: CheckKind::Manual,
            command: None,
            scope: VerificationScope {
                roots: Vec::new(),
                inputs: unit.write_scopes.clone(),
                outputs: unit.write_scopes.clone(),
                dependency_files: Vec::new(),
                environment_keys: Vec::new(),
            },
            required: true,
            expectation: CheckExpectation::Manual {
                prompt: Some("按需求逐项核对精确变更结果；未知扩展名不视为机械改动已通过".into()),
            },
        }];
    }
    union_for(&surfaces)
        .into_iter()
        .map(|t| CheckSpec {
            id: t.id.clone(),
            cwd: if t.manual {
                None
            } else {
                Some(PathRef {
                    root_id: unit
                        .write_scopes
                        .first()
                        .map(|p| p.root_id.clone())
                        .unwrap_or_else(|| "workspace".into()),
                    relative_path: t.cwd.clone().unwrap_or_else(|| ".".into()),
                })
            },
            independent_verifier: !t.manual,
            kind: if t.manual {
                CheckKind::Manual
            } else {
                CheckKind::Command
            },
            command: if t.manual {
                None
            } else {
                Some(CommandSpec::Requested {
                    command_text: t.command_text.clone(),
                    shell_hint: None,
                })
            },
            scope: VerificationScope {
                roots: Vec::new(),
                inputs: unit.write_scopes.clone(),
                outputs: Vec::new(),
                dependency_files: lock_files(&surfaces)
                    .into_iter()
                    .map(|mut p| {
                        p.root_id = unit
                            .write_scopes
                            .first()
                            .map(|w| w.root_id.clone())
                            .unwrap_or_else(|| "workspace".into());
                        p
                    })
                    .collect(),
                environment_keys: Vec::new(),
            },
            required: t.required,
            expectation: if t.manual {
                CheckExpectation::Manual {
                    prompt: Some(t.command_text.clone()),
                }
            } else {
                CheckExpectation::Command {
                    allowed_exit_codes: vec![0],
                }
            },
        })
        .collect()
}

/// 锁文件作为依赖纳入范围：锁文件一变，旧检查结论即失效（#15）。
fn lock_files(surfaces: &BTreeSet<ChangeSurface>) -> Vec<PathRef> {
    let mut out = Vec::new();
    if surfaces.contains(&ChangeSurface::Backend) {
        out.push(PathRef {
            root_id: "ws".into(),
            relative_path: "src-tauri/Cargo.lock".into(),
        });
    }
    if surfaces.contains(&ChangeSurface::Frontend) {
        out.push(PathRef {
            root_id: "ws".into(),
            relative_path: "pnpm-lock.yaml".into(),
        });
    }
    out
}

/// 本机能否运行某检查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runnability {
    /// 可运行。
    Runnable,
    /// 跑不了（`reason` 是稳定码：`unverified`）。
    Blocked {
        reason: &'static str,
        detail: String,
    },
}

/// 判定可运行性：平台 + 工具链 + 权限。
pub fn runnability(
    template: &CheckTemplate,
    platform: &str,
    has_toolchain: bool,
    has_approval: bool,
) -> Runnability {
    if !template.platforms.is_empty() && !template.platforms.iter().any(|p| p == platform) {
        return Runnability::Blocked {
            reason: "unverified",
            detail: format!(
                "检查 `{}` 只在 {:?} 上有意义，本机是 {platform}：保持未验证，不按通过处理",
                template.id, template.platforms
            ),
        };
    }
    if !has_toolchain {
        return Runnability::Blocked {
            reason: "unverified",
            detail: format!("检查 `{}` 所需工具链缺失：保持未验证", template.id),
        };
    }
    if !template.manual && !has_approval {
        return Runnability::Blocked {
            reason: "unverified",
            detail: format!(
                "检查 `{}` 需要执行审批，当前未获批：保持未验证",
                template.id
            ),
        };
    }
    Runnability::Runnable
}

/// 建议清单的机器可读摘要（进工具 out 的 **data**，不是 warnings）。
pub fn suggestion_summary(checks: &[CheckSpec]) -> serde_json::Value {
    serde_json::json!({
        "checks": checks.iter().map(|c| serde_json::json!({
            "id": c.id,
            "kind": c.kind,
            "required": c.required,
            "command": c.command.as_ref().map(|_| true).unwrap_or(false),
            "inputs": c.scope.inputs.iter().map(|p| p.relative_path.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "note": "检查建议由宿主按真实改动面生成；执行仍需既有工具与审批",
    })
}

#[cfg(test)]
#[path = "verification_policy_tests.rs"]
mod tests;
