import { memo, useCallback, useEffect, useRef, useState } from "react";
import { Alert, Button, Image, Tooltip } from "antd";
import {
  CheckOutlined,
  CopyOutlined,
  EditOutlined,
  MessageOutlined,
  FolderAddOutlined,
  DownOutlined,
  ThunderboltOutlined,
  RightOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useActiveRun, useRun } from "../../stores/run";
import { useSessions } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import { upgradeDiagrams } from "../../utils/diagrams";
import type { UiItem } from "../../stores/run";
// 滚动锚点（会话保存与恢复优化 · 批1）：锚点读写与现场态落盘的调用链见本文件「滚动锚点」一节。
import { captureAnchor, collectNodes, isAtBottom, itemSig, restoreAnchor } from "../../utils/scrollAnchor";
import type { ScrollAnchor } from "../../utils/scrollAnchor";
import { getScrollAnchor, scheduleAnchor, setScrollAnchor } from "../../utils/uiState";
import SubagentItemCard from "../subagent/SubagentItemCard";
import { TimelineSegsView } from "./segments";

// 消息时间戳：无时间则留空（旧存档 / 恢复期间）；绝不用当前时间伪造（[docs/titlebar-content-batch](../../../../docs/titlebar-content-batch.md) 缺陷修复）
function ts(iso: string | undefined): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const d = new Date(t);
  const pad = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

// 最后一条 item 的 kind（effect 依赖只挂 kind，避免依赖整个数组）
function lastItem_kind(items: UiItem[]): string {
  return items.length ? items[items.length - 1].kind : "";
}

/** 消息行锚点标注：scrollAnchor.ts 的 collectNodes 按 data-sig / data-idx 收集可锚定节点。
 *  仅作定位参考（不加样式、不改结构语义）；传字面量属性而非对象，避免 memo 因对象身份失效。 */
interface AnchorAttrs {
  anchorSig: string;
  anchorIdx: number;
}

/** 用户消息：文本气泡 + 图片缩略（可预览）；悬停操作提供复制与「修改」（经 ws:composer-fill 回填 Composer，不自动发送）。 */
const UserMessage = memo(function UserMessage({
  text,
  createdAt,
  images,
  anchorSig,
  anchorIdx,
}: {
  text: string;
  createdAt?: string;
  images?: { mediaType: string; data: string }[];
} & AnchorAttrs) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  // 复制进剪贴板：成功后短暂切换为 ✓ 反馈
  const doCopy = () => {
    if (!text) return;
    void navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => {});
  };
  // 「修改」：内容经 ws:composer-fill 回填 Composer（覆盖当前草稿，与队列「编辑」同语义）；Composer 自行聚焦，不直接发送。
  // 附件一并携带：带图消息点修改图片回显，无图消息覆盖清空现有附件
  const onEdit = () => {
    if (!text) return;
    window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text, images } }));
  };
  return (
    <div className="msg user" data-sig={anchorSig} data-idx={anchorIdx}>
      <div className="role">
        <span className="ts">{ts(createdAt)}</span>
        <span>{t("chat.you")}</span>
      </div>
      {text && <div className="bubble user-bubble">{text}</div>}
      {text && (
        <div className="user-msg-actions">
          <Tooltip title={t("chat.copy")}>
            <Button
              type="text"
              size="small"
              aria-label={t("chat.copy")}
              icon={copied ? <CheckOutlined /> : <CopyOutlined />}
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

const AssistantMessage = memo(function AssistantMessage({
  item,
  streaming,
  onUserToggle,
  anchorSig,
  anchorIdx,
}: {
  item: Extract<UiItem, { kind: "assistant" }>;
  streaming: boolean;
  onUserToggle?: () => void;
} & AnchorAttrs) {
  return (
    <div className="msg assistant" data-sig={anchorSig} data-idx={anchorIdx}>
      <div className="role">
        <span>CodeWave</span>
        <span className="ts">{ts(item.createdAt)}</span>
      </div>
      {/* 按 timeline 顺序穿插渲染各段（与子代理过程抽屉共用同一段渲染；docs/subagent-interaction-drawer 抽取至 segments.tsx） */}
      <TimelineSegsView timeline={item.timeline} toolsMap={item.toolsMap} streaming={streaming} onUserToggle={onUserToggle} />
      {streaming && <span className="cursor">▍</span>}
    </div>
  );
});

/** 聊天消息区：单实例随 activeKey 换数据不重挂；维护「贴底跟随 / 用户接管」滚动模型——
 *  贴底时内容增长自动下滚；滚轮上滚或展开卡片即阅读意图暂停跟随；恢复仅限滚回底部或
 *  「回到底部」按钮。程序化滚动用豁免窗口 + 目标值比对，防止自家跳转被误判为用户滚动
 *  （[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md)）。 */
export default function ChatMessages() {
  const { t } = useTranslation();
  const active = useActiveRun();
  const hasSession = useSessions((s) => !!s.activeKey);
  const scroller = useRef<HTMLDivElement>(null);
  // 2.4：是否贴底（贴底 -> 自动下滚；未贴底 -> 显示「回到底部」按钮）
  const [atBottom, setAtBottom] = useState(true);
  const stickBottom = useRef(true);
  const progScroll = useRef(0); // 程序化滚动的豁免窗口：窗口内自家触发的滚动事件不参与贴底判定
  const progTarget = useRef(Infinity); // 最近一次程序化跳底的 scrollTop 目标（目标比对豁免，docs/thinking-scroll-fix §2.3）
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

  const scrollToBottom = (smooth = false) => {
    const el = scroller.current;
    if (!el) return;
    progScroll.current = Date.now() + 150;
    progTarget.current = el.scrollHeight;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
    stickBottom.current = true;
    setAtBottom(true);
  };

  /** 程序化滚动登记豁免窗口：自家产生的 scroll 事件不参与「用户接管」判定（否则还原动作会
   *  把自己当成用户滚动，把待还原的锚点当场取消）。沿用 scrollToBottom 的目标值比对机制：
   *  scroll 事件在下一帧才派发，所以我可以在赋值之后读回实际 scrollTop 作为目标值。 */
  const markProgScroll = (el: HTMLElement) => {
    progScroll.current = Date.now() + 150;
    progTarget.current = el.scrollTop;
  };

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
      requestAnimationFrame(() => scrollToBottom(false));
    }
  }, [lastLen, lastKind]);

  // 切 Tab / 首次激活：按 ui-state 的滚动锚点还原 —— 取代原先的「无条件贴底硬重置」。
  // ChatMessages 是单实例随 activeKey 换数据不重挂，贴底/跟随态必须在本轮 commit 里重定（否则跨 Tab 残留），
  // 同时恢复到上次离开时的阅读位置（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md) §2.4）。
  // 声明在跟随 effect 之前，保证同一次 commit 内先落位，跟随 effect 才不会把视口拽回底部。
  useEffect(() => {
    progScroll.current = 0;
    progTarget.current = Infinity;
    anchorSid.current = activeKey;
    anchorHit.current = false;
    pendingAnchor.current = null;
    const el = scroller.current;
    const anchor = activeKey ? getScrollAnchor(activeKey) : null;
    // 无锚点（新会话 / 从未滚动过）或上次本就贴底 ⇒ 保持贴底：与旧逻辑行为一致，不做无谓的中间位还原
    if (!anchor || anchor.kind === "bottom") {
      stickBottom.current = true;
      setAtBottom(true);
      if (el) restoreAnchor(el, anchor); // bottom / null ⇒ scrollTop = scrollHeight
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
    markProgScroll(el);
    if (anchorHit.current) syncFollowState(el); // 落在底部附近就当贴底跟随，否则显示「回到底部」
  }, [activeKey]);

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
      markProgScroll(el);
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
  }, [activeKey, lastLen]);

  // 内容增长 / 定稿（streaming 翻转）时：贴底则跟随滚动；同时升级 katex/mermaid 占位符
  // （流式期间 diagrams 跳过 mermaid；收尾时 streamCount 变化重跑本 effect 补渲染）
  const streamCount = active.items.reduce((a, i) => a + (i.kind === "assistant" && i.streaming ? 1 : 0), 0);
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    // 用户暂停跟随（滚轮上滚/卡片展开）= 阅读意图，优先于几何判断：
    // 旧的几何 nearBottom<80 重接管会把刚暂停的跟随立即撤销（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md)）。
    // 几何恢复只保留两条路：onScroll nearBottom<40 与「回到底部」按钮。
    if (!stickBottom.current) return;
    void upgradeDiagrams(el).then(() => {
      if (stickBottom.current) {
        progScroll.current = Date.now() + 150;
        progTarget.current = el.scrollHeight;
        el.scrollTop = el.scrollHeight;
      }
    });
  }, [
    active.items.length,
    active.subs.length,
    streamCount, // done/error/cancelled 是三条翻 streaming 的收尾路径——必须触发补渲染
    active.items.reduce(
      (a, i) =>
        a +
        (i.kind === "assistant"
          ? i.timeline.reduce((n, s) => n + (s.kind === "tool" || s.kind === "sub" ? 1 : s.text.length), 0)
          : 0),
      0,
    ),
  ]);

  // 用户滚动 -> 更新贴底态（离底 >40px 记为离开）。程序化豁免窗口用目标值比对：
  // 窗口内只有「scrollTop ≈ 跳转目标」的自家事件被吞；偏离目标即用户接管滚动
  // （拖滚动条 / 键盘 PageUp 不产生 wheel 事件，走此路径暂停跟随，[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md) §2.3）。
  // 不做「落点是否到底」检查：流式期间渲染与滚动交错使几何读数漂移，会把正常跟随误判为用户滚动。
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    // 滚动即记锚点（自家程序化滚动也记：跟随贴底期间记下的就是「贴底」，切回来照旧贴底）
    recordAnchor(activeKey);
    if (Date.now() < progScroll.current) {
      if (Math.abs(el.scrollTop - progTarget.current) < 40) return; // 自家事件，豁免
      progScroll.current = 0; // 被外部打断 -> 交出控制权，按用户滚动处理
    }
    const nearBottom = isAtBottom({ scrollTop: el.scrollTop, scrollHeight: el.scrollHeight, clientHeight: el.clientHeight });
    stickBottom.current = nearBottom;
    setAtBottom(nearBottom);
  };

  // 滚轮上滚 = 阅读意图：立即暂停跟随并清空程序化滚动豁免窗口（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md)）。
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

  function pick(s: string) {
    void useRun.getState().send(s);
  }

  const renderItem = (item: UiItem, i: number) => {
    // 锚点指纹：scrollAnchor.collectNodes 按 data-sig（主）+ data-idx（精确提示）收集可锚定节点；
    // sig 是字符串，memo 组件按值比较不受影响（不传对象，避免身份每次变化击穿 memo）。
    const anchorSig = itemSig(item);
    if (item.kind === "user") {
      return (
        <UserMessage
          key={i}
          text={item.text}
          createdAt={item.createdAt}
          images={item.images}
          anchorSig={anchorSig}
          anchorIdx={i}
        />
      );
    }
    if (item.kind === "assistant") {
      return (
        <AssistantMessage
          key={i}
          item={item}
          streaming={item.streaming}
          onUserToggle={suspendFollow}
          anchorSig={anchorSig}
          anchorIdx={i}
        />
      );
    }
    if (item.kind === "sub") {
      return <SubagentItemCard key={item.subId} subId={item.subId} />;
    }
    if (item.kind === "notice") {
      return (
        <div key={i} className="notice-line dim" data-sig={anchorSig} data-idx={i} style={{ margin: "8px 0", fontSize: 12.5 }}>· {item.text}</div>
      );
    }
    if (item.kind === "error") {
      // 错误行不打锚点标：antd Alert 的 props 类型不透传 data-*（AlertProps 无 HTMLAttributes 兜底），
      // collectNodes 会自然跳过它并把锚点落到上一条已标注消息上 —— 位置仍然正确。
      // [docs/auth-error-guidance](../../../../docs/auth-error-guidance.md)：鉴权/计费失败指向 provider 设置而非死胡同报错；
      // 修复回路 = 指引 + 一次点击，绝不自动弹模态框
      const hintKey =
        item.errorKind === "auth" ? "notice.authErrorHint"
        : item.errorKind === "billing" ? "notice.billingErrorHint"
        : null;
      return (
        <Alert
          key={i}
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

  return (
    <div className="chat-body" style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column", position: "relative" }}>
      <div ref={scroller} className="chat-messages" onScroll={onScroll} onWheel={onWheel}>
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
        {active.items.map(renderItem)}

        {active.suggestions.length > 0 && !active.running && (
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
                  icon={<RightOutlined />}
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
            onClick={() => scrollToBottom(true)}
            aria-label={t("chat.scrollToBottom")}
          />
        </div>
      )}
    </div>
  );
}
