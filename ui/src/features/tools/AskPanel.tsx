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
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Input, Tag } from "antd";
import { CalendarOutlined, CopyOutlined, LeftOutlined, RightOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
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
  const questions = useMemo(() => (ask?.questions ?? []) as any[], [ask?.questions]);

  // L-2：新问题到来时重置全部交互状态，不残留上一次 ask；
  // 空载荷缺陷修复：推荐选项默认选中（视觉与状态一致；否则提交按钮会发出空 selections）
  useEffect(() => {
    const init: Record<string, string[]> = {};
    for (const q of questions as any[]) {
      const rec = (q.options ?? []).find((o: any) => o.recommended);
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

  const approvalShape =
    ask.approval === true ||
    (questions.length === 1 && ((questions[0]?.options ?? []) as any[]).some((o) => o.id === "approve"));
  // 计划批准去重前置条件（下方 .q-text 渲染消费）：单题 + 批准形 + 带计划卡的 ask
  const planApprovalSingle = isPlan && questions.length === 1 && approvalShape;
  const approveId = ask.approveId ?? null;
  const approveOptionId =
    approveId ?? (((questions[0]?.options ?? []) as any[]).find((o) => o.id === "approve")?.id ?? null);

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

  function pickAndSubmitIfApprove(qid: string, optId: string) {
    if (approvalShape && approveOptionId != null && optId === approveOptionId) {
      void submitWith({ [qid]: [optId] });
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
        const rec = ((q.options ?? []) as any[]).find((o) => o.recommended);
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
    // [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：有效应答（≥1 题 selections 非空或有说明）与后端 has_valid_answer 对齐
    let anyAnswer = false;
    for (const q of questions) {
      const sel = override[q.id] ?? selected[q.id] ?? [];
      const note = notesSrc[q.id] ?? "";
      answers[q.id] = { selections: sel, note };
      // Plan 档协议：选中批准选项即在本地把胶囊同步为 auto_edit 档（后端在 ask 工具内切档）
      // [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准命中优先后端下发的 approve_id；保留正则兜底（旧载荷 / 无 id 情形）
      if (
        sel.includes("approve") ||
        sel.some((s) => /approve|执行方案/i.test(s)) ||
        (approveOptionId != null && sel.includes(approveOptionId))
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
    // (1) plan 档批准协议 / arch 批准闸（switchToAutoEdit）；(2) ConfirmEach 有效应答（[docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)）
    if (approved || anyAnswer) {
      const { tabs, activeKey, updatePrefs } = useSessions.getState();
      const tab = tabs.find((t) => t.key === activeKey);
      const planPath = approved && (tab?.prefs.approval_mode === "plan" || ask!.switchToAutoEdit);
      const lightPath = tab?.prefs.approval_mode === "confirm_each" && anyAnswer;
      if (tab && (planPath || lightPath)) {
        void updatePrefs(tab.key, { approval_mode: "auto_edit" });
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
    const opts = (cur?.options ?? []) as any[];
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
  const options = (cur?.options ?? []) as any[];

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
                {options.map((opt: any, i: number) => {
                  const on = sel.includes(opt.id);
                  // 形态优先级：批准 > single（模型声明互斥）> 多选——radio 声明互斥语义
                  const singleQ = !approvalShape && cur?.single === true;
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
                      {opt.description && <span className="desc">{opt.description}</span>}
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