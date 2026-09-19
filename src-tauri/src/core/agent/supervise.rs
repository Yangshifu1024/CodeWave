//! 运行监督（[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）：
//! drive_agent 每步批次后把调用摘要喂给 SupervisionState——失败重复按双层签名判定：
//! 完全相同调用（工具+args 哈希）反复失败（真卡死）3 次纠偏、5 次终止；同工具同失败码
//! 任意参数连环失败（烧穿）放宽到 6 次纠偏、10 次终止（照顾 command 跑测试等每轮
//! 参数不同的正常迭代）。完全相同的成功调用反复出现同样纠偏。纠偏注入
//! <supervision-notice>，仍不收敛则终止 run，杜绝重复操作烧穿步数/token 预算。
//! 空转看门狗（feed_batch）：连续多步只有重复读等零进展只读操作（全成功、参数互异，
//! 失败/重复签名均捕捉不到）8 步纠偏、14 步终止；同文件分段连读 10 次专用纠偏。
//! 只读 run（`IdlePolicy::NudgeOnly`，如 explore/reviewer/code-reviewer 子代理）在**空转层**放宽到
//! READONLY_IDLE_NUDGE_AT 步纠偏、只纠偏不终止——只读调研天然是「大段只读步骤 + 偶尔产出」，
//! 该层的预算天花板交给 max_steps（[docs/subagent-idle-watchdog-misfire]）。
//! 注意 NudgeOnly 只守卫空转层：失败重复层（精确 3/5、宽松 6/10）、max_steps 与 MAX_TEXT_TURNS
//! 强制汇报门对只读 run 照常生效，仍会终止本 run。
//! 主会话/子代理/任务运行三路统一生效（共用 drive_agent），不加新事件键——纠偏消息进历史。

use std::collections::{HashSet, VecDeque};

/// 滑动窗口长度（最近 N 次工具调用）。
pub const WINDOW: usize = 12;
/// 同一失败签名（含参数）触发纠偏的窗口内次数。
pub const FAIL_NUDGE_AT: usize = 3;
/// 纠偏后同一失败签名（含参数）窗口内再累计到此次数 → 硬介入终止。
pub const FAIL_ESCALATE_AT: usize = 5;
/// 同工具+同失败码（任意参数）触发纠偏的窗口内次数（宽松层：照顾 command 等每轮
/// 参数必然不同的工具——跑测试失败→改→换命令再跑是正常迭代，不得按重复失败误伤）。
pub const FAIL_LOOSE_NUDGE_AT: usize = 6;
/// 宽松层纠偏后窗口内再累计到此次数 → 硬介入终止。
pub const FAIL_LOOSE_ESCALATE_AT: usize = 10;
/// 完全相同调用（工具 + args 哈希）触发纠偏的窗口内次数。
pub const SAME_CALL_NUDGE_AT: usize = 4;
/// 空转看门狗：连续无进展步数触发纠偏（独立计数器，不受 WINDOW 驱逐影响；
/// 计数单位 = 步/批次，全表最宽松——空转是最弱信号，成功调用且参数互异）。
pub const IDLE_NUDGE_AT: usize = 8;
/// 空转看门狗纠偏后仍无进展累计到此次数 → 终止（纠偏后约 6 步收敛余地）。
pub const IDLE_ESCALATE_AT: usize = 14;
/// 同一文件连续分段读触发专用纠偏的次数（只纠偏不终止：大文件分段读可能是合法的，
/// 终止仍只由 IDLE_ESCALATE_AT 驱动）。
pub const SEGMENT_NUDGE_AT: usize = 10;
/// 只读 run 的空转纠偏阈值：比通用 IDLE_NUDGE_AT 更宽（只读调研里「重读已读文件」很常见，
/// 8 步就提示会持续噪声打断），且空转层**没有对应的终止阈值**——只纠偏不终止
///（仅限空转层；失败重复层与步数/汇报门照常生效）。
pub const READONLY_IDLE_NUDGE_AT: usize = 16;

/// 空转看门狗处置策略（每 run 一份，由 `DriveParams.idle_policy` 决定）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IdlePolicy {
    /// 默认：IDLE_NUDGE_AT 步纠偏、IDLE_ESCALATE_AT 步硬终止
    /// （主会话 / 任务运行 / 可写子代理，语义与引入只读策略前逐字一致）
    #[default]
    Stop,
    /// 只读 run：空转层 READONLY_IDLE_NUDGE_AT 步纠偏一次、只纠偏不终止
    ///（**仅空转层**；失败重复层与步数/汇报门不受本策略影响，照常终止）
    NudgeOnly,
}

/// 只读工具名集合（进展判定用；core 层不依赖 tools 类型，按名称判读）。
pub(crate) const READONLY_TOOLS: &[&str] = &[
    "read",
    "batch_read",
    "grep",
    "calculate",
    "list_files",
    "web_fetch",
    "render_html",
];

/// 单次工具调用的监督签名（批次层摘要）。
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CallSig {
    /// 工具名
    pub name: String,
    /// 失败码；成功为 None
    pub error: Option<String>,
    /// args 串哈希（识别完全相同的重复调用）
    pub args_hash: u64,
}

/// 监督裁决。
pub enum Verdict {
    /// 继续观察
    None,
    /// 注入纠偏消息（文本进历史）
    Nudge(String),
    /// 硬介入：终止本 run（文本进错误信息）
    Escalate(String),
}

/// 每步批次摘要（空转看门狗用）：drive_agent 每步从批次调用列表提取。
#[derive(Default)]
pub struct BatchDigest {
    /// 批次内出现任何非只读工具调用（写/command/ask/wait/network/subagent 等副作用或交互）
    pub has_non_readonly: bool,
    /// read/batch_read 读到的文件路径（已由调用方按工作区归一化）
    pub read_paths: Vec<String>,
}

/// 每 run 独立的监督状态机（drive_agent 局部变量，非线程安全）。
#[derive(Default)]
pub struct SupervisionState {
    /// 空转看门狗策略（默认 Stop，保持既有行为）
    policy: IdlePolicy,
    window: VecDeque<CallSig>,
    /// 已注入过纠偏的 key（同签名每次 run 只纠偏一次）
    nudged: HashSet<String>,
    /// 空转看门狗：连续无进展步数（独立计数器，不受窗口驱逐影响）
    idle_streak: usize,
    /// 空转看门狗：本 run 已读过的文件（文件级去重；首次读 = 进展）
    read_files: HashSet<String>,
    /// 同文件连续分段读计数（容忍余量）
    segment_streak: Option<(String, usize)>,
}

impl SupervisionState {
    /// 按策略构造（[docs/subagent-idle-watchdog-misfire]）：只读 run 传 `NudgeOnly`；
    /// 其余场景保持 `Default`（= `Stop`），行为逐字不变。
    pub fn with_idle_policy(policy: IdlePolicy) -> Self {
        Self {
            policy,
            ..Default::default()
        }
    }

    pub fn feed(&mut self, sig: CallSig) -> Verdict {
        self.window.push_back(sig.clone());
        while self.window.len() > WINDOW {
            self.window.pop_front();
        }
        let count_in_window = |s: &SupervisionState, target: &CallSig| {
            s.window.iter().filter(|c| *c == target).count()
        };
        // 规则二：失败重复检测，双层签名（优先于规则一判定，达到硬介入阈值直接终止）：
        // 精确层 = 完全相同的失败调用（含参数）——真卡死，原样重试烧预算；
        // 宽松层 = 同工具同失败码任意参数——只有大面积连环失败（窗口内近满）才算烧穿。
        if let Some(err) = &sig.error {
            let exact_key = format!("fail:{}:{err}:{}", sig.name, sig.args_hash);
            let loose_key = format!("fail_loose:{}:{err}", sig.name);
            let fails_exact = self
                .window
                .iter()
                .filter(|c| {
                    c.name == sig.name
                        && c.error.as_deref() == Some(err.as_str())
                        && c.args_hash == sig.args_hash
                })
                .count();
            let fails_loose = self
                .window
                .iter()
                .filter(|c| c.name == sig.name && c.error.as_deref() == Some(err.as_str()))
                .count();
            if fails_exact >= FAIL_ESCALATE_AT && self.nudged.contains(&exact_key) {
                return Verdict::Escalate(format!(
                    "监督介入：「{}」同一调用失败（{err}）在纠偏后仍原样重复 {fails_exact} 次，循环未收敛，本次 run 终止。已完成部分保留在历史中，请基于现状收尾。",
                    sig.name
                ));
            }
            if fails_loose >= FAIL_LOOSE_ESCALATE_AT && self.nudged.contains(&loose_key) {
                return Verdict::Escalate(format!(
                    "监督介入：「{}」失败（{err}）在纠偏后窗口内仍累计 {fails_loose} 次，大面积失败未收敛，本次 run 终止。已完成部分保留在历史中，请基于现状收尾。",
                    sig.name
                ));
            }
            if fails_exact >= FAIL_NUDGE_AT && self.nudged.insert(exact_key) {
                return Verdict::Nudge(format!(
                    "<supervision-notice>检测到重复失败：「{}」同一调用在最近操作中失败 {fails_exact} 次（{err}）。立即改变策略：修正参数/换方法/跳过该步骤并在汇报中说明，不要原样重试；若确认无法自行修正，改用 ask 工具向用户说明卡点并请示下一步，不要继续空转。</supervision-notice>",
                    sig.name
                ));
            }
            if fails_loose >= FAIL_LOOSE_NUDGE_AT && self.nudged.insert(loose_key) {
                return Verdict::Nudge(format!(
                    "<supervision-notice>检测到连环失败：「{}」在最近操作中失败 {fails_loose} 次（{err}，参数各不相同）。若在正常迭代（如修代码后重跑测试）请忽略本提示继续；否则说明该工具路线整体不通，立即换方法/跳过该步骤并在汇报中说明。</supervision-notice>",
                    sig.name
                ));
            }
        }
        // 规则一：完全相同的成功调用反复出现（失败重复由规则二覆盖）
        if sig.error.is_none() && count_in_window(self, &sig) >= SAME_CALL_NUDGE_AT {
            let key = format!("same:{}:{}", sig.name, sig.args_hash);
            if self.nudged.insert(key) {
                return Verdict::Nudge(format!(
                    "<supervision-notice>检测到重复调用：「{}」在最近操作中原样出现 {} 次（参数完全相同）。立即改变策略：换方法、缩小范围或跳过该步骤并在汇报中说明，不要原样重发。</supervision-notice>",
                    sig.name,
                    count_in_window(self, &sig)
                ));
            }
        }
        Verdict::None
    }

    /// 空转看门狗：每步批次后喂一次摘要。进展信号（非只读工具/首次读新文件）清零全部
    /// 空转计数；否则 idle_streak +1——达纠偏阈值纠偏一次、纠偏后累计到终止阈值则终止。
    /// 阈值与「空转层是否终止」由 policy 决定：`Stop` = 8/14，`NudgeOnly` = 16 纠偏一次、
    /// 空转层不终止（失败重复层与步数/汇报门不受 policy 影响）。
    /// 同文件连续分段读达 SEGMENT_NUDGE_AT 发专用纠偏（不升级终止）。
    /// 自动压缩后调用 `reset_idle()` 允许模型无惩罚重读重建上下文。
    pub fn feed_batch(&mut self, digest: &BatchDigest) -> Verdict {
        // 进展信号 1：批次内任何非只读工具
        if digest.has_non_readonly {
            self.clear_idle();
            return Verdict::None;
        }
        // 进展信号 2：read/batch_read 读到本 run 首次的文件
        let mut has_first_read = false;
        for p in &digest.read_paths {
            if self.read_files.insert(p.clone()) {
                has_first_read = true;
            }
        }
        if has_first_read {
            self.clear_idle();
            return Verdict::None;
        }
        // 空转步：重复读已读文件（含分段）或参数互异的其他只读调用。
        // 同文件连续分段读单独计数（容忍余量）：本步读过文件则更新 segment_streak
        if let Some(last) = digest.read_paths.last() {
            match &mut self.segment_streak {
                Some((f, n)) if f == last => {
                    *n += 1;
                    let (f, n) = (f.clone(), *n);
                    if n >= SEGMENT_NUDGE_AT && self.nudged.insert("segment".into()) {
                        return Verdict::Nudge(format!(
                            "<supervision-notice>对「{f}」已连续分段读取 {n} 次。改用一次完整读取该文件，或用 grep 定位目标行段后再精读；无目的的分段翻阅是无效操作。</supervision-notice>"
                        ));
                    }
                }
                _ => self.segment_streak = Some((last.clone(), 1)),
            }
        }
        self.idle_streak += 1;
        // 只读 run 的空转层只纠偏不终止（[docs/subagent-idle-watchdog-misfire]）：该层预算
        // 天花板是 max_steps，不是「看起来像在空转」——硬终止对它属系统性误伤。
        // 失败重复层与步数/汇报门不在本守卫范围，照常终止。
        let stopping = self.policy == IdlePolicy::Stop;
        if stopping && self.idle_streak >= IDLE_ESCALATE_AT && self.nudged.contains("idle") {
            return Verdict::Escalate(format!(
                "监督介入：纠偏后仍连续 {} 步无实质进展（持续重复读取，无任何推进动作），本次 run 终止。已完成部分保留在历史中，请基于现状收尾。",
                self.idle_streak
            ));
        }
        let nudge_at = if stopping {
            IDLE_NUDGE_AT
        } else {
            READONLY_IDLE_NUDGE_AT
        };
        if self.idle_streak >= nudge_at && self.nudged.insert("idle".into()) {
            let text = if stopping {
                "<supervision-notice>检测到连续多步未产生实质进展（仅重复读取已读过的内容，无写入/提问/新信息收集）。请立即停止重复读取：① 用一两句话输出当前已知结论与下一步计划；② 若需要用户决策，用 ask 工具提问确认；③ 若已可行动，直接执行下一步（写文件/修改）。继续空读将导致本 run 被终止。</supervision-notice>"
            } else {
                "<supervision-notice>检测到连续多步仅重复读取已读过的内容。只读角色没有写工具，调研期反复精读同一批文件属正常节奏，可忽略本提示；若确实在反复空转，建议先用一两句话小结当前已知结论与剩余待查项，或直接输出最终汇报结束本 run。本提示不会终止本 run（空转层不终止；步数预算与失败重复、汇报门仍照常生效）。</supervision-notice>"
            };
            return Verdict::Nudge(text.into());
        }
        Verdict::None
    }

    /// 自动压缩后调用：允许模型重读文件重建上下文（压缩丢细节后重读是真实需求，
    /// 不得按空转计）。idle 计数清零 + 已读文件集合清空（重读按首次计）。
    pub fn reset_idle(&mut self) {
        self.clear_idle();
        self.read_files.clear();
    }

    fn clear_idle(&mut self) {
        self.idle_streak = 0;
        self.segment_streak = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(name: &str, error: Option<&str>, hash: u64) -> CallSig {
        CallSig {
            name: name.into(),
            error: error.map(Into::into),
            args_hash: hash,
        }
    }

    #[test]
    fn repeated_failure_nudges_once_then_escalates() {
        let mut s = SupervisionState::default();
        for _ in 0..FAIL_NUDGE_AT - 1 {
            assert!(matches!(
                s.feed(sig("edit", Some("E_VERSION_STALE"), 1)),
                Verdict::None
            ));
        }
        // 第 3 次：纠偏
        assert!(matches!(
            s.feed(sig("edit", Some("E_VERSION_STALE"), 1)),
            Verdict::Nudge(_)
        ));
        // 纠偏后再来两次（窗口内累计 5）：硬介入
        assert!(matches!(
            s.feed(sig("edit", Some("E_VERSION_STALE"), 1)),
            Verdict::None
        ));
        assert!(matches!(
            s.feed(sig("edit", Some("E_VERSION_STALE"), 1)),
            Verdict::Escalate(_)
        ));
    }

    #[test]
    fn different_error_codes_are_independent_signatures() {
        let mut s = SupervisionState::default();
        for code in ["E_A", "E_B", "E_C"] {
            assert!(matches!(s.feed(sig("edit", Some(code), 1)), Verdict::None));
        }
    }

    /// 纠偏必须给模型「人机交互」出路：无法自纠时改用 ask 请示用户，而非继续空转烧预算
    ///（会话卡死修复的兜底文案约束）。
    #[test]
    fn failure_nudge_mentions_ask_fallback() {
        let mut s = SupervisionState::default();
        for _ in 0..FAIL_NUDGE_AT - 1 {
            let _ = s.feed(sig("edit", Some("E_ARGS"), 1));
        }
        assert!(
            matches!(
                s.feed(sig("edit", Some("E_ARGS"), 1)),
                Verdict::Nudge(ref t) if t.contains("ask")
            ),
            "精确层纠偏文案必须含 ask 求助指引"
        );
    }

    #[test]
    fn identical_success_calls_nudge_at_threshold() {
        let mut s = SupervisionState::default();
        for _ in 0..SAME_CALL_NUDGE_AT - 1 {
            assert!(matches!(s.feed(sig("read", None, 7)), Verdict::None));
        }
        assert!(matches!(s.feed(sig("read", None, 7)), Verdict::Nudge(_)));
    }

    #[test]
    fn identical_failure_not_double_nudged_by_same_call_rule() {
        let mut s = SupervisionState::default();
        for _ in 0..FAIL_NUDGE_AT {
            let _ = s.feed(sig("edit", Some("E_X"), 3));
        }
        // 第 4 次失败：失败规则已纠偏过，同调用规则不得再发第二条
        assert!(matches!(s.feed(sig("edit", Some("E_X"), 3)), Verdict::None));
    }

    #[test]
    fn varying_args_failures_do_not_nudge_until_loose_threshold() {
        // command 每轮参数不同（跑测试→改→换命令重跑）：5 次以内不得误伤
        let mut s = SupervisionState::default();
        for i in 0..FAIL_LOOSE_NUDGE_AT - 1 {
            assert!(
                matches!(
                    s.feed(sig("command", Some("E_EXIT_CODE"), i as u64)),
                    Verdict::None
                ),
                "第 {i} 次不同参数失败不应触发"
            );
        }
        // 第 6 次：宽松层纠偏（明确提示正常迭代可忽略）
        assert!(matches!(
            s.feed(sig("command", Some("E_EXIT_CODE"), 99)),
            Verdict::Nudge(text) if text.contains("正常迭代")
        ));
    }

    #[test]
    fn varying_args_failures_escalate_at_loose_threshold() {
        let mut s = SupervisionState::default();
        for i in 0..FAIL_LOOSE_NUDGE_AT as u64 {
            let _ = s.feed(sig("command", Some("E_EXIT_CODE"), i));
        }
        // 纠偏后继续：7~9 次 None，第 10 次（宽松层终止阈值）硬介入
        for i in FAIL_LOOSE_NUDGE_AT as u64..FAIL_LOOSE_ESCALATE_AT as u64 - 1 {
            assert!(matches!(
                s.feed(sig("command", Some("E_EXIT_CODE"), i)),
                Verdict::None
            ));
        }
        assert!(matches!(
            s.feed(sig("command", Some("E_EXIT_CODE"), 100)),
            Verdict::Escalate(text) if text.contains("大面积失败")
        ));
    }

    #[test]
    fn exact_failure_layer_counts_per_args_hash() {
        // 精确层按 args_hash 分开计数：同工具同错误码但参数不同不累计
        let mut s = SupervisionState::default();
        for _ in 0..FAIL_NUDGE_AT - 1 {
            assert!(matches!(s.feed(sig("edit", Some("E_X"), 1)), Verdict::None));
        }
        // 换参数的失败只进宽松层计数（此时 5 次 < 6）
        for hash in [2, 3] {
            assert!(matches!(
                s.feed(sig("edit", Some("E_X"), hash)),
                Verdict::None
            ));
        }
        // 回到原参数：精确层第 3 次 → 纠偏
        assert!(matches!(
            s.feed(sig("edit", Some("E_X"), 1)),
            Verdict::Nudge(_)
        ));
    }

    #[test]
    fn window_eviction_resets_counts() {
        let mut s = SupervisionState::default();
        for i in 0..WINDOW as u64 {
            assert!(matches!(s.feed(sig("noise", None, i)), Verdict::None));
        }
        // 早期的一次失败已被挤出窗口：新的失败序列重新从 1 计数
        assert!(matches!(s.feed(sig("edit", Some("E_Y"), 9)), Verdict::None));
        assert!(matches!(s.feed(sig("edit", Some("E_Y"), 9)), Verdict::None));
        assert!(matches!(
            s.feed(sig("edit", Some("E_Y"), 9)),
            Verdict::Nudge(_)
        ));
    }

    // ===== 空转看门狗（feed_batch）=====

    fn idle_step(paths: &[&str]) -> BatchDigest {
        BatchDigest {
            has_non_readonly: false,
            read_paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn idle_loop_nudges_then_escalates() {
        let mut s = SupervisionState::default();
        // 场景用无 read 的空转步（如重复 grep）：同文件连读会同步牵入分段纠偏
        //（SEGMENT_NUDGE_AT 先/后触发），干扰本用例要隔离验证的空转计数
        for _ in 0..IDLE_NUDGE_AT - 1 {
            assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::None));
        }
        // 第 8 步：空转纠偏
        assert!(matches!(
            s.feed_batch(&idle_step(&[])),
            Verdict::Nudge(text) if text.contains("实质进展")
        ));
        // 纠偏后继续空转：9~13 步 None，第 14 步终止
        for _ in (IDLE_NUDGE_AT + 1)..IDLE_ESCALATE_AT {
            assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::None));
        }
        assert!(matches!(
            s.feed_batch(&idle_step(&[])),
            Verdict::Escalate(text) if text.contains("本次 run 终止")
        ));
    }

    #[test]
    fn non_readonly_tool_clears_idle_streak() {
        let mut s = SupervisionState::default();
        for _ in 0..IDLE_NUDGE_AT - 1 {
            let _ = s.feed_batch(&idle_step(&[]));
        }
        // 一次非只读调用（ask/command/edit 等）= 进展，计数清零
        let mut d = BatchDigest::default();
        d.has_non_readonly = true;
        assert!(matches!(s.feed_batch(&d), Verdict::None));
        // 清零后重新计数：需再累计满 IDLE_NUDGE_AT 步才纠偏
        for _ in 0..IDLE_NUDGE_AT - 1 {
            assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::None));
        }
        assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::Nudge(_)));
    }

    #[test]
    fn first_read_of_new_file_clears_idle_streak() {
        let mut s = SupervisionState::default();
        // 首读 a.ts 登记为进展；重复读累计（离纠偏阈值留 1 步余量）
        assert!(matches!(s.feed_batch(&idle_step(&["a.ts"])), Verdict::None));
        for _ in 0..IDLE_NUDGE_AT - 2 {
            assert!(matches!(s.feed_batch(&idle_step(&["a.ts"])), Verdict::None));
        }
        // 读新文件 = 进展：清零且 b.ts 登记为已读
        assert!(matches!(s.feed_batch(&idle_step(&["b.ts"])), Verdict::None));
        // b.ts 再读已是重复：重新累计满 IDLE_NUDGE_AT 步才纠偏
        for _ in 0..IDLE_NUDGE_AT - 1 {
            assert!(matches!(s.feed_batch(&idle_step(&["b.ts"])), Verdict::None));
        }
        assert!(matches!(
            s.feed_batch(&idle_step(&["b.ts"])),
            Verdict::Nudge(_)
        ));
    }

    #[test]
    fn varied_readonly_params_still_count_as_idle() {
        // 变体空转：read 与 grep 交替、参数互异——不能绕过看门狗
        let mut s = SupervisionState::default();
        let _ = s.feed_batch(&idle_step(&["a.ts"]));
        for i in 0..IDLE_NUDGE_AT - 1 {
            let d = if i % 2 == 0 {
                idle_step(&["a.ts"])
            } else {
                BatchDigest {
                    has_non_readonly: false,
                    read_paths: vec![], // grep 步：无 read 路径
                }
            };
            let _ = s.feed_batch(&d);
        }
        assert!(matches!(
            s.feed_batch(&idle_step(&["a.ts"])),
            Verdict::Nudge(_)
        ));
    }

    #[test]
    fn segment_reads_nudge_without_escalation() {
        let mut s = SupervisionState::default();
        // 同文件连续读时空转与分段计数同步增长：第 8 步先触发空转纠偏（两计数独立去重），
        // 第 10 步再触发分段专用纠偏；分段纠偏本身不升级终止——终止仍只由 IDLE_ESCALATE_AT 驱动
        assert!(matches!(
            s.feed_batch(&idle_step(&["big.ts"])),
            Verdict::None
        ));
        for _ in 0..IDLE_NUDGE_AT - 1 {
            assert!(matches!(
                s.feed_batch(&idle_step(&["big.ts"])),
                Verdict::None
            ));
        }
        assert!(matches!(
            s.feed_batch(&idle_step(&["big.ts"])),
            Verdict::Nudge(text) if text.contains("实质进展")
        ));
        assert!(matches!(
            s.feed_batch(&idle_step(&["big.ts"])),
            Verdict::None
        ));
        assert!(matches!(
            s.feed_batch(&idle_step(&["big.ts"])),
            Verdict::Nudge(text) if text.contains("分段读取")
        ));
        // 分段纠偏后不立即终止（此处尚未达 IDLE_ESCALATE_AT）
        assert!(matches!(
            s.feed_batch(&idle_step(&["big.ts"])),
            Verdict::None
        ));
    }

    #[test]
    fn reset_idle_allows_penalty_free_reread() {
        let mut s = SupervisionState::default();
        let _ = s.feed_batch(&idle_step(&["a.ts"]));
        for _ in 0..IDLE_NUDGE_AT - 2 {
            let _ = s.feed_batch(&idle_step(&["a.ts"]));
        }
        // 自动压缩后重置：已读集合清空 → 重读 a.ts 按首次计（进展）且计数清零
        s.reset_idle();
        assert!(matches!(s.feed_batch(&idle_step(&["a.ts"])), Verdict::None));
        // 重置后重新累计：再次达到纠偏阈值才纠偏（未在纠偏前误触发）
        for _ in 0..IDLE_NUDGE_AT - 1 {
            assert!(matches!(s.feed_batch(&idle_step(&["a.ts"])), Verdict::None));
        }
        assert!(matches!(
            s.feed_batch(&idle_step(&["a.ts"])),
            Verdict::Nudge(_)
        ));
    }

    // ===== 只读 run 策略（IdlePolicy::NudgeOnly，[docs/subagent-idle-watchdog-misfire]）=====

    #[test]
    fn readonly_policy_never_escalates() {
        // 100 步纯空转：只允许一次纠偏，空转层永不终止（旧行为会在第 14 步杀掉 run；
        // 失败重复层与步数/汇报门不属本用例范围，它们仍会终止）
        let mut s = SupervisionState::with_idle_policy(IdlePolicy::NudgeOnly);
        let mut nudges = 0;
        for _ in 0..100 {
            match s.feed_batch(&idle_step(&[])) {
                Verdict::Nudge(_) => nudges += 1,
                Verdict::Escalate(t) => panic!("只读 run 不得终止：{t}"),
                Verdict::None => {}
            }
        }
        assert_eq!(nudges, 1, "只读 run 的纠偏同一 run 只发一次");
    }

    #[test]
    fn readonly_policy_nudges_at_sixteen() {
        let mut s = SupervisionState::with_idle_policy(IdlePolicy::NudgeOnly);
        for _ in 0..READONLY_IDLE_NUDGE_AT - 1 {
            assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::None));
        }
        // 第 16 步纠偏，且文案不再威胁终止（只读 run 已无终止）
        assert!(matches!(
            s.feed_batch(&idle_step(&[])),
            Verdict::Nudge(text) if !text.contains("被终止")
        ));
    }

    #[test]
    fn default_policy_is_stop_and_still_escalates_at_fourteen() {
        // 策略默认值 = Stop：主会话/任务运行/可写子代理的空转语义逐字不变
        assert_eq!(IdlePolicy::default(), IdlePolicy::Stop);
        let mut s = SupervisionState::default();
        for _ in 0..IDLE_NUDGE_AT {
            let _ = s.feed_batch(&idle_step(&[]));
        }
        for _ in (IDLE_NUDGE_AT + 1)..IDLE_ESCALATE_AT {
            assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::None));
        }
        assert!(matches!(
            s.feed_batch(&idle_step(&[])),
            Verdict::Escalate(text) if text.contains("本次 run 终止")
        ));
    }

    #[test]
    fn readonly_policy_keeps_progress_signals() {
        // 只读策略只改「无进展时怎么处置」，进展信号语义不变：非只读工具仍清零计数
        let mut s = SupervisionState::with_idle_policy(IdlePolicy::NudgeOnly);
        for _ in 0..READONLY_IDLE_NUDGE_AT - 1 {
            let _ = s.feed_batch(&idle_step(&[]));
        }
        let mut d = BatchDigest::default();
        d.has_non_readonly = true;
        assert!(matches!(s.feed_batch(&d), Verdict::None));
        // 清零后需重新累计满阈值才纠偏
        for _ in 0..READONLY_IDLE_NUDGE_AT - 1 {
            assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::None));
        }
        assert!(matches!(s.feed_batch(&idle_step(&[])), Verdict::Nudge(_)));
    }
}
