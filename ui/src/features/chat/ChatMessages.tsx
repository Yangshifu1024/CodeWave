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

/** 用户消息：文本气泡 + 图片缩略（可预览）；悬停操作提供复制与「修改」（经 ws:composer-fill 回填 Composer，不自动发送）。 */
const UserMessage = memo(function UserMessage({
  text,
  createdAt,
  images,
}: {
  text: string;
  createdAt?: string;
  images?: { mediaType: string; data: string }[];
}) {
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
    <div className="msg user">
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
}: {
  item: Extract<UiItem, { kind: "assistant" }>;
  streaming: boolean;
  onUserToggle?: () => void;
}) {
  return (
    <div className="msg assistant">
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

  const scrollToBottom = (smooth = false) => {
    const el = scroller.current;
    if (!el) return;
    progScroll.current = Date.now() + 150;
    progTarget.current = el.scrollHeight;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
    stickBottom.current = true;
    setAtBottom(true);
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

  // 切 Tab 重置跟随态：ChatMessages 是单实例随 activeKey 换数据不重挂，
  // stickBottom/atBottom 跨 Tab 残留会让新会话假显「回到底部」且不跟随（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md) §2.4，评审建议）。
  // 声明在跟随 effect 之前，保证同一次 commit 内先跑重置。
  useEffect(() => {
    stickBottom.current = true;
    progScroll.current = 0;
    setAtBottom(true);
  }, [activeKey]);

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
    if (Date.now() < progScroll.current) {
      if (Math.abs(el.scrollTop - progTarget.current) < 40) return; // 自家事件，豁免
      progScroll.current = 0; // 被外部打断 -> 交出控制权，按用户滚动处理
    }
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
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
    if (item.kind === "user") {
      return <UserMessage key={i} text={item.text} createdAt={item.createdAt} images={item.images} />;
    }
    if (item.kind === "assistant") {
      return <AssistantMessage key={i} item={item} streaming={item.streaming} onUserToggle={suspendFollow} />;
    }
    if (item.kind === "sub") {
      return <SubagentItemCard key={item.subId} subId={item.subId} />;
    }
    if (item.kind === "notice") {
      return (
        <div key={i} className="notice-line dim" style={{ margin: "8px 0", fontSize: 12.5 }}>· {item.text}</div>
      );
    }
    if (item.kind === "error") {
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
