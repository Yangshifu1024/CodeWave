use crate::core::agent::goal::GoalStatus;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

/// ask 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 问题列表（1–5 题）。
    questions: Vec<Question>,
    /// G2 豁免：轻量路径声明（跳过分析产物校验；todos 非空硬要求不变）。schema key 是 camelCase，rename_all 必须保持对齐
    #[serde(default)]
    pub(super) lightweight: Option<bool>,
    /// G2 豁免：跳过分析声明，须有用户口令背书（要求最近一条用户消息命中口令表）
    #[serde(default)]
    pub(super) skip_analysis: Option<bool>,
    /// 完整流水线批准门（[docs/standard-workflow](../../../../docs/standard-workflow.md)，原 arch 批准闸）：批准时把 ConfirmEach 会话切到 AutoEdit（Plan 档批准本就会切）。
    /// 给完整流水线在常规档下绕开「开发子代理写审批不可达」的可行路径。
    /// 显式 rename：serde camelCase 会把 autoedit 段拼成 switchToAutoedit，与 schema key 不符。
    #[serde(default)]
    #[serde(rename = "switchToAutoEdit")]
    pub(super) switch_to_autoedit: Option<bool>,
    /// 显式计划正文（可选）：plan 档批准形询问时由模型携带完整方案 markdown。
    /// 落盘计划文件优先用此内容，缺失/空白时回退题干拼接（plan_text，历史行为兼容）。
    /// 此前「方案放 question」只是提示词协议，模型不遵从时计划文件只剩短题干（用户实测缺陷）。
    #[serde(default)]
    pub(super) plan: Option<String>,
}

/// 单个问题。
#[derive(Deserialize, Clone, serde::Serialize)]
pub struct Question {
    /// 问题 id（应答协议按此关联）。
    pub(super) id: String,
    /// 题干；批准形询问时完整方案文本放 Args.plan（显式契约），此处只放题干。
    pub(super) question: String,
    /// 可选项（最多 6 个）。
    #[serde(default)]
    pub(super) options: Vec<Option2>,
    /// 选项互斥（单选/radio）。serde default 兼容旧载荷；由模型按题声明。
    /// 仅 UI 语义：渲染 + 互斥切换——应答协议（selections 数组）不变。
    #[serde(default)]
    pub(super) single: bool,
}

/// 单个选项。
#[derive(Deserialize, Clone, serde::Serialize)]
pub struct Option2 {
    /// 选项 id。
    pub(super) id: String,
    /// 选项展示文案。
    pub(super) label: String,
    /// 可选的补充说明。
    #[serde(default)]
    pub(super) description: Option<String>,
    /// 是否推荐项（前端展示「推荐」pill）。
    #[serde(default)]
    pub(super) recommended: bool,
    /// 该选项被选中时要切换到的权限档位（批准类选项标记，C1）：`None` = 非批准类选项（如「补充意见」）。
    /// wire 值即 ApprovalMode 的 snake_case（"auto_edit" / "full_access" 等）；批准门据此把会话切到
    /// 用户所选档位，取代此前的子串猜测 + 硬编码 AutoEdit。
    /// wire 上为 `None` 时不出现 `mode` 键（`skip_serializing_if`）：前端声明是 `mode?: ApprovalMode`，
    /// 带 `"mode": null` 会让 `mode !== undefined` 这类判定误判（[docs/mode-gate-and-subagent-sync]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) mode: Option<crate::core::prefs::ApprovalMode>,
}

/// ask 工具：暂停 run 并向用户提问，等待应答后继续。
/// 入参为 questions 数组（每题 id/question/可选 options）；Interactive 分级——必须独占批次，
/// 挂起期间 run 阻塞在应答通道上，取消/关闭即视为拒绝（E_ASK_CANCELLED）。
/// 承载 plan 档批准协议：批准形询问 + 选中批准类选项 → G2/G3 门校验 → 切到该选项声明的档位
///（mode 字段，缺省 AutoEdit）并注入「执行方案」；
/// 计划文本自动落盘为计划文件供前端卡片查看。
/// 目标档（goal mode）澄清期的批准走**独立分支**：校验已登记目标（text + criteria）→ 切到所选档位，
/// 并**仅当最终档位是目标档**时把目标从澄清期推进到执行期（用户选了别的档 = 退出目标模式，状态不动）；
/// **不冻结计划基线**（G3 的范围确认在目标档保持关闭），也不过 G2/G3 计划门。
pub struct AskTool;

/// O8 口令表（固定关键词，Q3=A）：最近一条用户消息命中任一关键词时 skipAnalysis 声明才被采信。
const SKIP_ANALYSIS_PHRASES: &[&str] = &[
    "不用分析",
    "跳过分析",
    "直接改",
    "无需分析",
    "skip analysis",
];

/// 检查最近一条用户消息是否命中跳过分析口令。
fn user_message_hit_skip_phrase(rt: &crate::core::agent::SessionRuntime) -> bool {
    let history = rt.history.lock().unwrap();
    history
        .iter()
        .rev()
        .find(|m| m.role == crate::core::types::Role::User)
        .map(|m| {
            let text = m
                .content
                .iter()
                .filter_map(|c| match c {
                    crate::core::types::Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            // Y3（评审）：「直接改」这类口语子串误伤面大——只有与分析语义同时出现才认口令；
            // 其余短语本身已带分析语义，无需附加检查。
            let has_direct = text.contains("直接改") || text.contains("skip analysis");
            let has_analysis_word = text.contains("分析") || text.contains("analysis");
            SKIP_ANALYSIS_PHRASES.iter().any(|p| text.contains(p))
                && (!has_direct || has_analysis_word)
        })
        .unwrap_or(false)
}

#[async_trait::async_trait]
impl Tool for AskTool {
    fn name(&self) -> &'static str {
        "ask"
    }
    fn description(&self) -> &'static str {
        "暂停并向用户提出 1–5 个问题，可带多选选项。仅在改变方向的关键决策上节制使用（技术选型、破坏性范围等）。run 会挂起直到用户作答。选项互斥的问题标记 single=true（单选 UI，选中一项即替换之前的选择）；真正的多选则不要设置。plan 档批准协议：产出完整方案后（todos 已登记、分析已完成），必须调用 ask 发起单题询问，提供两个批准类选项 + 一个修订选项：id=\"approve\"（label 以自动编辑档执行，mode=\"auto_edit\"，recommended）、id=\"approve_full\"（label 以完全访问档执行，mode=\"full_access\"）与 id=\"revise\"（label 补充意见/Request changes）。批准类选项必须带 mode 字段声明「选中后把会话切到哪个权限档」（auto_edit=工作区内写入直通，fence 高危命令仍需确认；full_access=跳过审批弹窗），用户选中哪个批准类选项就切到哪档（可从任意档位一次跳档）。完整方案文本放 plan 字段（question 只放题干）——系统会把 plan 自动落盘为计划文件并向用户展示可查看的计划卡；plan 缺失时回退拼接所有题干。用户批准后，系统把会话切到所选档位并通过系统消息指示你立即执行方案（不要再次询问）。目标档（goal mode）澄清期的批准询问：选项 mode 用 \"goal\"——批准后目标从澄清期进入执行期，按账本做最小改动，不冻结计划基线。用户要求修改时，修订方案后再次询问。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["questions"],
  "properties": {
    "questions": {
      "type": "array",
      "minItems": 1,
      "maxItems": 5,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "question"],
        "properties": {
          "id": {"type": "string"},
          "question": {"type": "string"},
          "options": {
            "type": "array",
            "maxItems": 6,
            "items": {
              "type": "object",
              "additionalProperties": false,
              "required": ["id", "label"],
              "properties": {
                "id": {"type": "string"},
                "label": {"type": "string"},
                "description": {"type": "string"},
                "recommended": {"type": "boolean"},
                "mode": {"type": "string", "enum": ["auto_edit", "full_access", "confirm_each", "plan", "goal"], "description": "批准类选项专用（C1）：该选项被选中时把会话切到该权限档；批准门的两个批准类选项用 auto_edit / full_access；目标档澄清期的批准选项用 goal。非批准类选项（如补充意见）不要设置"}
              }
            }
          },
          "single": {"type": "boolean", "description": "选项互斥（单选）：选中一项即替换之前的选择；仅用于二选一问题，真正的多选问题不要设置"}
        }
      }
    },
    "lightweight": {"type": "boolean", "description": "G2 豁免：轻量路径声明（跳过分析产物校验，仍要求 todos 非空）"},
    "skipAnalysis": {"type": "boolean", "description": "G2 豁免：用户已明确要求跳过分析（需最近一条用户消息命中口令）"},
    "switchToAutoEdit": {"type": "boolean", "description": "完整流水线批准门（键名拼写必须精确）：批准时把 ConfirmEach 会话切到自动编辑档；仅对含 'approve' 选项的单题 ask 生效"},
    "plan": {"type": "string", "description": "完整方案正文（markdown）：plan 档批准形询问必须携带——系统把它落盘为计划文件，供计划卡「查看完整计划」展示；question 只放题干，缺失时系统回退拼接题干"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Interactive
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        if args.questions.is_empty() {
            return ToolOutcome::err("E_ARGS", "至少一个问题");
        }
        let ask_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        ctx.rt.open_ask(&ask_id, tx);
        // 竞态免疫（缺陷修复）：挂起期间前端可能先发 set_session_prefs（权限胶囊同步）
        // 把档位切到 AutoEdit；若应答后才重读档位，switch 会被误判为 false →
        // 基线冻结 / [system] 注入全部被跳过（日志证据：approved=true switch=false）。
        // switch 判定改用 open 时刻的档位快照。
        let mode_at_open = ctx.approval_mode();
        // [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：计划文件落盘泛化——所有 ask（不再限于批准闸形态）都把完整
        // 方案文本存为计划文件，前端计划卡的「查看完整计划」对一切 ask 生效；多题 ask 按
        // 顺序拼接（plan_text）。Plan 档没有写工具，落盘是系统行为；失败返回 None 优雅降级，不阻塞 ask。
        // 内容源：显式 plan 字段优先（批准形契约），缺失/空白回退题干拼接（历史行为兼容）。
        let plan_file = save_plan_file(ctx, &plan_body(args.plan.as_deref(), &args.questions));
        ctx.core.sink.emit(
            &ctx.rt.id,
            "ask:opened",
            json!({
                "session": ctx.rt.id, "ask_id": ask_id, "kind": "ask",
                "questions": args.questions,
                // arch 批准闸标记（附加字段，不新增事件键）：前端用它批准后同步权限胶囊
                "switch_to_auto_edit": arch_gate_shape(&args.questions) && args.switch_to_autoedit.unwrap_or(false),
                // [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：计划文件路径（计划卡「查看完整计划」按钮打开；落盘失败为 None）
                "plan_file": plan_file,
                // [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准形标记（后端宽松检测 = 单一事实源）；
                // 前端据此渲染单选 + 批准项直提，不再猜测 id === "approve"
                //（此前 id 自拟会使面板回退为多选切换）
                "approval": approval_shape(&args.questions),
                "approve_id": approve_option_id(&args.questions),
            }),
        );

        let answer = tokio::select! {
            _ = ctx.cancel.cancelled() => None,
            r = rx => r.ok(),
        };
        ctx.core.sink.emit(
            &ctx.rt.id,
            "ask:closed",
            json!({ "session": ctx.rt.id, "ask_id": ask_id }),
        );

        let Some(answer) = answer else {
            crate::core::session_log::warn(
                ctx.rt.as_ref(),
                &format!("ask [{ask_id}] 用户取消（run 中止或应答通道关闭）"),
            );
            return ToolOutcome::err("E_ASK_CANCELLED", "用户取消或未回答");
        };

        // 忽略检测（[docs/ask-ignore-not-answered-fix](../../../../docs/ask-ignore-not-answered-fix.md)）：前端「忽略」= 清空末页直提空载荷
        //（所有 selections 空 + 所有 note 空）；产品语义是「未回答 / 不作答」。
        // 此前空载荷仍返回 ToolOutcome::ok（仅尾注「（未回答）」），模型收到成功形态的结果
        // 会误读为「用户已应答 / 默许」而继续推进（真实案例：批准形询问被忽略后模型直接开工）。
        // 必须在工具层显式失败，让「停下等待用户指示」作为硬信号到达模型，
        // 而不是指望模型注意到尾注。本分支位于 ask:closed 之后，前端 ask 卡已正常关闭。
        if !has_valid_answer(&args.questions, &answer) {
            crate::core::session_log::warn(
                ctx.rt.as_ref(),
                &format!(
                    "ask [{ask_id}] 用户忽略（全空应答：所有题 selections/note 均空）→ 按 E_ASK_NOT_ANSWERED 返回"
                ),
            );
            return ToolOutcome::err(
                "E_ASK_NOT_ANSWERED",
                "用户忽略了本次询问（未作答）。这不是批准或确认：不要继续执行、不要重发同一询问；请简要说明当前状态或调整方案，然后停下等待用户的进一步指示。",
            );
        }

        // 组装模型可读的应答文本
        let mut lines = Vec::new();
        let mut approved = false;
        for q in &args.questions {
            let ans = &answer["answers"][&q.id];
            let sel: Vec<String> = ans["selections"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            // [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准候选 id 集合（宽松 is_approve_option 匹配，覆盖
            // label 合规的情形——此前 id 自拟且不含 approve/execute 子串会让计划批准静默失效）
            let approve_ids: std::collections::HashSet<&str> = q
                .options
                .iter()
                .filter(|o| is_approve_option(o))
                .map(|o| o.id.as_str())
                .collect();
            // Plan 档协议：选中批准选项（或选项含 approve/execute 字样）即视为方案批准 → 切自动编辑档
            let approved_hit = sel.iter().any(|s| {
                s == "approve"
                    || s.contains("approve")
                    || s.contains("执行")
                    || s.contains("Approve")
                    || approve_ids.contains(s.as_str())
            });
            if approved_hit {
                approved = true;
            }
            let note = ans["note"].as_str().unwrap_or("");
            let mut l = format!("Q({}): {}", q.id, q.question);
            if !sel.is_empty() {
                l.push_str(&format!(" → 选定：{}", sel.join(", ")));
            }
            if !note.is_empty() {
                l.push_str(&format!("；补充：{note}"));
            }
            if sel.is_empty() && note.is_empty() {
                l.push_str(" → （未回答）");
            }
            lines.push(l);
        }

        // 切换判定（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) + [docs/arch-orchestrator](../../../../docs/arch-orchestrator.md) + [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)）：Plan 档批准命中即切换（既有协议）；ConfirmEach
        // 档看 arch 批准闸标记（[docs/arch-orchestrator](../../../../docs/arch-orchestrator.md)）或「任一有效应答」（[docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：至少一题 selections/note
        // 非空）；AutoEdit/FullAccess 无需切换。有效应答与批准命中（approved，匹配 execute/approve
        // 子串）是两个独立判定，分开持有以免批准协议被任意应答语义污染。
        let valid_answer = has_valid_answer(&args.questions, &answer);
        let arch_flag =
            arch_gate_shape(&args.questions) && args.switch_to_autoedit.unwrap_or(false);
        // 批准门选档确认（C1-C3）：选中批准类选项（mode 非空）= 批准，且目标档位由该选项声明；
        // mode 是结构化单一事实源，宽松子串匹配（approved_hit）只作旧形态兜底。
        let selected_mode = selected_target_mode(&args.questions, &answer);
        if selected_mode.is_some() {
            approved = true;
        }
        // 未声明 mode 的批准/有效应答 → 回落 AutoEdit（改造前的固定行为）
        let switch_target = selected_mode.unwrap_or(crate::core::prefs::ApprovalMode::AutoEdit);
        let switch = wants_mode_switch(
            mode_at_open,
            approved,
            arch_flag,
            valid_answer,
            selected_mode.is_some(),
        );
        crate::core::session_log::info(
            ctx.rt.as_ref(),
            &format!(
                "ask [{ask_id}] 回答（approved={approved} valid={valid_answer} switch={switch}）：{}",
                crate::core::session_log::trunc(&lines.join(" | "), 400)
            ),
        );

        // 目标档批准分支（goal mode，独立于 plan 档批准协议）：打开 ask 时的档位是目标档，
        // 或用户选中的档位就是目标档。目标档有自己的批准点与门禁（已登记目标 + 验收标准），
        // **不冻结 approved_plan 基线**（G3 在目标档必须保持关闭），也不过 G2/G3 计划门。
        // 状态迁移的口径：**只有最终档位落在目标档**才推进澄清期 → 执行期；用户选了非目标档
        //（= 退出目标模式）时保持 `goal.status` 不动（目标留在澄清期：既无执行授权，也不会被
        // 账本闸门误伤），避免「档位非目标档 + 目标 executing」的不一致。
        // 分支判据（`goal_approval`）不看最终档位是有意为之：模型漏在选项里声明 mode 时
        // 批准照常生效（取值见下 final_mode）。
        if goal_approval(mode_at_open, switch_target, approved) {
            // 没有合同的执行一律拒绝：未登记目标 / 没有验收标准 → 批准不生效
            let Some(mut goal) = ctx.rt.goal_snapshot() else {
                return ToolOutcome::err(
                    "E_GOAL_NOT_REGISTERED",
                    "目标模式批准未生效：尚未登记目标。请先用 goal 工具登记目标（text + criteria + ledger），再重新发起批准询问。",
                );
            };
            if goal.criteria.is_empty() {
                return ToolOutcome::err(
                    "E_GOAL_NOT_REGISTERED",
                    "目标模式批准未生效：目标没有验收标准。请先用 goal 工具补齐可判定的 criteria，再重新发起批准询问。",
                );
            }
            // 最终档位：切档发生时取用户所选档位，否则保持打开 ask 时的档位
            //（模型漏声明 mode → `switch=false`，会话仍在目标档，此时批准必须能推进阶段）。
            let final_mode = if switch { switch_target } else { mode_at_open };
            let entered_execute = final_mode == crate::core::prefs::ApprovalMode::Goal;
            // 澄清期 → 执行期（落边车 + 发 goal:update：右栏目标卡与前端状态据此推进）：
            // **仅当最终档位是目标档**；最终档位不是目标档时状态不动（目标留在澄清期）。
            if entered_execute && goal.status != GoalStatus::Executing {
                goal.status = GoalStatus::Executing;
                ctx.rt.set_goal(Some(goal.clone()));
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "goal:update",
                    json!({ "session": ctx.rt.id, "goal": goal }),
                );
                let _ = ctx.core.store.save_goal(&ctx.rt.id, &Some(goal.clone()));
            }
            // 经 ask 从其它档位切进目标档：这条路径不过 `transition_prefs`，前档快照得在此补记
            //（目标达成后按它回落；缺省会错误地回落全局默认）。已有快照 / 打开时已是目标档不补。
            if let Some(prev) = crate::core::agent::goal::prev_mode_to_record(
                mode_at_open,
                switch_target,
                ctx.rt.goal_prev_mode(),
            ) {
                ctx.rt.set_goal_prev_mode(Some(prev));
            }
            // 切到用户所选档位（目标档批准时选项声明的 mode 就是 goal；已是该档则无操作）
            if switch {
                let mut p = ctx.rt.prefs();
                p.approval_mode = switch_target;
                ctx.rt.set_prefs(p);
            }
            crate::core::session_log::info(
                ctx.rt.as_ref(),
                &format!(
                    "目标批准 → {}（{} → {}），未冻结计划基线（G3 在目标档关闭）",
                    if entered_execute {
                        "进入执行期"
                    } else {
                        "未推进阶段（最终档位非目标档，目标留在澄清期）"
                    },
                    mode_label(mode_at_open),
                    mode_label(final_mode)
                ),
            );
            ctx.core.sink.emit(
                &ctx.rt.id,
                "run:inject",
                serde_json::json!({ "session": ctx.rt.id, "count": 1 }),
            );
            // 注入文案按**最终档位**渲染（B4 口径）：未进入执行期时不得声称「已进入执行期」
            //（模型据此误判自己已有执行授权，而工作区实际只读）。
            let guidance = if entered_execute {
                format!(
                    "[system] 方案已批准：目标模式已进入执行期（{}），请立即按方案推进，不要再次询问。只改账本内路径、只跑账本内程序；每完成一条验收标准立即用 goal 工具把该条 done 置 true，全部完成后把 status 置 done。",
                    mode_label(final_mode)
                )
            } else {
                format!(
                    "[system] 方案已批准：会话已切换到{}。目标**未**进入执行期（仍为澄清期，工作区只读）：如需按目标模式推进，请切回目标模式后重新发起批准询问。",
                    mode_label(final_mode)
                )
            };
            return ToolOutcome::ok(json!({
                "summary": format!("{}\n{guidance}", lines.join("\n")),
                "raw": answer,
                "plan_approved": true,
            }));
        }
        // 完整切换路径（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) 协议 / [docs/arch-orchestrator](../../../../docs/arch-orchestrator.md) arch 闸）：G2/G3 门 → 冻结 todos 基线 →
        // 切档 → 注入「执行方案」→ 返回 plan_approved
        if switch && (matches!(mode_at_open, crate::core::prefs::ApprovalMode::Plan) || arch_flag) {
            // G2/G3 硬门（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) §7）：批准生效前校验；失败则批准不生效（不切档、不注入）。
            if let Some(err) = plan_approval_gate(
                ctx,
                args.lightweight.unwrap_or(false),
                args.skip_analysis.unwrap_or(false),
            ) {
                return err;
            }
            // G3 基线：门已保证 todos 非空；重新批准新方案时重置上一轮的范围状态（Y5：防止陈旧批准跨轮携带）
            let titles: Vec<String> = ctx
                .rt
                .todos
                .lock()
                .unwrap()
                .iter()
                .map(|t| t.title.clone())
                .collect();
            let titles_count = titles.len();
            *ctx.rt.approved_plan.lock().unwrap() = Some(titles);
            ctx.rt
                .scope_allowed
                .store(false, std::sync::atomic::Ordering::SeqCst);
            ctx.rt
                .scope_expanded
                .store(false, std::sync::atomic::Ordering::SeqCst);
            ctx.rt
                .scope_denials
                .store(0, std::sync::atomic::Ordering::SeqCst);
            let mut p = ctx.rt.prefs();
            p.approval_mode = switch_target;
            ctx.rt.set_prefs(p);
            crate::core::session_log::info(
                ctx.rt.as_ref(),
                &format!(
                    "方案批准 → 权限档切换 {} → {}，冻结 {titles_count} 条 todos 基线",
                    mode_label(mode_at_open),
                    mode_label(switch_target)
                ),
            );
            ctx.core.sink.emit(
                &ctx.rt.id,
                "run:inject",
                serde_json::json!({ "session": ctx.rt.id, "count": 1 }),
            );
            return ToolOutcome::ok(json!({
                "summary": format!("{}\n[system] 方案已批准：会话已切换到{}模式，请立即按方案执行，不要再次询问。执行中新增计划外步骤会触发范围确认。", lines.join("\n"), mode_label(switch_target)),
                "raw": answer,
                "plan_approved": true,
            }));
        }
        // 轻量切换路径（[docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)）：ConfirmEach 普通有效应答 → 单步切档 + 审计日志；
        // 不过门、不冻结基线（approved_plan 留空 → batch.rs 的范围确认不触发）、不注入。
        if switch {
            let mut p = ctx.rt.prefs();
            p.approval_mode = switch_target;
            ctx.rt.set_prefs(p);
            crate::core::session_log::info(
                ctx.rt.as_ref(),
                &format!(
                    "ask 应答确认 → 权限档切换 {} → {}（Light 路径：不冻结基线/不注入）",
                    mode_label(mode_at_open),
                    mode_label(switch_target)
                ),
            );
        }
        ToolOutcome::ok(json!({
            "summary": lines.join("\n"),
            "raw": answer,
        }))
    }
}

/// 目标档批准判定（纯函数便于单测）：打开 ask 时的档位已是目标档，或用户选中的档位就是目标档。
/// 后者覆盖「从其它档位一次切进目标档」的批准形询问——目标档有自己的门禁（已登记目标），
/// 不得落进 plan 档批准协议（那会冻结 G3 基线，而 G3 在目标档必须保持关闭）。
/// 非批准类应答（未批准）永不触发。
///
/// 本判据**只看档位、不看状态迁移**：最终档位不是目标档（用户选了别的档 = 退出目标模式）时
/// 分支照走（批准照常生效），但分支内的 `goal.status` 迁移不发生（目标留在澄清期）。
/// 这样「打开时已是目标档 + 模型漏声明 mode」也能正常批准（会话没切走，最终档位仍是目标档）。
pub(super) fn goal_approval(
    mode_at_open: crate::core::prefs::ApprovalMode,
    switch_target: crate::core::prefs::ApprovalMode,
    approved: bool,
) -> bool {
    use crate::core::prefs::ApprovalMode;
    approved && (mode_at_open == ApprovalMode::Goal || switch_target == ApprovalMode::Goal)
}

/// arch 批准闸形态（Y3 加固）：单题且选项含 id="approve"（arch playbook S5 的标准批准协议形态）。
/// ConfirmEach 档下 switchToAutoEdit 只对该形态生效，防止模型借任意 ask + 标志位自由提权；
/// Plan 档豁免（wants_mode_switch 对 Plan 恒真；既有批准协议本就是该形态）。
pub(super) fn arch_gate_shape(questions: &[Question]) -> bool {
    questions.len() == 1 && questions[0].options.iter().any(|o| o.id == "approve")
}

/// C2：批准类选项判定（结构化单一事实源）——选项携带 mode 即批准类：选中它 = 批准 + 切到该档。
/// 宽松子串匹配（is_approve_option）保留为旧形态兜底，mode 路径优先。
pub(super) fn is_approve_class(o: &Option2) -> bool {
    o.mode.is_some()
}

/// [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准协议选项匹配（宽松）。id 命中 approve/execute 子串
///（既有 approved_hit 语义），或 label 含协议规定的计划批准措辞——label 才是模型
/// 实际对齐协议文本的字段；id 自拟时 label 通常仍合规（真实案例：协议 label
/// 保留、id 自拟）。label 匹配用完整两词短语而非裸动词，
/// 避免「运行测试」之类选项被误判为批准项。
/// 选档确认后（C1/C2）：mode 非空即批准类（结构化优先），子串规则保留作旧形态兜底。
pub(super) fn is_approve_option(o: &Option2) -> bool {
    let id = o.id.to_lowercase();
    let label = o.label.to_lowercase();
    // C2：结构化 mode 字段优先（批准类选项即便 label 未用协议措辞也算批准项）
    is_approve_class(o)
        || id.contains("approve")
        || o.id.contains("执行")
        || o.label.contains("执行方案")
        || o.label.contains("批准方案")
        || label.contains("approve")
}

/// [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准形 ask（宽松；仅驱动前端渲染单选与批准项直提——无安全语义）：
/// 单题 ∧ 任一选项命中批准协议。严格形态（arch_gate_shape）另有用途，
/// 两者不得混同。
pub(super) fn approval_shape(questions: &[Question]) -> bool {
    questions.len() == 1 && questions[0].options.iter().any(is_approve_option)
}

/// [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：主批准选项的 id（前端直提检测用；无批准项时返回 None）。
/// 选档确认后语义修正：不再是「首个」——推荐项优先，其次按声明顺序的首个批准项；无批准项时返回 None
///（前端回退到严格检查）。
pub(super) fn approve_option_id(questions: &[Question]) -> Option<String> {
    questions
        .first()
        .and_then(|q| {
            // 选档确认后「首个」不再确定：推荐项优先（两个批准类选项并存时主批准项 = recommended 的那个）
            q.options
                .iter()
                .filter(|o| is_approve_option(o))
                .min_by_key(|o| u8::from(!o.recommended))
        })
        .map(|o| o.id.clone())
}

/// C2/C3：选中项声明的目标档位——遍历各题选中项，命中带 mode 的选项即取其 mode（先 id 后 label 兜底匹配）。
/// 无批准类选项被选中时返回 None（调用点回落 AutoEdit，保持改造前行为）。
pub(super) fn selected_target_mode(
    questions: &[Question],
    answer: &Value,
) -> Option<crate::core::prefs::ApprovalMode> {
    questions.iter().find_map(|q| {
        let sel = answer["answers"][&q.id]["selections"].as_array()?;
        sel.iter().filter_map(|s| s.as_str()).find_map(|s| {
            q.options
                .iter()
                .find(|o| o.id == s || o.label == s)
                .and_then(|o| o.mode)
        })
    })
}

/// 档位中文名（summary / 审计日志文案用；与前端权限胶囊的档位命名一致）。
pub(super) fn mode_label(mode: crate::core::prefs::ApprovalMode) -> &'static str {
    use crate::core::prefs::ApprovalMode;
    match mode {
        ApprovalMode::ConfirmEach => "逐条确认",
        ApprovalMode::AutoEdit => "自动编辑",
        ApprovalMode::Plan => "计划",
        ApprovalMode::FullAccess => "完全访问",
        ApprovalMode::Goal => "目标模式",
    }
}

/// [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：把完整方案文本落盘为计划文件 `<workspace>/.codewave/tasks/plan-<UTC 时间戳>-<rand4>.md`。
/// 失败返回 None（前端计划卡优雅降级为只显示题干），不阻塞 ask 流程。
pub(super) fn save_plan_file(ctx: &ToolCtx, plan_text: &str) -> Option<String> {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f");
    let rand: String = uuid::Uuid::new_v4().simple().to_string()[..4].to_string();
    let rel = format!(
        "{}/tasks/plan-{ts}-{rand}.md",
        crate::core::config::MANAGED_DIR_NAME
    );
    let resolved = crate::tools::pathutil::resolve_write(&ctx.write_roots(), &rel).ok()?;
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let body = format!("# 计划\n\n{plan_text}\n");
    match crate::util::atomic::atomic_write(&resolved, body.as_bytes()) {
        Ok(()) => {
            crate::core::session_log::info(
                ctx.rt.as_ref(),
                &format!("计划文件已落盘：{}", resolved.display()),
            );
            register_plan_artifact(ctx, &resolved);
            Some(resolved.to_string_lossy().into_owned())
        }
        Err(e) => {
            crate::core::session_log::warn(ctx.rt.as_ref(), &format!("计划文件落盘失败：{e}"));
            None
        }
    }
}

/// [docs/session-cleanup](../../../../docs/session-cleanup.md)：计划文件归属登记——落盘成功后写进会话产物边车并标记
/// 种类为 `plan`（清理时随会话连带删除；右栏「文件」面板不展示计划文件）。三条纪律同 create/edit 工具：
/// 路径归一（`canonical_best_effort`）、任务运行态跳过（无产物消费方，避免边车泄漏）、
/// 登记失败只记告警（绝不阻塞 ask 流程）。归属会话用既有的 `root_session_id ?? id` 口诀（子代理归属主会话）。
fn register_plan_artifact(ctx: &ToolCtx, resolved: &std::path::Path) {
    if ctx.rt.is_task_runtime {
        return;
    }
    let owner = ctx
        .rt
        .root_session_id
        .clone()
        .unwrap_or_else(|| ctx.rt.id.clone());
    let canonical = crate::tools::pathutil::canonical_best_effort(resolved)
        .to_string_lossy()
        .into_owned();
    if let Err(e) = ctx.core.store.append_artifact_kind(
        &owner,
        &canonical,
        crate::core::sessions::ArtifactOp::Create,
        crate::core::sessions::ArtifactKind::Plan,
    ) {
        tracing::warn!("计划文件归属登记失败（{canonical}）：{e}");
    }
}

/// 切换判定（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) + [docs/arch-orchestrator](../../../../docs/arch-orchestrator.md) + [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)）：Plan 档仅批准命中才切（协议不变）；
/// ConfirmEach 档看 arch 批准闸标记（[docs/arch-orchestrator](../../../../docs/arch-orchestrator.md)）或任一有效应答（[docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)）；
/// AutoEdit/FullAccess 本就放开，仅当用户显式选档（选中带 mode 的批准类选项）才切——跨档允许，
/// 可从任意档位跳到用户所选档位（C5：confirm_each/plan 可一次跳两档到 full_access）。
pub(super) fn wants_mode_switch(
    mode: crate::core::prefs::ApprovalMode,
    approved: bool,
    switch_flag: bool,
    valid_answer: bool,
    // 用户选中的批准类选项是否声明了目标档位（mode 非空）
    mode_requested: bool,
) -> bool {
    use crate::core::prefs::ApprovalMode;
    match mode {
        ApprovalMode::Plan => approved,
        ApprovalMode::ConfirmEach => switch_flag || valid_answer,
        // 显式选档优先（C5）：本就放开的档位也按用户所选目标切（含升级 auto_edit → full_access）
        // 目标模式同理：它本就放开（工作区内写直通、审批由账本+阶段机制接管），按用户所选档切入
        ApprovalMode::AutoEdit | ApprovalMode::FullAccess | ApprovalMode::Goal => mode_requested,
    }
}

/// [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：有效应答检查——至少一题 selections 非空或 note 非空（trim 后）。
/// 与批准命中（approved_hit，匹配 execute/approve 子串）相互独立；分开持有以免
/// 批准协议被任意应答语义污染。
pub(super) fn has_valid_answer(questions: &[Question], answer: &Value) -> bool {
    questions.iter().any(|q| {
        let ans = &answer["answers"][&q.id];
        let has_sel = ans["selections"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        has_sel || !ans["note"].as_str().unwrap_or("").trim().is_empty()
    })
}

/// [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：计划文件正文 = 所有题干按顺序拼接（多题 ask 每题一段，空行分隔）。
pub(super) fn plan_text(questions: &[Question]) -> String {
    questions
        .iter()
        .map(|q| q.question.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 计划文件正文源选择：显式 plan 字段优先（trim 后非空才采信，防纯空白占位），
/// 否则回退题干拼接（历史行为兼容）。提取为纯函数以便单测。
pub(super) fn plan_body(plan: Option<&str>, questions: &[Question]) -> String {
    plan.map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| plan_text(questions))
}

/// 批准生效点的 G2/G3 校验。通过返回 None；失败返回拒绝性 ToolOutcome（批准不生效）。
pub(super) fn plan_approval_gate(
    ctx: &ToolCtx,
    lightweight: bool,
    skip_declared: bool,
) -> Option<ToolOutcome> {
    use std::sync::atomic::Ordering;

    // G3 前置：批准硬性要求 todos 非空（构造上消灭「批准了但无基线」状态；Q7=A 只查非空）
    let todo_count = ctx.rt.todos.lock().unwrap().len();
    if todo_count == 0 {
        return Some(ToolOutcome::err(
            "E_PLAN_TODOS_REQUIRED",
            "批准未生效：当前会话没有任何已登记的 todos。请先用 plan 工具登记方案 todos（含验证项），再重新发起批准询问。",
        ));
    }

    // G2 豁免通道：用户口令命中（防模型编造）；轻量声明（跳过分析产物校验；todos 非空已在上方向硬校验）
    if skip_declared && user_message_hit_skip_phrase(&ctx.rt) {
        return None;
    }
    if lightweight {
        return None;
    }

    // G2：分析产物必须存在（pm/tester 子代理成功返回时置位）
    if !ctx.rt.analysis_done.load(Ordering::SeqCst) {
        let denials = ctx.rt.analysis_gate_denials.fetch_add(1, Ordering::SeqCst) + 1;
        let escalate = if denials >= 2 {
            "\n提示：你已多次被拒。请向用户说明情况，或请用户回复「不用分析/直接改」后携带 skipAnalysis=true 重试。"
        } else {
            ""
        };
        return Some(ToolOutcome::err(
            "E_PLAN_ANALYSIS_REQUIRED",
            format!(
                "批准未生效：方案缺少分析产物（尚未调用 product-manager/tester 子代理产出需求分析或根因分析）。请先完成分析再重新发起批准询问；微小改动可携带 lightweight=true 走轻量路径。{escalate}"
            ),
        ));
    }
    None
}
