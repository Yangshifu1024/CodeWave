//! 目标模式（goal mode）的纯数据面与纯函数判定：目标登记、阶段推导、账本白名单、停滞判定。
//!
//! 本模块只做纯数据 + 纯函数，不碰 IO、不碰锁：状态本身存在 `SessionRuntime.goal`
//! （内存 + `sessions/<id>.goal.json` 边车持久化），读写入口是 `tools/goal.rs` 的 `goal` 工具，
//! 阶段注入（系统提示块）与档位切换由驱动层消费本模块的 `goal_phase` / `render_goal_block`。
//! **唯一例外**是文件末尾的运行时闸门（`ledger_gate` / `goal_hard_block`）：它们是执行期
//! 「零弹窗」的判定入口，按需读写 `SessionRuntime` 与目标边车（`store.save_goal`）。
//!
//! 生命周期：澄清期（Clarify）登记目标与验收标准 → 执行期（Execute）只允许勾选完成与记录进展
//! → 完成（Done）/ 中止（Aborted）。Done/Aborted 之后再登记即视为开新目标。

use serde::{Deserialize, Serialize};

/// 同一目标的澄清轮次上限（超出由驱动层提示收敛，不再无限追问）。
pub const GOAL_TEXT_TURN_LIMIT: u32 = 8;
/// 账本越界的重试上限（达到后由驱动层升级为硬停，不再让模型反复试探）。
pub const LEDGER_DENIAL_RETRY_LIMIT: u32 = 3;
/// 停滞软提醒阈值（`stall_streak` 达到此值注入提醒）。
pub const STALL_NUDGE_AT: u32 = 5;
/// 停滞硬停阈值（`stall_streak` 达到此值终止本 run）。
pub const STALL_STOP_AT: u32 = 10;
/// 续跑目标的档位校验文案：host 命令层（`goal_resume_mode_guard`）与 core
/// （`AgentCore::resume_goal`）是同一道门的两处落地，**共用这一份常量**——两套文案必然漂移。
pub const GOAL_RESUME_MODE_REQUIRED: &str =
    "E_GOAL_MODE_REQUIRED: 会话当前不在目标模式，无法续跑目标。请先切回目标模式后再续跑。";

/// 目标生命周期状态（wire 形态 snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// 澄清期：目标/验收标准仍在登记或修订，工作区只读。
    Clarify,
    /// 执行期：按账本做最小改动。
    Executing,
    /// 执行期暂停（等用户确认，不再动工作区）。
    Paused,
    /// 目标达成（全部验收标准已勾选）。
    Done,
    /// 目标中止（用户放弃或转向）。
    Aborted,
}

impl GoalStatus {
    /// 中文名（提示块、汇总与错误文案共用）。
    pub fn label(self) -> &'static str {
        match self {
            GoalStatus::Clarify => "澄清中",
            GoalStatus::Executing => "执行中",
            GoalStatus::Paused => "已暂停",
            GoalStatus::Done => "已完成",
            GoalStatus::Aborted => "已中止",
        }
    }
}

/// 单条验收标准：可判定的标题 + 是否已达成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCriterion {
    /// 标题（非空；执行期锁定不可改）。
    pub title: String,
    /// 是否已达成。
    pub done: bool,
}

/// 账本：本次目标允许触碰的路径与程序白名单（执行期的越界硬拦依据）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalLedger {
    /// 允许写入/修改的路径（前缀匹配，目录或文件）。
    #[serde(default)]
    pub paths: Vec<String>,
    /// 允许运行的程序（程序名或可执行文件路径）。
    #[serde(default)]
    pub programs: Vec<String>,
}

/// 目标状态（会话级；`SessionRuntime.goal` 的唯一内容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    /// 目标陈述（一句话；执行期锁定）。
    pub text: String,
    /// 验收标准（执行期只允许改 `done`）。
    pub criteria: Vec<GoalCriterion>,
    /// 账本（执行期锁定）。
    pub ledger: GoalLedger,
    /// 生命周期状态。
    pub status: GoalStatus,
    /// 已做出的关键决策（执行期只追加）。
    #[serde(default)]
    pub decisions: Vec<String>,
    /// 待办事项（执行期只追加）。
    #[serde(default)]
    pub pending: Vec<String>,
    /// 阻塞项（无法自行解决，需用户介入）。
    #[serde(default)]
    pub blocked: Vec<String>,
    /// 已进行的执行轮数（驱动层累加）。
    #[serde(default)]
    pub rounds: u32,
    /// 连续无进展轮数（驱动层累加，判定见 `stall_verdict`）。
    #[serde(default)]
    pub stall_streak: u32,
    /// 账本越界被拒次数（驱动层累加，上限见 `LEDGER_DENIAL_RETRY_LIMIT`）。
    #[serde(default)]
    pub ledger_denials: u32,
}

/// 目标阶段：决定工具集、系统提示块与账本是否生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalPhase {
    /// 澄清期：只读调研 + 登记/修订目标。
    Clarify,
    /// 执行期：按账本做最小改动。
    Execute,
}

/// 由状态推导阶段（`None` = 目标模式已开启但尚未登记 → 澄清期）。
/// Done/Aborted 归入澄清期：上一个目标已结束，再动工作区必须重新登记新目标。
pub fn goal_phase(state: Option<&GoalState>) -> Option<GoalPhase> {
    match state {
        None => Some(GoalPhase::Clarify),
        Some(s) => match s.status {
            GoalStatus::Clarify => Some(GoalPhase::Clarify),
            GoalStatus::Executing | GoalStatus::Paused => Some(GoalPhase::Execute),
            GoalStatus::Done | GoalStatus::Aborted => Some(GoalPhase::Clarify),
        },
    }
}

/// 是否已登记目标（`Some` 即已登记，与目标内容无关）。
pub fn is_registered(state: Option<&GoalState>) -> bool {
    state.is_some()
}

/// 账本判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerVerdict {
    /// 在账本内：放行。
    Allowed,
    /// 在账本外：拒绝（驱动层据 `LEDGER_DENIAL_RETRY_LIMIT` 决定是否硬停）。
    Outside,
}

/// 待判定的目标（借用形态，避免为一次判定克隆字符串）。
#[derive(Debug, Clone, Copy)]
pub enum LedgerTarget<'a> {
    /// 文件/目录路径。
    Path(&'a str),
    /// 程序名或可执行文件路径。
    Program(&'a str),
}

/// 系统路径前缀（归一化后比较；`strip_drive` 之后按无盘符形态匹配，故对所有盘符生效）。
///
/// 刻意**不含** `/var` 与 `/tmp`：macOS 的临时目录就在 `/var/folders/...` 下，
/// 把 `/var` 列为系统路径会让「账本登记临时目录」在 macOS 上一律被拒（本地平台看不出来）。
pub const SYSTEM_PATH_PREFIXES: &[&str] = &[
    "/etc",
    "/usr",
    "/bin",
    "/sbin",
    "/boot",
    "/dev",
    "/proc",
    "/sys",
    "/lib",
    "/lib64",
    "/root",
    "/system",
    "/library",
    "/windows",
    "/program files",
    "/program files (x86)",
    // Windows 8.3 短名形态：文本比较看不到短名背后的真实目录，补上兜底
    "/progra~1",
    "/progra~2",
    "/programdata",
    "/$recycle.bin",
];

/// 账本白名单判定：路径前缀匹配、程序名匹配；**根路径 / 家目录本身 / 系统路径一律拒绝**
/// （无论账本怎么写）。账本里的非法条目同样不生效（纵深防御：脏条目不会变成整盘授权）。
pub fn ledger_allows(ledger: &GoalLedger, target: LedgerTarget<'_>) -> LedgerVerdict {
    match target {
        LedgerTarget::Path(p) => {
            if is_forbidden_path(p) {
                return LedgerVerdict::Outside;
            }
            // 两侧都先做「宽松归一化」（解析已存在的最近祖先）：账本条目与工具目标路径可能来自
            // 不同解析链（一侧 canonicalize 过、一侧没有），只做文本比较会把同一条路径判成越界。
            let cand = normalize_existing(p);
            let hit = ledger
                .paths
                .iter()
                .any(|a| !is_forbidden_path(a) && path_prefix_match(&normalize_existing(a), &cand));
            verdict(hit)
        }
        LedgerTarget::Program(p) => {
            if p.trim().is_empty() {
                return LedgerVerdict::Outside;
            }
            let hit = ledger.programs.iter().any(|a| program_match(a, p));
            verdict(hit)
        }
    }
}

/// 命中 → Allowed，未命中 → Outside。
fn verdict(hit: bool) -> LedgerVerdict {
    if hit {
        LedgerVerdict::Allowed
    } else {
        LedgerVerdict::Outside
    }
}

/// 路径是否被硬性拒绝（不可作为账本条目，也不可作为账本判定的候选）：
/// 空值、裸根（`/`、`C:`、`C:\`）、含 `..` 组件、家目录本身、系统路径前缀。
pub fn is_forbidden_path(raw: &str) -> bool {
    let norm = normalize_path(raw);
    if norm.is_empty() || norm == "/" {
        return true;
    }
    // 裸盘符根：`c:`（`C:\` / `C:` 归一后都是 `c:`）
    if norm.len() == 2 && norm.as_bytes()[1] == b':' {
        return true;
    }
    // 相对逃逸：`..` 组件一律拒绝（本函数不做路径解析，文本判定必须保守）
    if norm.split('/').any(|c| c == "..") {
        return true;
    }
    // 家目录本身：整份白名单等于全盘授权，拒绝
    if home_dirs().iter().any(|h| *h == norm) {
        return true;
    }
    let bare = strip_drive(&norm);
    SYSTEM_PATH_PREFIXES
        .iter()
        .any(|s| path_prefix_match(s, bare))
}

/// 归一化：小写、反斜杠转正斜杠、去尾部斜杠（保留根 `/`）。
fn normalize_path(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/").to_ascii_lowercase();
    // Windows `canonicalize()` 返回 verbatim 形态（`\\?\C:\x` → `//?/c:/x`）；账本条目与工具
    // 传入的目标路径未必来自同一条链路（一侧规范化过、一侧没有），不剥前缀就会让**同一条路径**
    // 永远匹配不上——表现为“明明在账本里的目录被拒”。UNC verbatim（`\\?\UNC\srv\share`）还原为 `//srv/share`。
    let stripped = s
        .strip_prefix("//?/")
        .or_else(|| s.strip_prefix("//./"))
        .map(|rest| match rest.strip_prefix("unc/") {
            Some(r) => format!("//{r}"),
            None => rest.to_string(),
        });
    if let Some(v) = stripped {
        s = v;
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// 宽松归一化：在 [`normalize_path`] 基础上，把**已存在的最近祖先**做一次真实路径解析
/// （Windows 上 `canonicalize` 会把 8.3 短名展开成长名，并带 `\\?\` verbatim 前缀），
/// 再把剩下不存在的尾巴按原顺序拼回去；解析不到（相对路径/盘符都拿不到）时退回纯文本规范化。
///
/// 为什么必需：账本条目与工具传入的目标路径**可能来自不同解析链**——一侧经
/// `fs::canonicalize`（短名→长名、加 verbatim 前缀）另一侧是原始文本，纯文本前缀比较
/// 会把同一条路径判成「账本外」，累计三次即硬停本轮执行（本机实测：`tempfile` 给出
/// `C:\Users\YANGZH~1\…` 而 `canonicalize` 得到 `C:\Users\Yangzhenbiao\…`）。
/// 代价是每条目 1~N 次 `canonicalize` 系统调用，仅发生在目标档执行期的闸门判定上。
fn normalize_existing(raw: &str) -> String {
    let plain = normalize_path(raw);
    if plain.is_empty() {
        return plain;
    }
    let mut probe = std::path::PathBuf::from(raw.trim());
    let mut tail: Vec<String> = Vec::new();
    loop {
        if let Ok(resolved) = std::fs::canonicalize(&probe) {
            let mut out = normalize_path(&resolved.to_string_lossy());
            for seg in tail.iter().rev() {
                out.push('/');
                out.push_str(seg);
            }
            return out;
        }
        match (probe.parent(), probe.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_string_lossy().to_ascii_lowercase());
                probe = parent.to_path_buf();
            }
            // 走到相对路径尽头仍解析不到：退回纯文本形态（保守但不会误判为系统路径）
            _ => return plain,
        }
    }
}

/// 去掉 Windows 盘符前缀（`c:/windows` → `/windows`），使系统路径表对所有盘符生效。
fn strip_drive(norm: &str) -> &str {
    let b = norm.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        &norm[2..]
    } else {
        norm
    }
}

/// 家目录（归一化形态；`USERPROFILE` / `HOME` 任一存在即计入）。
fn home_dirs() -> Vec<String> {
    ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| normalize_path(&v))
        .filter(|v| !v.is_empty() && v != "/")
        .collect()
}

/// 路径前缀匹配（归一化后比较；边界必须是路径分隔符，防 `/src2` 命中 `/src`）。
pub fn path_prefix_match(allowed: &str, candidate: &str) -> bool {
    let a = normalize_path(allowed);
    let c = normalize_path(candidate);
    if a.is_empty() || c.is_empty() {
        return false;
    }
    if a == c {
        return true;
    }
    c.starts_with(&a) && c.as_bytes().get(a.len()) == Some(&b'/')
}

/// 程序白名单匹配：归一化（小写、去 `.exe`、反斜杠转正斜杠）后比较全路径或末段名，
/// 于是账本写 `cargo` 能匹配 `C:\Users\x\.cargo\bin\cargo.exe`，写全路径也能匹配裸名。
pub fn program_match(allowed: &str, candidate: &str) -> bool {
    let a = normalize_program(allowed);
    let c = normalize_program(candidate);
    if a.is_empty() || c.is_empty() {
        return false;
    }
    a == c || basename(&a) == basename(&c)
}

/// 程序名归一化：小写、反斜杠转正斜杠、去 `.exe` 后缀。
fn normalize_program(raw: &str) -> String {
    let s = raw.trim().replace('\\', "/").to_ascii_lowercase();
    let s = s.strip_suffix(".exe").unwrap_or(s.as_str()).to_string();
    if s.is_empty() || s == "/" {
        return String::new();
    }
    s
}

/// 路径末段（无分隔符时即整串）。
fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// 停滞判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallVerdict {
    /// < 5：继续。
    Continue,
    /// 5..=9：注入提醒。
    Nudge,
    /// >= 10：终止本 run。
    Stop,
}

/// 连续无进展轮数 → 停滞动作（阈值常量见 `STALL_NUDGE_AT` / `STALL_STOP_AT`）。
pub fn stall_verdict(streak: u32) -> StallVerdict {
    if streak >= STALL_STOP_AT {
        StallVerdict::Stop
    } else if streak >= STALL_NUDGE_AT {
        StallVerdict::Nudge
    } else {
        StallVerdict::Continue
    }
}

/// 阶段化系统提示块（每步注入；块内已含目标摘要，模型无需再调工具确认现状）。
pub fn render_goal_block(state: Option<&GoalState>) -> String {
    match state {
        None => "\n<goal-mode>目标模式（澄清期）：尚未登记目标。先用只读调研确认现状，然后调用 goal 工具登记目标：text（一句话目标）、criteria（可判定的验收标准）、ledger（本次允许触碰的路径与程序）。澄清期只读，不得修改工作区；目标经用户确认进入执行期后，才允许按账本动工作区。**账本宁宽勿窄，且必须写实**：执行期出现账本外的路径写入或程序会被直接拒绝（子代理的写入与命令同样过账本）、累计 3 次即硬停本轮，而执行期不允许再向用户提问。两栏都要写实：① 路径 = 本次会改动的**每一个目录**（含子代理要动的目录、构建产物与测试快照目录）；② 程序 = 执行期会真正调用的**可执行程序名**（构建/测试/包管理/git/node/python 等），按裸程序名书写（写成 `cargo test` 这种整条命令行匹配不上）。漏列的代价是自停：越界项会记进 blocked 并出现在收尾报告里，用户据此补授权后才能续跑。</goal-mode>".to_string(),
        Some(s) => {
            let body = render_goal_summary(s);
            match s.status {
                GoalStatus::Clarify => format!(
                    "\n<goal-mode>目标模式（澄清期）：目标已登记，尚未进入执行期。可用 goal 工具修订 text/criteria/ledger（整体覆盖旧值），澄清期不得修改工作区。**账本要写实**（宁宽勿窄）：路径栏列全本次会改动的每一个目录，程序栏列全会真正调用的可执行程序名（构建/测试/包管理/git/node/python 等，写裸程序名）——执行期账本外的写入与命令会被直接拒绝（子代理同样过账本）、累计 3 次即自停。\n{body}\n</goal-mode>"
                ),
                GoalStatus::Executing => format!(
                    "\n<goal-mode>目标模式（执行期）：只做达成目标所必需的最小改动，越界即停。规则：① 只改账本内路径、只跑账本内程序，越界请求会被拒绝（累计 3 次后硬停）；② 目标 text、验收标准标题与账本已锁定，改则报 E_GOAL_CONTRACT_LOCKED；③ 每完成一条验收标准立即用 goal 工具把该条 done 置 true；④ 全部标准完成后把 status 置 done；⑤ 遇到无法自行解决的阻塞：记入 blocked 并停下报告，不得绕过。\n{body}\n</goal-mode>"
                ),
                GoalStatus::Paused => format!(
                    "\n<goal-mode>目标模式（执行期 · 已暂停）：等待用户确认，不要继续修改工作区；得到确认后由用户恢复执行。\n{body}\n</goal-mode>"
                ),
                GoalStatus::Done => format!(
                    "\n<goal-mode>目标模式（已完成）：上一个目标已达成，工作区不再有执行期授权。如需继续，请重新登记新目标。\n{body}\n</goal-mode>"
                ),
                GoalStatus::Aborted => format!(
                    "\n<goal-mode>目标模式（已中止）：上一个目标已中止，工作区不再有执行期授权。如需继续，请重新登记新目标。\n{body}\n</goal-mode>"
                ),
            }
        }
    }
}

/// 纯文本汇总（计划卡 / 收尾报告 / tool_result 共用；空的分节直接省略）。
pub fn render_goal_summary(state: &GoalState) -> String {
    let mut out = String::new();
    out.push_str(&format!("目标：{}\n", state.text));
    out.push_str(&format!(
        "状态：{}（第 {} 轮）\n",
        state.status.label(),
        state.rounds
    ));
    let done = state.criteria.iter().filter(|c| c.done).count();
    out.push_str(&format!("验收标准（{done}/{}）：\n", state.criteria.len()));
    if state.criteria.is_empty() {
        out.push_str("- （尚未定义验收标准）\n");
    } else {
        for (i, c) in state.criteria.iter().enumerate() {
            let mark = if c.done { "x" } else { " " };
            out.push_str(&format!("- [{mark}] {}. {}\n", i + 1, c.title));
        }
    }
    if !state.ledger.paths.is_empty() || !state.ledger.programs.is_empty() {
        out.push_str(&format!(
            "账本：路径 {} 项{}；程序 {} 项{}\n",
            state.ledger.paths.len(),
            listing(&state.ledger.paths),
            state.ledger.programs.len(),
            listing(&state.ledger.programs),
        ));
    }
    push_section(&mut out, "决策", &state.decisions);
    push_section(&mut out, "待办", &state.pending);
    push_section(&mut out, "阻塞", &state.blocked);
    out.trim_end().to_string()
}

/// 追加一个分节（空列表不输出）。
fn push_section(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("{title}：\n"));
    for it in items {
        out.push_str(&format!("- {it}\n"));
    }
}

/// 内联列举（最多 5 项，其余折叠为「等 N 项」）。
fn listing(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let head: Vec<&str> = items.iter().take(5).map(|s| s.as_str()).collect();
    let more = items.len().saturating_sub(head.len());
    if more == 0 {
        format!("（{}）", head.join("、"))
    } else {
        format!("（{} 等 {} 项）", head.join("、"), items.len())
    }
}

// ---------- 执行期账本闸门（运行时消费点） ----------
//
// 本节的函数是「执行期零弹窗」的合法性来源：文件写入与命令执行在放行前一律过账本闸门，
// 账本内放行、账本外即拒（记一次拒绝，达上限请求硬停），全程不问人。
// 非目标档 / 目标档澄清期 / 未登记目标一律直接放行——**绝不干预其它档位**（回归红线）。

/// 目标档执行期判定（各消费点共用的单一判定入口）：会话当前是目标档**且**目标处于执行期。
/// 非目标档 / 澄清期 / 未登记目标一律 false；「绝不干预其它档位」的红线由它把关。
pub fn goal_execute_phase(rt: &super::SessionRuntime) -> bool {
    if rt.prefs().approval_mode != crate::core::prefs::ApprovalMode::Goal {
        return false;
    }
    goal_phase(rt.goal_snapshot().as_ref()) == Some(GoalPhase::Execute)
}

/// 账本闸门的**作用域 runtime**：目标状态只属于主会话（子代理 / 任务运行不持有自己的目标
/// ——`new_sub` 不继承 `goal`，`SUB_BASE_EXCLUDES` 也排除了 `goal` 工具），故判定一律取
/// **根会话**（父会话）的账本。
///
/// 为什么必须这样取：子代理的档位跟随根会话（`drive::refresh_subagent_mode`），执行期
/// `goal_execute_phase` 对子代理也为 true，而闸门若读子 runtime 自己的目标（恒为 `None`）
/// 就会**直接放行**——「账本 = 执行期改动面的唯一约束」在并行子代理下失效（子代理可写
/// 账本外路径）。取根会话还顺带统一收敛口径：子代理的越界记在根会话的计数上，达上限由
/// 根会话自停出报告（子代理自己的 abort 标记无人消费）。
///
/// 取不到根会话（会话已删除 / 未注册）时退回 `rt` 自身：宁可放行也不 panic，此时子代理
/// 已随根会话的取消链收尾（`drive::cancel_session_subagents`）。
pub fn goal_gate_rt(
    core: &super::AgentCore,
    rt: &std::sync::Arc<super::SessionRuntime>,
) -> std::sync::Arc<super::SessionRuntime> {
    if rt.is_main_session {
        return rt.clone();
    }
    rt.root_session_id
        .as_deref()
        .and_then(|root| core.session(root))
        .unwrap_or_else(|| rt.clone())
}

/// 目标档执行期的账本闸门：命中放行；越界则记一次拒绝（`ledger_denials` +1，越界项记入
/// `blocked`，落边车）并在达到 `LEDGER_DENIAL_RETRY_LIMIT` 时请求硬停（驱动层在 step
/// 边界收尾出报告）。
///
/// 返回 `Err(错误码文本)` = 应拒绝该次工具调用（文案由调用点用 `ledger_denial_message` 拼）。
/// 非目标档 / 澄清期 / 未登记目标一律 `Ok(())`——绝不干预其它档位（回归红线）。
/// **`rt` 传的是作用域 runtime**（子代理 / 任务运行由调用点经 `goal_gate_rt` 换成根会话）。
pub fn ledger_gate(
    rt: &super::SessionRuntime,
    target: LedgerTarget<'_>,
) -> Result<(), &'static str> {
    if !goal_execute_phase(rt) {
        return Ok(());
    }
    let Some(state) = rt.goal_snapshot() else {
        return Ok(());
    };
    if ledger_allows(&state.ledger, target) == LedgerVerdict::Allowed {
        return Ok(());
    }
    // 越界记账：计数 +1（自停阈值依据）**并记下越界了什么**——收尾报告与汇总的「阻塞」
    // 分节据此告诉用户该往账本的哪一栏补条目（「账本漂移 → 自停 → 用户回来补授权」闭环的
    // 最后一环）。走 `mutate_goal`（读-改-写全程持锁）：并行子代理与根会话共用同一份目标
    // 状态，「snapshot → 改 → set」会互相覆盖计数，自停阈值因此可能永远差一两次。
    let item = ledger_denial_item(target);
    let Some(state) = rt.mutate_goal(move |s| {
        s.ledger_denials = s.ledger_denials.saturating_add(1);
        if !s.blocked.contains(&item) {
            s.blocked.push(item);
        }
    }) else {
        return Ok(());
    };
    persist_goal(rt, &state);
    if state.ledger_denials >= LEDGER_DENIAL_RETRY_LIMIT {
        request_abort_if_consumable(rt);
    }
    Err("E_GOAL_OUTSIDE_LEDGER")
}

/// 越界项的可读文本（记入 `blocked`，收尾报告与 `render_goal_summary` 原样展示）。
/// 带上「路径 / 程序」的区分：用户据此知道该补账本的哪一栏（两栏是独立的判定表）。
fn ledger_denial_item(target: LedgerTarget<'_>) -> String {
    match target {
        LedgerTarget::Path(p) => format!("账本外操作被拒（路径）：{p}"),
        LedgerTarget::Program(p) => format!("账本外操作被拒（程序）：{p}"),
    }
}

/// 请求硬停（仅主会话）：子代理 / 任务运行没有自己的 run 循环，`take_goal_abort` 只在主会话
/// 的 step 边界轮询——置在它们身上只是脏标记（无人消费），收敛统一由根会话负责。
fn request_abort_if_consumable(rt: &super::SessionRuntime) {
    if rt.is_main_session {
        rt.request_goal_abort();
    }
}

/// 越界拒绝的模型侧文案（各调用点共用，措辞一致）：带上越界次数与后续指引。
pub fn ledger_denial_message(rt: &super::SessionRuntime, target: &str) -> String {
    let denials = rt.goal_snapshot().map(|g| g.ledger_denials).unwrap_or(0);
    let tail = if denials >= LEDGER_DENIAL_RETRY_LIMIT {
        "已达越界上限，本轮执行已请求停止：请停下并说明情况，由用户确认后再继续。"
    } else {
        "请改用账本内的路径 / 程序；越界累计到上限会直接停止本轮执行。"
    };
    format!(
        "目标档执行期越界：{target} 不在本次目标的账本授权范围内（第 {denials} 次越界）。{tail}"
    )
}

/// 目标档执行期的硬拦记录（L1/L2 硬拦、L3 灾难与高危命令）：把该项追加进 `blocked`（去重）、
/// 落边车，并请求硬停——驱动层在 step 边界消费 `take_goal_abort` 收尾出报告。
/// 非目标档执行期一律 no-op（绝不干预其它档位）；非主会话（子代理 / 任务运行）不落边车、
/// 不请求硬停（子代理的 abort 标记无人消费），由根会话的闸门收敛。
/// **`rt` 传的是作用域 runtime**（子代理 / 任务运行由调用点经 `goal_gate_rt` 换成根会话）。
pub fn goal_hard_block(rt: &super::SessionRuntime, item: impl Into<String>) {
    if !goal_execute_phase(rt) {
        return;
    }
    // 目标缺席（并发清除）时 `mutate_goal` 返回 None：no-op，绝不凭空登记目标
    let item = item.into();
    let Some(state) = rt.mutate_goal(move |s| {
        if !s.blocked.contains(&item) {
            s.blocked.push(item);
        }
    }) else {
        return;
    };
    persist_goal(rt, &state);
    request_abort_if_consumable(rt);
}

/// 前档快照补记判定（纯函数便于单测）：经 ask 批准**从其它档位切进目标档**时，
/// 打开 ask 时的档位才是「批准前档位」——这条路径不过 `transition_prefs`，得在此补记。
/// 返回 `Some(mode)` = 应记为前档；`None` = 无需补记：所选档位不是目标档 /
/// 打开时已是目标档（进档点已记过）/ 已有快照（不覆盖）。
pub fn prev_mode_to_record(
    mode_at_open: crate::core::prefs::ApprovalMode,
    switch_target: crate::core::prefs::ApprovalMode,
    prev: Option<crate::core::prefs::ApprovalMode>,
) -> Option<crate::core::prefs::ApprovalMode> {
    use crate::core::prefs::ApprovalMode;
    if switch_target != ApprovalMode::Goal || mode_at_open == ApprovalMode::Goal || prev.is_some() {
        return None;
    }
    Some(mode_at_open)
}

/// 命令文本 → 程序名（账本程序判定的入口）：首个 token，先剥引号与空白。
/// 放在本模块是因为 command / service 两个工具都要用，而它们互不依赖；
/// 引号包裹的可执行路径（`"C:\Program Files\x.exe" -a`）取到配对的收尾引号为止。
pub fn program_of_command(cmd: &str) -> String {
    let s = cmd.trim();
    for q in ['"', '\''] {
        if let Some(rest) = s.strip_prefix(q) {
            if let Some(end) = rest.find(q) {
                return rest[..end].trim().to_string();
            }
        }
    }
    s.split_whitespace().next().unwrap_or_default().to_string()
}

/// 目标状态落边车（`ledger_gate` / `goal_hard_block` 手边只有 runtime，没有 `AgentCore`）：
/// store 由 `rt.data_dir` 重建——与 `AgentCore::new` 用的是同一个 `sessions/` 目录。
/// 失败只记 warn：边车是辅助视图，绝不阻断工具调用链。
fn persist_goal(rt: &super::SessionRuntime, goal: &GoalState) {
    // 子代理 / 任务运行没有自己的目标边车：闸门的作用域 runtime 是根会话（`goal_gate_rt`），
    // 正常路径根本走不到这里；降级路径（根会话已删除）下更要挡住——`sub_*.goal.json` 是孤儿
    // 文件（清理链刻意不碰 `sub_` / `task_` 前缀，见 `core::sessions::cleanup`），落下即永驻。
    if !rt.is_main_session {
        return;
    }
    let store = crate::core::sessions::SessionStore::new(rt.data_dir.clone());
    if let Err(e) = store.save_goal(&rt.id, &Some(goal.clone())) {
        tracing::warn!("session {} 目标边车落盘失败：{e}", rt.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: GoalStatus) -> GoalState {
        GoalState {
            text: "把 X 改成 Y".into(),
            criteria: vec![
                GoalCriterion {
                    title: "改完 X".into(),
                    done: false,
                },
                GoalCriterion {
                    title: "测试通过".into(),
                    done: true,
                },
            ],
            ledger: GoalLedger {
                paths: vec!["/work/proj/src".into()],
                programs: vec!["cargo".into()],
            },
            status,
            decisions: Vec::new(),
            pending: Vec::new(),
            blocked: Vec::new(),
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
        }
    }

    // ---------- 状态与阶段 ----------

    #[test]
    fn status_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&GoalStatus::Executing).unwrap(),
            r#""executing""#
        );
        let s: GoalStatus = serde_json::from_str(r#""aborted""#).unwrap();
        assert_eq!(s, GoalStatus::Aborted);
    }

    #[test]
    fn state_serde_defaults_missing_optional_fields() {
        // 边车/前端只给必需字段时，可选计数字段回落 0（serde default 向前兼容）
        let s: GoalState =
            serde_json::from_str(r#"{"text":"t","criteria":[],"ledger":{},"status":"clarify"}"#)
                .unwrap();
        assert!(s.decisions.is_empty() && s.pending.is_empty() && s.blocked.is_empty());
        assert_eq!((s.rounds, s.stall_streak, s.ledger_denials), (0, 0, 0));
        // 账本的两个列表同样可缺省
        assert!(s.ledger.paths.is_empty() && s.ledger.programs.is_empty());
    }

    #[test]
    fn phase_matrix() {
        assert_eq!(goal_phase(None), Some(GoalPhase::Clarify));
        assert_eq!(
            goal_phase(Some(&state(GoalStatus::Clarify))),
            Some(GoalPhase::Clarify)
        );
        assert_eq!(
            goal_phase(Some(&state(GoalStatus::Executing))),
            Some(GoalPhase::Execute)
        );
        assert_eq!(
            goal_phase(Some(&state(GoalStatus::Paused))),
            Some(GoalPhase::Execute)
        );
        assert_eq!(
            goal_phase(Some(&state(GoalStatus::Done))),
            Some(GoalPhase::Clarify)
        );
        assert_eq!(
            goal_phase(Some(&state(GoalStatus::Aborted))),
            Some(GoalPhase::Clarify)
        );
    }

    #[test]
    fn registration_flag_is_presence_only() {
        assert!(!is_registered(None));
        assert!(is_registered(Some(&state(GoalStatus::Clarify))));
        assert!(is_registered(Some(&state(GoalStatus::Aborted))));
    }

    // ---------- 停滞判定 ----------

    #[test]
    fn stall_thresholds() {
        assert_eq!(stall_verdict(0), StallVerdict::Continue);
        assert_eq!(stall_verdict(4), StallVerdict::Continue);
        assert_eq!(stall_verdict(STALL_NUDGE_AT), StallVerdict::Nudge);
        assert_eq!(stall_verdict(9), StallVerdict::Nudge);
        assert_eq!(stall_verdict(STALL_STOP_AT), StallVerdict::Stop);
        assert_eq!(stall_verdict(999), StallVerdict::Stop);
    }

    // ---------- 账本：路径 ----------

    #[test]
    fn path_prefix_matches_at_component_boundary() {
        assert!(path_prefix_match("/w/src", "/w/src"));
        assert!(path_prefix_match("/w/src", "/w/src/a.rs"));
        assert!(path_prefix_match("/w/src/", "/w/src/a.rs"));
        // 词法前缀不算命中（/w/src2 不在 /w/src 之内）
        assert!(!path_prefix_match("/w/src", "/w/src2/a.rs"));
        assert!(!path_prefix_match("/w/src", "/w/other"));
        assert!(!path_prefix_match("", "/w/src"));
    }

    #[test]
    fn windows_backslashes_and_case_are_normalized() {
        assert!(path_prefix_match(r"D:\Work\Proj", r"d:\work\proj\src\a.rs"));
        assert!(path_prefix_match("/Work/Proj", "/work/proj"));
    }

    #[test]
    fn ledger_path_inside_is_allowed_outside_is_rejected() {
        let ledger = GoalLedger {
            paths: vec![r"D:\Work\Proj\src".into()],
            programs: vec![],
        };
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Path(r"d:\work\proj\src\a.rs")),
            LedgerVerdict::Allowed
        );
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Path(r"D:\Work\Proj\docs\a.md")),
            LedgerVerdict::Outside
        );
    }

    #[test]
    fn forbidden_paths_are_never_allowed_even_when_listed() {
        let forbidden = [
            "/",
            "",
            "  ",
            "C:",
            "C:\\",
            "D:/",
            "C:\\Windows\\System32",
            "/etc/passwd",
            "/usr/bin/env",
            "/bin/sh",
            "/System/Library/x",
            "C:\\Program Files\\x",
            "src/../../etc/passwd",
        ];
        for f in forbidden {
            assert!(is_forbidden_path(f), "{f} 应被判为禁用路径");
        }
        // 账本里即使写了禁用条目也不生效（脏条目 ≠ 整盘授权）
        for entry in ["/", "C:\\", "/etc", "/usr"] {
            let ledger = GoalLedger {
                paths: vec![entry.into()],
                programs: vec![],
            };
            assert_eq!(
                ledger_allows(&ledger, LedgerTarget::Path("/etc/passwd")),
                LedgerVerdict::Outside,
                "账本条目 {entry} 不得放行 /etc/passwd"
            );
            assert_eq!(
                ledger_allows(&ledger, LedgerTarget::Path("C:\\Users\\x\\a.txt")),
                LedgerVerdict::Outside,
                "账本条目 {entry} 不得放行盘上任意文件"
            );
        }
    }

    #[test]
    fn home_dir_itself_is_forbidden_but_subdirs_are_not() {
        let home = ["USERPROFILE", "HOME"]
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .expect("测试环境必有 HOME 或 USERPROFILE");
        assert!(is_forbidden_path(&home), "家目录本身必须被拒");
        assert!(is_forbidden_path(&format!(
            "{home}{}",
            std::path::MAIN_SEPARATOR
        )));
        let sub = format!("{home}{}proj", std::path::MAIN_SEPARATOR);
        assert!(!is_forbidden_path(&sub), "家目录下的子目录不是禁用路径");
        let ledger = GoalLedger {
            paths: vec![sub.clone()],
            programs: vec![],
        };
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Path(&format!("{sub}/a.rs"))),
            LedgerVerdict::Allowed
        );
    }

    #[test]
    fn ordinary_temp_paths_are_not_forbidden() {
        // 临时目录（Windows 在 C:\Users\...\AppData\Local\Temp、macOS 在 /var/folders/...）
        // 必须可用：系统路径表刻意不含 /var 与 /tmp
        assert!(!is_forbidden_path("/tmp/x"));
        assert!(!is_forbidden_path("/var/folders/ab/T/x"));
        assert!(!is_forbidden_path("C:\\Users\\x\\AppData\\Local\\Temp\\x"));
        assert!(!is_forbidden_path(r"D:\Work\SideProjects\CodeWave"));
    }

    // ---------- 账本：程序 ----------

    #[test]
    fn program_matching_by_name_or_full_path() {
        let ledger = GoalLedger {
            paths: vec![],
            programs: vec!["cargo".into()],
        };
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Program("cargo")),
            LedgerVerdict::Allowed
        );
        assert_eq!(
            ledger_allows(
                &ledger,
                LedgerTarget::Program(r"C:\Users\x\.cargo\bin\cargo.exe")
            ),
            LedgerVerdict::Allowed
        );
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Program("rm")),
            LedgerVerdict::Outside
        );
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Program("")),
            LedgerVerdict::Outside
        );
        // 全路径条目也能匹配裸程序名
        let ledger = GoalLedger {
            paths: vec![],
            programs: vec![r"C:\Python\python.exe".into()],
        };
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Program("python")),
            LedgerVerdict::Allowed
        );
        assert!(!program_match("cargo", "cargo-clippy"));
    }

    #[test]
    fn empty_ledger_rejects_everything() {
        let ledger = GoalLedger::default();
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Path("/w/a.rs")),
            LedgerVerdict::Outside
        );
        assert_eq!(
            ledger_allows(&ledger, LedgerTarget::Program("cargo")),
            LedgerVerdict::Outside
        );
    }

    // ---------- 渲染 ----------

    #[test]
    fn summary_lists_criteria_and_progress() {
        let mut s = state(GoalStatus::Executing);
        s.decisions.push("用 A 方案".into());
        s.pending.push("补测试".into());
        s.blocked.push("缺凭证".into());
        s.rounds = 3;
        let r = render_goal_summary(&s);
        assert!(r.contains("目标：把 X 改成 Y"), "{r}");
        assert!(r.contains("状态：执行中（第 3 轮）"), "{r}");
        assert!(r.contains("验收标准（1/2）："), "{r}");
        assert!(r.contains("- [ ] 1. 改完 X"), "{r}");
        assert!(r.contains("- [x] 2. 测试通过"), "{r}");
        assert!(r.contains("账本：路径 1 项"), "{r}");
        assert!(r.contains("决策：\n- 用 A 方案"), "{r}");
        assert!(r.contains("待办：\n- 补测试"), "{r}");
        assert!(r.contains("阻塞：\n- 缺凭证"), "{r}");
    }

    #[test]
    fn summary_omits_empty_sections() {
        let s = state(GoalStatus::Clarify);
        let r = render_goal_summary(&s);
        assert!(!r.contains("决策："), "{r}");
        assert!(!r.contains("待办："), "{r}");
        assert!(!r.contains("阻塞："), "{r}");
        // 空验收标准也要明确说明，避免模型以为「无标准 = 已完成」
        let mut empty = state(GoalStatus::Clarify);
        empty.criteria.clear();
        assert!(render_goal_summary(&empty).contains("（尚未定义验收标准）"));
    }

    #[test]
    fn block_covers_every_status() {
        assert!(render_goal_block(None).contains("<goal-mode>"));
        for st in [
            GoalStatus::Clarify,
            GoalStatus::Executing,
            GoalStatus::Paused,
            GoalStatus::Done,
            GoalStatus::Aborted,
        ] {
            let b = render_goal_block(Some(&state(st)));
            assert!(b.starts_with("\n<goal-mode>"), "{st:?}");
            assert!(b.ends_with("</goal-mode>"), "{st:?}");
            assert!(b.contains("目标：把 X 改成 Y"), "{st:?} 摘要未注入：{b}");
            assert!(b.contains(st.label()), "{st:?} 缺状态名：{b}");
        }
        // 执行期块必须点明合同锁定与账本约束（模型据此避免无效重试）
        let exec = render_goal_block(Some(&state(GoalStatus::Executing)));
        assert!(exec.contains("E_GOAL_CONTRACT_LOCKED"), "{exec}");
        assert!(exec.contains("账本"), "{exec}");
    }

    // ---------- 执行期账本闸门（运行时消费点） ----------

    use crate::core::prefs::ApprovalMode;

    /// 闸门夹具：真实（临时目录）runtime + 指定档位 + 指定目标状态与账本。
    /// 账本路径 = 工作区（`ws`）；越界样本用另一个临时目录（`outside`）。
    struct GateFixture {
        rt: std::sync::Arc<super::super::SessionRuntime>,
        ws: tempfile::TempDir,
        outside: tempfile::TempDir,
        data_dir: tempfile::TempDir,
    }

    fn gate_fixture(mode: ApprovalMode, status: Option<GoalStatus>) -> GateFixture {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let rt = crate::core::agent::test_support::make_runtime(
            std::fs::canonicalize(ws.path()).unwrap(),
            dd.path().to_path_buf(),
            vec![],
        );
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: mode,
            model_id: None,
            reasoning_effort: None,
        });
        if let Some(status) = status {
            let mut g = state(status);
            g.ledger = GoalLedger {
                paths: vec![
                    std::fs::canonicalize(ws.path())
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                ],
                programs: vec!["cargo".into()],
            };
            rt.set_goal(Some(g));
        }
        GateFixture {
            rt,
            ws,
            outside,
            data_dir: dd,
        }
    }

    /// 账本内的写入目标（工作区内）。
    fn inside_path(f: &GateFixture) -> String {
        f.ws.path()
            .join("src")
            .join("a.rs")
            .to_string_lossy()
            .into_owned()
    }

    /// 账本外的写入目标（另一临时目录）。
    fn outside_path(f: &GateFixture) -> String {
        f.outside.path().join("a.rs").to_string_lossy().into_owned()
    }

    /// ① 账本内路径 / 程序放行，账本外被拒（错误码正确）；拒绝计数落内存 + 边车。
    #[test]
    fn ledger_gate_allows_inside_and_rejects_outside() {
        let f = gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Executing));
        assert_eq!(
            ledger_gate(&f.rt, LedgerTarget::Path(&inside_path(&f))),
            Ok(())
        );
        let err = ledger_gate(&f.rt, LedgerTarget::Path(&outside_path(&f)))
            .expect_err("账本外路径必须被拒");
        assert_eq!(err, "E_GOAL_OUTSIDE_LEDGER");
        assert_eq!(f.rt.goal_snapshot().unwrap().ledger_denials, 1);
        // ② 程序名同理
        assert_eq!(ledger_gate(&f.rt, LedgerTarget::Program("cargo")), Ok(()));
        assert_eq!(
            ledger_gate(&f.rt, LedgerTarget::Program("rm")),
            Err("E_GOAL_OUTSIDE_LEDGER")
        );
        assert_eq!(f.rt.goal_snapshot().unwrap().ledger_denials, 2);
        // 🟡-4：越界项要能被用户看到（收尾报告 / 汇总的「阻塞」分节）——按路径 / 程序区分
        let blocked = f.rt.goal_snapshot().unwrap().blocked;
        assert!(
            blocked.iter().any(|b| b.contains("账本外操作被拒（路径）")),
            "{blocked:?}"
        );
        assert!(
            blocked
                .iter()
                .any(|b| b.contains("账本外操作被拒（程序）：rm")),
            "{blocked:?}"
        );
        // 同一项反复越界不重复堆行（去重），但计数照记
        assert_eq!(
            ledger_gate(&f.rt, LedgerTarget::Program("rm")),
            Err("E_GOAL_OUTSIDE_LEDGER")
        );
        assert_eq!(f.rt.goal_snapshot().unwrap().ledger_denials, 3);
        assert_eq!(
            f.rt.goal_snapshot().unwrap().blocked.len(),
            2,
            "越界项应去重"
        );
        // 拒绝必须落边车（不是只改内存）
        let store = crate::core::sessions::SessionStore::new(f.data_dir.path().to_path_buf());
        assert_eq!(store.load_goal(&f.rt.id).unwrap().ledger_denials, 3);
        // 拒绝文案带上次数
        assert!(ledger_denial_message(&f.rt, "rm").contains("第 3 次越界"));
    }

    /// ③ 回归红线：非目标档 / 澄清期 / 未登记目标一律不受干预（不计数、不硬停）。
    #[test]
    fn ledger_gate_never_touches_other_modes_or_clarify() {
        for mode in [
            ApprovalMode::ConfirmEach,
            ApprovalMode::AutoEdit,
            ApprovalMode::Plan,
            ApprovalMode::FullAccess,
        ] {
            let f = gate_fixture(mode, Some(GoalStatus::Executing));
            assert_eq!(
                ledger_gate(&f.rt, LedgerTarget::Path(&outside_path(&f))),
                Ok(()),
                "{mode:?} 不得被账本闸门干预"
            );
            assert_eq!(ledger_gate(&f.rt, LedgerTarget::Program("rm")), Ok(()));
            assert_eq!(f.rt.goal_snapshot().unwrap().ledger_denials, 0, "{mode:?}");
            assert!(!f.rt.take_goal_abort(), "{mode:?} 不得被请求硬停");
        }
        // 目标档但非执行期：澄清期 / Done / Aborted（后两者归澄清期）
        for status in [GoalStatus::Clarify, GoalStatus::Done, GoalStatus::Aborted] {
            let f = gate_fixture(ApprovalMode::Goal, Some(status));
            assert_eq!(
                ledger_gate(&f.rt, LedgerTarget::Path(&outside_path(&f))),
                Ok(()),
                "{status:?} 不得被账本闸门干预"
            );
            assert_eq!(
                f.rt.goal_snapshot().unwrap().ledger_denials,
                0,
                "{status:?}"
            );
            assert!(!f.rt.take_goal_abort(), "{status:?}");
        }
        // 未登记目标
        let f = gate_fixture(ApprovalMode::Goal, None);
        assert_eq!(
            ledger_gate(&f.rt, LedgerTarget::Path(&outside_path(&f))),
            Ok(())
        );
        assert!(f.rt.goal_snapshot().is_none(), "不得凭空登记目标");
        assert!(!f.rt.take_goal_abort());
        // 执行期判定矩阵
        assert!(!goal_execute_phase(&f.rt));
        let f = gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Executing));
        assert!(goal_execute_phase(&f.rt));
        let f = gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Paused));
        assert!(goal_execute_phase(&f.rt), "暂停仍属执行期");
    }

    /// ④ 连续越界达上限 → 请求硬停；未达上限不得硬停。
    #[test]
    fn ledger_gate_denials_reach_limit_and_request_abort() {
        let f = gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Executing));
        let outside = outside_path(&f);
        for n in 1..LEDGER_DENIAL_RETRY_LIMIT {
            assert_eq!(
                ledger_gate(&f.rt, LedgerTarget::Path(&outside)),
                Err("E_GOAL_OUTSIDE_LEDGER")
            );
            assert!(!f.rt.take_goal_abort(), "第 {n} 次越界不该硬停");
        }
        assert_eq!(
            ledger_gate(&f.rt, LedgerTarget::Path(&outside)),
            Err("E_GOAL_OUTSIDE_LEDGER")
        );
        assert!(
            f.rt.take_goal_abort(),
            "达 LEDGER_DENIAL_RETRY_LIMIT 必须请求硬停"
        );
        assert_eq!(
            f.rt.goal_snapshot().unwrap().ledger_denials,
            LEDGER_DENIAL_RETRY_LIMIT
        );
        // 硬停标记不改变闸门判定（由驱动层消费）；账本内仍放行
        assert_eq!(
            ledger_gate(&f.rt, LedgerTarget::Path(&inside_path(&f))),
            Ok(())
        );
    }

    /// ⑤（硬拦侧）目标档执行期硬拦：blocked 记录（去重）+ 请求硬停；其它档位 no-op。
    #[test]
    fn goal_hard_block_records_item_and_requests_abort() {
        let f = gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Executing));
        goal_hard_block(&f.rt, "高危命令被硬拦：git push --force");
        goal_hard_block(&f.rt, "高危命令被硬拦：git push --force");
        let g = f.rt.goal_snapshot().unwrap();
        assert_eq!(
            g.blocked,
            vec!["高危命令被硬拦：git push --force".to_string()],
            "blocked 记录应去重"
        );
        assert!(f.rt.take_goal_abort());
        let store = crate::core::sessions::SessionStore::new(f.data_dir.path().to_path_buf());
        assert_eq!(
            store.load_goal(&f.rt.id).unwrap().blocked.len(),
            1,
            "应落边车"
        );
        // 澄清期 / 非目标档：no-op
        let f2 = gate_fixture(ApprovalMode::AutoEdit, Some(GoalStatus::Executing));
        goal_hard_block(&f2.rt, "x");
        assert!(f2.rt.goal_snapshot().unwrap().blocked.is_empty());
        assert!(!f2.rt.take_goal_abort());
    }

    // ---------- 闸门作用域：子代理 / 任务运行取根会话的账本 ----------

    /// 子代理闸门夹具：真实 core + 主会话 runtime（已注册）+ `new_sub` 派生的子代理 runtime。
    struct SubGateFixture {
        core: std::sync::Arc<crate::core::agent::AgentCore>,
        main: std::sync::Arc<super::super::SessionRuntime>,
        sub: std::sync::Arc<super::super::SessionRuntime>,
        ws: tempfile::TempDir,
        outside: tempfile::TempDir,
        data_dir: tempfile::TempDir,
    }

    fn sub_gate_fixture(mode: ApprovalMode, status: Option<GoalStatus>) -> SubGateFixture {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let main = core.get_or_create_session(
            "goal-sub-main",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        main.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: mode,
            model_id: None,
            reasoning_effort: None,
        });
        if let Some(status) = status {
            let mut g = state(status);
            g.ledger = GoalLedger {
                paths: vec![
                    std::fs::canonicalize(ws.path())
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                ],
                programs: vec!["cargo".into()],
            };
            main.set_goal(Some(g));
        }
        let sub = super::super::SessionRuntime::new_sub(&main, "sub_gate_probe".to_string());
        SubGateFixture {
            core,
            main,
            sub,
            ws,
            outside,
            data_dir: dd,
        }
    }

    /// ⑥（🟡-1）闸门作用域 = 根会话：子代理 runtime 自身不持有目标，判定必须取父会话的账本
    /// ——否则「档位跟随为执行期 + 子 runtime 无目标」会让子代理的写入直接放行。
    #[test]
    fn ledger_gate_scope_follows_root_session() {
        let f = sub_gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Executing));
        let gov = goal_gate_rt(&f.core, &f.sub);
        assert!(
            std::sync::Arc::ptr_eq(&gov, &f.main),
            "子代理的账本作用域必须是根会话"
        );
        assert!(goal_execute_phase(&gov), "档位跟随根会话 → 执行期");
        // 账本外路径：拒（修复前读子 runtime 自己的目标 → None → 直接放行）
        let outside = f.outside.path().join("a.rs").to_string_lossy().into_owned();
        assert_eq!(
            ledger_gate(&gov, LedgerTarget::Path(&outside)),
            Err("E_GOAL_OUTSIDE_LEDGER")
        );
        // 账本内路径：放行
        let inside =
            f.ws.path()
                .join("src")
                .join("a.rs")
                .to_string_lossy()
                .into_owned();
        assert_eq!(ledger_gate(&gov, LedgerTarget::Path(&inside)), Ok(()));
        // 越界记在根会话上（收敛口径一致：达上限由根会话自停出报告）
        let g = f.main.goal_snapshot().unwrap();
        assert_eq!(g.ledger_denials, 1);
        assert!(
            g.blocked
                .iter()
                .any(|b| b.contains("账本外操作被拒（路径）")),
            "{:?}",
            g.blocked
        );
        let store = crate::core::sessions::SessionStore::new(f.data_dir.path().to_path_buf());
        assert_eq!(
            store.load_goal(&f.main.id).unwrap().ledger_denials,
            1,
            "越界落根会话边车"
        );
        assert!(
            !store.goal_path(&f.sub.id).exists(),
            "子代理不得留下 sessions/sub_*.goal.json 脏文件"
        );
    }

    /// ⑦（🟡-1 守卫）非主会话的闸门：判定照常（拒 / 拦），但不落边车、不请求硬停（无人消费）；
    /// 根会话查不到时退回自身（不 panic）。
    #[test]
    fn gate_guards_for_non_main_runtime() {
        let f = sub_gate_fixture(ApprovalMode::Goal, Some(GoalStatus::Executing));
        // 降级路径才会走到「子 runtime 自己带目标」：此时仍不得落 sub_*.goal.json、不得置 abort
        let mut g = state(GoalStatus::Executing);
        g.ledger = GoalLedger {
            paths: vec![],
            programs: vec![],
        };
        f.sub.set_goal(Some(g));
        let outside = f.outside.path().join("a.rs").to_string_lossy().into_owned();
        assert_eq!(
            ledger_gate(&f.sub, LedgerTarget::Path(&outside)),
            Err("E_GOAL_OUTSIDE_LEDGER")
        );
        goal_hard_block(&f.sub, "高危命令被硬拦：git push --force");
        let g = f.sub.goal_snapshot().unwrap();
        assert_eq!(g.ledger_denials, 1, "判定照常（内存态计数）");
        assert!(
            g.blocked.iter().any(|b| b.contains("高危命令被硬拦")),
            "硬拦项仍记内存态 blocked：{:?}",
            g.blocked
        );
        assert!(!f.sub.take_goal_abort(), "非主会话不请求硬停（无人消费）");
        let store = crate::core::sessions::SessionStore::new(f.data_dir.path().to_path_buf());
        assert!(!store.goal_path(&f.sub.id).exists(), "非主会话不落目标边车");
        // 根会话查不到（未注册的 runtime 派生的子代理）→ 退回自身，不 panic；无目标 → 放行
        let standalone = crate::core::agent::test_support::make_runtime(
            std::fs::canonicalize(f.ws.path()).unwrap(),
            f.data_dir.path().to_path_buf(),
            vec![],
        );
        let orphan = super::super::SessionRuntime::new_sub(&standalone, "sub_orphan".to_string());
        let gov = goal_gate_rt(&f.core, &orphan);
        assert!(
            std::sync::Arc::ptr_eq(&gov, &orphan),
            "查不到根会话时退回自身"
        );
        assert_eq!(
            ledger_gate(&gov, LedgerTarget::Path(&outside)),
            Ok(()),
            "无目标 → 放行（绝不 panic）"
        );
    }

    /// 程序名提取：首个 token，先剥引号与空白。
    #[test]
    fn program_of_command_strips_quotes_and_whitespace() {
        assert_eq!(program_of_command("  cargo test  "), "cargo");
        assert_eq!(
            program_of_command("\"C:\\Program Files\\x.exe\" -a"),
            r"C:\Program Files\x.exe"
        );
        assert_eq!(program_of_command("'./run.sh' --fast"), "./run.sh");
        assert_eq!(program_of_command("mkdir"), "mkdir");
        assert_eq!(program_of_command(""), "");
        assert_eq!(program_of_command("   "), "");
    }

    /// ⑧（判定侧）经 ask 从其它档位切进目标档时补记前档；已有快照 / 已是目标档不补。
    #[test]
    fn prev_mode_recorded_only_when_switching_into_goal() {
        assert_eq!(
            prev_mode_to_record(ApprovalMode::Plan, ApprovalMode::Goal, None),
            Some(ApprovalMode::Plan)
        );
        assert_eq!(
            prev_mode_to_record(ApprovalMode::ConfirmEach, ApprovalMode::Goal, None),
            Some(ApprovalMode::ConfirmEach)
        );
        // 已有快照：不覆盖（回落目标不得被改写）
        assert_eq!(
            prev_mode_to_record(
                ApprovalMode::Plan,
                ApprovalMode::Goal,
                Some(ApprovalMode::AutoEdit)
            ),
            None
        );
        // 打开 ask 时已是目标档：进档点已记过
        assert_eq!(
            prev_mode_to_record(ApprovalMode::Goal, ApprovalMode::Goal, None),
            None
        );
        // 所选档位不是目标档：与目标档快照无关
        assert_eq!(
            prev_mode_to_record(ApprovalMode::Plan, ApprovalMode::AutoEdit, None),
            None
        );
        assert_eq!(
            prev_mode_to_record(ApprovalMode::Goal, ApprovalMode::AutoEdit, None),
            None
        );
    }
}
