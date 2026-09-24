// 键盘高亮类名是 "kb"（绝不能用 "cursor"：app.css 曾有个全局 `.cursor`——流式等待指示的闪烁动画，
// 类名相撞会让高亮行永远闪烁，用户报的「执行计划按钮行闪烁」正是这个）。
// `.cursor` 与 `@keyframes blink` 已随 [docs/chat-loading-indicator](../../../../docs/chat-loading-indicator.md)
// 删除（等待指示改为语义化的 `.ws-streaming-indicator` + antd 加载图标）。
// **别再新建任何叫 cursor 的类**：它与 CSS 的 cursor 属性同名，是历史误伤高发点。
// [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：询问窗口重构——卡片布局 + 编号选项 + 键盘导航（Tab/方向键移动、数字快选、Enter 确认）。
// 审批形态：需要权限 + 命令块 + 允许 / 始终允许本项目 / 拒绝 三选项；
// 询问形态：分页多题作答 + 补充说明 + 忽略/提交；计划批准形态额外渲染「方案」卡
// （完整方案 markdown + 复制 + 查看完整方案 -> FileViewerModal 打开后端落盘的计划文件）。
// [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准形渲染（后端判定的单选 + 直提）、复选/单选指示物、
// 补充说明输入纳入键盘导航环。
// 单题形态：模型声明的 `single: true`（非批准题）渲染为替换单选（选了替换、重选清空）；
// 应答协议（selections 数组）不变。
// [docs/mode-gate-and-subagent-sync]：批准门选档确认——选项可携带 `mode`（选中后要切到的权限档位）：
// 带 mode 的选项一律直提（两个档位选项都可直提），提交后按**实际选中的档位**同步胶囊（不再硬编码 auto_edit）；
// 胶囊同步与后端 wants_mode_switch 同源：只要本次应答带 mode 就同步——**不再看当前档位**
// （历史上只在 plan / confirm_each 两条路径同步，档位已是 auto_edit / full_access 时后端会切档而前端不跟，
//  随后任意一次 prefs 全量写入就把后端档位静默翻回去）；
// 旧形态（无 mode）保留 approve_id / 正则兼容路径，胶囊回落 auto_edit。
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Input, Tag } from "antd";
import { CalendarOutlined, CopyOutlined, LeftOutlined, RightOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { ApprovalMode, AskOptionPayload, AskQuestionPayload } from "../../ipc/types";
import { useActiveRun, useRun } from "../../stores/run";
import { useSessions } from "../../stores/sessions";
import { renderMarkdown } from "../../utils/markdown";
import FileViewerModal from "../files/FileViewerModal";

/** 判断键盘事件目标是否在输入框内（输入框内不拦截方向键/数字/空格等按键）。 */
function isFormTarget(e: React.KeyboardEvent): boolean {
  const tag = (e.target as HTMLElement)?.tagName;
  return tag === "INPUT" || tag === "TEXTAREA";
}

/** 回车提交判定（缺陷修复：ask 卡内的回车提交在输入框内曾完全失效）。
 *
 *  旧行为：isFormTarget 命中即早退，而展开卡片后焦点就在「补充说明」输入框里（用户敲回车时
 *  必然在此），于是回车 / 数字键 / 方向键全部被放行——提交按钮只认鼠标，「提交回答不支持回车」。
 *  新行为：只把**回车**从输入框放行路径里拎出来提交（无 Shift、非 IME 组合中）；
 *  其余按键保持旧语义（输入框内不拦截，方向键/空格照常编辑文本）。 */
function enterCommits(e: React.KeyboardEvent): boolean {
  if (e.key !== "Enter" || e.shiftKey) return false;
  if ((e.nativeEvent as KeyboardEvent)?.isComposing) return false;
  e.preventDefault();
  return true;
}

/** 询问面板：审批（三选项单选直提）/ 询问（分页多题 + 编号选项 + 键盘导航环含补充输入 + 忽略/提交）
 *  两种形态；计划卡对所有 ask 展示（后端一律落盘计划文件）；批准/ConfirmEach 有效应答后本地同步切自动编辑档。 */
export default function AskPanel() {
  const { t } = useTranslation();
  const ask = useActiveRun().ask;
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [cursor, setCursor] = useState(0); // 键盘高亮（审批：选中项；询问：当前页，末槽 = 补充输入）
  const [page, setPage] = useState(0); // 询问多题分页
  const [planOpen, setPlanOpen] = useState(false);
  const noteRef = useRef<any>(null); // docs/ask-approval-shape-note-nav：导航环末槽聚焦补充输入
  const sessionId = useSessions((s) => s.activeKey);
  // [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：当前会话审批档（ConfirmEach 在 ask 卡上显示切档提示）
  const tabMode = useSessions((s) => s.tabs.find((x) => x.key === s.activeKey)?.prefs.approval_mode);
  const askId = ask?.askId;
  const questions = useMemo<AskQuestionPayload[]>(() => ask?.questions ?? [], [ask?.questions]);
  // 批准类选项 → 目标档位（后端批准门下发的「以 X 档执行」选项）；缺 mode = 非批准类选项
  const modeById = useMemo(() => {
    const m = new Map<string, ApprovalMode>();
    for (const q of questions) for (const o of q.options ?? []) if (o.mode) m.set(o.id, o.mode);
    return m;
  }, [questions]);

  // L-2：新问题到来时重置全部交互状态，不残留上一次 ask；
  // 空载荷缺陷修复：推荐选项默认选中（视觉与状态一致；否则提交按钮会发出空 selections）
  useEffect(() => {
    const init: Record<string, string[]> = {};
    for (const q of questions) {
      const rec = (q.options ?? []).find((o) => o.recommended);
      if (rec) init[q.id] = [rec.id];
    }
    setSelected(init);
    setNotes({});
    setCursor(0);
    setPage(0);
    setPlanOpen(false);
  }, [askId, questions]);

  // ---------- 审批：三选项 + 单选高亮（hooks 必须在提前 return 之前跑完） ----------
  const approvalOptions = useMemo(() => {
    const opts: { id: string; label: string; desc: string; approved: boolean; always: boolean }[] = [
      { id: "allow", label: t("ask.allow"), desc: t("ask.allowDesc"), approved: true, always: false },
    ];
    if (ask?.allowAlways) {
      opts.push({ id: "always", label: t("ask.alwaysAllow"), desc: t("ask.alwaysAllowDesc"), approved: true, always: true });
    }
    opts.push({ id: "deny", label: t("ask.deny"), desc: t("ask.denyDesc"), approved: false, always: false });
    return opts;
  }, [ask?.allowAlways, t]);

  if (!ask) return null;

  const isApproval = ask.kind === "approval";
  const isPlan = ask.kind === "ask" && !!ask.planFile;
  const cur = questions[page];

  function answer(option: { approved: boolean; always: boolean }) {
    void useRun.getState().resolveAsk(ask!.askId, { approved: option.approved, always: option.always });
  }

  // [docs/mode-gate-and-subagent-sync]：带 mode 的选项 = 批准类选项——即便后端未下发 approval 标记也按批准形渲染
  // （防契约漏字段导致点选不直提）。
  const approvalShape =
    ask.approval === true ||
    modeById.size > 0 ||
    (questions.length === 1 && (questions[0]?.options ?? []).some((o) => o.id === "approve"));
  // 批准闸形状（与后端 arch_gate_shape 同口径：单题 + 含 id="approve"）：该形状上只有选中批准项才动档位——
  // 「补充意见」是「别开工，我要改方案」，不切档（免得前端把胶囊抬到自动编辑档而后端没动，两侧分叉）
  const gateShape =
    questions.length === 1 && ((questions[0]?.options ?? []) as any[]).some((o) => o.id === "approve");
  // 计划批准去重前置条件（下方 .q-text 渲染消费）：单题 + 批准形 + 带计划卡的 ask
  const planApprovalSingle = isPlan && questions.length === 1 && approvalShape;
  const approveId = ask.approveId ?? null;
  const approveOptionId =
    approveId ?? ((questions[0]?.options ?? []).find((o) => o.id === "approve")?.id ?? null);
  // 预览选项（[docs/preview-skill](../../../../docs/preview-skill.md)）：批准门的第四选项，语义是「先看预览」——
  // 既不是批准、也不算有效应答（后端 preview_only 同样排除），故既不触发切档，也不能被当成批准项。
  // 只在批准形询问里认（与后端 preview_option_ids 同口径；普通澄清询问里的同名选项不受影响）
  const previewOptionId = approvalShape
    ? ((questions[0]?.options ?? []) as any[]).find((o) => {
        // 与后端 is_preview_option 同口径：id 优先（大小写不敏感），模型自拟 id 时按 label 兜底
        // （认不出预览项，它就会落回「有效应答」→ ConfirmEach 档被静默切档）
        const id = String(o?.id ?? "").toLowerCase();
        const label = String(o?.label ?? "").toLowerCase();
        return id === "preview" || label.includes("先看预览") || label.includes("preview first");
      })?.id ?? null
    : null;

  /** 选项说明行：后端下发的 description 优先；完全访问档**恒定**追加兜底风险说明
   *  （[docs/mode-gate-and-subagent-sync]：选它会跳过所有审批弹窗，必须就地说明代价）。
   *  兜底文案与后端 description 并存而非互相覆盖——否则模型自带一句轻描淡写的 description
   *  就能把风险提示顶掉（提示注入式淡化），兜底也就失去意义。 */
  function optionDesc(opt: AskOptionPayload): string {
    const risk = opt.mode === "full_access" ? t("ask.fullAccessRisk") : "";
    if (!risk) return opt.description ?? "";
    return opt.description ? `${opt.description}${riskSeparator()}${risk}` : risk;
  }

  /** 兜底风险文案与后端 description 之间的分隔（中英标点各自成套：英文用空格，中文用全角分号）。 */
  function riskSeparator(): string {
    return t("ask.fullAccessRiskSep");
  }

  function toggle(qid: string, optId: string) {
    setSelected((prev) => {
      if (approvalShape) {
        return { ...prev, [qid]: [optId] };
      }
      const q = questions.find((x) => x.id === qid);
      // 单选题（模型声明的互斥选项）：替换单选——选一个替换上一个；
      // 单击已选项保持选中（radio 语义，与批准形一致，不出现取消态）；
      // 「未作答」路径由忽略按钮承担（清空当前题）
      if (q?.single) {
        return { ...prev, [qid]: [optId] };
      }
      const cur = prev[qid] ?? [];
      const next = cur.includes(optId) ? cur.filter((x) => x !== optId) : [...cur, optId];
      return { ...prev, [qid]: next };
    });
  }

  /** 直提判定：批准形下选中「批准类选项」即直接提交（免二次提交钮）。
   *  结构化路径 = 选项带 mode（批准门两个档位选项，都可直提）；兼容路径 = 旧形态按后端 approve_id。
   *  直提走**合并已选状态**的提交（多题场景下不丢其他题的已选答案）。 */
  function pickAndSubmitIfApprove(qid: string, optId: string) {
    // 直提候选三选一：① 带 mode 的档位选项（结构化，两个档位都可直提）；② 兼容路径的批准项（approve_id）；
    // ③ 批准门第四选项「先看预览」——四选项的批准门里，让用户为「先看预览」再点一次「提交回答」纯属多余；
    // 预览项提交后不切档（后端 preview_only 排除），只是把「要看预览」这件事交给模型
    const direct =
      approvalShape &&
      (modeById.has(optId) ||
        (approveOptionId != null && optId === approveOptionId) ||
        (previewOptionId != null && optId === previewOptionId));
    if (direct) {
      // 合并已选状态后再直提（缺陷修复：此前只带当前题的答案，多题场景下其他题已选/已填的内容被丢弃；
      // 单题批准门是主路径，不受影响）——本次点击覆盖当前题，其余题保持已选值
      void submitWith({ ...selected, [qid]: [optId] });
      return;
    }
    toggle(qid, optId);
  }

  async function submit() {
    // 空载荷缺陷修复的双保险：提交按钮路径上仍未作答的题回退到推荐选项；
    // 「忽略」路径（ignoreCurrent 清空后直提）绕开 submit()，保持「未作答」语义不被污染
    const fallback: Record<string, string[]> = { ...selected };
    for (const q of questions) {
      if ((fallback[q.id] ?? []).length === 0) {
        const rec = (q.options ?? []).find((o) => o.recommended);
        if (rec) fallback[q.id] = [rec.id];
      }
    }
    await submitWith(fallback);
  }

  // 多题分页语义（缺陷修复）：非末页的提交动作只翻页——此前任意页点击「提交回答」
  // 都会把未浏览的题用推荐项静默填充后一次性 resolve；现在只有末页（含单题）才真正提交
  function goNextPage() {
    if (page < questions.length - 1) {
      setPage(page + 1);
      setCursor(0);
    }
  }

  async function submitWith(override: Record<string, string[]>, notesOverride?: Record<string, string>) {
    const notesSrc = notesOverride ?? notes;
    const answers: Record<string, { selections: string[]; note: string }> = {};
    let approved = false;
    /** 批准类选项的目标档位（结构化路径）；null = 旧形态无 mode → 胶囊同步回落 auto_edit */
    let approvedMode: ApprovalMode | null = null;
    // [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：有效应答（≥1 题 selections 非空或有说明）与后端 has_valid_answer 对齐
    let anyAnswer = false;
    // 预览-only（[docs/preview-skill](../../../../docs/preview-skill.md)）：批准形询问里选中的项全是预览项、
    // → 不算有效应答（与后端 preview_only 判定对齐；补充说明不算表态）。不排除它，ConfirmEach 档会因
    // 「有效应答 / arch 闸标记」把会话静默切到自动编辑档并按方案开工
    let allPreview = approvalShape && previewOptionId != null;
    let sawPreview = false;
    for (const q of questions) {
      const sel = override[q.id] ?? selected[q.id] ?? [];
      const note = notesSrc[q.id] ?? "";
      answers[q.id] = { selections: sel, note };
      if (previewOptionId != null && sel.includes(previewOptionId)) sawPreview = true;
      if (sel.some((s) => s !== previewOptionId)) allPreview = false;
      // 结构化判定（[docs/mode-gate-and-subagent-sync]）：选中带 mode 的选项即批准，并记下要切到的档位
      for (const s of sel) {
        const mode = modeById.get(s);
        if (mode) {
          approved = true;
          approvedMode = mode;
        }
      }
      // Plan 档协议：选中批准选项即在本地把胶囊同步为批准档位（后端在 ask 工具内切档）
      // [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：兼容路径——批准命中后端下发的 approve_id；保留字面 / 正则兜底（旧载荷无 mode / 无 id 情形）
      // **预览项先剔除**（[docs/preview-skill](../../../../docs/preview-skill.md)）：模型把预览项 id 自拟成含 approve / 执行方案
      // 字样（真实形态：id=preview_approve、label 守约「先看预览」，previewOptionId 的 label 兜底认得出它）时，
      // 宽松匹配会拿它的 id 当批准命中 → approved=true → planPath / switchToAutoEdit 通道把胶囊单边切到自动编辑档，
      // 而后端 approved_hit 已剔除 preview_ids、preview_only 也不切档 → 用户可见结果就是**单边静默提权**。
      // 与本条同批的收紧口径（后端）：候选集 = 批准类选项 − preview_option_ids。
      // 注意 previewOnly 只在下方管「有效应答」轻量通道，管不到这条批准通道——故必须在此处过滤，不能靠它兜底。
      const approveCandidates = previewOptionId == null ? sel : sel.filter((s) => s !== previewOptionId);
      if (
        !approved &&
        (approveCandidates.includes("approve") ||
          approveCandidates.some((s) => /approve|执行方案/i.test(s)) ||
          (approveOptionId != null && approveCandidates.includes(approveOptionId)))
      ) {
        approved = true;
      }
      if (sel.length > 0 || note.trim() !== "") {
        anyAnswer = true;
      }
    }
    // 先交付应答再同步胶囊（缺陷修复：此前 updatePrefs 先跑，后端 ask 工具恢复后读到新模式前
    // 前端已切到 AutoEdit，误判「无需切档」-> baseline/注入全部跳过；
    // 后端另有打开时的模式快照兜底；此处调整顺序从源头消除竞态）
    await useRun.getState().resolveAsk(ask!.askId, { answers });
    // 非渲染路径禁止 hooks（缺陷修复：submit 内调 useActiveTab 抛「Invalid hook call」，
    // 提交按钮看似失效）-> 改用 getState() 命令式读取
    // 同步条件（[docs/arch-orchestrator](../../../../docs/arch-orchestrator.md) R1 + [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)）：后端即将切档时本地同步胶囊，使胶囊/前端状态/后端
    // 三方一致；否则后续全量 updatePrefs 覆盖会把后端的 AutoEdit 静默翻回——

    // 「只看预览」不算切档依据——补充说明不算表态，只看选中的项；后端 preview_only 同样封掉两条通道：
    // ① 结构化选档（选中带 mode 的选项）② ConfirmEach 的有效应答 / arch 批准闸。
    const previewOnly = allPreview && sawPreview;
    const answerEffective = anyAnswer && !previewOnly;
    // (1) 批准门选档：只要本次应答带 mode 就同步（**与后端 wants_mode_switch 同源**，不看当前档位——
    //     档位已是 auto_edit / full_access 时后端照样会切档（含 auto_edit → full_access 升级），
    //     此前只在 plan / confirm_each 两条路径同步，正是「后端切了档、前端不跟」的根因）；
    // (2) plan 档批准协议 / arch 批准闸（switchToAutoEdit）；(3) ConfirmEach 有效应答
    //     （批准闸上只有批准项触发轻量切档，与后端 gate_shape 收紧同口径；非闸形状询问保持既有语义）
    if (approved || answerEffective) {
      const { tabs, activeKey, updatePrefs } = useSessions.getState();
      const tab = tabs.find((t) => t.key === activeKey);
      // 两条读 approved 的通道都要再乘 !previewOnly（与后端 `switch = !preview_only_answer && …` 同口径）：
      // approved 只由「选中项带 mode」（结构化）或「命中批准候选」（兼容）得出，两者都挡不住
      // 「模型违约给预览项挂 mode」——该载荷下 approved=true，planPath（plan 档或 switchToAutoEdit 标志）
      // 与 modePath（approvedMode 非空）都会单边把胶囊切档，而后端 preview_only_answer 为真、根本不切 →
      // 用户可见结果就是**单边静默提权**。lightPath 已由 answerEffective（含 !previewOnly）封住，无需再乘。
      const planPath = approved && !previewOnly && (tab?.prefs.approval_mode === "plan" || ask!.switchToAutoEdit);
      const lightPath =
        tab?.prefs.approval_mode === "confirm_each" && answerEffective && (!gateShape || approved);
      // [docs/mode-gate-and-subagent-sync]：结构化路径（选中的选项带 mode）优先且不看当前档位；
      // 旧形态（无 mode）走既有回落：仅 plan / confirm_each 两条路径同步，档位回落 auto_edit
      const modePath = approvedMode != null && !previewOnly;
      if (tab && (modePath || planPath || lightPath)) {
        void updatePrefs(tab.key, { approval_mode: approvedMode ?? "auto_edit" });
      }
    }
  }

  function ignoreCurrent() {
    if (!cur) return;
    // setSelected/setNotes 是异步的；直提必须显式传清空后的值，不能依赖 state 时序
    const clearedSel = { ...selected, [cur.id]: [] };
    const clearedNotes = { ...notes, [cur.id]: "" };
    if (page < questions.length - 1) {
      // 非末页（[docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md) 语义）：清空当前题应答并翻页；不 resolve
      setSelected(clearedSel);
      setNotes(clearedNotes);
      setPage(page + 1);
      setCursor(0);
    } else {
      // 末页（含单题）：忽略 = 清空当前题提交整个 ask（未作答）。
      // 缺陷修复：此前末页点击既不清空、不翻页、不 resolve 也不反馈——按钮像坏的。
      setSelected(clearedSel);
      setNotes(clearedNotes);
      void submitWith(clearedSel, clearedNotes);
    }
  }

  // ---------- 键盘（焦点不在输入框内时生效；卡片有焦点即可用；鼠标始终可用） ----------
  /** 回车统一出口：审批 = 应答当前高亮项；询问 = 非末页翻页 / 末页提交（输入框内与卡片上同一路径） */
  function commitAsk() {
    if (isApproval) {
      answer(approvalOptions[cursor] ?? approvalOptions[0]);
      return;
    }
    if (page < questions.length - 1) goNextPage();
    else void submit();
  }

  function onKeyApproval(e: React.KeyboardEvent) {
    if (enterCommits(e)) {
      commitAsk();
      return;
    }
    if (isFormTarget(e)) return;
    const n = approvalOptions.length;
    if (e.key === "ArrowDown" || e.key === "Tab") {
      e.preventDefault();
      setCursor((c) => (c + 1) % n);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => (c - 1 + n) % n);
    } else if (/^[1-9]$/.test(e.key)) {
      const i = Number(e.key) - 1;
      if (i < n) {
        e.preventDefault();
        answer(approvalOptions[i]);
      }
    } else if (e.key === "Enter") {
      e.preventDefault();
      answer(approvalOptions[cursor] ?? approvalOptions[0]);
    }
  }

  function onKeyAsk(e: React.KeyboardEvent) {
    if (enterCommits(e)) {
      commitAsk();
      return;
    }
    if (isFormTarget(e)) return;
    const opts = cur?.options ?? [];
    if (!cur) return;
    // [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：导航环包含补充输入（末槽 = opts.length）；无选项题的环只有输入框
    const ringLen = opts.length + 1;
    if (e.key === "ArrowDown" || e.key === "Tab") {
      // 末槽（补充输入）按 Tab：放行自然焦点进入输入框（onFocus 同步导航槽）——
      // 回车已让位给「下一题/提交」，输入框键盘可达性改由 Tab 兜底
      if (e.key === "Tab" && cursor >= opts.length) return;
      e.preventDefault();
      setCursor((c) => (c + 1) % ringLen);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => (c - 1 + ringLen) % ringLen);
    } else if (/^[1-9]$/.test(e.key)) {
      const i = Number(e.key) - 1;
      if (i < opts.length) {
        e.preventDefault();
        pickAndSubmitIfApprove(cur.id, opts[i].id);
      }
    } else if (e.key === "Enter") {
      e.preventDefault();
      // 批准形维持旧语义：回车 = 选中高亮项（批准项直提）；高亮在补充输入槽时聚焦输入框
      if (approvalShape) {
        const opt = opts[cursor];
        if (opt) pickAndSubmitIfApprove(cur.id, opt.id);
        else noteRef.current?.focus();
        return;
      }
      // 非批准形：回车 = 下一题 / 提交（选项选择走鼠标、数字键或空格）
      commitAsk();
    } else if (e.key === " ") {
      e.preventDefault();
      if (cursor >= opts.length) return;
      const opt = opts[cursor];
      if (opt) pickAndSubmitIfApprove(cur.id, opt.id);
    }
  }

  const planText = isPlan ? questions.map((q) => q.question).join("\n\n") : "";
  const sel = selected[cur?.id ?? ""] ?? [];
  const options = cur?.options ?? [];

  return (
    <div className="ask-wrap">
      <div className="ask-card" tabIndex={0} onKeyDown={isApproval ? onKeyApproval : onKeyAsk}>
        {/* 计划卡（docs/run-queue-and-ask-revamp/35）：所有 ask 均展示（后端一律落盘计划文件）；完整方案 + 复制 + 查看完整方案 */}
        {isPlan && (
          <div className="plan-card">
            <div className="plan-head">
              <span className="plan-title">
                <CalendarOutlined /> {t("ask.planTitle")}
              </span>
              <Button
                type="text"
                size="small"
                icon={<CopyOutlined />}
                title={t("ask.copyPlan")}
                onClick={() => void navigator.clipboard.writeText(planText).catch(() => {})}
              />
            </div>
            <div className="plan-body md" dangerouslySetInnerHTML={{ __html: renderMarkdown(planText) }} />
            {ask.planFile && (
              <div className="plan-foot">
                <Button size="small" type="primary" onClick={() => setPlanOpen(true)}>
                  {t("ask.viewFullPlan")} →
                </Button>
              </div>
            )}
          </div>
        )}

        <div className="ask-head">
          {/* docs/ask-ink-accent-and-composer-cover：标签去掉 antd preset 色（processing/warning）——元数据降噪，描边 pill 样式由 .ask-tag 接管；
              审批保留描边橙语义（色彩强度映射风险：无色=默认、橙=需注意、红=危险） */}
          <Tag className={`ask-tag${isApproval ? " warn" : ""}`}>
            {isApproval ? t("ask.approval") : t("ask.title")}
          </Tag>
          {!isApproval && cur && questions.length > 1 && (
            <span className="ask-pager">
              <span
                className={`pager-btn${page === 0 ? " off" : ""}`}
                role="button"
                aria-label={t("ask.prevPage")}
                title={t("ask.prevPage")}
                onClick={() => { if (page > 0) { setPage(page - 1); setCursor(0); } }}
              >
                <LeftOutlined />
              </span>
              <span className="pager-label">{page + 1}/{questions.length}</span>
              <span
                className={`pager-btn${page >= questions.length - 1 ? " off" : ""}`}
                role="button"
                aria-label={t("ask.nextPage")}
                title={t("ask.nextPage")}
                onClick={() => { if (page < questions.length - 1) { setPage(page + 1); setCursor(0); } }}
              >
                <RightOutlined />
              </span>
            </span>
          )}
        </div>

        {isApproval ? (
          <>
            <div className="ask-status">⧖ {t("ask.waitingConfirm")}</div>
            {ask.detail && <pre className="ask-detail">{ask.detail}</pre>}
            <div className="ask-options">
              {approvalOptions.map((o, i) => (
                <div
                  key={o.id}
                  className={`ask-opt single ${cursor === i ? "kb" : ""} ${cursor === i ? "picked" : ""}`}
                  role="radio"
                  aria-checked={cursor === i}
                  onClick={() => { setCursor(i); answer(o); }}
                >
                  {/* docs/ask-approval-shape-note-nav：单选指示物（选中跟随高亮——此形态点击即应答，高亮 = 待确认项） */}
                  <span className={`opt-box radio${cursor === i ? " checked" : ""}`} aria-hidden />
                  <span className="idx">{i + 1}.</span>
                  <span className="label">{o.label}</span>
                  <span className="desc">{o.desc}</span>
                </div>
              ))}
            </div>
            <div className="ask-foot">
              <span className="hint">ⓘ {t("ask.keyHint")}</span>
              <Button type="primary" onClick={() => answer(approvalOptions[cursor] ?? approvalOptions[0])}>
                {t("ask.confirm")}
              </Button>
            </div>
          </>
        ) : (
          <>
            {cur && (
              <div className="q-text">
                {/* 计划批准去重：上方计划卡已渲染完整方案时，题干收敛为固定问法——
                    question 字段携带的是整份方案，双渲染同一字段是噪音。
                    多题分页与非批准 ask 保留原文（单题批准形 ask 的计划卡内容即题干，零信息损失）。 */}
                {planApprovalSingle ? t("ask.proceedPlan") : cur.question}
              </div>
            )}
            {cur && options.length > 0 && (
              <div className="ask-options">
                {options.map((opt, i) => {
                  const on = sel.includes(opt.id);
                  // 形态优先级：批准 > single（模型声明互斥）> 多选——radio 声明互斥语义
                  const singleQ = !approvalShape && cur?.single === true;
                  const desc = optionDesc(opt);
                  return (
                    <div
                      key={opt.id}
                      className={`ask-opt ${approvalShape || singleQ ? "single" : "multi"} ${cursor === i ? "kb" : ""} ${on ? "picked" : ""}`}
                      role={approvalShape || singleQ ? "radio" : "checkbox"}
                      aria-checked={on}
                      onClick={() => { setCursor(i); pickAndSubmitIfApprove(cur.id, opt.id); }}
                    >
                      {/* docs/ask-approval-shape-note-nav：多选 = 复选框 / 单选 = 单选框——视觉声明选择语义 */}
                      <span className={`opt-box ${approvalShape || singleQ ? "radio" : "check"}${on ? " checked" : ""}`} aria-hidden />
                      <span className="idx">{i + 1}.</span>
                      <span className="label">{opt.label}</span>
                      {/* docs/ask-approval-shape-note-nav：推荐从 ✓ 后缀改为描边 pill——✓ 与复选框视觉相撞 */}
                      {opt.recommended && <span className="rec-pill">{t("ask.recommended")}</span>}
                      {/* 选项说明行：后端 description 优先；完全访问档恒定追加兜底风险说明（见 optionDesc） */}
                      {desc && <span className="desc">{desc}</span>}
                    </div>
                  );
                })}
              </div>
            )}
            {cur && (
              <>
                <Input
                  ref={noteRef}
                  className={`ask-note${cursor >= options.length ? " kb" : ""}`}
                  variant="borderless"
                  value={notes[cur.id] ?? ""}
                  placeholder={t("ask.note")}
                  onChange={(e) => setNotes((prev) => ({ ...prev, [cur.id]: e.target.value }))}
                  onFocus={() => setCursor(options.length)} // 聚焦即同步导航槽（鼠标/Tab，docs/ask-approval-shape-note-nav）
                  /* 输入框内回车 = 下一题 / 提交（缺陷修复：此前 isFormTarget 早退把所有按键放行，
                     在补充说明里敲回车无任何反应；Shift+回车 / IME 组合期放行，见 enterCommits） */
                  onKeyDown={(e) => { if (enterCommits(e)) commitAsk(); }}
                />
                <div className="note-hint">{t("ask.noteHint")}</div>
              </>
            )}
            <div className="ask-foot">
              <span className="hint">
                {/* docs/notification-click-reveal：ConfirmEach 在提交时切档；明示提示防止静默提权 */}
                {tabMode === "confirm_each" && <span className="switch-hint">⚠ {t("ask.switchHint")}</span>}
                {/* docs/ask-approval-shape-note-nav：hint 按形态切换（批准「Enter 确认」/ 多选「Enter 或空格切换」）；
                    单题（互斥）形态有独立提示——选择即替换、重选即清空 */}
                <span>ⓘ {approvalShape ? t("ask.keyHint") : cur?.single ? t("ask.keyHintSingle") : t("ask.keyHintMulti")}</span>
              </span>
              <span className="foot-btns">
                <Button onClick={ignoreCurrent}>{t("ask.ignore")}</Button>
                {/* 多题非末页 = 「下一题」（仅翻页不提交）；末页与单题 = 「提交回答」 */}
                <Button type="primary" onClick={page < questions.length - 1 ? goNextPage : submit}>
                  {page < questions.length - 1 ? t("ask.next") : t("ask.submit")}
                </Button>
              </span>
            </div>
          </>
        )}
      </div>
      {(isPlan || ask.planFile) && (
        <FileViewerModal sessionId={sessionId} path={planOpen ? ask.planFile ?? null : null} onClose={() => setPlanOpen(false)} />
      )}
    </div>
  );
}