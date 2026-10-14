// Right sidebar: segmented navigation, three retained panes, and a wide changes dialog.
// [docs/chat-and-interface](../../../../docs/chat-and-interface.md): info remains the default view.
// 日志 = 会话黑盒 / 全局滚动日志（[docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)）；文件 = 会话产物登记列表（create/edit 边车），点击弹窗查看
// 隐藏时保持挂载（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）：宽度经 0.2s 过渡收零（裁切式收缩）；visibility 待动画结束后才隐藏，
// 轮询 hook 均以 rightBarOpen 门控——隐藏即停轮询。顶栏切换按钮是唯一开合入口；
// rbTab 上收到 ui store 供外部切换（Composer 命令等）
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { Button, Card, ConfigProvider, Segmented, Select, Switch, Tabs } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { LogFileEntry } from "../../ipc/types";
import { useActiveTab } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import FilesPanel from "../files/FilesPanel";
import FileViewerModal from "../files/FileViewerModal";
import { useSessionFiles } from "../files/useSessionFiles";
import ChangesModal from "../workspace/ChangesModal";
import { useWorkspaceChanges } from "../workspace/useWorkspaceChanges";
import RightBarInfo from "./RightBarInfo";
import { useDisplayWidths } from "./useDisplayWidths";

const TAIL_LINES = 300;

const POLL_MS = 2000;

/// 行级着色：同时兼容会话日志 `[WARN]` 与 tracing 文本 ` WARN ` 两种前缀
function lineClass(line: string): string {
  if (line.includes("[ERROR]") || /\bERROR\b/.test(line)) return "err";
  if (line.includes("[WARN]") || /\bWARN\b/.test(line)) return "warn";
  return "";
}

/** 日志页签（[docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)）：会话视图（细粒度黑盒）/ 全局视图（tracing 滚动日志）。仅激活时轮询。 */
function LogPanel({ visible }: { visible: boolean }) {
  const { t } = useTranslation();
  const tab = useActiveTab();
  const [scope, setScope] = useState<"session" | "global">("session");
  const [auto, setAuto] = useState(true);
  const [files, setFiles] = useState<LogFileEntry[]>([]);
  const [fileName, setFileName] = useState<string | null>(null);
  const [content, setContent] = useState<string>("");
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState<string>("");
  const preRef = useRef<HTMLPreElement>(null);
  const stickRef = useRef(true);
  // 过期响应守卫：切换会话/范围后，迟到的旧响应不得覆盖新视图
  const reqIdRef = useRef(0);
  const sessionId = tab?.sessionId ?? null;
  const projectId = tab?.projectId ?? null;

  const refresh = useCallback(async () => {
    const myId = ++reqIdRef.current;
    try {
      if (scope === "session") {
        if (!sessionId) return;
        const r = await ipc.readSessionLog(sessionId, projectId, TAIL_LINES);
        if (reqIdRef.current !== myId) return;
        setError("");
        setTruncated(r.truncated);
        setContent((prev) => (prev === r.content ? prev : r.content));
      } else {
        let name = fileName;
        if (!name) {
          const list = await ipc.listLogFiles();
          if (reqIdRef.current !== myId) return;
          setFiles(list);
          name = list[0]?.name ?? null;
          setFileName(name);
          if (!name) return;
        }
        const r = await ipc.readLogFile(name, TAIL_LINES);
        if (reqIdRef.current !== myId) return;
        setError("");
        setTruncated(r.truncated);
        setContent((prev) => (prev === r.content ? prev : r.content));
      }
    } catch (e) {
      if (reqIdRef.current !== myId) return;
      setError(String(e).replace(/^Error[:\s]*/i, ""));
      setContent("");
    }
  }, [scope, sessionId, projectId, fileName]);

  // 全局日志文件列表（切到全局视图时拉取；会话视图不用）
  useEffect(() => {
    if (visible && scope === "global" && files.length === 0) {
      void ipc.listLogFiles().then(setFiles).catch(() => setFiles([]));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, scope]);

  // 手动刷新 + 自动轮询（仅页签激活时；依赖变化即重建定时器）
  useEffect(() => {
    if (!visible) return;
    void refresh();
    if (!auto) return;
    const t = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(t);
  }, [visible, auto, refresh]);

  // 内容更新时贴底（用户上滚阅读时不再强行下拽）
  useEffect(() => {
    const el = preRef.current;
    if (el && stickRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [content]);

  return (
    <div className="rb-log-panel">
      <div className="rb-log-tools">
        <Segmented
          size="small"
          value={scope}
          onChange={(v) => {
            setScope(v as "session" | "global");
            setContent("");
            setError("");
            setFileName(null);
          }}
          options={[
            { label: t("rightbar.scopeSession"), value: "session" },
            { label: t("rightbar.scopeGlobal"), value: "global" },
          ]}
        />
        <Switch size="small" checked={auto} onChange={setAuto} checkedChildren={t("rightbar.auto")} unCheckedChildren={t("rightbar.manual")} />
        <Button size="small" onClick={() => void refresh()}>{t("rightbar.refresh")}</Button>
        <Button size="small" onClick={() => void ipc.openLogsDir().catch(() => {})}>{t("rightbar.openDir")}</Button>
      </div>
      {scope === "global" && (
        <Select
          size="small"
          style={{ width: "100%", marginTop: 4 }}
          placeholder={t("rightbar.noLogFiles")}
          value={fileName}
          onChange={(v) => {
            setFileName(v);
            setContent("");
          }}
          options={files.map((f) => ({
            label: t("rightbar.logFileKb", { name: f.name, kb: Math.max(1, Math.round(f.size / 1024)) }),
            value: f.name,
          }))}
        />
      )}
      {error && <div className="rb-dim rb-log-error">{error}</div>}
      <pre
        ref={preRef}
        className="rb-log"
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 24;
        }}
      >
        {content
          ? content.split("\n").map((line, i) => (
              <div key={i} className={lineClass(line)}>{line}</div>
            ))
          : scope === "session" && !sessionId
            ? t("rightbar.noActiveSession")
            : error
              ? ""
              : t("rightbar.noLogs")}
      </pre>
      {truncated && <div className="rb-dim">{t("rightbar.tailOnly", { n: TAIL_LINES })}</div>}
    </div>
  );
}

/** Retain pane state while the changes dialog opens; hidden sidebars close their portals. */
export default function RightBar() {
  const { t } = useTranslation();
  const rightBarOpen = useUi((s) => s.rightBarOpen);
  const rbTab = useUi((s) => s.rbTab);
  const setRbTab = useUi((s) => s.setRbTab);
  // 右栏宽度 = 拖拽记忆值按窗口夹取后的显示值（与 AppShell 的分隔条同源；
  // CSS 变量必须写在这里，否则栏宽会一直停在默认 328）
  const { rightBar: rightBarWidth } = useDisplayWidths();
  const tab = useActiveTab();
  const sessionId = tab?.sessionId ?? null;
  const [lastPane, setLastPane] = useState(rbTab === "changes" ? "info" : rbTab);
  const [changesOwner, setChangesOwner] = useState(sessionId);
  const previous = useRef({ sessionId, rightBarOpen, rbTab });
  const contentTab = rbTab === "changes" ? lastPane : rbTab;
  const changesOpen = rightBarOpen && rbTab === "changes" && changesOwner === sessionId;
  useEffect(() => {
    const old = previous.current;
    previous.current = { sessionId, rightBarOpen, rbTab };
    if (rbTab === "changes" && old.rbTab !== "changes") setChangesOwner(sessionId);
    else if (rbTab === "changes" && (old.sessionId !== sessionId || (old.rightBarOpen && !rightBarOpen))) setRbTab(lastPane);
    if (rbTab !== "changes") setLastPane(rbTab);
  }, [sessionId, rightBarOpen, rbTab, lastPane, setRbTab]);
  // [docs/chat-and-interface](../../../../docs/chat-and-interface.md)：隐藏时传 null——面板关闭期间产物列表停止刷新
  const { files, refresh } = useSessionFiles(rightBarOpen ? sessionId : null);
  const changes = useWorkspaceChanges(sessionId, rightBarOpen && (rbTab === "info" || rbTab === "changes"));
  const [viewing, setViewing] = useState<{ sessionId: string | null; path: string } | null>(null);
  const viewFile = (path: string) => setViewing({ sessionId, path });
  // 查看弹窗随面板一起关闭（评审 🟢2）：其 Portal 渲染到 <body>，会逃出外壳的 visibility:hidden
  // 悬浮在隐藏面板之上；[docs/chat-and-interface](../../../../docs/chat-and-interface.md) 的隐藏即卸载曾恰好掩盖了这一点。
  useEffect(() => {
    if ((!rightBarOpen || rbTab === "changes") && viewing) setViewing(null);
  }, [rightBarOpen, rbTab, viewing]);
  // A viewer belongs to the session from which it was opened.
  useEffect(() => { setViewing(null); }, [sessionId]);

  const items = [
    { key: "info", label: t("rightbar.tabInfo"), children: <RightBarInfo visible={rightBarOpen && rbTab === "info"} files={files} changes={changes} onView={viewFile} /> },
    { key: "logs", label: t("rightbar.tabLogs"), children: <Card className="rb-panel-card"><LogPanel visible={rightBarOpen && rbTab === "logs"} /></Card> },
    { key: "files", label: files.length ? t("rightbar.tabFilesWithCount", { n: files.length }) : t("rightbar.tabFiles"), children: <Card className="rb-panel-card"><FilesPanel sessionId={sessionId} files={files} onView={viewFile} onRefresh={refresh} /></Card> },
  ];

  // [docs/chat-and-interface](../../../../docs/chat-and-interface.md)：隐藏时保持挂载——类名翻转为 .right-bar-closed（width/padding 经 0.2s 缓动收零、
  // visibility 待动画结束后隐藏）；顶栏切换按钮是唯一开合入口
  return (
    <ConfigProvider theme={{ components: { Card: { borderRadiusLG: 20, bodyPadding: 20 }, Collapse: { borderRadiusLG: 0, headerPadding: "18px 0", contentPadding: "0 0 18px" }, Segmented: { borderRadius: 12, borderRadiusSM: 10 } } }}>
    <div
      className={rightBarOpen ? "right-bar" : "right-bar right-bar-closed"}
      aria-hidden={!rightBarOpen}
      style={{ "--rb-width": `${rightBarWidth}px` } as CSSProperties}
    >
      <Tabs
        id="rightbar-views"
        size="small"
        className="rb-tabs"
        activeKey={contentTab}
        onChange={setRbTab}
        renderTabBar={() => <Segmented block className="rb-view-switcher" aria-label={t("rightbar.views")} value={contentTab} onChange={value => setRbTab(String(value))}
          options={items.map(item => ({ value: item.key, title: item.label, label: <span id={"rightbar-views-tab-" + item.key}>{item.label}</span> }))} />}
        items={items}
      />
      <ChangesModal open={changesOpen} sessionId={sessionId} changes={changes} onClose={() => setRbTab(lastPane)} />
      <FileViewerModal sessionId={sessionId} path={rightBarOpen && rbTab !== "changes" && viewing?.sessionId === sessionId ? viewing.path : null} onClose={() => setViewing(null)} />
    </div>
    </ConfigProvider>
  );
}
