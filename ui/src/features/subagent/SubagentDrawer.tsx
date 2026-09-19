import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Drawer, Tag, Tooltip } from "antd";
import { CaretRightOutlined, CloseOutlined, LoadingOutlined, RobotOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useActiveRun, useRun } from "../../stores/run";
import { renderCached, TimelineSegsView } from "../chat/segments";

/** 子代理抽屉可用宽度：45% 视口，夹在 360–560 之间。 */
function drawerWidth(): number {
  return Math.min(560, Math.max(360, Math.round(window.innerWidth * 0.45)));
}

/** 子代理过程抽屉（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)）：右侧滑入、默认常显（首个子代理启动即弹出）。
 *  关闭只置 open=false 不销毁——流式数据留存于 subStreams，可经聊天卡 / Composer 指示器重进；
 *  归档子代理（会话恢复 / 历史 run）由 run.openSubDrawer 按需拉取过程历史。 */
export default function SubagentDrawer() {
  const { t } = useTranslation();
  const active = useActiveRun();
  const { open, subId } = active.subDrawer;
  const sub = (subId && active.subs.find((x) => x.subId === subId)) || null;
  const stream = (subId && active.subStreams[subId]) || null;
  const running = sub?.status === "running";
  // 经 useMemo 在打开时重算宽度——open 翻转的同一渲染内取值就绪（useEffect 里的 ref 写入
  // 落在渲染之后且不触发重渲染，关闭期间 resize 曾以旧宽度打开抽屉）；antd 6 的 size 接受数字
  // open 不是宽度的输入，而是「打开这一刻重算」的触发器：drawerWidth() 读的是窗口/DOM 尺寸，
  // 状态写入发生在渲染之后，删掉这个依赖会以旧宽度打开抽屉
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const width = useMemo(() => drawerWidth(), [open]);

  // 轻量贴底：内容增长即滚到底部；滚轮上滚 = 阅读意图，暂停跟随。
  // 移植自 ChatMessages（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md)）：程序化跳转声明豁免窗口 + 目标值，onScroll 吞掉
  // 自身触发的滚动事件，仅用户驱动滚动才重判钉住状态——否则触控板小幅上滚（<40px）
  // 会被重判为「在底部」，下一帧流式又把视图拽回底部。
  const bodyRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const progUntil = useRef(0); // 程序化滚动事件的豁免窗口（ms 时间戳）
  const progTarget = useRef(Infinity); // 最近一次程序化跳转的 scrollTop 目标（目标比对，docs/thinking-scroll-fix §2.3）
  const prevSubId = useRef<string | null>(null);
  // 指派任务默认展开：任务全文一眼可见（此前默认折叠 + 后端 2000 字符有损截断，展开也看不到全文）。
  // 长任务会把过程流推向下方，需要时用标签行收起——折叠态保留单行 ellipsis 预览（hover 有完整 title）。
  // [docs/subdrawer-scroll-hardening](../../../../docs/subdrawer-scroll-hardening.md) §6 的折叠能力保留，仅改默认态。
  // 切换子代理时重置回展开。
  const [taskCollapsed, setTaskCollapsed] = useState(false);
  const [, setTick] = useState(0);
  // 内容签名对齐 ChatMessages 的跟随依赖（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md)）：每段贡献 1（tool/sub）或自身
  // 文本长度（text 与 thinking 一视同仁——thinking delta 合并进尾段而不增长 timeline，
  // 只数 timeline 长度会让纯思考流式期间跟随停摆）。
  const sig = stream
    ? `${stream.timeline.reduce((n, s) => n + (s.kind === "tool" || s.kind === "sub" ? 1 : s.text.length), 0)}:${Object.keys(stream.toolsMap).length}`
    : "";
  const scrollToBottom = useCallback(() => {
    const el = bodyRef.current;
    if (!el) return;
    progUntil.current = Date.now() + 150;
    progTarget.current = el.scrollHeight;
    el.scrollTop = el.scrollHeight;
  }, []);
  const suspendFollow = useCallback(() => {
    // 展开/收起思考块或工具卡 = 阅读意图（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md)）：暂停跟随，
    // 避免刚展开的卡片被下一帧流式拽出视口。
    stick.current = false;
    progUntil.current = 0;
  }, []);
  // 视口钳制兜底（[docs/subdrawer-scroll-hardening](../../../../docs/subdrawer-scroll-hardening.md)）：CSS 滚动链（wrapper > section > antd body > 本滚动容器）项目侧
  // 已全部钉死，但「抽屉随内容长高、什么都不滚」已复发两次
  // （[docs/rightbar-visual-batch](../../../../docs/rightbar-visual-batch.md) 的 .rb-tabs，然后 e766796）。此钳制不信任任何祖先：以视口减头部为界约束滚动容器。
  // CSS 链健康时该值等于 flex 后高度（无操作）；上游任一层运行时失灵时它单独恢复滚动。
  const clampToViewport = useCallback(() => {
    const el = bodyRef.current;
    if (!el) return;
    const header = el
      .closest(".ant-drawer-section")
      ?.querySelector<HTMLElement>(":scope > .ant-drawer-header");
    const cap = Math.max(120, window.innerHeight - (header?.offsetHeight ?? 0));
    const before = el.getBoundingClientRect().height;
    el.style.maxHeight = `${cap}px`;
    // 若 flex 链一直在约束滚动容器，其布局高度已等于 cap；内容溢出时钳制前反而更高
    // 说明链没有约束住——已自愈，但大声说出来。
    if (import.meta.env.DEV && before > cap + 2 && el.scrollHeight > cap) {
      console.warn(
        `[sub-drawer-probe] viewport clamp engaged (${Math.round(before)}px -> ${cap}px): CSS scroll chain is not bounding the scroller — inspect app.css drawer rules / antd DOM drift`,
      );
    }
  }, []);
  // 面板内容比 open 翻转晚一拍挂载（rc-drawer CSSMotion），等滚动容器真正出现时 open/sig 两个
  // effect 早已跑过且不会重跑。在此接住挂载：元素确实入文档后武装钳制并补发首次跟随
  // （实测：没有这一步，新开的抽屉停在 scrollTop 0，钳制也永不武装）。
  const setBodyRef = useCallback(
    (el: HTMLDivElement | null) => {
      bodyRef.current = el;
      if (!el) return;
      requestAnimationFrame(() => {
        clampToViewport();
        if (stick.current) scrollToBottom();
      });
    },
    [clampToViewport, scrollToBottom],
  );
  useEffect(() => {
    if (prevSubId.current !== subId) {
      // 切换子代理重置跟随（[docs/thinking-scroll-fix](../../../../docs/thinking-scroll-fix.md) §2.4 等价）：上一个子代理留下的暂停钉住
      // 不能让新子代理的视图滞留在离底部的半路。
      prevSubId.current = subId;
      stick.current = true;
      progUntil.current = 0;
      setTaskCollapsed(false);
    }
    clampToViewport(); // 内容挂载后再武装一次（打开那一次可能拿到 null ref）
    if (!stick.current) return;
    scrollToBottom();
  }, [sig, open, subId, scrollToBottom, clampToViewport]);
  useEffect(() => {
    if (!running) return;
    const id = setInterval(() => setTick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [running]);
  useEffect(() => {
    if (!open) return;
    // 首次运行可能早于面板 DOM（antd 在 open 动效后才挂载内容），bodyRef 仍为 null；
    // 下面的 sig effect 会在内容真正入文档后再调一次 clamp。
    clampToViewport();
    window.addEventListener("resize", clampToViewport);
    return () => window.removeEventListener("resize", clampToViewport);
  }, [open, subId, clampToViewport]);

  // 仅 dev 的运行时探针：内容视觉上比滚动容器高但滚动容器报告无溢出，说明某个祖先在裁切——
  // 每次打开转储一次逐层计算指标，让坏层可以从控制台直接辨认，而不是第三次盲改 CSS。
  const probed = useRef(false);
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    if (!open) {
      probed.current = false;
      return;
    }
    if (probed.current) return;
    const el = bodyRef.current;
    if (!el || el.scrollHeight <= el.clientHeight) return;
    // 滚动容器底缘超出视口才是「内容被裁切」的真判据；单看 scrollHeight 可能仍小于
    // innerHeight 而盒子本身已悬出视口（上方约束已坏）。
    if (el.getBoundingClientRect().bottom <= window.innerHeight + 1) return;
    probed.current = true;
    const dump = (node: Element | null, name: string) => {
      if (!node) return `[${name}] missing`;
      const cs = getComputedStyle(node);
      return `[${name}] display=${cs.display} height=${cs.height} overflow=${cs.overflow}/${cs.overflowY} client=${(node as HTMLElement).clientHeight} scroll=${(node as HTMLElement).scrollHeight}`;
    };
    const section = el.closest(".ant-drawer-section");
    console.warn(
      "[sub-drawer-probe] scroller reports no overflow while content exceeds the viewport — scroll chain is broken:\n" +
        [dump(section?.parentElement ?? null, "content-wrapper"), dump(section, "section"), dump(section?.querySelector(":scope > .ant-drawer-body") ?? null, "ant-drawer-body"), dump(el, "sub-drawer-body")].join("\n"),
    );
  }, [open, sig]);

  const role = sub?.name || sub?.role || "";
  const statusNode = !sub ? null : running ? (
    <Tag variant="filled" className="sub-drawer-status st-running">{t("subagent.statusRunning")}</Tag>
  ) : sub.status === "error" ? (
    <Tag variant="filled" className="sub-drawer-status st-error">{t("subagent.statusError")}</Tag>
  ) : (
    <Tag variant="filled" className="sub-drawer-status st-done">{t("subagent.statusDone")}</Tag>
  );

  return (
    <Drawer
      open={open && !!sub}
      onClose={() => useRun.getState().closeSubDrawer()}
      placement="right"
      size={width}
      mask={false}
      destroyOnHidden={false}
      closable={false}
      className="sub-drawer"
      rootClassName="sub-drawer-root"
      styles={{ body: { padding: 0 } }}
      title={
        <div className="sub-drawer-head">
          {/* 需求：关闭按钮在左上角（antd 默认右上；自绘头部统一） */}
          <Tooltip title={t("subagent.close")}>
            <Button
              type="text"
              size="small"
              className="sub-drawer-close"
              aria-label={t("subagent.close")}
              icon={<CloseOutlined />}
              onClick={() => useRun.getState().closeSubDrawer()}
            />
          </Tooltip>
          <RobotOutlined className="sub-drawer-robot" />
          <span className="sub-drawer-title" title={sub?.description}>
            {t("subagent.label")}
            {role && <span className="sub-drawer-role">{role}</span>}
            {sub?.description && <span className="sub-drawer-desc">· {sub.description}</span>}
          </span>
          {statusNode}
        </div>
      }
    >
      <div
        ref={setBodyRef}
        className="sub-drawer-body"
        onWheel={(e) => {
          if (e.deltaY < 0) {
            stick.current = false;
            progUntil.current = 0; // 本手势的滚动事件归用户所有：重新进入贴底判定（docs/thinking-scroll-fix）
          }
        }}
        onScroll={() => {
          const el = bodyRef.current;
          if (!el) return;
          if (Date.now() < progUntil.current && Math.abs(el.scrollTop - progTarget.current) < 40) return; // 自家跳转，豁免
          progUntil.current = 0;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {sub?.task && (
          <div className="msg user sub-drawer-task">
            <div
              className={`role sub-drawer-task-toggle${taskCollapsed ? "" : " open"}`}
              onClick={() => setTaskCollapsed((v) => !v)}
              role="button"
              aria-expanded={!taskCollapsed}
              title={taskCollapsed ? sub.task : undefined}
            >
              <span>{t("subagent.taskLabel")}</span>
              <CaretRightOutlined />
            </div>
            {taskCollapsed ? (
              <div
                className="bubble user-bubble sub-drawer-task-preview"
                onClick={() => setTaskCollapsed(false)}
              >
                {sub.task}
              </div>
            ) : (
              <div className="bubble user-bubble">{sub.task}</div>
            )}
          </div>
        )}
        {stream && (stream.timeline.length > 0 || stream.status === "running") ? (
          <div className="msg assistant sub-drawer-stream">
            <TimelineSegsView
              timeline={stream.timeline}
              toolsMap={stream.toolsMap}
              streaming={stream.status === "running"}
              onUserToggle={suspendFollow}
              // 过程流是模型原始输出的直通，后端只在事件通道剥离过 <report>，
              // 落盘/实时两条路径的正文都还带着标记 —— 在此渲染时剥离。
              // 归档分支（sub.report）保持不动：它本来就是后端剥离过的干净文本。
              stripReport
            />
            {/* 等待指示（[docs/chat-loading-indicator]）：与聊天窗口同款（antd 加载图标 + 同一语义化类名），
                位置仍在过程流的末尾，运行状态判定不变 */}
            {stream.status === "running" && (
              <span className="ws-streaming-indicator">
                <LoadingOutlined spin />
              </span>
            )}
          </div>
        ) : sub?.report ? (
          <>
            <div className="msg assistant">
              <div className="role">
                <span>{t("subagent.reportLabel")}</span>
              </div>
              <div className="md" dangerouslySetInnerHTML={{ __html: renderCached(sub.report) }} />
            </div>
            {!stream?.loaded && <div className="dim sub-drawer-note">{t("subagent.noProcess")}</div>}
          </>
        ) : (
          <div className="dim sub-drawer-note">{t("subagent.noProcess")}</div>
        )}
      </div>
    </Drawer>
  );
}
