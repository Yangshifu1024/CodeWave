//! plan 工具：todo 状态机（同一时刻至多一个 in_progress）；plan:update 事件 + 会话持久化。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// todo 状态：待办 / 进行中 / 已完成。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// 待办。
    Pending,
    /// 进行中（同一时刻至多一项）。
    InProgress,
    /// 已完成。
    Completed,
}

/// 单条计划项：标题 + 状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    /// 标题（非空）。
    pub title: String,
    /// 当前状态。
    pub status: TodoStatus,
}

/// plan 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 省略 = 读取当前计划；提供 = 全量替换。
    #[serde(default)]
    todos: Option<Vec<TodoIn>>,
}

/// 单条 todo 入参（wire 形态：状态为字符串）。
#[derive(Deserialize)]
pub struct TodoIn {
    /// 标题。
    title: String,
    /// `pending | in_progress | completed`。
    status: String,
}

/// plan 工具：读写当前会话的计划（todo 列表）。
/// 入参为可选 todos 数组，省略即读取、提供即全量替换；Meta 分级（操作 agent 自身状态）。
/// 批准基线联动：批准后替换计划时按标题集合 diff，新增标题视为计划外步骤并置范围扩张标记，
/// 供批次层在执行前要求用户确认。
pub struct PlanTool;

/// 状态机校验：≤100 项、标题非空、同一时刻至多一个 in_progress。
pub fn validate_todos(todos: &[Todo]) -> Result<(), String> {
    if todos.len() > 100 {
        return Err("todo 列表超过 100 项".into());
    }
    let mut in_progress = 0;
    for t in todos {
        if t.title.trim().is_empty() {
            return Err("todo title 不能为空".into());
        }
        if t.status == TodoStatus::InProgress {
            in_progress += 1;
            if in_progress > 1 {
                return Err("同一时刻最多一个 in_progress 任务".into());
            }
        }
    }
    Ok(())
}

/// 计划是否仍有未完成项（存在任一非 `Completed` 条目）。
///
/// 口径与 `tools::batch::plan_gate_verdict` 的三态判定同源：空列表 = 无计划（不算「未完成」），
/// 非空且全 `Completed` = 已收尾，其余 = 未完成。
///
/// 消费方：`core::agent::drive::text_turn_action` 的主会话分支
/// （[docs/main-run-finish-with-pending-todos](../../../../docs/main-run-finish-with-pending-todos.md)）
/// ——计划未收尾时，「只输出文字、无工具调用」的回合不得被当成「回答完毕」而静默收尾
/// （会话 f19c3890 的 50 字旁白即此形态）。
pub fn has_pending(todos: &[Todo]) -> bool {
    todos.iter().any(|t| t.status != TodoStatus::Completed)
}

/// 渲染为模型可读文本（tool_result 内容）。
pub fn render_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "（计划为空）".into();
    }
    todos
        .iter()
        .map(|t| {
            let mark = match t.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Completed => "[x]",
            };
            format!("{mark} {}", t.title)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait::async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &'static str {
        "plan"
    }
    fn description(&self) -> &'static str {
        "读取或替换当前计划（todo 列表）。传入 {\"todos\":[{\"title\":..., \"status\":\"pending|in_progress|completed\"}]} 即全量更新；传 {} 即读取。同一时刻至多一个任务可处于 in_progress。任何实现类请求（无论大小）都应在开始时调用本工具建立跟踪 todo 列表，并随工作推进保持状态最新；当前计划会自动注入每轮新的用户消息。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "todos": {
      "type": "array",
      "maxItems": 100,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "status"],
        "properties": {
          "title": {"type": "string", "minLength": 1},
          "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
        }
      }
    }
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        match args.todos {
            None => {
                let todos = ctx.rt.todos.lock().unwrap().clone();
                ToolOutcome::ok(json!({ "todos": todos, "rendered": render_todos(&todos) }))
            }
            Some(ins) => {
                let mut todos = Vec::with_capacity(ins.len());
                for t in ins {
                    let status = match t.status.as_str() {
                        "pending" => TodoStatus::Pending,
                        "in_progress" => TodoStatus::InProgress,
                        "completed" => TodoStatus::Completed,
                        other => return ToolOutcome::err("E_ARGS", format!("非法状态：{other}")),
                    };
                    todos.push(Todo {
                        title: t.title.trim().to_string(),
                        status,
                    });
                }
                if let Err(e) = validate_todos(&todos) {
                    return ToolOutcome::err("E_PLAN_INVALID", e);
                }
                // G3（[docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md) §7）：存在批准后基线时，全量替换按标题集合 diff。
                // 判定口径走 classify_new_todos 而非裸 diff：批准时登记的是「文件级改动点」，S6 执行时又要求把
                // 范围切成人名任务包并补验证项，写法必然被改写（`包 A（backend-dev）：新增 core/x.rs` vs
                // `新增 core/x.rs + 单测`）。这类「细化 / 改写」不再算计划外步骤，只有真新增才置范围扩张标记。
                // 纯函数判定在 classify_new_todos（含测试）；重命名按「删 + 增」处理，只对新增告警。
                {
                    let baseline = ctx.rt.approved_plan.lock().unwrap().clone();
                    let current: Vec<String> = todos.iter().map(|t| t.title.clone()).collect();
                    if let Some(baseline) = baseline {
                        let deltas = classify_new_todos(&baseline, &current);
                        if !ctx
                            .rt
                            .scope_allowed
                            .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            if deltas
                                .iter()
                                .any(|(_, k)| matches!(k, NewTodoClass::New(_)))
                            {
                                ctx.rt
                                    .scope_expanded
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                            } else if !deltas.is_empty() {
                                // 静默纳入已批准范围，但留痕：范围是安全门，误报可以放过、误放不能无声无息。
                                // 文案只说「已识别」不说「已并入」：并入 approved_plan 的动作在 batch.rs 的写通道
                                // 门（merge_into_approved_baseline）里，本工具只读基线、不写它。若本 run 没有任何写工具
                                // 命中写通道，这些标题会一直停在「已识别」而未真正并入——日志如实描述现状，
                                // 免得排障时被这条日志误导成「基线已含细化标题」。
                                crate::core::session_log::info(
                                    ctx.rt.as_ref(),
                                    &format!(
                                        "G3：{} 条细化标题已识别（不弹范围确认；首次写操作命中写通道时并入基线）：{}",
                                        deltas.len(),
                                        deltas
                                            .iter()
                                            .map(|(t, _)| crate::core::session_log::trunc(t, 60))
                                            .collect::<Vec<_>>()
                                            .join(" | ")
                                    ),
                                );
                            }
                        }
                    }
                }
                *ctx.rt.todos.lock().unwrap() = todos.clone();
                // 本 run 真的改过计划（[docs/main-run-finish-with-pending-todos](../../../../docs/main-run-finish-with-pending-todos.md) §8.2）：
                // 计划未收尾的收尾门只看本标记，否则上个 run 的陈旧计划会劫持本 run 的普通提问。
                // 置位点在写入成功之后——被档位排除/参数非法的调用并未改变计划。
                ctx.rt
                    .plan_called_this_run
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                // 事件 + 持久化（payload 带 session：RightBar 与任务面板按会话路由，与其他事件一致）
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "plan:update",
                    json!({ "session": ctx.rt.id, "todos": todos }),
                );
                let _ = ctx.core.store.save_todos(&ctx.rt.id, &todos);
                ToolOutcome::ok(json!({ "todos": todos, "rendered": render_todos(&todos) }))
            }
        }
    }
}

/// G3：比较基线与新增 todo 标题，返回新增标题（不在基线中，保持原序，去重）。
/// plan.rs 的告警与 batch.rs 的确认弹窗复用；纯函数便于测试。
///
/// **口径刻意是精确相等**：这是旧接口，batch.rs 仍在用它做「计划外步骤」弹窗清单。
/// 需要抗「写法改写」的细化口径请用 [`classify_new_todos`]（含归一化与细分子串判定）。
pub fn diff_new_todos(baseline: &[String], current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|c| !baseline.iter().any(|b| b == *c))
        .cloned()
        .collect()
}

/// 新增 todo 标题的分类结果（G3 范围确认的判定口径）。
///
/// 存在的意义：批准时冻结的标题（方案里的文件级改动点）与执行时登记的标题
/// （S6 要求的任务包写法 + 验证项）语义常同而形异，精确相等会把它们全判成计划外步骤。
/// 只认「细化」这一种形变，不做语义相似度——放宽过头等于让真新增蒙混过关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewTodoClass {
    /// 真正的新步骤：归一化后既不等于任何基线条目的归一化，也不是其子串。
    /// 只有它会触发「计划外步骤确认」。
    New(String),
    /// 细化 / 改写：归一化后等于某基线条目的归一化，或就是它的子串。
    /// 视作已批准范围内的表述细化，静默纳入（留 session_log 痕迹），不弹门。
    Refinement(String),
}

/// 标题归一化：只剥纯装饰性成分，绝不做语义改写。
///
/// 顺序：① 序号前缀 ② 任务包前缀 ③ 标点→空格 ④ 空白合并。
/// **保留**：`cargo test` 的 `+`、`core/x.rs` 的 `.` 等有语义的符号——前者剥了会让
/// 「+ 单测」变成基线子串而静默放过真新增，后者剥了会把 `wait_targets.rs` 变成
/// `wait_targetsrs`，两侧标题错位后子串判据彻底失效。
pub fn normalize_title(raw: &str) -> String {
    let s = strip_task_pkg_prefix(strip_ordinal_prefix(raw.trim()));
    // 标点→空格而非直接删除：删会把「新增，登记表」粘成「新增登记表」，
    // 两侧归一化方式不一致反而让本该判 Refinement 的标题判不出来。
    let chars: Vec<char> = s.chars().collect();
    let spaced: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if c == '.' {
                // 装饰点（`1.` 的尾点）当空格；`core/x.rs` / `v1.2` 的点前后都是字母数字，保留。
                return if is_decorative_dot(&chars, i) { ' ' } else { c };
            }
            if matches!(
                c,
                '\u{FF08}'
                    | '\u{FF09}'
                    | '('
                    | ')'
                    | '\u{FF1A}'
                    | ':'
                    | '\u{FF0C}'
                    | ','
                    | '\u{3002}'
                    | '\u{3001}'
                    | '\u{FF1B}'
                    | ';'
                    | '\u{FF01}'
                    | '!'
                    | '\u{FF1F}'
                    | '?'
            ) {
                ' '
            } else {
                c
            }
        })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 是否是可以安全删掉的「纯标点」句点：`core/x.rs` 的点、`v1.2` 的点都要留下。
/// 仅当点的前后都不是字母数字（`1.` `新增。`）时才当作装饰。
fn is_decorative_dot(chars: &[char], i: usize) -> bool {
    let before = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
    let after = chars.get(i + 1).copied();
    let alnum = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
    !(alnum(before) && alnum(after))
}

/// 项目符号：无数字序号的列表符号（`- ` `* ` `• ` `— `）。
///
/// 与数字序号的区别在于**必须紧跟分隔符**：`a-b` 的连接号、`-1` 的负号都不是项目符号，
/// 只有符号后面是空白/标点才当装饰。`-1: 负数兼容` 因此原样保留；`-` 收尾（后面没东西）
/// 则由末尾的「剥完只剩装饰就不剥」兜住。
fn is_bullet_symbol(c: char) -> bool {
    matches!(c, '-' | '*' | '\u{2022}' | '\u{2014}' | '\u{2013}')
}

/// 剥序号前缀：仅当「序号后面还有实义内容」时才剥。
/// 光秃秃的 `1.` 剥成空串反而凭空造出一个基线子串（空串是任何串的子串），只会放宽安全门。
///
/// 两条入口：
/// ① 数字序号（可选外层括号）：`1. ` `2、` `(3) ` `【4】`
/// ② 纯符号项目符号：`- ` `* ` `• ` `— `
///
/// **分隔符集里刻意没有冒号**：冒号是「任务包前缀」的收尾符（`包 A（backend-dev）：…`），
/// 归 [`strip_task_pkg_prefix`] 管。这里若把 `:` 当序号分隔符，`1: 步骤` 会先被吃掉
/// `1: ` 变成 `步骤`，正文里的序号语义（以及「`1` 是不是标签」的判定）全丢了。
fn strip_ordinal_prefix(s: &str) -> &str {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let wrapped = matches!(
        chars.first(),
        Some('(') | Some('\u{FF08}') | Some('\u{3010}') | Some('[')
    );
    if wrapped {
        i += 1;
    }
    let digits_start = i;
    while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
        i += 1;
    }
    if i == digits_start {
        // 入口②：没有数字，但符号本身可能就是项目符号；符号后必须跟分隔符才认。
        // 用 `get` 而非下标：空串（`normalize_title("")`）不得在此 panic。
        if !chars
            .get(digits_start)
            .copied()
            .is_some_and(is_bullet_symbol)
        {
            return s; // 既没有数字序号，也不是项目符号
        }
        i += 1;
        // `-1: 负数兼容` 的 `-` 是负号而非项目符号：符号后必须出现分隔符。
        if !chars
            .get(i)
            .is_some_and(|c| c.is_whitespace() || matches!(*c, '\u{3001}' | '.' | ':' | '\u{FF1A}'))
        {
            return s;
        }
    } else {
        let closed = matches!(
            chars.get(i),
            Some(')') | Some('\u{FF09}') | Some('\u{3011}') | Some(']')
        );
        if closed {
            i += 1; // `(3) 标题` 的括号本身就是序号与正文的分界，不再要求额外分隔符
        } else {
            // 序号后必须跟分隔符；没有分隔符就不是序号，是数字标题（`2024 年度统计`）
            let is_sep = match chars.get(i) {
                Some('.') => is_decorative_dot(&chars, i),
                Some(c) => matches!(*c, '\u{3001}' | '-' | '\u{2014}' | '*' | '\u{2022}'),
                None => false,
            };
            if !is_sep {
                return s;
            }
        }
    }
    // 吃掉连续的分隔符与空白（`1. ` `2、` `(3) ` `- ` `* `）
    while let Some(c) = chars.get(i) {
        let dot = *c == '.' && is_decorative_dot(&chars, i);
        if c.is_whitespace()
            || dot
            || matches!(*c, '\u{3001}' | '-' | '\u{2014}' | '*' | '\u{2022}')
        {
            i += 1;
        } else {
            break;
        }
    }
    let rest: String = chars[i..].iter().collect();
    if rest.trim().is_empty() {
        return s; // 剥完只剩装饰 —— 不剥
    }
    // 返回借用：rest 是局部值，但内容与原串逐字相同，按字节切片回原串。
    let skip = s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(s.len());
    s[skip..].trim_start()
}

/// 剥任务包前缀：`包 A（backend-dev）：` / `任务包C:` / `包 A(frontend-dev)：新增 …`。
///
/// 判据是「真任务包形态」而非形态近似：反复剥掉以中英文冒号收尾的前缀段（判据见
/// [`looks_like_label`]），直到前缀段再也认不出来；余下文本仍非空才认可。
/// 剥不动就原样返回，绝不吞正文。
fn strip_task_pkg_prefix(s: &str) -> String {
    let mut cur = s.trim().to_string();
    // 最多剥几层：真实标题不会出现嵌套多层的包前缀，设上限防病态输入
    for _ in 0..4 {
        // 逐字符找冒号：`：` 是多字节，`find` 返回的字节下标不能直接切片
        let colon = cur
            .char_indices()
            .find(|(_, c)| *c == ':' || *c == '\u{FF1A}')
            .map(|(b, c)| (b, c.len_utf8()));
        let Some((pos, w)) = colon else { break };
        if pos == 0 {
            break;
        }
        let head = &cur[..pos];
        if !looks_like_label(head) {
            break;
        }
        let rest = cur[pos + w..].trim().to_string();
        if rest.is_empty() {
            break;
        }
        cur = rest;
    }
    cur
}

/// 括号内文字的字符数上限（含头尾空白，`:extend` 等标识符正好卡在这里）。
/// 真正的角色/包标识很短，而「顺手」「数据库」「含删除」这类普通语义短语虽短但含中文（由
/// [`is_agent_id_token`] 拦），更长一点的括号补语则靠这条上限拦。
const ROLE_PAREN_MAX_CHARS: usize = 24;

/// 「任务包」/「包」字头汉字（后者是前者的后缀，两条都要覆盖）。
const PKG_PREFIX_WORDS: [&str; 2] = ["\u{4EFB}\u{52A1}\u{5305}", "\u{5305}"];

/// 已知子代理角色词表（`agents::builtin()` 的规范名）。
///
/// 这是判据 A ① 的白名单：`包 A（backend-dev）：…` 剥掉安全，`删除 users 表 migration (数据库)：…`
/// 剥掉就是静默放行真新增（**这是安全门：误报可以放过、误放不行**）。
/// 枚举而非纯形态推断，是因为「无空格 + 无中文逗号」这类形态信号分不开 `backend-dev` 与
/// `migration` / `nocache`（一个英文语义短语的括号定语形态与角色标识完全一致）。
/// 词表与注册表同源；模型自造非规范角色名时走下面的形态兜底，代价是多弹一次门。
const KNOWN_AGENT_ROLES: [&str; 9] = [
    "explore",
    "backend-dev",
    "frontend-dev",
    "app-dev",
    "reviewer",
    "code-reviewer",
    "product-manager",
    "tester",
    "title",
];

/// 任务包前缀段的判据（只用于「这整段是不是标签」的判断，**不用于判定正文语义**）。
///
/// ## 三条判据（任一命中即认）
/// - **A · 配对括号内是角色/包标识**：括号必须真配对，且括号内命中 [`KNOWN_AGENT_ROLES`]
///   词表（`包 A（backend-dev）` / `包 C（代码审查 reviewer）`），或整体符合
///   [`is_agent_id_token`] 形态（无空白、无中文逗号、无括号、≤24 字符且至少一个 ASCII
///   字母或数字）。
/// - **B · `包` / `任务包` 字头 + 真包序号**：`包 A` / `包1` / `任务包C` / `包 A2`。
///   「包」字头本身**不是**信号：`包管理策略重构 (含删除)：更新 README` 的主体是
///   「包管理策略重构」——管理策略重构是计划外步骤，只因字头是「包」就整段丢弃会静默放行。
/// - **C · 字母与数字混排的短标识**（`pkgB` / `pkg_2`）：排除版本号形态
///   （`v2: 修 schema` 说的是「按 v2 修」，剥掉会让归一化与基线错位）。
///
/// ## 被这张表堵住的静默放行（审查员实测复现，基线 → 当前标题 → 应判 New）
/// | 基线 | 当前标题 | 旧判据 |
/// |---|---|---|
/// | `跑 cargo test --workspace` | `删除 users 表 migration (数据库)：跑 cargo test --workspace` | 剥（括号定语）|
/// | `改 core/x.rs` | `把 API key 硬编码进源码 (顺手)：改 core/x.rs` | 剥（括号定语）|
/// | `更新 README` | `顺手清理 ~/.cache (可选)：更新 README` | 剥（括号定语）|
/// | `更新 README` | `删库 )( ：更新 README` | 剥（只数括号个数，不看顺序）|
/// | `更新 README` | `删库 (a)：回滚 (b)：更新 README` | 剥（括号内是单字母也算“标识”）|
/// | `更新 README` | `包管理策略重构 (含删除)：更新 README` | 剥（只看字头是「包」）|
///
/// 「纯数字不算 label」依然是硬底线：`1: 步骤` 的 `1` 是正文序号，不是标签名。
fn looks_like_label(head: &str) -> bool {
    let h = head.trim();
    if h.is_empty() {
        return false;
    }
    let pkg_head = PKG_PREFIX_WORDS.iter().any(|w| h.starts_with(w));
    // 底线兜底（两轮审查逼出来的）：标签只允许「包/任务包 字头 + 包序号 + 可选括号角色」
    // 这一种结构，**不得携带任何其他汉字**。否则 `修 p0 崩溃：更新 README`（判据 C）、
    // `包 A 顺手删库：更新 README`（判据 B）会把整个中文子动作当成标签剥掉，剩下的正文
    // 命中基线 → 判细化 → 真新增静默放行且不弹门。
    // 注意这条**不能**写成「含 CJK 即否」——真实标签 `包 A（backend-dev）：` 本身就含「包」字，
    // 那样会把本次要修的降噪也一起砍掉；所以只查「包字头与括号之外」的汉字。
    if has_stray_cjk_outside(h) {
        return false;
    }
    // 判据 C 先跑（最窄的形态判据），命中即止
    if has_latin_digit_id_shape(h) {
        return true;
    }
    // 判据 B：`包 A（backend-dev）` 尾部的括号定语走 A，其余 `包 A` / `任务包C` 走包序号
    if paren_content_is_agent_id(h) || (pkg_head && has_pkg_ordinal(h)) {
        return true;
    }
    false
}

/// 判据 C：字母与数字混排的**纯 ASCII 短标识**（`任务包C` 里的 C、`pkgB` / `pkg_2`）。
///
/// 汉字正文已由 `looks_like_label` 的兜底拦掉（标签只有包字头 + 括号角色两种带汉字的成分），
/// 所以走到这里的 head 若被认可，剩下的就是纯 ASCII 短标识。
fn has_latin_digit_id_shape(h: &str) -> bool {
    let has_latin = h.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = h.chars().any(|c| c.is_ascii_digit());
    if !has_latin || !has_digit || h.chars().count() > 24 {
        return false;
    }
    // 版本号形态例外：`v2:` / `V1:` / `v10:` 的字母+数字混排与 `pkgB` 在形态上无法区分，
    // 但前者是正文的一部分（`v2: 修 schema` 说的是「按 v2 修 schema`），剥掉会让归一化
    // 结果与基线错位。版本号形态有固定文法（单字母 v + 数字 + 可选 .数字），据此排除。
    let is_version_tag = {
        let mut cs = h.chars();
        let lead = cs.next();
        matches!(lead, Some('v') | Some('V'))
            && cs.clone().next().is_some_and(|c| c.is_ascii_digit())
            && cs.all(|c| c.is_ascii_digit() || c == '.')
    };
    !is_version_tag
}

/// 配对括号与其内部原文，顺序按左括号在串中的位置。**嵌套 / 类别错配（`（backend-dev)`）
/// / 残缺一律返回空**——错配形态无法从本段判断哪层才是标签，保守起见一律不剥；
/// 「无括号」与「括号结构坏掉」都归为空，两者在下游都是「不认标签」，故不必区分。
fn paired_paren_inners(h: &str) -> Vec<String> {
    let mut stack: Vec<(usize, char)> = Vec::new(); // (左括号字节偏移, 左括号字符)
    let mut out: Vec<String> = Vec::new();
    for (b, c) in h.char_indices() {
        match c {
            '(' | '\u{FF08}' => {
                stack.push((b, c));
                if stack.len() > 1 {
                    return Vec::new(); // 嵌套
                }
            }
            ')' | '\u{FF09}' => match stack.pop() {
                Some((ob, o_open)) if is_open_paren(o_open) && is_close_paren(c) => {
                    out.push(h[ob + o_open.len_utf8()..b].to_string());
                }
                _ => return Vec::new(), // 类别错配：栈空（`)(` 顺序错乱）或中英文括号混用
            },
            _ => {}
        }
    }
    out
}

/// 判据 A：括号必须真配对，且**至少一对**括号内是角色/包标识。
///
/// 旧实现只数 `(` 与 `)` 的个数是否相等，于是 `删库 )( ：…`（顺序错乱）与
/// `删库 (a)：回滚 (b)：…`（单字母“定语”）都整段被丢——两层标签剥完剩下正文，
/// 于是一个真新增步骤静默变成了「细化」。
fn paren_content_is_agent_id(h: &str) -> bool {
    paired_paren_inners(h).iter().any(|raw| {
        let t = raw.trim();
        // 长度按字符算（中文按 1 字计，否则纯中文定语得靠词表逐条枚举）
        !t.is_empty() && t.chars().count() <= ROLE_PAREN_MAX_CHARS && is_agent_id_token(t)
    })
}

fn is_open_paren(c: char) -> bool {
    matches!(c, '(' | '\u{FF08}')
}

fn is_close_paren(c: char) -> bool {
    matches!(c, ')' | '\u{FF09}')
}

/// 角色/包标识的形态判据（判据 A 的白名单之外的兜底）。
///
/// **硬条件**（缺一不可）：“无空白/无中文逗号” + “无 CJK 汉字” + “无括号” + “≤24 字符”
/// + **“至少一个 ASCII 字母或数字”**。
///
/// 最后一条不可省：形如 `删库 (a)` 的括号定语里没有字母数字，光靠长度与分隔符判据会把它放过去。
/// “无 CJK” 一条同时挡掉 `(顺手)` / `(可选)` / `(含删除)`（纯中文）；它在**白名单之前**执行，
/// 所以规范角色名里带中文说明时（`包 C（代码审查 reviewer）`）会落到下面的 kebab 兜底而剥得掉，
/// 而未收录的自造中文定语按不确定处理 → 多弹一次门，宁可误报不可误放。
fn is_agent_id_token(t: &str) -> bool {
    if t.chars().count() > ROLE_PAREN_MAX_CHARS {
        return false;
    }
    if t.chars()
        .any(|c| c.is_whitespace() || matches!(c, '\u{FF0C}' | '\u{3001}' | '\u{3002}'))
    {
        return false;
    }
    if t.chars().any(is_cjk) {
        return false;
    }
    if t.contains(['(', ')', '\u{FF08}', '\u{FF09}']) {
        return false;
    }
    if KNOWN_AGENT_ROLES.contains(&t) {
        return true;
    }
    if !t.chars().any(|c| c.is_ascii_alphanumeric()) {
        return false; // `a` / `b` / `：` 这类纯符号或纯标点的“定语”不是标识
    }
    // kebab / snake / 点分隔形态：backend-dev / pkg_2 / reviewer.v2
    // （判据 C 只管「字母+数字混排」，`pkgB` / `docs-writer` 这类没数字的落到这里）
    t.contains(['-', '_', '.'])
}

/// 判据 B：`包` / `任务包` 后面必须跟一个**包序号形态**（`包 A` / `包1` / `任务包C` /
/// `包 A2`），可有可无的分隔（空格/连字符/下划线/点/右括号）。
///
/// 「包管理策略重构」的「管理」不是序号 → 不认；「包 A2（backend-dev）」的序号是 `A2` → 认。
fn has_pkg_ordinal(h: &str) -> bool {
    let Some(rest) = PKG_PREFIX_WORDS
        .iter()
        .find(|w| h.starts_with(**w))
        .map(|w| &h[w.len()..])
    else {
        return false;
    };
    let ordinal: String = rest
        .trim_start_matches([' ', '\u{3000}', '-', '_', '.', ')', '\u{FF09}'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    !ordinal.is_empty()
}

/// 包字头形态里是否残留了**不属于标签**的汉字（即包序号之后的正文尾巴）。
///
/// 标签段只允许两种汉字：`包` / `任务包` 字头本身，以及括号内的角色定语
/// （`包 A（代码审查 reviewer）`）。其余位置的汉字都是正文——`包 A 顺手删库`、
/// `包B 删库跑路` 的序号后面还跟着中文动作，那不是标签，整段被剥等于静默放行真新增。
fn has_stray_cjk_outside(h: &str) -> bool {
    let mut in_paren = false;
    for c in h.chars() {
        match c {
            '(' | '\u{FF08}' => in_paren = true,
            ')' | '\u{FF09}' => in_paren = false,
            _ if is_cjk(c) => {
                if !in_paren && !PKG_PREFIX_WORDS.iter().any(|w| w.contains(c)) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// 是否是 CJK 汉字（含中日韩统一表意文字与扩展 A）。
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
    )
}

/// 子串判定的长度下限（归一化后按 **UTF-8 字节** 计，不是字符数）。
///
/// **为什么要下限**：子串是「细化成基线的一部分」，但**短片段天然是长句的子串**，
/// 去掉下限等于给任何计划外动作开后门——基线 `新增 core/x.rs + 单测`，执行时冒出
/// `单测` / `verify` / `fix` 都会静默放行，而 `单测` 完全可以指另一个文件的另一件事。
///
/// **按字符计而不是字节**：中文字符占 3 字节，按字节算时 `顺手删库`（4 字）就有 12 字节、
/// 直接越过下限，而它完全可能是另一处真正独立的计划外动作——下限形同虚设。
/// 按字符计则中英一视同仁：8 个字符足够容纳 `新增 core/x.rs`（13 字符）这类真实细化，
/// 又拦得住 `单测`（2）/ `verify`（6）/ `顺手删库`（4）这类短动作。
///
/// 调高阈值只会把更多真新增放进「静默放行」，调低只会多弹门——**升门槛要拿证据，
/// 不拿证据就只能往严的方向走**，故取 8 字符而非更松的字节口径。
const SUBSTRING_MIN_CHARS: usize = 8;

/// G3 分类判定：把 `current` 中不在 `baseline` 里的标题逐个分类（保持原序、去重）。
///
/// 口径：
/// - 归一化后**等于**某基线归一化，或**是其子串**（且非空、不短于 [`SUBSTRING_MIN_CHARS`]）
///   → [`NewTodoClass::Refinement`]
/// - 其余 → [`NewTodoClass::New`]
///
/// 为什么这样切不放过真新增：子串方向是单向的（细化标题 ⊆ 基线条目，不是反之），
/// 且比较前先剥掉了装饰壳（`looks_like_label` 是安全门，误认标签会把整段正文丢掉）。
/// 基线里的「新增 core/wait_targets.rs 登记表内核 + 单测」，执行时细化成
/// 「新增 core/wait_targets.rs 登记表内核」会判 Refinement；反过来执行时冒出
/// 「顺手把 openers.rs 也修了」这种与基线无包含关系的条目，两者互不为子串，仍是 New，
/// 照常弹门。而「顺手删库」这种短片段即使碰巧也是基线子串，也因短于
/// [`SUBSTRING_MIN_CHARS`] 被判 New。
pub fn classify_new_todos(baseline: &[String], current: &[String]) -> Vec<(String, NewTodoClass)> {
    let norm_base: Vec<String> = baseline
        .iter()
        .map(|b| normalize_title(b))
        .filter(|b| !b.is_empty())
        .collect();
    let mut out: Vec<(String, NewTodoClass)> = Vec::new();
    for c in current {
        if baseline.iter().any(|b| b == c) {
            continue; // 精确命中：既非新增也非细化，直接跳过
        }
        if out.iter().any(|(t, _)| t == c) {
            continue; // 去重，保留首次出现的原序
        }
        let n = normalize_title(c);
        let long_enough = n.chars().count() >= SUBSTRING_MIN_CHARS;
        let is_refinement = !n.is_empty()
            && norm_base
                .iter()
                .any(|b| n == *b || (long_enough && b.contains(&n)));
        out.push((
            c.clone(),
            if is_refinement {
                NewTodoClass::Refinement(c.clone())
            } else {
                NewTodoClass::New(c.clone())
            },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_rules() {
        let ok = vec![
            Todo {
                title: "a".into(),
                status: TodoStatus::Completed,
            },
            Todo {
                title: "b".into(),
                status: TodoStatus::InProgress,
            },
            Todo {
                title: "c".into(),
                status: TodoStatus::Pending,
            },
        ];
        assert!(validate_todos(&ok).is_ok());

        let two_ip = vec![
            Todo {
                title: "a".into(),
                status: TodoStatus::InProgress,
            },
            Todo {
                title: "b".into(),
                status: TodoStatus::InProgress,
            },
        ];
        assert_eq!(
            validate_todos(&two_ip).unwrap_err(),
            "同一时刻最多一个 in_progress 任务"
        );

        let empty_title = vec![Todo {
            title: "  ".into(),
            status: TodoStatus::Pending,
        }];
        assert!(validate_todos(&empty_title).is_err());
    }

    #[test]
    fn has_pending_distinguishes_unfinished_plan() {
        // 空列表 = 无计划，不算未完成（否则主会话每次提问都会被续跑门拦下）
        assert!(!has_pending(&[]));
        // 全完成 = 已收尾
        assert!(!has_pending(&[
            Todo {
                title: "a".into(),
                status: TodoStatus::Completed
            },
            Todo {
                title: "b".into(),
                status: TodoStatus::Completed
            },
        ]));
        // 存在 in_progress / pending 即未完成（与 f19c3890 事故形态一致）
        assert!(has_pending(&[
            Todo {
                title: "a".into(),
                status: TodoStatus::Completed
            },
            Todo {
                title: "b".into(),
                status: TodoStatus::InProgress
            },
        ]));
        assert!(has_pending(&[
            Todo {
                title: "a".into(),
                status: TodoStatus::Completed
            },
            Todo {
                title: "c".into(),
                status: TodoStatus::Pending
            },
        ]));
    }

    #[test]
    fn rendered_marks() {
        let todos = vec![
            Todo {
                title: "done".into(),
                status: TodoStatus::Completed,
            },
            Todo {
                title: "now".into(),
                status: TodoStatus::InProgress,
            },
            Todo {
                title: "later".into(),
                status: TodoStatus::Pending,
            },
        ];
        let r = render_todos(&todos);
        assert!(r.contains("[x] done"));
        assert!(r.contains("[~] now"));
        assert!(r.contains("[ ] later"));
    }

    #[test]
    fn diff_new_todos_finds_additions_only() {
        // G3：检测新增；保留/完成/删除的标题不告警；重命名计为删+增
        let baseline = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            diff_new_todos(&baseline, &["a".into(), "b".into()]),
            Vec::<String>::new()
        );
        assert_eq!(
            diff_new_todos(&baseline, &["a".into()]),
            Vec::<String>::new()
        ); // 删除不告警
        assert_eq!(
            diff_new_todos(&baseline, &["a".into(), "b".into(), "c".into()]),
            vec!["c".to_string()]
        );
        assert_eq!(
            diff_new_todos(&baseline, &["a".into(), "x".into()]),
            vec!["x".to_string()]
        ); // 重命名 = 删 + 增
        assert_eq!(diff_new_todos(&[], &["a".into()]), vec!["a".to_string()]); // 空基线：一切都是新增
    }

    // ── G3 判定口径（归一化 + 细化分类）────────────────────────────────────

    #[test]
    fn normalize_strips_task_pkg_prefix() {
        // 事故形态：批准时登记「新增 core/x.rs …」，S6 执行时套上任务包前缀
        assert_eq!(
            normalize_title("包 A（backend-dev）：新增 core/wait_targets.rs 登记表内核 + 单测"),
            "新增 core/wait_targets.rs 登记表内核 + 单测"
        );
        assert_eq!(
            normalize_title("包 B(backend-dev):新增 core/plan.rs 判定口径"),
            "新增 core/plan.rs 判定口径"
        );
        assert_eq!(normalize_title("任务包C：改前端"), "改前端");
        // 只有序号/裸标识的前缀不吃掉：`1: 步骤` 的「步骤」才是正文
        assert_eq!(normalize_title("1: 步骤"), "1 步骤");
        // 版本号形态与 `pkgB` 在字母+数字混排上无法区分，但 `v2:` 是正文的一部分，剥掉会让
        // 归一化结果与基线错位（同一缺陷让细化标题判不出，静默放过真新增）
        assert_eq!(normalize_title("v2: 修 schema"), "v2 修 schema");
        assert_eq!(normalize_title("2024: 年度计划"), "2024 年度计划");
    }

    #[test]
    fn normalize_strips_ordinal_prefix() {
        for raw in [
            "1. 新增登记表内核",
            "2、新增登记表内核",
            "(3) 新增登记表内核",
            "- 新增登记表内核",
            "* 新增登记表内核",
            "• 新增登记表内核",
            "【4】新增登记表内核",
        ] {
            assert_eq!(
                normalize_title(raw),
                "新增登记表内核",
                "序号前缀未剥：{raw}"
            );
        }
        // 数字标题不是序号：`2024 年度统计` 不该被吃掉开头
        assert_eq!(normalize_title("2024 年度统计"), "2024 年度统计");
        // 剥完只剩装饰就不剥（避免凭空造出空串子串去放宽安全门）
        assert_eq!(normalize_title("1."), "1");
    }

    #[test]
    fn normalize_keeps_meaningful_symbols() {
        // `cargo test` 的 `+` 有语义：剥掉就会与基线的「新增 core/x.rs」判成子串而静默放过
        assert_eq!(
            normalize_title("新增 core/wait_targets.rs 登记表内核 + 单测"),
            "新增 core/wait_targets.rs 登记表内核 + 单测"
        );
        for sym in ['/', '+', '-', '_', '@', '#'] {
            assert!(
                normalize_title(&format!("a{sym}b")).contains(sym),
                "符号 {sym} 被误剥"
            );
        }
    }

    #[test]
    fn normalize_collapses_punctuation_and_whitespace() {
        assert_eq!(
            normalize_title("  新增，登记表  内核；跑测试。  "),
            "新增 登记表 内核 跑测试"
        );
    }

    #[test]
    fn classify_equal_is_refinement() {
        let baseline = vec!["新增 core/wait_targets.rs 登记表内核 + 单测".to_string()];
        let got = classify_new_todos(
            &baseline,
            &["1. 新增 core/wait_targets.rs 登记表内核 + 单测".into()],
        );
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].1,
            NewTodoClass::Refinement("1. 新增 core/wait_targets.rs 登记表内核 + 单测".into())
        );
    }

    #[test]
    fn classify_substring_is_refinement() {
        let baseline = vec!["新增 core/wait_targets.rs 登记表内核 + 单测".to_string()];
        // 细化成基线的一部分（去掉 `+ 单测`）
        let got = classify_new_todos(
            &baseline,
            &["包 A（backend-dev）：新增 core/wait_targets.rs 登记表内核".into()],
        );
        assert_eq!(
            got[0].1,
            NewTodoClass::Refinement(
                "包 A（backend-dev）：新增 core/wait_targets.rs 登记表内核".into()
            )
        );
    }

    #[test]
    fn classify_real_addition_is_new() {
        let baseline = vec!["新增 core/wait_targets.rs 登记表内核 + 单测".to_string()];
        // 与基线互不为子串 —— 真正的计划外步骤，必须弹门
        let got = classify_new_todos(&baseline, &["顺手把 openers.rs 的探测也修了".into()]);
        assert_eq!(
            got[0].1,
            NewTodoClass::New("顺手把 openers.rs 的探测也修了".into())
        );
    }

    #[test]
    fn classify_incident_form_recovers_pkg_rename_but_keeps_real_new_step() {
        // 事故形态：S6 把批准时的文件级改动点改写成任务包写法
        let baseline = vec![
            "新增 core/wait_targets.rs 登记表内核 + 单测".to_string(),
            "更新 docs/0-README.md 登记".to_string(),
        ];
        let got = classify_new_todos(
            &baseline,
            &[
                // 同一件事的改写 —— 剥掉包前缀后与基线归一化相等 → Refinement，不弹门
                "包 A（backend-dev）：新增 core/wait_targets.rs 登记表内核 + 单测".into(),
                // 但「回归验证：…」是 S6 新补的验证项，与基线毫无包含关系 —— 真新增，照常弹门
                "回归验证：cargo test / fmt --check / clippy / ui test + build".into(),
            ],
        );
        assert_eq!(got.len(), 2);
        assert!(
            matches!(got[0].1, NewTodoClass::Refinement(_)),
            "包前缀改写应判细化：{:?}",
            got[0]
        );
        assert!(
            matches!(got[1].1, NewTodoClass::New(_)),
            "无包含关系的验证项是真新增，不得静默放过：{:?}",
            got[1]
        );
    }

    #[test]
    fn classify_dedups_and_keeps_order() {
        let baseline = vec!["a".to_string()];
        let got = classify_new_todos(
            &baseline,
            &["z".into(), "y".into(), "z".into(), "a".into(), "x".into()],
        );
        // `a` 精确命中基线不算新增；`z` 重复只留首次；其余保序
        assert_eq!(
            got.into_iter().map(|(t, _)| t).collect::<Vec<_>>(),
            vec!["z".to_string(), "y".to_string(), "x".to_string()]
        );
    }

    #[test]
    fn classify_empty_baseline_all_new() {
        let got = classify_new_todos(&[], &["a".into(), "b".into()]);
        assert!(got.iter().all(|(_, k)| matches!(k, NewTodoClass::New(_))));
    }

    // ── 放行漏洞回归（code-reviewer 实测复现）────────────────────────────────
    // 安全门口径：误报可以放过，误放不行。下面每条都曾是「静默放行」的真新增步骤。

    /// 括号定语 / 包字头不得把**整段正文**丢掉（旧判据只数括号个数 + 看字头是「包」）。
    #[test]
    fn classify_label_shaped_lead_in_is_still_new() {
        let cases: Vec<(&str, &str)> = vec![
            // ① 半角括号里是普通英文语义短语 —— `migration` 恰好也匹配 kebab 兜底，靠词表拦
            (
                "跑 cargo test --workspace",
                "删除 users 表 migration (数据库)：跑 cargo test --workspace",
            ),
            // ② 中文「顺手」定语
            (
                "改 core/x.rs",
                "把 API key 硬编码进源码 (顺手)：改 core/x.rs",
            ),
            // ③ 中文「可选」定语，且正文提到与基线不同的目录
            ("更新 README", "顺手清理 ~/.cache (可选)：更新 README"),
            // ④ 括号顺序错乱（`)(`），旧判据只数个数 1==1 于是整段被丢
            ("更新 README", "删库 )( ：更新 README"),
            // ⑤ 两层单字母「定语」，旧判据剥一层还剩一层，两层全剥完只剩正文
            ("更新 README", "删库 (a)：回滚 (b)：更新 README"),
            // ⑥ 「包」字头 + 括号定语，但主体是「包管理策略重构」——计划外步骤
            ("更新 README", "包管理策略重构 (含删除)：更新 README"),
        ];
        for (base, cur) in cases {
            let got = classify_new_todos(&[base.to_string()], &[cur.to_string()]);
            assert_eq!(got.len(), 1, "{cur}");
            assert_eq!(
                got[0].1,
                NewTodoClass::New(cur.to_string()),
                "真新增被静默放行了（基线：{base}，当前：{cur}）"
            );
        }
    }

    /// 第二轮审查的绕过：「中文子动作 + 一个 ASCII 字母数字 + 冒号」是模型写 todo 的极常见形态。
    /// 初版的判据 C（head 含字母+数字即算标签）与判据 B（包字头 + 真序号）会把整个
    /// 中��子动作当成标签剥掉，剩下的正文命中基线 → 判细化 → 真新增静默放行。
    /// 修法是 `looks_like_label` 的 CJK 兜底：真正的标签是 ASCII 短标识，不带汉字正文。
    #[test]
    fn classify_cjk_lead_with_alnum_is_still_new() {
        let cases: Vec<(&str, &str)> = vec![
            ("更新 README", "修 p0 崩溃：更新 README"),
            ("更新 README", "按 p1 结论：更新 README"),
            ("升级 README 依赖", "升级 rust1.80 依赖：升级 README 依赖"),
            ("跑测试", "跑 k6 压测：跑测试"),
            ("更新 README", "清理 p0-p3 遗留分支：更新 README"),
            ("更新 README", "包 A 顺手删库：更新 README"),
            ("更新 README", "包B 删库跑路：更新 README"),
        ];
        for (base, cur) in cases {
            let got = classify_new_todos(&[base.to_string()], &[cur.to_string()]);
            assert_eq!(got.len(), 1, "{cur}");
            assert_eq!(
                got[0].1,
                NewTodoClass::New(cur.to_string()),
                "中文子动作被当成标签剥掉了（基线：{base}，当前：{cur}）"
            );
        }
    }

    /// 收紧后对照组仍然正确（这些本来就没被误剥，防口径回归）。
    #[test]
    fn classify_known_good_controls_stay_new() {
        let cases: Vec<(&str, &str)> = vec![
            (
                "跑 cargo test --workspace",
                "跑 cargo test --workspace 并删库",
            ),
            ("更新 README", "更新 README；删库"),
            // ⑤ 的同族：括号是配对的，但本段无标签证据
            ("更新 README", "删库 ) (：更新 README"),
        ];
        for (base, cur) in cases {
            let got = classify_new_todos(&[base.to_string()], &[cur.to_string()]);
            assert_eq!(got.len(), 1, "{cur}");
            assert_eq!(got[0].1, NewTodoClass::New(cur.to_string()), "{cur}");
        }
    }

    /// 收紧后真正要降噪的任务包写法仍然降噪（漏剥 = 反复弹门）。
    #[test]
    fn classify_real_task_pkg_titles_are_still_refinement() {
        let cases: Vec<(&str, &str)> = vec![
            // 白名单角色 + 包序号（含全角括号、中英文冒号、无空格几种写法）
            ("新增 core/x.rs", "包 A（backend-dev）：新增 core/x.rs"),
            ("改前端", "任务包C：改前端"),
            ("新增 core/x.rs", "包1(backend-dev):新增 core/x.rs"),
            ("新增 core/x.rs", "包 B（frontend-dev）：新增 core/x.rs"),
            // 词表未收录的自造角色名（kebab 兜底）
            ("新增 core/x.rs", "包 C（docs-writer）：新增 core/x.rs"),
            // 包序号带分隔与多字符（判据 B）
            ("新增 core/x.rs", "包-2：新增 core/x.rs"),
            ("新增 core/x.rs", "任务包 C：新增 core/x.rs"),
        ];
        for (base, cur) in cases {
            let got = classify_new_todos(&[base.to_string()], &[cur.to_string()]);
            assert_eq!(got.len(), 1, "{cur}");
            assert_eq!(
                got[0].1,
                NewTodoClass::Refinement(cur.to_string()),
                "任务包标签未被剥，噪声会反复弹门（基线：{base}，当前：{cur}）"
            );
        }
        // 剥离后的归一化值直接钉住（防以后改判据时无声劣化）
        assert_eq!(
            normalize_title("包 C（代码审查 reviewer）：更新 README"),
            "更新 README",
            "含中文定语 + 白名单角色的真实标签形态仍应剥掉"
        );
        // 判据 C 不受收紧影响
        assert_eq!(normalize_title("pkg2: 新增 core/x.rs"), "新增 core/x.rs");
    }

    /// 子串下限：短片段不得因「碰巧是基线子串」而静默放行。
    #[test]
    fn classify_short_fragment_is_new_despite_being_substring() {
        let base = "新增 core/x.rs + 单测".to_string();
        for cur in ["单测", "顺手删库", "verify"] {
            let got = classify_new_todos(std::slice::from_ref(&base), &[cur.to_string()]);
            assert_eq!(got.len(), 1, "{cur}");
            assert_eq!(
                got[0].1,
                NewTodoClass::New(cur.to_string()),
                "短于 {SUBSTRING_MIN_CHARS} 字符的片段不得走子串放行：{cur}"
            );
        }
        // 下限不得损伤真实细化：长片段仍是 Refinement
        for cur in [
            "新增 core/x.rs",
            "包 A（backend-dev）：新增 core/x.rs",
            // 已知取舍：一个完整文件路径（`core/x.rs` = 9 字节）在下限之上，仍判细化。
            // 这是有意的——「指出哪个文件」是细化最强的证据信号（与 `core/x.rs 单测` 不同，
            // 后者是另一个文件的意思；中文标题里这种歧义极小），把它一并拦掉会让降噪打折扣。
            "core/x.rs",
        ] {
            let got = classify_new_todos(std::slice::from_ref(&base), &[cur.to_string()]);
            assert_eq!(
                got[0].1,
                NewTodoClass::Refinement(cur.to_string()),
                "真实细化被下限误伤：{cur}"
            );
        }
    }

    // ── looks_like_label 形态判据直接断言 ───────────────────────────────────

    #[test]
    fn looks_like_label_accepts_real_pkg_forms() {
        for h in [
            "包 A（backend-dev）",
            "包 B(frontend-dev)",
            "包 C（代码审查 reviewer）",
            "任务包C",
            "任务包 C",
            "包1",
            "包-2",
            "任务包B2",
            "pkg2",
            "pkg_2",
        ] {
            assert!(looks_like_label(h), "真任务包形态未认：{h}");
        }
    }

    #[test]
    fn looks_like_label_rejects_bodies_with_colon() {
        for h in [
            "删除 users 表 migration (数据库)",
            "把 API key 硬编码进源码 (顺手)",
            "顺手清理 ~/.cache (可选)",
            "删库 ) (",
            "删库 (a)",
            "包管理策略重构 (含删除)",
            "包管理策略重构",
            "1",
            "2024",
            "v2",
            "f(x)",
            "包",
            "",
        ] {
            assert!(!looks_like_label(h), "正文被误认成标签：{h}");
        }
    }
}
