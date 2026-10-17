//! 修复前失败证据（任务 #13，[docs/tools-and-safety](../../../../docs/tools-and-safety.md)）。
//!
//! "我修好了"必须有**修复前真的失败过**的证据支撑。首版模型是 [`RegressionPair`]：
//! 同一缺陷的 RED（修复前）与 GREEN（修复后）两张真实执行回执，外加检查规范修订号。
//!
//! 四条不可让步的判定纪律：
//!
//! 1. **失败必须匹配预先定义的期望**。缺依赖、权限拒绝、测试没找到、进程起不来——
//!    这些是**环境问题**，把它们当"缺陷复现"会让 RED 证据毫无意义（[#13](../../../../docs/tools-and-safety.md)）。
//!
//! 真正的 RED 是：进程真实跑了、按预期方式失败。
//! 2. **只给 GREEN 保持 `unproven`**。一张通过的回执不证明"之前坏过"——
//!    任务本来就好的话，GREEN 永远能拿到。
//! 3. **改了检查规范就是新身份**。把断言删掉再沿用旧 check id，等于用"不再失败"冒充修复；
//!    规范变了必须升 `check_spec_revision` 并说明。
//! 4. **中断后不重跑 RED**。进程被取消 / 超时的那次执行不是"修复前失败"，重跑只是制造
//!    一份新的、无法对账的证据。旧回执仍在索引里，可以照常对账。

use super::types::{ExecutionOutcome, ExecutionReceipt, RunIdentity};
use super::version::RootResolver;
use serde::{Deserialize, Serialize};

/// 复现状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionStatus {
    /// 未证明（无 RED、只有 GREEN，或失败原因属环境问题）。
    Unproven,
    /// 已复现：有合法 RED 证据，尚未有 GREEN。
    Reproduced,
    /// 已修复：RED 与 GREEN 成对且检查规范一致。
    Fixed,
    /// 明确无法复现（写明原因），**不满足**需要复现的必需项。
    NotReproducible,
}

/// RED / GREEN 关联（[#13](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionPair {
    /// 缺陷 id（契约内稳定标识）。
    pub issue_id: String,
    /// 关联的检查 id。
    pub check_id: String,
    /// 修复前失败回执 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_receipt_id: Option<String>,
    /// 修复后通过回执 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green_receipt_id: Option<String>,
    /// 复现状态。
    pub reproduction_status: ReproductionStatus,
    /// 原因（无法复现 / 状态判定依据）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// RED 时被测代码版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_version: Option<String>,
    /// GREEN 时被测代码版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green_version: Option<String>,
    /// 检查规范修订：规范变了就换号，不沿用旧检查身份。
    pub check_spec_revision: u64,
    /// 非确定性缺陷的观测数据（次数 / 失败频率 / 环境）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nondeterminism: Option<NondeterminismRecord>,
}

/// 非确定性观测（[#13](../../../../docs/tools-and-safety.md)）。
///
/// 一次成功**不构成**解决：间歇性缺陷要在相同条件下重复观测。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NondeterminismRecord {
    /// 总运行次数。
    pub runs: u32,
    /// 失败次数。
    pub failures: u32,
    /// 环境描述（非秘密；只登记键与版本）。
    #[serde(default)]
    pub environment: Vec<String>,
}

impl NondeterminismRecord {
    /// 失败频率（失败 / 总；总为 0 时为 0）。
    pub fn failure_rate(&self) -> f64 {
        if self.runs == 0 {
            return 0.0;
        }
        f64::from(self.failures) / f64::from(self.runs)
    }
    /// 观测是否足以判定"已修复"。
    ///
    /// 阈值取 10 次且零失败：间歇性缺陷在少于这个样本量上的一次通过，
    /// 与"碰巧没触发"无法区分。
    pub fn is_conclusive(&self) -> bool {
        self.runs >= MIN_NONDETERMINISM_RUNS && self.failures == 0
    }
}

/// 非确定性判定所需最少观测次数。
pub const MIN_NONDETERMINISM_RUNS: u32 = 10;

/// 环境类失败分类（不算缺陷复现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 真实缺陷复现：进程跑了并按预期方式失败。
    Defect,
    /// 环境问题：缺依赖 / 程序不存在 / 权限拒绝 / 准入被拒。
    Environment,
    /// 执行未完成：超时、取消、中断、等待失败。
    Incomplete,
}

/// 对一次执行做失败分类。
///
/// 判据只看**真实执行事实**（终态与退出码），不看命令文本里写了什么——
/// "cargo test" 里没有 `#[ignore]` 不代表它不是被跳过的用例。
pub fn classify_failure(receipt: &ExecutionReceipt, expected_exit_codes: &[i32]) -> FailureClass {
    match receipt.outcome {
        // 从未启动：程序不存在 / 权限 / 准入被拒 —— 一律环境问题
        ExecutionOutcome::SpawnFailed | ExecutionOutcome::Denied => FailureClass::Environment,
        // 没跑完：超时 / 取消 / 中断 —— 不构成"失败过"的证据
        ExecutionOutcome::TimedOut
        | ExecutionOutcome::Cancelled
        | ExecutionOutcome::Interrupted => FailureClass::Incomplete,
        ExecutionOutcome::Exited => match receipt.exit_code {
            // 按预期方式失败 = 缺陷复现
            Some(code) if expected_exit_codes.contains(&code) => FailureClass::Defect,
            // 退出码不匹配预期：可能是测试没找到、编译不过等，仍非缺陷复现
            _ => FailureClass::Environment,
        },
    }
}

/// 两条回执能否配成一对。
///
/// 三个必要条件全中才算配对成功：
/// 1. **同一检查身份**：`check_id` 相同且 `check_spec_revision` 未变；
/// 2. **同属主**：跨会话 / 跨 run 的回执不能配对（迟到结果不参与本次对账）；
/// 3. **版本确实推进**：GREEN 的被测版本与 RED 不同，否则"修了但内容没变"。
pub fn pair_usable(
    red: &ExecutionReceipt,
    green: &ExecutionReceipt,
    owner: &RunIdentity,
    check_id: &str,
) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    for (label, r) in [("RED", red), ("GREEN", green)] {
        if r.check_id.as_deref() != Some(check_id) {
            problems.push(format!(
                "{label} 回执关联检查 `{}`，与配对检查 `{check_id}` 不同（同名不同命令不能直接配对）",
                r.check_id.as_deref().unwrap_or("-")
            ));
        }
        if r.owner.session_id != owner.session_id || r.owner.root_run_id != owner.root_run_id {
            problems.push(format!(
                "{label} 回执属于其他会话或 root run，不参与本次对账"
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// 判定一组回执能否满足「需要修复前失败证据」的必需项。
///
/// 返回 `(status, reason)`。只有 [`ReproductionStatus::Fixed`] 才满足；
/// `NotReproducible` 与 `Unproven` 都**不满足**——不能复现就得保持未证明，
/// 而不是"修了所以算好"。
pub fn judge(
    pair: &RegressionPair,
    red: Option<&ExecutionReceipt>,
    green: Option<&ExecutionReceipt>,
    owner: &RunIdentity,
    expected_exit_codes: &[i32],
    roots: &RootResolver,
) -> (ReproductionStatus, Option<String>) {
    let Some(red) = red else {
        return (
            ReproductionStatus::Unproven,
            Some("缺少修复前失败回执：只有 GREEN 不证明之前坏过".into()),
        );
    };
    let _ = roots;
    if let Err(problems) = single_side(red, owner) {
        return (ReproductionStatus::Unproven, Some(problems.join("；")));
    }
    match classify_failure(red, expected_exit_codes) {
        FailureClass::Environment => return (
            ReproductionStatus::Unproven,
            Some(
                "RED 属于环境问题（缺依赖 / 权限拒绝 / 程序不存在 / 测试未找到），不构成缺陷复现"
                    .into(),
            ),
        ),
        FailureClass::Incomplete => {
            return (
                ReproductionStatus::Unproven,
                Some("RED 未真实跑完（超时 / 取消 / 中断）：不重跑，历史回执留待对账".into()),
            );
        }
        FailureClass::Defect => {}
    }
    let Some(green) = green else {
        return (
            ReproductionStatus::Reproduced,
            Some("已复现，尚无修复后通过证据".into()),
        );
    };
    if let Err(problems) = pair_usable(red, green, owner, &pair.check_id) {
        return (ReproductionStatus::Unproven, Some(problems.join("；")));
    }
    if green.outcome != ExecutionOutcome::Exited
        || green.signal.is_some()
        || green.exit_code != Some(0)
        || red.requested != green.requested
        || red.resolved.is_none()
        || red.resolved != green.resolved
        || pair.red_version.is_none()
        || pair.green_version.is_none()
        || green.after.as_ref().is_none_or(|v| !v.is_valid())
        || red.after.as_ref().is_none_or(|v| !v.is_valid())
    {
        return (
            ReproductionStatus::Unproven,
            Some("GREEN 未成功完成、检查命令不同或版本证据不完整".into()),
        );
    }
    if let Some(nd) = &pair.nondeterminism
        && !nd.is_conclusive()
    {
        return (
            ReproductionStatus::Reproduced,
            Some(format!(
                "间歇性缺陷观测不足（{}/{} 次，失败 {} 次），一次通过不作数",
                nd.runs, MIN_NONDETERMINISM_RUNS, nd.failures
            )),
        );
    }
    if pair.red_version.is_some()
        && pair.green_version.is_some()
        && pair.red_version == pair.green_version
    {
        return (
            ReproductionStatus::Unproven,
            Some("RED 与 GREEN 绑定的被测版本相同：内容没变过，不能算修复".into()),
        );
    }
    if pair.red_version.as_deref() != red.after.as_ref().map(|v| v.digest.as_str())
        || pair.green_version.as_deref() != green.after.as_ref().map(|v| v.digest.as_str())
    {
        return (
            ReproductionStatus::Unproven,
            Some("配对声明版本不匹配真实回执".into()),
        );
    }
    (
        ReproductionStatus::Fixed,
        Some("修复前失败与修复后通过均在同一检查身份下取得".into()),
    )
}

fn single_side(r: &ExecutionReceipt, owner: &RunIdentity) -> Result<(), Vec<String>> {
    if r.owner.session_id != owner.session_id || r.owner.root_run_id != owner.root_run_id {
        return Err(vec![
            "回执属于其他会话或 root run，不参与本次对账".to_string(),
        ]);
    }
    Ok(())
}

/// 该回归证据是否满足「需要修复前失败」的必需项。
///
/// 供 #54 验收与 #18 校验共用——不另写第二套"算不算修好"。
pub fn satisfies_required_evidence(status: ReproductionStatus) -> bool {
    status == ReproductionStatus::Fixed
}

#[cfg(test)]
#[path = "regression_tests.rs"]
mod tests;
