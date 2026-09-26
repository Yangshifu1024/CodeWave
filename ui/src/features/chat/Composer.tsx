import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { App, BorderBeam, Button, Dropdown, Image, Input, Popover, Progress } from "antd";
import type { MenuProps } from "antd";
import {
  ArrowUpOutlined, BulbOutlined, CheckCircleOutlined, CloseOutlined,
  DownOutlined, ExclamationCircleOutlined, FileAddOutlined, FileExcelOutlined,
  FileOutlined, FilePdfOutlined, FileTextOutlined, FileWordOutlined,
  PlusOutlined, RobotOutlined, SafetyCertificateOutlined,
  SettingOutlined, StopOutlined, ThunderboltOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useActiveRun, useActiveDraft, useRun, useContextPct } from "../../stores/run";
import { useActiveTab, useSessions } from "../../stores/sessions";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import type { ApprovalMode, EffortLevel, GoalState, GoalStatus } from "../../ipc/types";
import { cacheDenominator, cacheSemanticsOf, findModel } from "../../utils/models";
import { baseName } from "../../utils/path";
import { GOAL_STATUS_DEFAULT, GOAL_STATUS_KEYS } from "../../utils/goal";
import { cacheHitRate, contextTier, type ContextTier } from "../../stores/runFrames";
import { formatInt, formatMs, formatRate, tokPerSec } from "./composerMetrics";
import { ipc } from "../../ipc/client";
import { listenFileDrop } from "../../ipc/dragdrop";
import CompactButton from "./ContextInfoBar";
import AskPanel from "../tools/AskPanel";
import QueuePanel from "./QueuePanel";
import ExternalDirPrompt from "./ExternalDirPrompt";
import type { DirDecision } from "./useComposerAttachments";
import { useComposerAttachments } from "./useComposerAttachments";
import { useComposerHistory } from "./useComposerHistory";
import { useComposerMentions } from "./useComposerMentions";
import { useComposerEvents } from "./useComposerEvents";
import { clampCaret, detectTrigger, type TriggerHit } from "./composerTriggers";
import { addRefs, mergeRefs, recoverRefs } from "./composerRefs";

const { TextArea } = Input;

// Shift+Tab 循环的权限档顺序（与权限下拉菜单项顺序一致）：
// plan（最安全）→ confirm_each（询问）→ auto_edit（自动）→ goal（自主）→ full_access（完全）
const MODE_ORDER: ApprovalMode[] = ["plan", "confirm_each", "auto_edit", "goal", "full_access"];

/** Composer：底部输入区 + 工具条（左：+/权限/子代理 ｜ 中：上下文/命中/速率 ｜ 右：压缩/模型/力度/发送）。
 *  键盘契约：Enter 发送、Shift+Enter 换行、Shift+Tab 循环权限档、空输入 ↑ 进入历史浏览、
 *  / 触发技能菜单、$ 触发子代理菜单、@ 触发提及菜单（↑↓ 导航、Enter/Tab 选中）；
 *  IME 组合期按键全部放行。ask 弹出时提问卡整体覆盖本组件与队列面板，回答后原样恢复
 * （草稿存 run store 每 Tab 桶，不丢）。触发符语义见 [docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)。 */
export default function Composer() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const active = useActiveRun();
  // [docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)：运行中的子代理（Composer 指示器的计数与菜单数据源）
  const runningSubs = active.subs.filter((s) => s.status === "running");
  const tab = useActiveTab();
  const contextPct = useContextPct();
  const compactThreshold = useSettings((s) => s.config?.compact_threshold ?? 0.6);
  const config = useSettings((s) => s.config);
  const prefs = tab?.prefs ?? { approval_mode: "auto_edit" as ApprovalMode, model_id: null, reasoning_effort: null };
  // 会话级缓存命中率：分母口径随**生效模型的协议**（[docs/prompt-caching-hardening](../../../../docs/prompt-caching-hardening.md)）——
  // anthropic 的 input 不含缓存、openai 的 input 已含 cached_tokens；语义解析不到（未配置模型 / 供应商已删）时
  // 不显示命中段：宁可缺省也不猜口径。分子与分母同源，title 复用同一分母
  const cacheSem = cacheSemanticsOf(config, prefs.model_id);
  const cacheHit = cacheSem ? cacheHitRate(active, cacheSem) : null;
  const hitDenom = cacheSem && active.usage ? cacheDenominator(active.usage, cacheSem) : 0;
  const updatePrefs = useSessions((s) => s.updatePrefs);
  // [docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md)：ask/审批弹出时提问卡覆盖整个输入区（zcode 式：ask 面板是唯一底部输入），
  // Composer 本体与队列面板暂不渲染、回答后原样恢复；草稿存 run store 每 Tab 桶，子树卸载不丢
  const askActive = !!active.ask;

  // 草稿按 Tab 隔离：文本、引用与待发附件存 run store 平行分桶（drafts[key]，见 ComposerDraft 注释），切会话各自保留、
  // 发送成功 clearDraft 清空；setText/setImages/setRefs 与 useState 同形（支持 updater），直接注入下方子 hooks
  const draft = useActiveDraft();
  const text = draft.text;
  // refs 兜底：旧 ui-state 快照（后加字段）回填的桶可能缺该字段
  const refs = draft.refs ?? [];
  const setText = useRun.getState().setDraftText;
  const setImages = useRun.getState().setDraftImages;
  const setRefs = useRun.getState().setDraftRefs;
  // 流光显隐（[docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md)）：输入框聚焦态——仅输入框聚焦或任务进行中时出现
  const [composerFocused, setComposerFocused] = useState(false);
  // 隐藏走 composer-beam-idle（app.css 中 display:none），动画停摆、零绘制
  const beamActive = active.running || composerFocused;
  // 菜单高亮下标（三个菜单互斥，共用一个下标）
  const [activeIndex, setActiveIndex] = useState(0);
  // IME 组合追踪（WebKit 差异）：WebKit 以 isComposing=false（keyCode 229）触发组合确认的 Enter keydown，
  // 单看 nativeEvent.isComposing 不够——经 composition 事件自行维护真值。
  const composingRef = useRef(false);
  const taRef = useRef<any>(null);
  // / @ $ 菜单的宽度上限来源：输入卡片实测宽度（长 description 不再撑出视口，见下方 menuStyle）
  const cardRef = useRef<HTMLDivElement>(null);
  const [menuWidth, setMenuWidth] = useState(0);
  // 工具条分级显示（[docs/composer-responsive-toolbar]）：根据 composer 卡片实测宽度切三档。
  // 复用下方 ResizeObserver 测宽，零额外监听。
  // 阈值：narrow < 600（常规非全宽窗口都进窄档，只显 +、模式图标、压缩、发送）；
  //      medium 600-820（图标 + 模型名 + 力度文字 + 发送）；
  //      normal ≥ 820（全显：toolbar-info + provider/model + chev）。
  type ComposerWidth = "narrow" | "medium" | "normal";
  const [composerWidth, setComposerWidth] = useState<ComposerWidth>("normal");
  // 光标位置（触发判定与回填的唯一锚点，[docs/composer-trigger-caret](../../../../docs/composer-trigger-caret.md)）：
  // onChange 拿事件里的 selectionStart；点击/方向键移光标不过 onChange，由 onSelect/onClick/onKeyUp 补同步。
  // ref 供事件回调读即时值，state 供**渲染期复验**（菜单开合要判「trigger 在当前位置是否仍成立」）
  const caretRef = useRef(0);
  const [caret, setCaret] = useState(0);
  const moveCaret = (n: number) => {
    caretRef.current = n;
    setCaret(n);
  };
  // 光标处的触发片段（null = 无）：菜单开合与回填范围都由它决定，不再从整段 text 派生
  const [trigger, setTrigger] = useState<TriggerHit | null>(null);
  /** 外部链路（fill / 历史召回 / 队列编辑 / 发送清空）直接改写草稿文本，不经过 onInputChange：
   *  这里作废触发片段并把光标收敛到新文末，否则菜单会挂在旧位置、`+` 菜单会插到过期偏移 */
  const onTextReplaced = (len: number) => {
    setTrigger(null);
    moveCaret(len);
  };

  // 聚焦关注点的 hooks（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构）：附件 / 全局事件 / 历史召回 / 提及·技能·子代理菜单
  //
  // 项目外目录放行（[docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)）：
  // 判定与放行都在后端，这里是「问一次」的桥——把 resolve 存起来，等用户点完三选一再让流程继续。
  // 用 Promise 而不是「先弹框、下次事件再续跑」：一次拖入多个外部文件时流程要原地暂停，
  // 拆成状态机会把后面每个文件的处理都变成回调套回调。
  const [extDir, setExtDir] = useState<string | null>(null);
  const extResolve = useRef<((v: DirDecision) => void) | null>(null);
  const askExternalDir = (dir: string) =>
    new Promise<DirDecision>((resolve) => {
      extResolve.current = resolve;
      setExtDir(dir);
    });
  const decideExternalDir = (v: DirDecision) => {
    setExtDir(null);
    const r = extResolve.current;
    extResolve.current = null;
    r?.(v);
  };

  const attachments = useComposerAttachments({
    t,
    message,
    images: draft.images,
    setImages,
    // 显式锁定当前 Tab：判定与放行都是异步的，期间切 Tab 也不能把引用写进别的会话
    sessionId: tab?.key ?? null,
    // [docs/composer-file-ref-chips](../../../../docs/composer-file-ref-chips.md)：引用记进草稿 refs（chip 展示），
    // 不再往正文里押 `@路径`（发送前一刻由 mergeRefs 合成）
    onRefs: (incoming) => setRefs((cur) => addRefs(cur ?? [], incoming), tab?.key),
    askExternalDir,
  });
  const { images, recalledImages, addPaths, onPaste } = attachments;

  /** 系统拖入的文件（[docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)）：
   *  网页层的 ondrop 在窗口开启系统拖放后不再触发，只能走 Tauri 事件通道。 */
  const [dropping, setDropping] = useState(false);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let dropped = false;
    void listenFileDrop({
      onOver: () => setDropping(true),
      onLeave: () => setDropping(false),
      onDrop: (paths) => {
        setDropping(false);
        void addPaths(paths);
      },
    }).then((un) => {
      // 订阅是异步完成的：组件已卸载就直接取消，别留一个永远收不到的监听
      if (dropped) un();
      else unlisten = un;
    });
    return () => {
      dropped = true;
      unlisten?.();
    };
  }, [addPaths]);

  /** 附件按钮：开原生文件选择框（拿真实路径，这是「原地引用」的前提）。 */
  async function pickFiles() {
    try {
      const paths = await ipc.selectDocumentFiles();
      if (paths.length) await addPaths(paths);
    } catch (e) {
      message.warning(String(e).replace(/^Error[:\s]*/i, ""));
    }
  }

  /** 移除一项文件引用：引用不在正文里，只从草稿 refs 桶里剔除（不碰正文文字） */
  function removeRef(ref: string) {
    setRefs((cur) => (cur ?? []).filter((r) => r !== ref), tab?.key);
  }

  useComposerEvents({ taRef, setText, setImages, setRefs, recalledImages, onTextReplaced });
  const history = useComposerHistory({
    tabKey: tab?.key,
    setText,
    setImages,
    setRefs,
    recalledImages,
    onTextReplaced,
  });
  const mentions = useComposerMentions({
    setActiveIndex,
    // 提及选中文件 → 进引用 chip（引用不再写进正文，[docs/composer-file-ref-chips]）
    addFileRef: (path) => setRefs((cur) => addRefs(cur ?? [], [path]), tab?.key),
    // 回填只替换「光标处那段触发片段」（实现在下方 replaceFragment）
    replaceFragment,
  });
  const { histIdx, setHistIdx, draftRef, recallHistory, applyRecall, exitRecall } = history;
  const {
    mentionResults, skillResults, agentResults,
    refreshMention, refreshSkills, refreshAgents,
    pickMention, pickSkill, pickAgent,
    clearMentions, clearSkills, clearAgents,
  } = mentions;

  // 会话切换：收起提及/技能/子代理菜单（候选是上一会话的查询结果，残留会把旧菜单顶进新会话）并复位高亮
  const tabKey = tab?.key;
  useEffect(() => {
    clearMentions();
    clearSkills();
    clearAgents();
    setActiveIndex(0);
    setTrigger(null); // 触发片段属于上一会话的光标位置，一并作废
    moveCaret(0);
    // clear* 由 useComposerMentions 每次渲染新建（普通函数声明）：纳入依赖会让本 effect 每渲染重跑，
    // 刚拉回的候选列表立刻被清空；这里只按切 Tab 驱动是有意为之
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tabKey]);

  // [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：队列条目「编辑」-> 文本、引用与附件回填输入框并聚焦（附件复用历史召回图片同一兜底：上限 4 张 / 20MB）。
  // 队列条目文本是发送时合成过的（含末尾 `@路径`），回填前先经 recoverRefs 抽回引用 chip
  // （[docs/composer-file-ref-chips]；带回往门禁，手打形态不解析）
  // 回填显式锁定点击所在 Tab：effect 提交前切 Tab 也不会把队列内容写进新会话、或因消费落空而二次回填
  const draftFromQueue = active.draftFromQueue;
  useEffect(() => {
    if (draftFromQueue == null) return;
    const targetKey = tab?.key;
    const parsed = recoverRefs(draftFromQueue.text);
    useRun.getState().setDraftText(parsed.text, targetKey);
    useRun.getState().setDraftRefs(parsed.refs, targetKey);
    onTextReplaced(parsed.text.length);
    if (draftFromQueue.images?.length) {
      useRun.getState().setDraftImages(
        recalledImages(draftFromQueue.images.map((im) => ({ mediaType: im.mime, data: im.data }))),
        targetKey,
      );
    }
    useRun.getState().consumeDraftFromQueue(targetKey);
    const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
    el?.focus?.();
    // onTextReplaced 每次渲染都会重新创建；此处只按队列条目/Tab/图片回填变化执行。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draftFromQueue, tab?.key, recalledImages]);

  // 会话生效模型：会话覆盖 -> 全局活跃（展示与发送守卫同一数据源，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)；摊平视图 [docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）
  const globalModel = findModel(config, config?.active_model_id ?? null);
  const sessionModel = prefs.model_id ? findModel(config, prefs.model_id) : null;
  const effectiveModel = sessionModel ?? globalModel;
  const effortValue: EffortLevel | "default" = prefs.reasoning_effort ?? "default";

  // （命令入口已自 / 菜单移除：git/diff/tasks/stats 走顶栏四入口、compact 走工具条按钮，
  //   [docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）

  /** 从 + 菜单插入触发字符（@ / / / $）：插到**光标处**（原来追加到末尾），并把光标推到插入内容之后；
   *  菜单开合交给 onInputChange 按光标前片段重新判定（[docs/composer-trigger-caret]）。 */
  function insertTrigger(ch: string) {
    const caretNow = clampCaret(text, caretRef.current);
    const before = text.slice(0, caretNow);
    // 与光标前的字隔开（行首/空白后则不加空格），触发片段因此从光标前的边界开始
    const insert = before === "" || /\s$/.test(before) ? ch : ` ${ch}`;
    const next = before + insert + text.slice(caretNow);
    const nextCaret = caretNow + insert.length;
    setText(next);
    moveCaret(nextCaret);
    void onInputChange(next, nextCaret);
    requestAnimationFrame(() => {
      const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
      el?.focus?.();
      el?.setSelectionRange?.(nextCaret, nextCaret);
    });
  }

  /** 把 DOM 里的真实光标同步进状态（点击 / 方向键 / 选中改变光标都不会经过 onChange） */
  function syncCaret() {
    const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
    const sel = el?.selectionStart;
    if (typeof sel === "number") moveCaret(clampCaret(String(el?.value ?? text), sel));
  }

  /** 回填：把「光标处那段触发片段」替换成 insert，光标落到插入内容之后；
   *  光标之后的正文一字不改；无触发片段时插到光标处。
   *  片段**现算**（不信任上一次 onChange 留下的 trigger 快照）：方向键能把光标移到片段之前，
   *  照搬旧 start 会拼出重复正文（审查 🔴）。 */
  function replaceFragment(insert: string) {
    const caretNow = clampCaret(text, caretRef.current);
    const live = detectTrigger(text, caretNow);
    let start = Math.min(live ? live.start : caretNow, caretNow);
    // 空插入 = 纯删除片段（提及选文件转 chip）：顺手吃掉片段前那整段空白，免得正文留个尾空格
    if (insert === "") while (start > 0 && /[ \t]/.test(text[start - 1])) start -= 1;
    const next = text.slice(0, start) + insert + text.slice(caretNow);
    const nextCaret = start + insert.length;
    setText(next);
    setTrigger(null);
    moveCaret(nextCaret);
    requestAnimationFrame(() => {
      const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
      el?.focus?.();
      el?.setSelectionRange?.(nextCaret, nextCaret);
    });
  }

  // 菜单键盘导航：↑↓ 移动高亮，Enter/Tab 选中；取模防越界（列表变短也安全）
  // 三个互斥菜单共用同一高亮下标：/ 技能 -> $ 子代理 -> @ 提及
  // 开合由「光标处的触发片段 + 候选结果」共同决定（[docs/composer-trigger-caret]：不再从整段 text 派生）。
  // trigger 只是「上一次输入判定的快照」：光标可能已被方向键/点击移走（或文本被外部链路改写），
  // 渲染期用当前光标复验——不成立就当没有触发片段，菜单随之收起，不会按过期片段回填
  const hit = trigger && trigger.end === caret ? trigger : null;
  const slashOpen = hit?.kind === "slash" && skillResults.length > 0;
  const agentOpen = hit?.kind === "dollar" && agentResults.length > 0;
  const mentionOpen = hit?.kind === "at" && mentionResults.length > 0;
  const menuCount = slashOpen
    ? skillResults.length
    : agentOpen
      ? agentResults.length
      : mentionOpen
        ? mentionResults.length
        : 0;
  const selIdx = menuCount ? activeIndex % menuCount : 0;

  function moveMenu(delta: number) {
    setActiveIndex((i) => {
      if (!menuCount) return 0;
      return (i + delta + menuCount) % menuCount;
    });
  }

  function onKeydown(e: React.KeyboardEvent) {
    // IME 守卫（三重检查）：Chromium 以 isComposing=true（keyCode 229）标记组合中的 keydown，WebKit
    // 则以 isComposing=false（keyCode 229）触发组合确认的 Enter——因此同时认可 keyCode 229 与
    // composition 事件追踪的自有状态。覆盖组合窗口与确认 keydown 两个阶段；真正的 Enter
    // （无组合、keyCode 13）不受影响。
    const native = e.nativeEvent as KeyboardEvent;
    if (native.isComposing || native.keyCode === 229 || composingRef.current) return;
    if (e.key === "Tab" && e.shiftKey) {
      // Shift+Tab：四档权限模式循环（[docs/composer-shift-tab-mode-cycle](../../../../docs/composer-shift-tab-mode-cycle.md)）；放在菜单/发送处理之前，@/$ 菜单开着也能切
      e.preventDefault();
      const next = MODE_ORDER[(MODE_ORDER.indexOf(prefs.approval_mode) + 1) % MODE_ORDER.length];
      switchMode(next);
      return;
    }
    if (e.key === "Escape") {
      if (histIdx !== null) {
        exitRecall(); // 浏览中：退出并还原进入前草稿
        return;
      }
      clearMentions();
      clearSkills();
      clearAgents();
      setTrigger(null);
      return;
    }
    if (slashOpen || agentOpen || mentionOpen) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault(); // 方向键在菜单打开时用于导航，不移动输入框内光标
        moveMenu(e.key === "ArrowDown" ? 1 : -1);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (slashOpen) {
          const s = skillResults[selIdx];
          if (s) pickSkill(s);
        } else if (agentOpen) {
          const a = agentResults[selIdx];
          if (a) pickAgent(a);
        } else {
          const item = mentionResults[selIdx];
          if (item) pickMention(item);
        }
        return;
      }
      if (e.key === "Tab" && (agentOpen || mentionOpen)) {
        e.preventDefault();
        if (agentOpen) {
          const a = agentResults[selIdx];
          if (a) pickAgent(a);
        } else {
          const item = mentionResults[selIdx];
          if (item) pickMention(item);
        }
        return;
      }
    }
    // ↑↓ 历史召回：空输入时 ↑ 进入浏览态（最新一条）；浏览中 ↑ 向旧翻（到最旧即停）/ ↓ 向新翻
    // 输入非空且未浏览时不劫持方向键（保留多行光标语义）；打开的菜单已在上方菜单分支消费
    if (e.key === "ArrowUp" && (text === "" || histIdx !== null)) {
      e.preventDefault();
      const hist = recallHistory();
      if (hist.length === 0) return; // 无历史，不动作
      if (histIdx !== null) {
        const prev = Math.max(0, histIdx - 1); // 停在最旧而不是回绕
        applyRecall(hist[prev], prev);
      } else {
        draftRef.current = { text, images, refs }; // 进入浏览态：快照当前草稿（含引用，退出时一并还原）
        applyRecall(hist[hist.length - 1], hist.length - 1);
      }
      return;
    }
    if (e.key === "ArrowDown" && histIdx !== null) {
      e.preventDefault();
      const hist = recallHistory();
      const next = histIdx + 1;
      if (next >= hist.length) {
        exitRecall(); // 越过最新：退出浏览态并还原进入前草稿
        return;
      }
      applyRecall(hist[next], next);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  async function onInputChange(v: string, caretFromEvent?: number) {
    if (histIdx !== null) {
      setHistIdx(null); // 编辑即退出浏览态：保留当前文本作为编辑基底
      draftRef.current = null;
    }
    setText(v);
    // 触发判定：只看**光标前的片段**（[docs/composer-trigger-caret]）。
    // `/`、`$` 限消息开头（模型侧「消息以 /<name> / $<role> 开头」才是点名）；`@` 行首或空白后即可。
    const caretNext =
      caretFromEvent != null ? clampCaret(v, caretFromEvent) : clampCaret(v, v.length);
    moveCaret(caretNext);
    const hit = detectTrigger(v, caretNext);
    setTrigger(hit);
    if (hit?.kind === "slash") {
      clearMentions();
      clearAgents();
      await refreshSkills(hit.query);
      return;
    }
    clearSkills(); // 离开 / 片段：作废在途技能查询并收起菜单
    if (hit?.kind === "at") {
      clearAgents();
      await refreshMention(hit.query);
    } else if (hit?.kind === "dollar") {
      clearMentions();
      await refreshAgents(hit.query);
    } else {
      clearMentions();
      clearAgents();
    }
  }

  // ---------- 发送 ----------

  async function send() {
    // 引用不在正文里（[docs/composer-file-ref-chips]）：发送前一刻合成 `@路径` 追加到末尾，
    // 送给后端与模型的内容与改造前完全一致（chip 只是展示层）
    const v = mergeRefs(text, refs);
    if (!v) return;
    if (!effectiveModel) {
      useRun.getState().pushItem(useSessions.getState().activeKey, { kind: "error", text: t("app.needModel") });
      useUi.getState().showSettings("providers");
      return;
    }
    // vision 软阻断：模型声明不支持图片（vision === false）且有附件时拦截（自 [docs/provider-management-refactor](../../../../docs/provider-management-refactor.md) 起为硬布尔）
    if (images.length > 0 && effectiveModel && !effectiveModel.vision) {
      message.warning(t("composer.visionUnsupported"));
      return;
    }
    // M-2：接受后才清空——运行中按 Enter 不再静默吞掉输入。
    // 目标 Tab 在调用时捕获：startChat 异步窗口内切 Tab，清空的仍是发送方草稿而非新会话的
    const targetKey = useSessions.getState().activeKey ?? undefined;
    const accepted = await useRun.getState().send(v, images.map(({ mime, data }) => ({ mime, data })));
    if (accepted) {
      useRun.getState().clearDraft(targetKey); // 文本 + 附件一并清空（发送方 Tab 桶）
      onTextReplaced(0); // 文本清空且不经 onChange：作废触发片段并把光标收回文首
      setHistIdx(null); // 发送后序列自然追加新消息；复位指针
      draftRef.current = null;
    }
  }

  // 上下文占用百分比（工具条）：有明细时显示百分比 + 用量/窗口 + 自动压缩阈值；否则显示 —（尚无 run）。
  // 分档（相对阈值）与阈值段渲染统一在下方 contextLabel；命中率为独立段（缓存是否生效）。
  const thresholdPct = Math.round(compactThreshold * 100);

  // 发送按钮三态：空闲有内容 = 发送；运行中无内容 = 停止；运行中有内容 = 提交（入队）。
  // 「有内容」含仅挂引用 chip（不写正文）的情形
  const hasDraft = !!text.trim() || refs.length > 0;
  const stopActive = active.running && !hasDraft;

  // 菜单宽度上限 = 输入卡片实测宽度（技能 description 过长时不再撑破视口，与聊天框宽度一致）；
  // ResizeObserver 覆盖窗口缩放、侧栏宽度变化等一切来源（不依赖 window resize）；
  // ask 态卡片卸载、恢复后依赖变化重测。
  // 同步计算工具条分级显示（composerWidth）：< 240px → narrow（仅图标），240~360 → medium
  // （隐藏 toolbar-info + provider），≥ 360 → normal（全部）。
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const sync = () => {
      const w = el.getBoundingClientRect().width;
      setMenuWidth(w);
      setComposerWidth(w < 600 ? "narrow" : w < 820 ? "medium" : "normal");
    };
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => ro.disconnect();
  }, [askActive]);

  const menuStyle = { maxHeight: 320, overflowY: "auto", maxWidth: menuWidth || undefined } as const;

  // ---------- 工具条上下文/命中率显示 ----------

  // 阈值合法性（与后端 clamp(0.05,0.95) 同域）：非法时圈显示中性 ok（不臆测风险），popover 不显示阈值段
  const thresholdValid = Number.isFinite(compactThreshold) && compactThreshold > 0 && compactThreshold <= 1;
  const ctxTier: ContextTier = thresholdValid && active.breakdown
    ? contextTier(active.breakdown.ratio, compactThreshold)
    : "ok";
  const hitPct = cacheHit != null ? `${Math.round(cacheHit * 100)}%` : "";
  // 进度圈 strokeColor 按档取色（4 档全彩，红橙黄绿——与 AGENTS.md 「色彩强度映射风险等级」一致）
  const ctxProgressColor =
    ctxTier === "danger" ? "var(--ws-err)" :
    ctxTier === "warn" ? "var(--ws-warn)" :
    ctxTier === "yellow" ? "#fadb14" :  // 黄：antd 标准 yellow-5，与 RB 配额黄同源
    "var(--ws-ok)";                       // ok：绿

  // ---------- 本轮生成速率（[docs/composer-token-rate](../../../../docs/composer-token-rate.md)） ----------

  // 数据源是 run store 的 runMetrics（不是组件 state）：ask 弹窗遮住 Composer 导致卸载后恢复仍同值。
  // 无数据（从未发过本轮 / 本轮还没收到 usage 帧 / 整页重载回 blank 桶）时 rate 为 null → 整段不渲染，
  // 上下文与命中段不受影响（AC-8）。速率只在工具条展示。
  const metrics = active.runMetrics;
  const rate = tokPerSec(metrics);
  const toolMs = metrics?.toolMs ?? 0;
  /** 速率段悬浮明细（原生 title）：本轮平均速率 / 首步 TTFT / 输出 tokens / 生成耗时 / 工具等待合计。
   *  「工具等待」为 0 时整行不出：没等过工具就不提这一句，免得单步 run 的工具箱显得有噪音。 */
  const rateTitle = rate == null ? "" : [
    `${t("composer.rateTitle")}: ${formatRate(rate)} tok/s`,
    ...(metrics?.ttftMs != null ? [`${t("composer.rateTtft")}: ${formatMs(metrics.ttftMs)}`] : []),
    `${t("composer.rateOutput")}: ${formatInt(metrics?.output ?? 0)}`,
    `${t("composer.rateGenMs")}: ${formatMs(metrics?.genMs ?? 0)}`,
    ...(toolMs > 0 ? [`${t("composer.rateToolWait")}: ${formatMs(toolMs)}`] : []),
  ].join("\n");

  // ---------- 下拉菜单 ----------

  const rich = (title: string, desc: string, cls?: string) => (
    <div className={cls ? `menu-item-rich ${cls}` : "menu-item-rich"}>
      <span>{title}</span>
      <span className="desc">{desc}</span>
    </div>
  );

  const modeDescKeys: Record<ApprovalMode, string> = {
    plan: "composer.modePlanDesc",
    confirm_each: "composer.modeConfirmEachDesc",
    auto_edit: "composer.modeAutoEditDesc",
    goal: "composer.modeGoalDesc",
    full_access: "composer.modeFullAccessDesc",
  };
  const modeLabels: Record<ApprovalMode, string> = {
    plan: t("composer.modePlan"),
    confirm_each: t("composer.modeConfirmEach"),
    auto_edit: t("composer.modeAutoEdit"),
    goal: t("composer.modeGoal"),
    full_access: t("composer.modeFullAccess"),
  };
  const modeIcons: Record<ApprovalMode, React.ReactNode> = {
    plan: <FileTextOutlined />,
    confirm_each: <ExclamationCircleOutlined />,
    auto_edit: <CheckCircleOutlined />,
    goal: <ThunderboltOutlined />,
    full_access: <SafetyCertificateOutlined />,
  };
  // 权限档着色（[docs/composer-shift-tab-mode-cycle](../../../../docs/composer-shift-tab-mode-cycle.md) §5）：plan 不着色；
  // confirm = 绿 / auto = 黄 / goal = 橙 / full = 红。同一映射同时供给触发胶囊（按钮 className）与下拉项（rich 标题着色）
  const modeClass: Partial<Record<ApprovalMode, string>> = {
    confirm_each: "approval-confirm",
    auto_edit: "approval-auto",
    goal: "approval-goal",
    full_access: "approval-full",
  };
  // 菜单点击与 Shift+Tab 共用（[docs/composer-shift-tab-mode-cycle](../../../../docs/composer-shift-tab-mode-cycle.md)）。后端在每次 LLM 轮 / 工具调用时实时读取 prefs，
  // 因此流式进行中切档也会在下一个请求 / 下一次工具调用生效
  const switchMode = (mode: ApprovalMode) => {
    void updatePrefs(tab?.key ?? "", { approval_mode: mode });
    message.success(t("composer.modeSwitched", { label: modeLabels[mode] }));
  };
  const switchModel = (id: string) => {
    void updatePrefs(tab?.key ?? "", { model_id: id });
    const m = (config?.providers ?? []).flatMap((p) => p.models).find((x) => x.id === id);
    message.success(t("composer.modelSwitched", { label: m ? m.model : id }));
  };
  const approvalMenu: MenuProps = {
    selectedKeys: [prefs.approval_mode],
    items: (Object.entries(modeIcons) as [ApprovalMode, React.ReactNode][]).map(([mode, icon]) => ({
      key: mode,
      icon,
      label: rich(modeLabels[mode], t(modeDescKeys[mode]), modeClass[mode]),
    })),
    onClick: ({ key }) => switchMode(key as ApprovalMode),
  };

  // 模型菜单分组 = 供应商（config 顺序即菜单顺序，[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）；空名回退「其他」
  const modelMenu: MenuProps = {
    selectedKeys: effectiveModel ? [effectiveModel.id] : [],
    items: [
      ...(config?.providers ?? [])
        .filter((p) => p.models.length > 0)
        .map((p) => ({
          type: "group" as const,
          label: p.name || t("composer.other"),
          children: p.models.map((m) => ({
            key: m.id,
            label: (
              <>
                {m.model}
                {m.vision && <span className="vision-tag">{t("composer.vision")}</span>}
              </>
            ),
          })),
        })),
      { type: "divider" },
      { key: "__manage", icon: <SettingOutlined />, label: t("composer.manageProviders") },
    ],
    onClick: ({ key }) => {
      if (key === "__manage") {
        useUi.getState().showSettings("providers");
        return;
      }
      switchModel(key);
    },
  };

  const effortLabels: Record<EffortLevel, string> = {
    low: t("composer.effortLow"),
    medium: t("composer.effortMedium"),
    high: t("composer.effortHigh"),
    max: t("composer.effortMax"),
  };
  const effortMenu: MenuProps = {
    selectedKeys: [effortValue],
    items: [
      { key: "default", label: rich(t("composer.effortDefault"), t("composer.effortDefaultDesc")) },
      { type: "divider" },
      ...(["low", "medium", "high", "max"] as EffortLevel[]).map((e) => ({
        key: e,
        label: effortLabels[e],
      })),
    ],
    onClick: ({ key }) =>
      void updatePrefs(tab?.key ?? "", { reasoning_effort: key === "default" ? null : (key as EffortLevel) }),
  };

  const plusMenu: MenuProps = {
    items: [
      { key: "attach", icon: <FileAddOutlined />, label: t("composer.attach") },
      { key: "at", label: t("composer.useAt") },
      { key: "slash", label: t("composer.useSlash") },
      { key: "dollar", label: t("composer.useDollar") },
    ],
    onClick: ({ key }) => {
      if (key === "attach") {
        void pickFiles();
      } else if (key === "at") {
        insertTrigger("@");
      } else if (key === "slash") {
        insertTrigger("/");
      } else if (key === "dollar") {
        insertTrigger("$");
      }
    },
  };

  return (
    <div className="composer-wrap">
      {askActive ? (
        <AskPanel />
      ) : (
        <>
          <QueuePanel />

          {/* 目标模式提示条（`ApprovalMode::Goal`）：澄清阶段说明「这条消息就是目标」，
              执行阶段显示轮次 + 状态，暂停态给出「继续推进」。纯展示层，不动触发符/光标逻辑 */}
          {prefs.approval_mode === "goal" && (
            <GoalBanner goal={active.goal ?? null} sessionKey={tab?.key ?? ""} />
          )}

      {/* / 技能菜单（list_skills IPC，命令入口已移除）：点击/Enter 回填 /<name> 前缀，
          发送后由 <available-skills> 的点名语义引导模型加载技能（技能详情弹层在右栏技能行） */}
      <Popover
        open={slashOpen}
        placement="topLeft"
        arrow={false}
        content={
          <div className="menu" style={menuStyle}>
            {skillResults.map((s, i) => (
              <Button
                key={s.name}
                type="text"
                size="small"
                className={`menu-item${i === selIdx ? " active" : ""}`}
                style={{ justifyContent: "flex-start", display: "flex", width: "100%" }}
                onMouseEnter={() => setActiveIndex(i)}
                onClick={() => pickSkill(s)}
              >
                <code style={{ marginRight: 8, color: "var(--ws-accent)" }}>/{s.name}</code>
                <span className="dim">{s.description}</span>
              </Button>
            ))}
          </div>
        }
      >
        <span />
      </Popover>

      <Popover
        open={mentionOpen}
        placement="topLeft"
        arrow={false}
        content={
          <div className="menu" style={menuStyle}>
            {mentionResults.map((item, i) => (
              <Button
                key={item.path}
                type="text"
                size="small"
                className={`menu-item${i === selIdx ? " active" : ""}`}
                style={{ justifyContent: "flex-start", display: "flex", width: "100%" }}
                onMouseEnter={() => setActiveIndex(i)}
                onClick={() => pickMention(item)}
              >
                <code style={item.isDir ? { color: "var(--ws-accent)" } : undefined}>{item.label}</code>
              </Button>
            ))}
          </div>
        }
      >
        <span />
      </Popover>

      {/* $ 子代理菜单（list_agents IPC）：回填 $<role> 前缀，发送后由核心提示 $<role> 点名规则
          引导主代理经 subagent 工具委派；进度在子代理卡/抽屉展示 */}
      <Popover
        open={agentOpen}
        placement="topLeft"
        arrow={false}
        content={
          <div className="menu" style={menuStyle}>
            {agentResults.map((a, i) => (
              <Button
                key={a.name}
                type="text"
                size="small"
                className={`menu-item${i === selIdx ? " active" : ""}`}
                style={{ justifyContent: "flex-start", display: "flex", width: "100%" }}
                onMouseEnter={() => setActiveIndex(i)}
                onClick={() => pickAgent(a)}
              >
                <code style={{ marginRight: 8, color: "var(--ws-accent)" }}>${a.name}</code>
                <span className="dim">{a.description}</span>
              </Button>
            ))}
          </div>
        }
      >
        <span />
      </Popover>

      {/* 文件选择走原生对话框（ipc.select_document_files）：网页内的 <input type=file> 只给文件内容、
          拿不到真实路径，而本应用对文件是原地引用，必须有路径。 */}

      {/* 项目外目录放行确认：一次拖入多个外部文件时，后面的会排队等这一个决定 */}
      <ExternalDirPrompt dir={extDir} onDecide={decideExternalDir} />

      <div className="composer">
        {/* 多条流光边框（docs/antd6-upgrade-and-composer-border-beam，antd 6 BorderBeam）：条纹均匀分布；颜色跟随主题强调色，
            antd 内部已处理 prefers-reduced-motion 隐藏。
            时机收敛（docs/ask-ink-accent-and-composer-cover）：仅输入框聚焦 / 任务进行中显示（原 :focus-within 强调色边框取消，
            流光即聚焦反馈）；时长不变：空闲 12s 慢速巡航、任务运行 3s 提速 */}
        <BorderBeam
          count={3}
          duration={active.running ? 3 : 12}
          color="var(--ws-accent)"
          className={beamActive ? undefined : "composer-beam-idle"}
        >
          <div className="composer-card" ref={cardRef} data-narrow={composerWidth}>
          {(images.length > 0 || refs.length > 0) && (
            <div className="composer-attachments">
              {/* 缩略图点击打开大图预览（多图可切换），与会话内已发送图片同一交互；
                  .x 删除按钮在预览触发层之外，点击不会误开预览 */}
              <Image.PreviewGroup>
                {images.map((img) => (
                  <span key={img.id} className="attach-chip">
                    <Image
                      src={img.dataUrl}
                      alt={img.name}
                      width={28}
                      height={28}
                      preview={{ src: img.dataUrl }}
                      style={{ borderRadius: 4, flex: "none" }}
                    />
                    <span className="attach-name">{img.name}</span>
                    <CloseOutlined className="x" onClick={() => setImages((v) => v.filter((x) => x.id !== img.id))} />
                  </span>
                ))}
              </Image.PreviewGroup>
              {/* 文件引用 chip（[docs/composer-file-ref-chips](../../../../docs/composer-file-ref-chips.md)）：
                  与图片缩略图同处一个容器、同一套 .attach-chip 视觉；只显示文件名，悬停（title）看完整引用路径。
                  引用本体住在草稿 refs 里（不在正文），故删除 chip 只动 refs，不碰正文 */}
              {refs.map((ref) => (
                <span key={ref} className="attach-chip ref-chip" title={ref} data-ref={ref}>
                  {refIcon(ref)}
                  <span className="attach-name">{baseName(ref)}</span>
                  <CloseOutlined
                    className="x"
                    role="button"
                    aria-label={t("composer.removeRef")}
                    onClick={() => removeRef(ref)}
                  />
                </span>
              ))}
            </div>
          )}

          <TextArea
            className="composer-input"
            variant="borderless"
            value={text}
            autoSize={{ minRows: 2, maxRows: 8 }}
            placeholder={
              active.running
                ? t("composer.queuePlaceholder")
                : // 目标模式澄清阶段：提示用户这条消息就是目标
                  prefs.approval_mode === "goal" && (active.goal?.status ?? GOAL_STATUS_DEFAULT) === "clarify"
                  ? t("composer.goalPlaceholder")
                  : t("app.inputPlaceholder")
            }
            spellCheck={false}
            onFocus={() => setComposerFocused(true)}
            onBlur={() => setComposerFocused(false)}
            onChange={(e) => void onInputChange(e.target.value, e.target.selectionStart ?? undefined)}
            onKeyDown={onKeydown}
            // 光标是触发判定与回填的锚点：点击/方向键移动它不会经过 onChange，这里补同步
            onSelect={syncCaret}
            onClick={syncCaret}
            onKeyUp={syncCaret}
            onCompositionStart={() => {
              composingRef.current = true;
            }}
            onCompositionEnd={() => {
              composingRef.current = false;
            }}
            onPaste={onPaste}
            ref={taRef}
          />

          <div className="composer-toolbar">
            <div className="toolbar-left">
              <Dropdown menu={plusMenu} trigger={["click"]}>
                <Button type="text" icon={<PlusOutlined />} title={t("composer.attach")} />
              </Dropdown>
              <Dropdown menu={approvalMenu} trigger={["click"]}>
                <Button
                  type="text"
                  className={modeClass[prefs.approval_mode]}
                  title={t("composer.modeShortcutHint")}
                >
                  {modeIcons[prefs.approval_mode]}
                  <span className="tb-label tb-mode-text">{modeLabels[prefs.approval_mode]}</span>
                  <DownOutlined className="tb-chev" />
                </Button>
              </Dropdown>
              {/* docs/subagent-interaction-drawer：运行中子代理指示器（计数 = 运行中子代理数，0 隐藏）。
                  单个时点击直达过程抽屉；多个时弹向上菜单、选中后打开。 */}
              {runningSubs.length === 1 && (
                <Button
                  type="text"
                  className="subs-indicator"
                  title={t("subagent.runningCount", { n: 1 })}
                  icon={<RobotOutlined />}
                  onClick={() => void useRun.getState().openSubDrawer(null, runningSubs[0].subId)}
                >
                  <span className="tb-label">{runningSubs.length}</span>
                </Button>
              )}
              {runningSubs.length > 1 && (
                <Dropdown
                  trigger={["click"]}
                  placement="topLeft"
                  menu={{
                    items: runningSubs.map((s) => ({
                      key: s.subId,
                      label: (
                        <span className="subs-menu-item">
                          <RobotOutlined />
                          <span className="subs-menu-role">{s.name || s.role}</span>
                          {s.description && <span className="subs-menu-desc">· {s.description}</span>}
                          <span className={`subs-menu-st st-${s.status}`}>
                            {s.status === "running" ? "●" : s.status === "error" ? "✕" : "✓"}
                          </span>
                        </span>
                      ),
                    })),
                    onClick: ({ key }) => void useRun.getState().openSubDrawer(null, key),
                  }}
                >
                  <Button
                    type="text"
                    className="subs-indicator"
                    title={t("subagent.runningCount", { n: runningSubs.length })}
                    icon={<RobotOutlined />}
                  >
                    <span className="tb-label">{runningSubs.length}</span>
                    <DownOutlined className="tb-chev" />
                  </Button>
                </Dropdown>
              )}
            </div>
            {/* 信息段独立成块（.toolbar-info）：上下文 / 命中 / 速率原先住在 .toolbar-right 内，
                被 margin-left:auto 推到最右并与模型/力度/发送挤在一起，用户反馈看不到（版式审计）。
                本轮把上下文/命中挪到进度圈 hover 的 Popover 里，.toolbar-info 只剩速率段；
                进度圈 + 压缩按钮挪到 .toolbar-right 原压缩按钮处。 */}
            <div className="toolbar-info">
              {rate != null && (
                // 「在跑」点仅在运行中渲染，运行结束后消失而数值保留（AC-7）
                <span className="ctx-rate" title={rateTitle}>
                  {`${formatRate(rate)} tok/s`}
                  {active.running && <span className="rate-dot" title={t("composer.rateRunning")} />}
                </span>
              )}
            </div>
            <div className="toolbar-right">
              {/* 进度圈 = 上下文占用可视化入口（替换原 CompactButton 位），hover 弹 Popover 显示
                  上下文 / 阈值 / 命中 / 压缩操作。圈心 % 数字，环 stroke 按 4 档（红橙黄绿）切色。 */}
              <Popover
                placement="top"
                trigger="hover"
                content={
                  <div className="ctx-popover">
                    {active.breakdown ? (
                      <>
                        <div className="ctx-popover-row">
                          <span className="ctx-popover-label">{t("composer.ctxCurrent")}</span>
                          <span className="ctx-popover-value">
                            {`${Math.round(active.breakdown.total_tokens / 100) / 10}k / ${Math.round(active.breakdown.context_window / 100) / 10}k (${contextPct}%)`}
                          </span>
                        </div>
                        {cacheHit != null && (
                          <div className="ctx-popover-row">
                            <span className="ctx-popover-label">{t("composer.cacheHit")}</span>
                            <span className="ctx-popover-value">{hitPct}</span>
                            <span className="ctx-popover-meta">{`（${active.usage?.cacheRead ?? 0} / ${hitDenom}）`}</span>
                          </div>
                        )}
                        {thresholdValid && (
                          <div className="ctx-popover-row ctx-popover-threshold-row">
                            <span className="ctx-popover-label">{`${t("settings.compactThreshold")} (${thresholdPct}%)`}</span>
                            {/* 压缩按钮内联到阈值行尾部（不占独立行），省一行垂直空间 */}
                            <CompactButton className="ctx-popover-compact-btn" />
                          </div>
                        )}
                      </>
                    ) : (
                      <div className="ctx-popover-empty">
                        {t("app.context")} —
                        <CompactButton className="ctx-popover-compact-btn" />
                      </div>
                    )}
                  </div>
                }
              >
                <span className="ctx-progress-wrap">
                  <Progress
                    type="dashboard"
                    percent={contextPct}
                    size={20}
                    strokeColor={ctxProgressColor}
                    showInfo={false}
                    className={`ctx-progress ctx-tier-${ctxTier}`}
                  />
                </span>
              </Popover>
              <Dropdown menu={modelMenu} trigger={["click"]}>
                <Button
                  type="text"
                  className="tb-model-select"
                  aria-label={t("app.model")}
                  title={effectiveModel ? `${effectiveModel.providerName} / ${effectiveModel.model}` : t("composer.goSettings")}
                >
                  <span className="tb-label">
                    {effectiveModel
                      ? (effectiveModel.providerName ? (
                          <>
                            <span className="tb-model-provider">{effectiveModel.providerName} / </span>
                            <span className="tb-model-name">{effectiveModel.model}</span>
                          </>
                        ) : (
                          <span className="tb-model-name">{effectiveModel.model}</span>
                        ))
                      : <span className="tb-model-name">{t("composer.noModel")}</span>}
                  </span>
                  <DownOutlined className="tb-chev" />
                </Button>
              </Dropdown>
              <Dropdown menu={effortMenu} trigger={["click"]}>
                <Button type="text" className="tb-effort-select" title={t("settings.reasoning")}>
                  <BulbOutlined />
                  <span className="tb-label tb-effort-text">
                    {effortValue === "default" ? t("composer.effortDefault") : effortLabels[effortValue]}
                  </span>
                  <DownOutlined className="tb-chev" />
                </Button>
              </Dropdown>
              <Button
                className="send-btn"
                shape="circle"
                danger={stopActive}
                type="primary"
                disabled={!active.running && !hasDraft}
                aria-label={stopActive ? t("app.stop") : active.running ? t("composer.submit") : t("app.send")}
                title={stopActive ? t("app.stop") : active.running ? t("composer.submit") : t("app.send")}
                onClick={() => void (stopActive ? useRun.getState().cancel() : send())}
                icon={stopActive ? <StopOutlined /> : <ArrowUpOutlined />}
              />
            </div>
          </div>
          </div>
        </BorderBeam>
      </div>

      {/* 拖入提示条：只在拖拽悬停时出现，落下即消失（实际处理在 addPaths） */}
      {dropping && <div className="composer-drop-hint">{t("composer.dropHere")}</div>}
        </>
      )}
    </div>
  );
}

/** 引用 chip 的文件类型图标：按扩展名分派（Office 三兄弟与 PDF 用专用图标，常见文本/源码用文本图标，
 *  其余一律通用文件图标）。只影响观感，不参与任何判定。 */
function refIcon(ref: string) {
  const ext = ref.split(".").pop()?.toLowerCase() ?? "";
  if (["xlsx", "xlsm", "xls", "csv"].includes(ext)) return <FileExcelOutlined />;
  if (["docx", "doc"].includes(ext)) return <FileWordOutlined />;
  if (ext === "pdf") return <FilePdfOutlined />;
  if ([
    "md", "txt", "json", "jsonc", "log", "yml", "yaml", "toml", "ini",
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "rb", "php",
    "c", "h", "cpp", "hpp", "cs", "swift", "sh", "ps1", "bat", "sql", "html", "css", "scss", "vue",
  ].includes(ext)) return <FileTextOutlined />;
  return <FileOutlined />;
}

/** 目标模式提示条（`ApprovalMode::Goal`）：澄清阶段说明「这条消息就是目标」，
 *  执行阶段显示轮次 + 状态（执行中/已暂停/已达成/已中止），暂停态给出「继续推进」。
 *  纯展示层：不碰触发符与光标逻辑（composerTriggers.ts / caretRef 一概不动）。 */
function GoalBanner({ goal, sessionKey }: { goal: GoalState | null; sessionKey: string }) {
  const { t } = useTranslation();
  // 无目标 = 待澄清（与后端「切档后下一条消息即目标」的口径一致）
  const status: GoalStatus = goal?.status ?? GOAL_STATUS_DEFAULT;
  return (
    <div className={`goal-banner st-${status}`} data-goal-status={status}>
      <span className="goal-banner-text">
        {status === "clarify"
          ? t("composer.goalBannerClarify")
          : `${t("composer.goalRunning", { n: goal?.rounds ?? 0 })} · ${t(GOAL_STATUS_KEYS[status])}`}
      </span>
      {status === "paused" && (
        <>
          <Button type="text" size="small" className="goal-resume-btn" onClick={() => void useRun.getState().resumeGoal(sessionKey)}>
            {t("composer.goalResume")}
          </Button>
          <Button type="text" size="small" title={t("composer.goalReviseHint")} onClick={() => void useRun.getState().reopenGoal(sessionKey)}>
            {t("composer.goalRevise")}
          </Button>
        </>
      )}
    </div>
  );
}
