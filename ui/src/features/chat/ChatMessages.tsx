import { fontSizeCss } from "../../utils/fonts";
import { Fragment, memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Alert, Button, Collapse, Divider, Image, Tooltip, Typography } from "antd";
import {
  CheckOutlined,
  CloseOutlined,
  CopyOutlined,
  EditOutlined,
  MessageOutlined,
  FolderAddOutlined,
  DownOutlined,
  LoadingOutlined,
  ThunderboltOutlined,
  ArrowRightOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { MAX_PAGED_PAGES, useActiveRun, useRun } from "../../stores/run";
import { useSessions } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import { upgradeDiagrams } from "../../utils/diagrams";
import { copyText } from "../../utils/clipboard";
import { renderUserMarkdown } from "../../utils/markdown";
import type { UiItem } from "../../stores/run";
// 滚动锚点（会话保存与恢复优化 · 批1）：锚点读写与现场态落盘的调用链见本文件「滚动锚点」一节。
import {
  bottomScrollTarget,
  boundaryAtIndexes,
  captureAnchor,
  collectNodes,
  isAtBottom,
  isSelfScroll,
  itemKeysOf,
  itemSig,
  restoreAnchor,
} from "../../utils/scrollAnchor";
import type { ScrollAnchor } from "../../utils/scrollAnchor";
import { getScrollAnchor, scheduleAnchor, setScrollAnchor } from "../../utils/uiState";
import SubagentItemCard from "../subagent/SubagentItemCard";
import { TimelineSegsView } from "./segments";
import ChatScrollbar from "./ChatScrollbar";
import { captureOutputAnchor, restoreOutputAnchor, type OutputAnchor } from "../../utils/outputAnchor";
import { compactTranscript } from "./compactTranscript";
import type { TranscriptEntry } from "./compactTranscript";
import { taskDuration, taskTranscript } from "./taskTranscript";
import type { ToolOrigin } from "../tools/toolPresentation";
import type { ToolExpansion } from "../tools/ToolCallCard";

type ExpansionState = Record<string, Record<string, ToolExpansion>>;
type GroupExpansionState = Record<string, Record<string, boolean>>;
const expansionScope = (sessionKey: string | null, messageKey: string) => JSON.stringify([sessionKey, messageKey]);

// 最后一条 item 的 kind（effect 依赖只挂 kind，避免依赖整个数组）
function lastItem_kind(items: UiItem[]): string {
  return items.length ? items[items.length - 1].kind : "";
}

/** 消息行锚点标注：scrollAnchor.ts 的 collectNodes 按 data-key（稳定键，主）+ data-sig（指纹兜底）收集可锚定节点。
 *  仅作定位参考（不加样式、不改结构语义）；传字面量属性而非对象，避免 memo 因对象身份失效。 */
interface AnchorAttrs {
  anchorKey: string;
  anchorSig: string;
}

/** 用户消息：文本气泡 + 图片缩略（可预览）；悬停操作提供复制与「修改」（经 ws:composer-fill 回填 Composer，不自动发送）。 */
const UserMessage = memo(function UserMessage({
  text,
  images,
  anchorKey,
  anchorSig,
}: {
  text: string;
  images?: { mediaType: string; data: string }[];
} & AnchorAttrs) {
  const { t } = useTranslation();
  // 复制反馈三态：idle 未点 / done 成功 / fail 失败——失败必须有区别于成功的可见反馈，
  // 否则又会退回「点了没反应」（旧代码 .catch(() => {}) 吞错即此病灶）。
  const [copyState, setCopyState] = useState<"idle" | "done" | "fail">("idle");
  // 统一走 utils/clipboard.ts（原生插件主路径，见该文件头注释）
  const doCopy = () => {
    if (!text) return;
    void copyText(text).then((ok) => {
      setCopyState(ok ? "done" : "fail");
      setTimeout(() => setCopyState("idle"), 1200);
    });
  };
  // 「修改」：内容经 ws:composer-fill 回填 Composer（覆盖当前草稿，与队列「编辑」同语义）；Composer 自行聚焦，不直接发送。
  // 附件一并携带：带图消息点修改图片回显，无图消息覆盖清空现有附件
  const onEdit = () => {
    if (!text) return;
    window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text, images } }));
  };
  return (
    <div className="msg user" data-key={anchorKey} data-sig={anchorSig}>
      {text && <div className="bubble user-bubble md" dangerouslySetInnerHTML={{ __html: renderUserMarkdown(text) }} />}
      {text && (
        <div className="user-msg-actions">
          <Tooltip title={t("chat.copy")}>
            <Button
              type="text"
              size="small"
              aria-label={t("chat.copy")}
              icon={copyState === "done" ? <CheckOutlined /> : copyState === "fail" ? <CloseOutlined /> : <CopyOutlined />}
              onClick={doCopy}
            />
          </Tooltip>
          <Tooltip title={t("chat.editInComposer")}>
            <Button type="text" size="small" aria-label={t("chat.editInComposer")} icon={<EditOutlined />} onClick={onEdit} />
          </Tooltip>
        </div>
      )}
      {images && images.length > 0 && (
        <div className="user-attachments">
          <Image.PreviewGroup>
            {images.map((im, i) => (
              <Image
                key={i}
                className="attach-thumb"
                src={`data:${im.mediaType};base64,${im.data}`}
                alt={t("chat.attachment")}
              />
            ))}
          </Image.PreviewGroup>
        </div>
      )}
    </div>
  );
});

const FinalAnswerCopy = memo(function FinalAnswerCopy({ text }: { text: string }) {
  const { t } = useTranslation();
  const [copyState, setCopyState] = useState<"idle" | "done" | "fail">("idle");
  const attempt = useRef(0);
  const resetTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => {
    attempt.current++;
    clearTimeout(resetTimer.current);
  }, []);
  const doCopy = async () => {
    const current = ++attempt.current;
    clearTimeout(resetTimer.current);
    setCopyState("idle");
    const ok = await copyText(text).catch(() => false);
    if (current !== attempt.current) return;
    setCopyState(ok ? "done" : "fail");
    resetTimer.current = setTimeout(() => setCopyState("idle"), 1200);
  };
  const feedback = copyState === "done" ? t("chat.copied") : copyState === "fail" ? t("chat.copyAnswerFailed") : undefined;
  return (
    <div className="final-answer-actions" style={{ marginTop: 8 }}>
      <Tooltip title={feedback ?? t("chat.copyAnswer")}>
        <Button type="text" size="small" aria-label={t("chat.copyAnswer")}
          style={{ color: "var(--ws-text-2)" }}
          icon={copyState === "done" ? <CheckOutlined /> : copyState === "fail" ? <CloseOutlined /> : <CopyOutlined />}
          onClick={() => { void doCopy(); }}
        >{feedback}</Button>
      </Tooltip>
    </div>
  );
});

const AssistantMessage = memo(function AssistantMessage({
  item,
  streaming,
  hasPendingProcess,
  waitingForApproval,
  onUserToggle,
  anchorKey,
  anchorSig,
  autoOpenCallKey,
  onConsumeAutoOpen,
  compact,
  origins,
  expansionState,
  groupExpansionState,
  sessionKey,
  onToolExpansionChange,
  onGroupExpansionChange,
  finalRevealKey,
  onFinalRevealProgress,
  onFinalRevealComplete,
  finalAnswer,
}: {
  item: Extract<UiItem, { kind: "assistant" }>;
  streaming: boolean;
  hasPendingProcess: boolean;
  waitingForApproval: boolean;
  onUserToggle?: () => void;
  /** 自动弹预览信号（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：透传到工具卡，命中即弹框 */
  autoOpenCallKey?: string | null;
  /** 卡片已消费该信号后回抛（清 store，保证一次性、不重放） */
  onConsumeAutoOpen?: (callKey: string) => void;
  compact: boolean;
  origins?: Record<string, ToolOrigin>;
  expansionState: ExpansionState;
  groupExpansionState: GroupExpansionState;
  sessionKey: string | null;
  onToolExpansionChange: (messageKey: string, callKey: string, expansion: ToolExpansion) => void;
  onGroupExpansionChange: (messageKey: string, callKey: string, expanded: boolean) => void;
  finalRevealKey?: string;
  onFinalRevealProgress?: (finished: boolean, root: HTMLElement | null) => void;
  onFinalRevealComplete?: () => void;
  finalAnswer?: string;
} & AnchorAttrs) {
  const { t } = useTranslation();
  const toolExpansions = useMemo(() => {
    const out: Record<string, ToolExpansion> = {};
    for (const callKey of Object.keys(item.toolsMap)) {
      const owner = origins?.[callKey]?.key ?? anchorKey;
      const value = expansionState[expansionScope(sessionKey, owner)]?.[callKey];
      if (value) out[callKey] = value;
    }
    return out;
  }, [item.toolsMap, origins, anchorKey, expansionState, sessionKey]);
  const updateToolExpansion = useCallback((callKey: string, expansion: ToolExpansion) => {
    onToolExpansionChange(origins?.[callKey]?.key ?? anchorKey, callKey, expansion);
  }, [origins, anchorKey, onToolExpansionChange]);
  const groupExpansions = useMemo(() => {
    const out: Record<string, boolean> = {};
    // 键空间必须覆盖组根的全部可能取值：工具 callKey 与 `thinking:<段下标>`
    // （纯思考组没有工具，只按 toolsMap 遍历会读不到它的展开态）。
    // 读侧按需查表，这里直接登记两类键。
    for (const callKey of Object.keys(item.toolsMap)) {
      const owner = origins?.[callKey]?.key ?? anchorKey;
      out[callKey] = !!groupExpansionState[expansionScope(sessionKey, owner)]?.[callKey];
    }
    for (const [segKey, owner] of Object.entries(origins ?? {})) {
      if (!segKey.startsWith("thinking:")) continue;
      out[segKey] = !!groupExpansionState[expansionScope(sessionKey, owner.key)]?.[segKey];
    }
    return out;
  }, [item.toolsMap, origins, anchorKey, groupExpansionState, sessionKey]);
  const updateGroupExpansion = useCallback((callKey: string, expanded: boolean) => {
    onGroupExpansionChange(origins?.[callKey]?.key ?? anchorKey, callKey, expanded);
  }, [origins, anchorKey, onGroupExpansionChange]);
  let tailIndex = item.timeline.length - 1;
  while (tailIndex >= 0) {
    const segment = item.timeline[tailIndex];
    if (segment.kind === "text" && (segment.deferred || !segment.text.trim())) { tailIndex--; continue; }
    if (segment.kind === "tool") {
      const tool = item.toolsMap[segment.callKey];
      // Match TimelineSegsView's missing-tool and successful-subagent filtering.
      if (!tool || (tool.tool === "subagent" && tool.status !== "error")) { tailIndex--; continue; }
    }
    break;
  }
  const tail = item.timeline[tailIndex];
  const hasDeferredText = item.timeline.some(seg => seg.kind === "text" && seg.deferred);
  const processTail = compact && (tail?.kind === "tool" || tail?.kind === "thinking");
  const processOnly = processTail
    && item.timeline.every(seg => seg.kind === "tool" || seg.kind === "thinking" || (seg.kind === "text" && (seg.deferred || !seg.text.trim())));
  return (
    <div className={`msg assistant${processOnly ? " process-only" : ""}${processTail ? " process-tail" : ""}`} data-key={anchorKey} data-sig={anchorSig}>
      {/* 按 timeline 顺序穿插渲染各段（与子代理过程抽屉共用同一段渲染；docs/subagents 抽取至 segments.tsx） */}
      <TimelineSegsView
        timeline={item.timeline}
        toolsMap={item.toolsMap}
        streaming={streaming}
        onUserToggle={onUserToggle}
        autoOpenCallKey={autoOpenCallKey}
        onConsumeAutoOpen={onConsumeAutoOpen}
        toolOrigins={origins}
        toolExpansions={toolExpansions}
        onToolExpansionChange={updateToolExpansion}
        groupExpansions={groupExpansions}
        onGroupExpansionChange={updateGroupExpansion}
        finalRevealKey={finalRevealKey}
        onFinalRevealProgress={onFinalRevealProgress}
        onFinalRevealComplete={onFinalRevealComplete}
      />
      {finalAnswer && <FinalAnswerCopy text={finalAnswer} />}
      {streaming && compact && (
        <Typography.Text type="secondary" className="ws-streaming-status" role="status">
          {waitingForApproval ? t("chat.activityWaiting") : hasPendingProcess ? t("chat.activityProcessing")
            : hasDeferredText || tail?.kind === "text" ? t("chat.activityResponding") : t("chat.activityThinking")}
        </Typography.Text>
      )}
      {/* Detailed display keeps a fallback indicator when concrete process state is absent. */}
      {streaming && !compact && hasDeferredText && !hasPendingProcess && <Typography.Text type="secondary" role="status">{t("chat.activityResponding")}</Typography.Text>}
      {streaming && !compact && !hasDeferredText && !hasPendingProcess && !(tail?.kind === "thinking" && tail.startedAt !== undefined && tail.durationMs === undefined) && (
        <span className="ws-streaming-indicator">
          <LoadingOutlined spin />
        </span>
      )}
    </div>
  );
});

/** 上下文压缩边界分隔线（批2 P2）：插在「段号 ≥ 边界 `seq` 的第一条消息」之前，语义是
 *  「此前的上下文已被压缩——模型不再看到更早内容，但完整历史仍可向前翻页查看」。
 *
 *  轻量提示用 antd `Divider`（项目约定：组件样式一律 antd 接管，不手搓皮肤），悬停给出完整解释。
 *  `source`（compact / shrink）两者文案相同，只落到 `data-boundary-source` 上供排障；`at` 留在 store 里。
 *  根元素无 `data-key`：滚动锚点收集（collectNodes）天然跳过它，不参与锚点定位。 */
const CompactionDivider = memo(function CompactionDivider({ seq, source }: { seq: number; source?: string }) {
  const { t } = useTranslation();
  return (
    <Divider plain data-boundary-seq={seq} data-boundary-source={source ?? "compact"} style={{ margin: "10px 0" }}>
      <Tooltip title={t("chat.compactedBoundaryHint")}>
        <span style={{ fontSize: fontSizeCss(12.5) }}>{t("chat.compactedBoundary")}</span>
      </Tooltip>
    </Divider>
  );
});

/** 瞬时跳底的豁免窗口：只需覆盖本次 scroll 事件的派发（事件在下一帧才到） */
const PROG_SCROLL_MS = 150;
/** 平滑跳底的豁免窗口：要盖住整段动画。浏览器平滑滚动时长随距离增长，这里取经验上限，
 *  由 [docs/chat-and-interface](../../../../docs/chat-and-interface.md) 的手动清单第 3 项校准 */
const SMOOTH_SCROLL_MS = 700;

/** 聊天消息区：单实例随 activeKey 换数据不重挂；维护「贴底跟随 / 用户接管」滚动模型——
 *  贴底时内容增长自动下滚；滚轮上滚或展开卡片即阅读意图暂停跟随；恢复仅限滚回底部或
 *  「回到底部」按钮。程序化滚动用豁免窗口 + **落点比对**（bottomScrollTarget / isSelfScroll）：
 *  瞬时跳底只豁免落点，平滑动画才用 from→target 区间，防止自家跳转被误判为用户滚动
 *  （[docs/chat-and-interface](../../../../docs/chat-and-interface.md)、[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。 */
export default function ChatMessages() {
  const { t, i18n } = useTranslation();
  const active = useActiveRun();
  const outputDisplay = useUi((s) => s.outputDisplay);
  const [toolExpansions, setToolExpansions] = useState<ExpansionState>({});
  const [groupExpansions, setGroupExpansions] = useState<GroupExpansionState>({});
  const [taskExpansions, setTaskExpansions] = useState<Record<string, boolean>>({});
  const taskAnchor = useRef<{ sessionId: string | null; anchor: OutputAnchor } | null>(null);
  const displayAnchor = useRef<{ sessionId: string | null; anchor: OutputAnchor } | null>(null);
  const hasSession = useSessions((s) => !!s.activeKey);
  const scroller = useRef<HTMLDivElement>(null);
  // 2.4：是否贴底（贴底 -> 自动下滚；未贴底 -> 显示「回到底部」按钮）
  const [atBottom, setAtBottom] = useState(true);
  const stickBottom = useRef(true);
  // 滚动条显隐交给 ChatScrollbar 子组件（自定义 overlay，scroll 时显、停 1s 后隐）——见 ChatScrollbar.tsx。
  // 这里不再持有 isScrolling/scrollIdleTimer。
  const progScroll = useRef(0); // 程序化滚动的豁免窗口：窗口内自家触发的滚动事件不参与贴底判定
  const progFrom = useRef(0); // 发起程序化滚动时的 scrollTop（豁免区间的下端）
  const progTarget = useRef(Infinity); // 程序化滚动的落点 scrollTop（豁免区间的上端，docs/chat-and-interface）
  // ---------- 滚动锚点：记录 → 落盘 → 还原 ----------
  // 记录：onScroll → recordAnchor → uiState.scheduleAnchor（200ms 节流）→ captureAnchor(容器) → setScrollAnchor → 落盘防抖
  // 还原：activeKey 变化 → getScrollAnchor(会话) → restoreAnchor(容器, 锚点)（无锚/贴底 ⇒ scrollTop = scrollHeight；
  //       有消息锚但 sig 已不存在 ⇒ 返回 false 并降级贴底）；懒加载首帧 items 为空 ⇒ 不落位，
  //       等 items 0→N 由下面的「二次校正」effect 在下一帧重定位。
  // pendingAnchor = 待还原的消息锚（bottom 无需还原）；anchorSid = 锚点归属会话（切 Tab 中途异步回包据此作废）；
  // anchorHit = 本轮激活是否命中过（命中过就不做降级）
  const pendingAnchor = useRef<ScrollAnchor | null>(null);
  const anchorSid = useRef<string | null>(null);
  const anchorHit = useRef(false);

  /** 登记程序化滚动的豁免窗口：窗口内落在 [from, target] 区间（±BOTTOM_EPS）的滚动事件算自家事件。
   *  target 必须是**浏览器实际落点**（bottomScrollTarget，会被钳到 scrollHeight - clientHeight），
   *  绝不是 scrollHeight —— 拿未钳值比对时差值恒等于 clientHeight（几百像素），豁免永不成立。
   *  from 默认等于 target（瞬时跳底：落点即豁免点）；只有平滑滚动才显式传「发起位置」，
   *  因为动画期间位置要在 from→target 之间逐帧移动。
   *  useCallback（而非普通函数）：它被三个 effect 直接引用，稳定引用才能安心进依赖数组。 */
  const markProgScroll = useCallback(
    (el: HTMLElement, target: number, ms = PROG_SCROLL_MS, from = target) => {
      progFrom.current = from;
      progTarget.current = target;
      progScroll.current = Date.now() + ms;
    },
    [],
  );

  /** 程序化跳底：全部「我们自己滚到底」的唯一入口（激活贴底 / 内容增长跟随 / 「回到底部」按钮）。
   *  一并登记豁免窗口与跟随态 —— 少了豁免，这次跳底产生的 scroll 事件会被 onScroll 按几何判定：
   *  流式下内容往往在事件派发前又长了一截，当场判成「用户滚走了」，跟随被关掉且不再自愈
   *  （[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。
   *  el 允许为 null（容器尚未挂载时仍要维持跟随态）。 */
  const jumpToBottom = useCallback(
    (el: HTMLElement | null, smooth = false) => {
      if (el) {
        const target = bottomScrollTarget(el);
        // 瞬时跳底：落点即豁免点（不传 from）。给区间会随「离底多远」变宽 ——「发送后强制跳底」
        // 与「切 Tab 贴底」的区间能盖住整段阅读位置，窗口内用户拖滚动条 / PageUp 的真实上滚会被吞。
        // 平滑动画才需要区间：动画期间位置在 from→target 之间逐帧移动，且窗口要盖住整段动画。
        if (smooth) markProgScroll(el, target, SMOOTH_SCROLL_MS, el.scrollTop);
        else markProgScroll(el, target);
        el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
      }
      stickBottom.current = true;
      setAtBottom(true);
    },
    [markProgScroll],
  );
  const onFinalRevealProgress = useCallback((finished: boolean, root: HTMLElement | null) => {
    const el = scroller.current;
    const sessionId = useSessions.getState().activeKey;
    if (stickBottom.current) jumpToBottom(el);
    if (finished) void upgradeDiagrams(root).catch(() => {}).then(() => {
      if (stickBottom.current && scroller.current === el && useSessions.getState().activeKey === sessionId) jumpToBottom(el);
    });
  }, [jumpToBottom]);

  /** 按真实几何同步「贴底/跟随」态：阈值沿用 scrollAnchor.isAtBottom（BOTTOM_EPS=40，与 onScroll 的 <40 同源，
   *  不另发明一套判定） */
  const syncFollowState = (el: HTMLElement) => {
    const at = isAtBottom({ scrollTop: el.scrollTop, scrollHeight: el.scrollHeight, clientHeight: el.clientHeight });
    stickBottom.current = at;
    setAtBottom(at);
  };

  /** 滚动 → 记录锚点：节流交给 uiState.scheduleAnchor（同一窗口内只读一次布局，滚动这种高频回调不反复量 DOM）。
   *  reader 里再校一次会话：防抖窗口内切了 Tab 时容器里已是新会话的消息，读到的几何绝不能写给旧会话 ——
   *  此时按「贴底」记录（与旧版切 Tab 一律回底部一致，不会把旧会话锚到别人的位置上）。 */
  const recordAnchor = (key: string | null) => {
    if (!key) return; // 无活跃会话（空态）不记：没有归属的锚点无处可还原
    scheduleAnchor(key, () => {
      const el = scroller.current;
      if (!el || useSessions.getState().activeKey !== key) return { kind: "bottom" };
      return captureAnchor(el);
    });
  };
  // 展开/收起思考块或工具卡 = 阅读意图：立即暂停自动跟随，
  // 否则流式期间 stickBottom 恒为 true 会持续把视图拽到底部、把展开内容顶出视口。
  // 恢复跟随只有两条路：手动滚回底部（onScroll nearBottom）或「回到底部」按钮。
  const suspendFollow = useCallback(() => {
    stickBottom.current = false;
    setAtBottom(false);
  }, []);

  // 2.9：发送消息后强制滚到底部（items 增长且最后一条是新的用户消息）
  const activeKey = useSessions((s) => s.activeKey);
  useEffect(() => useRun.subscribe((state, previous) => {
    const sessionId = useSessions.getState().activeKey;
    if (!sessionId || (state.tabs[sessionId]?.taskRuns === previous.tabs[sessionId]?.taskRuns
      && state.tabs[sessionId]?.pendingFinals === previous.tabs[sessionId]?.pendingFinals
      && state.tabs[sessionId]?.finalReveals === previous.tabs[sessionId]?.finalReveals) || !scroller.current) return;
    taskAnchor.current = { sessionId, anchor: captureOutputAnchor(scroller.current) };
  }), []);
  useLayoutEffect(() => {
    const saved = taskAnchor.current;
    taskAnchor.current = null;
    const el = scroller.current;
    if (saved && el && saved.sessionId === activeKey && !saved.anchor.atBottom) {
      // A hidden process member now maps to the summary header; its old deep offset is no longer meaningful.
      if (saved.anchor.fallback.kind === "item") {
        const key = saved.anchor.fallback.key;
        const hidden = Array.from(el.querySelectorAll<HTMLElement>(".task-process [data-anchor-aliases]"))
          .some(node => (JSON.parse(node.dataset.anchorAliases ?? "[]") as { key: string }[]).some(alias => alias.key === key));
        if (hidden) saved.anchor = { ...saved.anchor, segmentKey: null, fallback: { ...saved.anchor.fallback, offset: 0 } };
      }
      restoreOutputAnchor(el, saved.anchor);
      markProgScroll(el, el.scrollTop);
    }
  }, [active.taskRuns, active.pendingFinals, active.finalReveals, taskExpansions, activeKey, markProgScroll]);
  // store 通知在 DOM 更新前捕获锚点，设置页与聊天入口共用此保护。
  useEffect(() => useUi.subscribe((state, previous) => {
    if (state.outputDisplay === previous.outputDisplay || !scroller.current) return;
    displayAnchor.current = { sessionId: useSessions.getState().activeKey, anchor: captureOutputAnchor(scroller.current) };
    suspendFollow();
  }), [suspendFollow]);
  useLayoutEffect(() => {
    const anchor = displayAnchor.current;
    displayAnchor.current = null;
    const el = scroller.current;
    if (!anchor || !el || anchor.sessionId !== activeKey) return;
    restoreOutputAnchor(el, anchor.anchor);
    markProgScroll(el, el.scrollTop);
    if (anchor.anchor.atBottom) jumpToBottom(el);
  }, [outputDisplay, activeKey, jumpToBottom, markProgScroll]);
  // ---------- 自动弹预览（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)） ----------
  // 批准门选「先看预览」→ AskPanel 置位一次性信号；run store 在本 run 第一张合格 widget 落地时
  // 把它转成目标 callKey，这里只负责透传给工具卡（卡片自己判有没有 html、弹不改它的本地 state）。
  const autoOpenCallKey = active.widgetAutoOpen?.callKey ?? null;
  // 消费 = 把 store 里的信号清掉。用订阅来的 activeKey（而非 getState 再取）：它是依赖数组里的真依赖，
  // 切 Tab 时回调随会话一起换新，避免拿旧会话的信号去消费；同时稳定引用——它是透传进 memo 组件的回调，
  // 每次渲染新建会让 memo 全量失效（流式期间 items 频繁变，代价被放大）。
  const consumeAutoOpen = useCallback(
    (callKey: string) => {
      if (!activeKey) return;
      useRun.getState().consumeWidgetAutoOpen(activeKey, callKey);
    },
    [activeKey],
  );
  const lastLen = active.items.length;
  const lastKind = lastItem_kind(active.items);
  const prevRef = useRef({ len: 0, kind: "" });
  useEffect(() => {
    const grew = lastLen > prevRef.current.len;
    const userJustSent = lastKind === "user" && prevRef.current.kind !== "user";
    prevRef.current = { len: lastLen, kind: lastKind };
    if (grew && userJustSent) {
      stickBottom.current = true;
      setAtBottom(true);
      requestAnimationFrame(() => jumpToBottom(scroller.current));
    }
  }, [lastLen, lastKind, jumpToBottom]);

  // 切 Tab / 首次激活：按 ui-state 的滚动锚点还原 —— 取代原先的「无条件贴底硬重置」。
  // ChatMessages 是单实例随 activeKey 换数据不重挂，贴底/跟随态必须在本轮 commit 里重定（否则跨 Tab 残留），
  // 同时恢复到上次离开时的阅读位置（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。
  // 声明在跟随 effect 之前，保证同一次 commit 内先落位，跟随 effect 才不会把视口拽回底部。
  useEffect(() => {
    progScroll.current = 0;
    // 哨兵用 NaN（与任何值比较恒 false = 永不豁免）：Infinity 在区间语义下会变成「≥ from-40 全豁免」，
    // 后人若只改这一处、忘了同步关窗口就会踩雷
    progTarget.current = NaN;
    anchorSid.current = activeKey;
    anchorHit.current = false;
    pendingAnchor.current = null;
    const el = scroller.current;
    const anchor = activeKey ? getScrollAnchor(activeKey) : null;
    // 无锚点（新会话 / 从未滚动过）或上次本就贴底 ⇒ 保持贴底：与旧逻辑行为一致，不做无谓的中间位还原
    if (!anchor || anchor.kind === "bottom") {
      // 走 jumpToBottom 而不是 restoreAnchor：同样落到底部，但一并登记豁免窗口（见其注释）
      jumpToBottom(el);
      return;
    }
    // 有消息锚：先落位，并停掉自动跟随 —— 否则流式新增消息的下滚会把刚还原的位置顶掉。
    pendingAnchor.current = anchor;
    stickBottom.current = false;
    if (!el) return;
    // 骨架 Tab（懒加载首帧 items 还是空的）：此刻 collectNodes 读不到节点，restoreAnchor 会按「找不到」
    // 降级贴底 —— 先跳底部再跳回锚点是白闪一下。干脆不落位：交给下面的「二次校正」等消息渲染出来一次落位。
    if (collectNodes(el).length === 0) return;
    anchorHit.current = restoreAnchor(el, anchor);
    markProgScroll(el, el.scrollTop);
    if (anchorHit.current) syncFollowState(el); // 落在底部附近就当贴底跟随，否则显示「回到底部」
  }, [activeKey, jumpToBottom, markProgScroll]);

  // 二次校正（懒加载）：Tab 刚加载完的首帧布局未稳定（items 从空变为有内容），锚点还原会偏 ——
  // 依赖 lastLen（items.length）：内容就绪时重跑，并在接下来的帧里重定位到命中为止。
  // 幂等与有界：命中或取消（切 Tab / 用户接管）即停；每轮最多 3 帧，既不会逐帧重设 scrollTop（抖动），
  // 也不会因自身触发的重渲染而自激成死循环。
  useEffect(() => {
    const anchor = pendingAnchor.current;
    const el = scroller.current;
    if (!anchor || !el) return;
    if (collectNodes(el).length === 0) return; // 消息节点尚未渲染：等 items.length 变化的下一轮
    let tries = 0;
    let raf = requestAnimationFrame(function settle() {
      if (pendingAnchor.current !== anchor) return; // 已取消（切 Tab / 用户接管滚动）
      if (useSessions.getState().activeKey !== anchorSid.current) return; // 已切走：不把位置写到别的会话
      const hit = restoreAnchor(el, anchor);
      markProgScroll(el, el.scrollTop);
      if (hit) {
        anchorHit.current = true;
        pendingAnchor.current = null;
        syncFollowState(el);
        return;
      }
      // 未命中：多为 markdown/代码块/图片尚未撑高导致布局漂移 —— 再补一帧；上限 2 次即停
      if (++tries <= 2) {
        raf = requestAnimationFrame(settle);
        return;
      }
      pendingAnchor.current = null;
      // 本轮激活从未命中过（锚点消息已被压缩/裁掉）⇒ 按 scrollAnchor 的约定降级贴底；
      // 命中过则保留用户当前所见位置，不再把它拉到底部 —— 二次校正绝不能成为新的抖动源。
      if (!anchorHit.current) {
        stickBottom.current = true;
        setAtBottom(true);
        // 这条锚点再也命不中（消息被裁/被删）：就地改写成「贴底」，免得每次激活都空跑一轮无效校正
        if (activeKey) setScrollAnchor(activeKey, { kind: "bottom" });
      }
    });
    return () => cancelAnimationFrame(raf);
  }, [activeKey, lastLen, markProgScroll]);

  // 内容增长 / 定稿（streaming 翻转）时：贴底则跟随滚动；同时升级 katex/mermaid 占位符
  // （流式期间 diagrams 跳过 mermaid；收尾时 streamCount 变化重跑本 effect 补渲染）
  const streamCount = active.items.reduce((a, i) => a + (i.kind === "assistant" && i.streaming ? 1 : 0), 0);
  // 内容指纹（下方 effect 的依赖项）：抽成变量是为了满足 exhaustive-deps 的「依赖数组不做复杂表达式」，
  // 每次渲染都会算，与内联在数组里等价
  const contentLen = active.items.reduce(
    (a, i) =>
      a +
      (i.kind === "assistant"
        ? i.timeline.reduce((n, s) => n + (s.kind === "tool" || s.kind === "sub" ? 1 : s.text.length), 0)
        : 0),
    0,
  );
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    // 用户暂停跟随（滚轮上滚/卡片展开）= 阅读意图，优先于几何判断：
    // 旧的几何 nearBottom<80 重接管会把刚暂停的跟随立即撤销（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。
    // 几何恢复只保留两条路：onScroll nearBottom<40 与「回到底部」按钮。
    if (!stickBottom.current) return;
    // 先同步贴底：跟随不能押在图表升级的 promise 上（升级链路一旦 reject，视图就再也跟不上）
    jumpToBottom(el);
    // 升级后高度会变（katex/mermaid 撑高）：贴底则再补一次；升级失败只影响图表渲染，不该影响聊天，故吞掉
    void upgradeDiagrams(el)
      .catch((e) => {
        // 升级失败（动态导入失败等）只影响图表渲染，不该影响聊天跟随；DEV 下留个信号（仓库惯例）
        if (import.meta.env.DEV) console.warn("[diagram-upgrade]", e);
      })
      .then(() => {
        if (stickBottom.current && scroller.current === el) jumpToBottom(el);
      });
  }, [
    active.items.length,
    active.subs.length,
    streamCount, // done/error/cancelled 是三条翻 streaming 的收尾路径——必须触发补渲染
    contentLen,
    active.pendingFinals,
    active.finalReveals,
    jumpToBottom,
  ]);

  // 用户滚动 -> 更新贴底态（离底 >40px 记为离开）。程序化豁免窗口用落点比对：
  // 瞬时跳底只豁免「落点 ±BOTTOM_EPS」这一点，平滑动画才用 [from, target] 区间（动画期间位置逐帧移动）。
  // 越过区间即用户接管滚动（拖滚动条 / 键盘 PageUp 不产生 wheel 事件，走此路径暂停跟随，
  // [docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。
  // 不做「落点是否到底」检查：流式期间渲染与滚动交错使几何读数漂移，会把正常跟随误判为用户滚动。
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    // 滚动即记锚点（自家程序化滚动也记：跟随贴底期间记下的就是「贴底」，切回来照旧贴底）
    recordAnchor(activeKey);
    if (Date.now() < progScroll.current) {
      if (isSelfScroll(el.scrollTop, progFrom.current, progTarget.current)) return; // 自家事件，豁免
      progScroll.current = 0; // 被外部打断 -> 交出控制权，按用户滚动处理
    }
    const nearBottom = isAtBottom({ scrollTop: el.scrollTop, scrollHeight: el.scrollHeight, clientHeight: el.clientHeight });
    stickBottom.current = nearBottom;
    setAtBottom(nearBottom);
    // 滚动条显隐交给 ChatScrollbar 子组件监听同一个 scroll 事件，这里不再维护
  };

  // 滚轮上滚 = 阅读意图：立即暂停跟随并清空程序化滚动豁免窗口（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。
  // 缺陷背景：流式期间跟随 effect 每帧续 150ms 豁免，高频思考 delta 下窗口永续，
  // 用户上滚产生的滚动事件全被豁免吞掉 -> stickBottom 恒 true、视图被拽住无法上滚。
  // 挂在滚动容器根上：任意元素的上滚 wheel 都会冒泡至此，思考块（展开与否）与流式体一并覆盖。
  // 恢复跟随不在此处理：暂停时已清空豁免窗口，滚回底部产生的滚动事件会被 onScroll
  // 正常处理并恢复跟随（nearBottom<40）；wheel 向下分支在无布局环境会误判底部，故刻意不写。
  const onWheel = (e: React.WheelEvent) => {
    if (e.deltaY < 0 && stickBottom.current) {
      stickBottom.current = false;
      progScroll.current = 0; // 本手势的滚动事件不再豁免，正常参与贴底判定
      setAtBottom(false);
    }
  };

  // ---------- 历史分页（批2 P3）：入口显隐 / 逐段前翻 / 收起更早的 ----------
  // 门 = 「段式会话」本身（`format === "new"`），不掺 hasMore / loadedPages：单段新格式会话顶部
  // 也要给终态「已到最早的消息」——否则「确实没有更早内容」与「入口坏了」在界面上无法区分（AC-12），
  // 「有没有更早内容」交给门内的 canLoadEarlier 决定。legacy 会话首屏即整份（format = legacy）、
  // 旧后端不给 paging，两种情形都不出这一行。
  const paging = active.paging ?? null;
  const pagedEarlier = !!paging && paging.loadedPages > 1;
  const atPageLimit = !!paging && paging.loadedPages >= MAX_PAGED_PAGES;
  const canLoadEarlier = !!paging && paging.format === "new" && paging.hasMore && !atPageLimit;
  const pagingBar = !!paging && paging.format === "new";
  // 坏段提示（批2 P2）：>0 才渲染（0 / 缺字段时零打扰）；口径 = 已加载段范围内跳过的段数
  const badSegments = paging?.badSegments ?? 0;

  /** 前翻：先把当前阅读位置记成待还原锚点——更早一段前插后由「二次校正」effect 把它落回原处（AC-13）。
   *  稳定键（data-key）保证锚点项在前插后仍能命中：这正是批1 的 idx 语义被分页破坏的那一环。
   *  贴底锚点不登记：前插不动 scrollTop，视口自然停在更早内容那一侧（强行还原贴底反而被弹回最新处）。 */
  const onLoadEarlier = () => {
    const el = scroller.current;
    if (!el || !activeKey) return;
    const anchor = captureAnchor(el);
    pendingAnchor.current = anchor.kind === "item" ? anchor : null;
    suspendFollow();
    void useRun.getState().loadEarlier(activeKey);
  };

  /** 收起更早的（AC-15 内存有界）：丢掉前翻加载的旧段，回首屏那一段 */
  const onCollapseEarlier = () => {
    if (activeKey) useRun.getState().collapseEarlier(activeKey);
  };

  function pick(s: string) {
    void useRun.getState().send(s);
  }

  /** 与 items 同序的稳定渲染键（AC-14）：恢复自磁盘的前缀用「段号 + 段内序号」，其余按 live 序数（见 itemKeysOf） */
  const keys = itemKeysOf(active.items, active.itemKeys);

  /** 压缩边界分隔线的落点（批2 P2）：由稳定键的段号现算（见 boundaryAtIndexes）——
   *  前翻把更早段插到头部后位置自然跟随，store 侧不存任何下标。 */
  const boundaryAt = boundaryAtIndexes(keys, active.boundaries);
  const protectedKeys = new Set(keys.filter(key => Object.values(toolExpansions[expansionScope(activeKey, key)] ?? {}).some(expansion => expansion.expanded)));
  // Keep an opened group's root stable when earlier history is prepended.
  const groupRoots = new Set(keys.filter(key => Object.values(groupExpansions[expansionScope(activeKey, key)] ?? {}).some(Boolean)));
  const entries = outputDisplay === "compact"
    ? compactTranscript(active.items, keys, boundaryAt, protectedKeys, groupRoots)
    : active.items.map((item, index) => ({ item, index, key: keys[index], members: [] }));
  const taskEntries = taskTranscript(entries, active.taskRuns ?? [], active.subs);
  // Commit collapsed process first. Two animation frames guarantee a browser paint before final text.
  useEffect(() => {
    if (!activeKey) return;
    const pending = Object.keys(active.pendingFinals ?? {});
    if (!pending.length) return;
    if (document.hidden) {
      for (const runId of pending) useRun.getState().revealFinal(activeKey, runId, false);
      return;
    }
    const ready = new Set(taskEntries.flatMap(entry => "kind" in entry && entry.finalKey ? [entry.run.run_id] : []));
    for (const runId of pending) if (!ready.has(runId)) useRun.getState().revealFinal(activeKey, runId, false);
    const staged = pending.filter(runId => ready.has(runId));
    let secondFrame = 0;
    const firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(() => {
        for (const runId of staged) useRun.getState().revealFinal(activeKey, runId);
      });
    });
    return () => { cancelAnimationFrame(firstFrame); if (secondFrame) cancelAnimationFrame(secondFrame); };
    // taskRuns and pendingFinals are published together by the terminal reducer.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeKey, active.pendingFinals]);
  useEffect(() => {
    const sessionId = activeKey;
    return () => {
      if (!sessionId) return;
      for (const runId of Object.keys(useRun.getState().tabs[sessionId]?.pendingFinals ?? {})) useRun.getState().revealFinal(sessionId, runId, false);
      for (const runId of Object.keys(useRun.getState().tabs[sessionId]?.finalReveals ?? {})) useRun.getState().finishFinalReveal(sessionId, runId);
    };
  }, [activeKey]);
  useEffect(() => {
    const finishHidden = () => {
      if (!document.hidden || !activeKey) return;
      const tab = useRun.getState().tabs[activeKey];
      for (const runId of Object.keys(tab?.pendingFinals ?? {})) useRun.getState().revealFinal(activeKey, runId, false);
      for (const runId of Object.keys(tab?.finalReveals ?? {})) useRun.getState().finishFinalReveal(activeKey, runId);
    };
    document.addEventListener("visibilitychange", finishHidden);
    return () => document.removeEventListener("visibilitychange", finishHidden);
  }, [activeKey]);
  const updateToolExpansion = useCallback((messageKey: string, callKey: string, expansion: ToolExpansion) => {
    const scope = expansionScope(activeKey, messageKey);
    setToolExpansions(previous => ({ ...previous, [scope]: { ...previous[scope], [callKey]: expansion } }));
  }, [activeKey]);
  const updateGroupExpansion = useCallback((messageKey: string, callKey: string, expanded: boolean) => {
    const scope = expansionScope(activeKey, messageKey);
    setGroupExpansions(previous => ({ ...previous, [scope]: { ...previous[scope], [callKey]: expanded } }));
  }, [activeKey]);

  /** 渲染一条转录项。`itemKey` = 稳定键（AC-14）：既作 React key，也打给 DOM 供滚动锚点定位——
   *  用数组下标作 key 时，分页前插会让所有节点错位（同一条消息被重挂、局部状态丢失）。 */
  const renderItem = (item: UiItem, itemKey: string, origins?: Record<string, ToolOrigin>, originalItem: UiItem = item, finalRunId?: string, finalAnswer?: string) => {
    // 锚点定位依据：scrollAnchor.collectNodes 按 data-key（主）+ data-sig（指纹兜底）收集；
    // 两者都是字符串，memo 组件按值比较不受影响（不传对象，避免身份每次变化击穿 memo）。
    const anchorSig = itemSig(originalItem);
    if (item.kind === "user") {
      return (
        <UserMessage
          key={itemKey}
          text={item.text}
          images={item.images}
          anchorKey={itemKey}
          anchorSig={anchorSig}
        />
      );
    }
    if (item.kind === "assistant") {
      return (
        <AssistantMessage
          key={itemKey}
          item={item}
          streaming={item.streaming}
          hasPendingProcess={item.timeline.some(seg => seg.kind === "sub"
            ? active.subs.some(sub => sub.subId === seg.subId && sub.status === "running")
            : seg.kind === "tool" && item.toolsMap[seg.callKey]?.tool !== "subagent"
              && ["running", "waiting"].includes(item.toolsMap[seg.callKey]?.status ?? ""))}
          waitingForApproval={item.timeline.some(seg => seg.kind === "tool" && item.toolsMap[seg.callKey]?.status === "waiting")}
          onUserToggle={suspendFollow}
          anchorKey={itemKey}
          anchorSig={anchorSig}
          autoOpenCallKey={autoOpenCallKey}
          onConsumeAutoOpen={consumeAutoOpen}
          compact={outputDisplay === "compact"}
          origins={origins}
          expansionState={toolExpansions}
          groupExpansionState={groupExpansions}
          sessionKey={activeKey}
          onToolExpansionChange={updateToolExpansion}
          onGroupExpansionChange={updateGroupExpansion}
          finalRevealKey={finalRunId && active.finalReveals?.[finalRunId] ? `${activeKey}:${finalRunId}` : undefined}
          onFinalRevealProgress={onFinalRevealProgress}
          onFinalRevealComplete={finalRunId && activeKey ? () => useRun.getState().finishFinalReveal(activeKey, finalRunId) : undefined}
          finalAnswer={finalAnswer}
        />
      );
    }
    if (item.kind === "sub") {
      return <SubagentItemCard key={itemKey} subId={item.subId} />;
    }
    if (item.kind === "notice") {
      // 注入通知带正文（steer 通道：run:inject 载荷含 texts，[docs/runtime-and-scheduling](../../../../docs/runtime-and-scheduling.md)）。
      // 防御性过滤：正常路径后端已滤掉空串；万一整组为空则**退回无正文形态**——不渲染空正文行、也不挂 Tooltip
      //（否则悬停会弹出一个空白浮层）。此时逐字等价于旧的单行 notice，是回归基线。
      const injected = (item.injectedTexts ?? []).filter((s) => s.trim() !== "");
      if (injected.length === 0) {
        return (
          <div key={itemKey} className="notice-line dim" data-key={itemKey} data-sig={anchorSig} style={{ margin: "8px 0", fontSize: fontSizeCss(12.5) }}>· {item.text}</div>
        );
      }
      // Tooltip **每条各挂**（而非挂整块）：① 悬停哪一行就给哪一行的全文，N 条时不会张冠李戴；
      // ② 单条 title 是纯字符串，antd 默认 `wordWrap: break-word` + max-width 就能折行显示长文本，
      //    不依赖「\n 要靠 white-space: pre-wrap 才换行」这类容器样式语义（挂整块就得额外配 pre-wrap）；
      // ③ rc-trigger 的浮层按需挂载，N 个 Tooltip 实例在未悬停时不产生任何 DOM，开销可忽略。
      // Tooltip 只克隆子元素加 ref/事件，不额外插 DOM —— 截断行的 width 仍由父级约束，ellipsis 生效。
      // 视觉权重：外层 div 已带 `dim`，正文行继承颜色，不提到 user 气泡级别；paddingLeft 让后续行对齐到「· 」后的正文起始处。
      return (
        <div key={itemKey} className="notice-line dim" data-key={itemKey} data-sig={anchorSig} style={{ margin: "8px 0", fontSize: fontSizeCss(12.5) }}>
          <div>· {item.text}</div>
          <div style={{ paddingLeft: 14 }}>
            {injected.map((s, i) => (
              <Tooltip key={`${itemKey}-inj-${i}`} title={s}>
                <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s}</div>
              </Tooltip>
            ))}
          </div>
        </div>
      );
    }
    if (item.kind === "error") {
      // 错误行不打锚点标：antd Alert 的 props 类型不透传 data-*（AlertProps 无 HTMLAttributes 兜底），
      // collectNodes 会自然跳过它并把锚点落到上一条已标注消息上 —— 位置仍然正确。
      // [docs/providers-and-network](../../../../docs/providers-and-network.md)：鉴权/计费失败指向 provider 设置而非死胡同报错；
      // 修复回路 = 指引 + 一次点击，绝不自动弹模态框
      const hintKey =
        item.errorKind === "auth" ? "notice.authErrorHint"
        : item.errorKind === "billing" ? "notice.billingErrorHint"
        : null;
      return (
        <Alert
          key={itemKey}
          type="error"
          showIcon={false}
          style={{ margin: "8px 0" }}
          message={item.text}
          description={
            hintKey ? (
              <div className="error-guide" style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
                <span>{t(hintKey)}</span>
                <Button size="small" onClick={() => useUi.getState().showSettings("providers")}>
                  {t("notice.openModelSettings")}
                </Button>
              </div>
            ) : undefined
          }
        />
      );
    }
  };

  const renderEntry = (entry: TranscriptEntry, finalRunId?: string, finalAnswer?: string) => (
    <Fragment key={entry.key}>
      {(boundaryAt.get(entry.index) ?? []).map(b => (
        <CompactionDivider key={`bd:${b.seq}:${b.source ?? "compact"}`} seq={b.seq} source={b.source} />
      ))}
      {renderItem(entry.item, entry.key, entry.origins, entry.members[0]?.item ?? entry.item, finalRunId, finalAnswer)}
    </Fragment>
  );

  return (
    <div className="chat-body" style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column", position: "relative" }}>
      <div ref={scroller} className="chat-messages" onScroll={onScroll} onWheel={onWheel}>
        {/* 自定义 overlay 滚动条：仿 MiniMax Code / Slack 范式，native 滚动条已隐藏（见 app.css） */}
        <ChatScrollbar target={scroller} />
        {active.items.length === 0 && !hasSession && (
          <div className="chat-empty-guide">
            <div className="guide-icon"><MessageOutlined /></div>
            <div className="guide-title">{t("app.emptyTitle")}</div>
            <div className="guide-desc">{t("app.emptyDesc")}</div>
            <div className="guide-actions">
              <Button
                type="primary"
                size="large"
                icon={<MessageOutlined />}
                onClick={() => void useSessions.getState().openFreeSession()}
              >
                {t("app.emptyNewChat")}
              </Button>
              <Button
                size="large"
                icon={<FolderAddOutlined />}
                onClick={() => useUi.setState({ createProjectRequested: true })}
              >
                {t("app.emptyNewProject")}
              </Button>
            </div>
          </div>
        )}
        {/* 坏段提示（批2 P2）：转录顶部一条轻量告警——有段读不出来，但其余内容正常；不打断阅读 */}
        {badSegments > 0 && (
          <Alert type="warning" showIcon style={{ margin: "8px 0" }} title={t("chat.badSegments", { n: badSegments })} />
        )}
        {/* 历史分页入口（批2 P3 AC-12）：随内容滚动，不打锚点标（collectNodes 自然跳过） */}
        {pagingBar && (
          <div className="paging-bar" style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 8, padding: "4px 0" }}>
            {canLoadEarlier ? (
              <Button type="text" size="small" loading={paging!.loading} onClick={onLoadEarlier}>
                {t("chat.loadEarlier")}
              </Button>
            ) : (
              <span className="dim" style={{ fontSize: fontSizeCss(12.5) }}>
                {atPageLimit ? t("chat.loadEarlierCap", { pages: MAX_PAGED_PAGES }) : t("chat.earliest")}
              </span>
            )}
            {pagedEarlier && (
              <Button type="text" size="small" onClick={onCollapseEarlier}>
                {t("chat.collapseEarlier")}
              </Button>
            )}
          </div>
        )}
        {taskEntries.map(entry => {
          if (!("kind" in entry)) return renderEntry(entry);
          const scope = expansionScope(activeKey, entry.run.run_id);
          const expanded = !entry.canCollapse || (taskExpansions[scope] ?? entry.run.status !== "done");
          const label = t("chat.taskProcess", { duration: taskDuration(entry.run.duration_ms, i18n.language) });
          const statusLabel = entry.run.status === "done" ? ""
            : ` · ${entry.run.status === "error" ? t("chat.taskStatus.error") : t("chat.taskStatus.cancelled")}`;
          const aliases = entry.process.flatMap(part => part.members.length
            ? part.members.map(member => ({ key: member.key, sig: itemSig(member.item) }))
            : [{ key: part.key, sig: itemSig(active.items[part.index] ?? part.item) }]);
          const processKeys = entry.process.flatMap(part => part.item.kind === "assistant"
            ? part.item.timeline.flatMap((seg, index) => {
              if (seg.kind === "tool") return [seg.callKey];
              if (seg.kind === "sub") return [`sub:${seg.subId}`];
              if (seg.kind !== "thinking" && seg.kind !== "text") return [];
              const owner = part.origins?.[`${seg.kind}:${index}`]?.key ?? part.key;
              const source = active.items[keys.indexOf(owner)];
              const sourceIndex = source?.kind === "assistant" ? source.timeline.indexOf(seg) : -1;
              // Both the expanded projection and the original live stream can be the captured reader position.
              return [`${seg.kind}:${index}`, ...(seg.kind === "text" && seg.outputKey ? [seg.outputKey] : []),
                ...(sourceIndex >= 0 ? [`${seg.kind}:${sourceIndex}`] : [])];
            }) : []);
          const messageKeys = [...new Set(entry.process.flatMap(part => [part.key, ...Object.values(part.origins ?? {}).map(origin => origin.key)]))];
          return (
            <Fragment key={entry.key}>
              {entry.process.length > 0 ? (
                <Collapse className="task-process" ghost activeKey={expanded ? [entry.key] : []}
                  expandIconPlacement="end"
                  styles={{
                    root: { marginBottom: 12 },
                    header: { paddingInline: 0, borderBottom: "1px solid var(--ws-border)", color: "var(--ws-text-2)" },
                    title: { flex: "0 1 auto", marginInlineEnd: 0 },
                    body: { padding: 0 },
                  }}
                  onChange={next => {
                    if (scroller.current) taskAnchor.current = { sessionId: activeKey, anchor: captureOutputAnchor(scroller.current) };
                    suspendFollow();
                    setTaskExpansions(previous => ({ ...previous, [scope]: next.includes(entry.key) }));
                  }}
                  items={[{ key: entry.key,
                    collapsible: entry.canCollapse ? undefined : "disabled",
                    label: <span data-anchor-aliases={!expanded ? JSON.stringify(aliases) : undefined}
                      data-output-key={!expanded ? entry.key : undefined}
                      data-output-group={!expanded ? JSON.stringify(processKeys) : undefined}
                      data-output-message-keys={!expanded ? JSON.stringify(messageKeys) : undefined}
                    >{label}{statusLabel}</span>,
                    children: <div hidden={!expanded}>{entry.process.map(part => renderEntry(part))}</div>,
                  }]} />
              ) : <div className="notice-line dim" style={{ margin: "8px 0" }}>{label}{statusLabel}</div>}
              {entry.visible.filter(part => !active.pendingFinals?.[entry.run.run_id] || part.key !== entry.finalKey).map(part => renderEntry(part,
                part.key === entry.finalKey ? entry.run.run_id : undefined,
                part.key === entry.finalKey && !active.finalReveals?.[entry.run.run_id] ? entry.run.final_answer ?? undefined : undefined))}
              {active.pendingFinals?.[entry.run.run_id] && <Typography.Text type="secondary" role="status">{t("chat.activityResponding")}</Typography.Text>}
            </Fragment>
          );
        })}

        {active.suggestions.length > 0 && !active.running && !Object.keys(active.pendingFinals ?? {}).length && !Object.keys(active.finalReveals ?? {}).length && (
          <div className="chips">
            <div className="chips-label">
              <ThunderboltOutlined /> {t("chat.suggestions")}
            </div>
            <div className="chips-row">
              {active.suggestions.map((s, i) => (
                <Button
                  key={i}
                  size="small"
                  shape="round"
                  variant="filled"
                  color="default"
                  icon={<ArrowRightOutlined />}
                  iconPosition="end"
                  onClick={() => pick(s)}
                >
                  {s}
                </Button>
              ))}
            </div>
          </div>
        )}
      </div>
      {!atBottom && (
        <div style={{ position: "absolute", left: 0, right: 0, bottom: 8, display: "flex", justifyContent: "center", pointerEvents: "none", zIndex: 5 }}>
          <Button
            size="small"
            shape="circle"
            icon={<DownOutlined />}
            style={{ pointerEvents: "auto", boxShadow: "0 2px 8px rgba(0,0,0,0.15)" }}
            onClick={() => jumpToBottom(scroller.current, true)}
            aria-label={t("chat.scrollToBottom")}
          />
        </div>
      )}
    </div>
  );
}
