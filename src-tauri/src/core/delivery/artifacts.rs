//! 交付声明与实际改动对应（任务 #12 拥有）。
//!
//! 「我改了什么」不能由模型自述：[`validate_claims`] 把每条声明拿回真实文件系统核对——
//! 文件在不在、是不是目录冒充文件、有没有越出授权根、内容哈希是不是当前值、是不是本轮真改动。
//!
//! 三条边界：
//!
//! 1. **模型给的 diff 只是建议**，不算证明；能算的只有当前真实内容与基线内容的差异。
//! 2. **删除声明反向验证**：它要证明的是「基线存在且当前不存在」，不是「文件还在」——
//!    照"存在即满足"写会把删除声明永远判失败，照"仍存在即满足"写会放行没删的文件。
//! 3. **越界先拒再报错**：路径可读但不在授权根内 → 拒绝且脱敏，不因为"读到了"就认可。

use super::types::{
    ArtifactConstraint, ArtifactKind, ArtifactRequirement, ChangeKind, ChangeRef, DeliveryResult,
    PathRef, Reason, TaskContract,
};
use super::version::RootResolver;
use std::path::Path;

/// 一条声明的核对结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimOutcome {
    /// 是否成立。
    pub satisfied: bool,
    /// 可定位原因（不成立时必填）。
    pub reason: Option<Reason>,
}

/// 逐条核对的汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimReport {
    /// 已成立的 id。
    pub satisfied: Vec<String>,
    /// 未成立的 id → 原因。
    pub unsatisfied: Vec<(String, Reason)>,
}

impl ClaimReport {
    /// 是否全部成立。
    pub fn all_satisfied(&self) -> bool {
        self.unsatisfied.is_empty()
    }
}

fn reason(code: &str, message: impl Into<String>, target: Option<&str>) -> Reason {
    Reason {
        code: code.to_string(),
        message: message.into(),
        target_id: target.map(str::to_string),
    }
}

/// 解析 `PathRef` 为真实绝对路径。**根必须已在快照内**——模型自带的 `root_id` 若不在解析器里，
/// 直接拒绝（它可能指向未授权目录）。
fn resolve(roots: &RootResolver, p: &PathRef) -> Result<std::path::PathBuf, Reason> {
    let Some(root) = roots.get(&p.root_id) else {
        return Err(reason(
            "E_EVIDENCE_INVALID",
            format!("未知根 `{}`，拒绝解析该路径", p.root_id),
            None,
        ));
    };
    crate::tools::pathutil::safe_join(root, &p.relative_path).map_err(|(code, msg)| {
        reason(
            "E_EVIDENCE_INVALID",
            format!("路径非法（{code}）：{msg}"),
            None,
        )
    })
}

/// 产物的实际观测事实（由宿主读取，不接受报告自带）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactObservation {
    /// 是否存在。
    pub exists: bool,
    /// 是否是目录。
    pub is_dir: bool,
    /// 是否是符号链接。
    pub is_symlink: bool,
    /// 字节数。
    pub size: Option<u64>,
    /// 内容摘要（目录 / 读不到时为 `None`）。
    pub sha256: Option<String>,
    /// 可读。
    pub readable: bool,
    /// 媒体类型（按扩展名启发式）。
    pub media_type: Option<String>,
}

/// 观测一个产物的真实状态。读不到 / 越界都返回 `readable=false` 而不是假数据。
pub fn observe_artifact(
    roots: &RootResolver,
    p: &PathRef,
) -> (ArtifactObservation, Option<Reason>) {
    let Some(root) = roots.get(&p.root_id) else {
        return (
            ArtifactObservation {
                exists: false,
                is_dir: false,
                is_symlink: false,
                size: None,
                sha256: None,
                readable: false,
                media_type: None,
            },
            Some(reason(
                "E_EVIDENCE_INVALID",
                format!("未知根 `{}`，拒绝解析该路径", p.root_id),
                None,
            )),
        );
    };
    let abs = match resolve(roots, p) {
        Ok(a) => a,
        Err(r) => {
            return (
                ArtifactObservation {
                    exists: false,
                    is_dir: false,
                    is_symlink: false,
                    size: None,
                    sha256: None,
                    readable: false,
                    media_type: None,
                },
                Some(r),
            );
        }
    };
    let Ok(meta) = std::fs::symlink_metadata(&abs) else {
        return (
            ArtifactObservation {
                exists: false,
                is_dir: false,
                is_symlink: false,
                size: None,
                sha256: None,
                readable: true,
                media_type: None,
            },
            None,
        );
    };
    let mut obs = ArtifactObservation {
        exists: true,
        is_dir: meta.is_dir(),
        is_symlink: meta.is_symlink(),
        size: Some(meta.len()),
        sha256: None,
        readable: true,
        media_type: guess_media_type(&p.relative_path),
    };
    if obs.is_symlink {
        // 符号链接只认「目标仍在声明根内」：**越界链接可读也不算数**——
        // 不加这道检查，`..`/外部绝对路径链接能把根外内容登记成"本任务产物"。
        let inside = match (std::fs::canonicalize(&abs), std::fs::canonicalize(root)) {
            (Ok(target), Ok(root)) => target.starts_with(&root),
            _ => false,
        };
        if !inside {
            return (
                obs,
                Some(reason(
                    "artifact_out_of_scope",
                    "产物符号链接指向声明根之外，拒绝采纳",
                    Some(&p.relative_path),
                )),
            );
        }
        obs.size = None;
        return (obs, None);
    }
    if obs.is_dir {
        obs.size = None;
        return (obs, None);
    }
    match std::fs::read(&abs) {
        Ok(bytes) => {
            obs.sha256 = Some(crate::core::delivery::version::digest_of_bytes(&bytes));
            obs.size = Some(bytes.len() as u64);
        }
        Err(_) => {
            obs.readable = false;
            obs.sha256 = None;
        }
    }
    (obs, None)
}

/// 核对单个产物要求是否成立。**目录冒充文件**在这里被拒（`kind` 与实际类型不符即不成立）。
/// Physical facts alone never discharge a structured constraint; only the acceptance ledger can.
pub(super) fn validate_artifact_physical(
    roots: &RootResolver,
    req: &ArtifactRequirement,
) -> ClaimOutcome {
    let mut physical = req.clone();
    physical
        .constraints
        .retain(|constraint| !matches!(constraint, ArtifactConstraint::StructuredCheck { .. }));
    validate_artifact(roots, &physical)
}

pub fn validate_artifact(roots: &RootResolver, req: &ArtifactRequirement) -> ClaimOutcome {
    let (obs, err) = observe_artifact(roots, &req.path);
    if let Some(r) = err {
        return ClaimOutcome {
            satisfied: false,
            reason: Some(r),
        };
    }
    let id = req.id.as_str();
    if !obs.exists {
        // 显式缺失：不成立（除非该产物已被用户取消——由调用方过滤）。
        return ClaimOutcome {
            satisfied: false,
            reason: Some(reason("artifact_missing", "产物不存在", Some(id))),
        };
    }
    if !obs.readable {
        return ClaimOutcome {
            satisfied: false,
            reason: Some(reason(
                "artifact_unreadable",
                format!(
                    "产物无法读取（{:?}）",
                    if obs.is_symlink { "symlink" } else { "file" }
                ),
                Some(id),
            )),
        };
    }
    // 类别一致性：声明为文件的目录、或声明为目录的文件，都判不成立。
    match req.kind {
        ArtifactKind::Directory => {
            if !obs.is_dir {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "artifact_kind_mismatch",
                        "声明为目录，实际不是目录",
                        Some(id),
                    )),
                };
            }
        }
        ArtifactKind::Text
        | ArtifactKind::Binary
        | ArtifactKind::Document
        | ArtifactKind::Other => {
            if obs.is_dir {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "artifact_kind_mismatch",
                        "声明为文件，实际是目录",
                        Some(id),
                    )),
                };
            }
        }
    }
    for c in &req.constraints {
        if let Some(r) = check_constraint(c, &obs, id) {
            return ClaimOutcome {
                satisfied: false,
                reason: Some(r),
            };
        }
    }
    ClaimOutcome {
        satisfied: true,
        reason: None,
    }
}

/// 单条内容约束的核对。
fn check_constraint(c: &ArtifactConstraint, obs: &ArtifactObservation, id: &str) -> Option<Reason> {
    match c {
        // `Exists` 已由上面的存在性判定覆盖。
        ArtifactConstraint::Exists => None,
        ArtifactConstraint::MediaType { media_type } => {
            if obs.media_type.as_deref() == Some(media_type.as_str()) {
                None
            } else {
                Some(reason(
                    "artifact_media_type",
                    format!(
                        "媒体类型不符：期望 {media_type}，实际 {}",
                        obs.media_type.as_deref().unwrap_or("未知")
                    ),
                    Some(id),
                ))
            }
        }
        ArtifactConstraint::MinBytes { bytes } => {
            let size = obs.size.unwrap_or(0);
            if size >= *bytes {
                None
            } else {
                Some(reason(
                    "artifact_too_small",
                    format!("字节数 {size} 小于下限 {bytes}"),
                    Some(id),
                ))
            }
        }
        ArtifactConstraint::MaxBytes { bytes } => {
            let size = obs.size.unwrap_or(0);
            if size <= *bytes {
                None
            } else {
                Some(reason(
                    "artifact_too_large",
                    format!("字节数 {size} 超过上限 {bytes}"),
                    Some(id),
                ))
            }
        }
        // 结构化检查由注册校验器判定（[`super::acceptance`] 在求值阶段执行），
        // 这里不冒充已通过——留待校验器真实跑过。
        ArtifactConstraint::StructuredCheck {
            validator_id,
            validator_version,
        } => Some(reason(
            "artifact_check_pending",
            format!(
                "结构化校验 `{validator_id}`{} 尚未执行",
                validator_version
                    .as_ref()
                    .map(|v| format!("@{v}"))
                    .unwrap_or_default()
            ),
            Some(id),
        )),
    }
}

/// 核对一条变更声明是否与当前真实状态一致。
///
/// 删除声明要求「`before_hash` 有值且当前确实不存在」；创建/修改声明要求「当前存在且
/// 哈希等于声明值」。**真实未修改的输入不能冒充本轮变更**——`before_hash == after_hash`
/// 且两者都等于当前内容时判为"无实际改动"。
pub fn validate_change(roots: &RootResolver, change: &ChangeRef) -> ClaimOutcome {
    let (obs, err) = observe_artifact(roots, &change.path);
    if let Some(r) = err {
        return ClaimOutcome {
            satisfied: false,
            reason: Some(r),
        };
    }
    if change.kind != ChangeKind::Deleted && change.after_hash.is_none() {
        return ClaimOutcome {
            satisfied: false,
            reason: Some(reason(
                "change_missing_current_hash",
                "变更声明缺少当前内容摘要",
                None,
            )),
        };
    }
    if change.kind == ChangeKind::Modified && change.before_hash.is_none() {
        return ClaimOutcome {
            satisfied: false,
            reason: Some(reason(
                "change_missing_baseline",
                "修改声明缺少基线摘要",
                None,
            )),
        };
    }
    match change.kind {
        ChangeKind::Deleted => {
            // 反向验证：基线存在 + 当前不存在，两条都要成立。
            if change.before_hash.is_none() {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "change_missing_baseline",
                        "删除声明缺少基线哈希，无法证明它曾存在",
                        None,
                    )),
                };
            }
            if obs.exists {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "change_still_present",
                        "声明已删除，但该路径当前仍存在",
                        None,
                    )),
                };
            }
            ClaimOutcome {
                satisfied: true,
                reason: None,
            }
        }
        ChangeKind::Created | ChangeKind::Modified => {
            if !obs.exists {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason("change_absent", "声明的改动目标当前不存在", None)),
                };
            }
            if obs.is_dir {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "change_is_directory",
                        "改动声明指向目录，目录变更需拆到具体条目",
                        None,
                    )),
                };
            }
            if let Some(expect) = &change.after_hash
                && obs.sha256.as_deref() != Some(expect.as_str())
            {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "change_hash_mismatch",
                        "当前内容摘要与声明不一致（声明可能基于旧版本）",
                        None,
                    )),
                };
            }
            // 未修改的输入不得冒充本轮变更。
            if let (Some(before), Some(after)) = (&change.before_hash, &change.after_hash)
                && before == after
                && change.kind == ChangeKind::Modified
            {
                return ClaimOutcome {
                    satisfied: false,
                    reason: Some(reason(
                        "change_not_modified",
                        "声明为修改，但前后摘要相同（真实未改动）",
                        None,
                    )),
                };
            }
            ClaimOutcome {
                satisfied: true,
                reason: None,
            }
        }
    }
}

/// 核对一份交付结果的全部声明（产物 + 变更）。
///
/// 已按用户范围变更取消的条目**跳过**（取消只减少当前有效集，原始记录保留在契约里）。
pub fn validate_claims(
    contract: &TaskContract,
    delivery: &DeliveryResult,
    roots: &RootResolver,
) -> ClaimReport {
    let mut report = ClaimReport {
        satisfied: Vec::new(),
        unsatisfied: Vec::new(),
    };
    for art in &contract.artifacts {
        if contract.is_cancelled(&art.id) {
            continue;
        }
        let outcome = validate_artifact(roots, art);
        match outcome.reason {
            Some(r) => report.unsatisfied.push((art.id.clone(), r)),
            None => report.satisfied.push(art.id.clone()),
        }
    }
    for ch in &delivery.changes {
        let outcome = validate_change(roots, ch);
        match outcome.reason {
            Some(r) => report
                .unsatisfied
                .push((format!("change:{}", ch.path.relative_path), r)),
            None => report
                .satisfied
                .push(format!("change:{}", ch.path.relative_path)),
        }
    }
    report
}

/// 扩展名 → 媒体类型的启发式映射（只覆盖产物约束会用到的常见类型；
/// 判不出来的返回 `None`，约束不匹配即不成立，不"猜一个通过"）。
fn guess_media_type(rel: &str) -> Option<String> {
    let ext = Path::new(rel)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())?;
    let mt = match ext.as_str() {
        "txt" | "md" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "json" | "toml" | "yaml"
        | "yml" | "css" | "html" | "py" | "sh" => "text/plain",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "zip" => "application/zip",
        _ => return None,
    };
    Some(mt.to_string())
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
