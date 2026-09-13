import { useEffect, useState } from "react";
import { Avatar, Button, Layout, Tooltip } from "antd";
import { BarChartOutlined, ClockCircleOutlined, InfoCircleOutlined, SettingOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { useRun } from "../../stores/run";
import { useActiveTab, useActiveWorkspace, useSessions } from "../../stores/sessions";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import { bindEvents } from "../../ipc/events";
import { ipc } from "../../ipc/client";
import { checkForUpdates } from "../../utils/updateCheck";
import ChatMessages from "../chat/ChatMessages";
import Composer from "../chat/Composer";
import SubagentDrawer from "../subagent/SubagentDrawer";
import SettingsModal from "../panels/SettingsModal";
import AboutModal from "../panels/AboutModal";
import TaskCenterPanel from "../panels/TaskCenterPanel";
import TokenStatsModal from "../panels/TokenStatsModal";
import RightBar from "./RightBar";
import ProjectNav from "./ProjectNav";
import TopBar, { SIDER_W_CLOSED, SIDER_W_OPEN } from "./TopBar";
import { useTitlebarActivation } from "./useTitlebar";

const { Header, Sider, Content } = Layout;

type GitInfo = { name: string | null; email: string | null };

/** 系统通知点击 -> 回跳会话（应用内通知堆栈与系统 notify:activate 共用）。
 *  窗口回前台由后端 on_activated 原生完成（Windows 前台锁 + capabilities 权限门控
 *  都不拦 Rust 侧调用，前端 JS 的窗口 API 此前被静默拒绝）。
 *  无 sessionId（toast 形态）或会话已删除时无 Tab 动作。 */
function handleNotifyActivate(sessionId?: string) {
  if (!sessionId) return;
  if (useSessions.getState().revealSession(sessionId)) {
    useUi.getState().dismissBySession(sessionId);
  }
}

/** git 提交身份（只读 IPC，跟随活跃会话；失败静默降级为未配置） */
function useGitInfo(): GitInfo {
  const tab = useActiveTab();
  const sessionId = tab?.sessionId ?? null;
  const [info, setInfo] = useState<GitInfo>({ name: null, email: null });
  useEffect(() => {
    let stale = false;
    void ipc
      .gitUserInfo(sessionId)
      .then((r) => {
        if (!stale) setInfo(r ?? { name: null, email: null });
      })
      .catch(() => {
        if (!stale) setInfo({ name: null, email: null });
      });
    return () => {
      stale = true;
    };
  }, [sessionId]);
  return info;
}

/** 左侧栏底部固定区：左 = git 身份条（邮箱头像 + 名字/邮箱行），右 = 任务/统计/设置 */
function SiderFooter() {
  const { t } = useTranslation();
  const info = useGitInfo();
  const seed = info.email || info.name || "";
  const initial = seed ? [...seed][0]!.toUpperCase() : "?";
  // [docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md)：默认生成态头像黑底白字；未配置身份保留灰色「?」作视觉区分
  const bg = seed ? "#000" : "var(--ws-border)";
  return (
    <div className="sider-footer">
      <div className="git-id" title={info.email ?? info.name ?? t("git.notConfigured")}>
        <Avatar className="git-avatar" size={26} style={{ background: bg, color: "#fff", flex: "none" }}>
          {initial}
        </Avatar>
        <div className="git-id-text">
          <span className="git-id-name">{info.name ?? t("git.notConfigured")}</span>
          <span className="git-id-email">{info.email ?? ""}</span>
        </div>
      </div>
      <div className="sider-footer-ops">
        <Tooltip title={t("app.tasks")}>
          <Button type="text" size="small" icon={<ClockCircleOutlined />} aria-label={t("app.tasks")} onClick={() => useUi.setState({ tasksOpen: true })} />
        </Tooltip>
        <Tooltip title={t("app.stats")}>
          <Button type="text" size="small" icon={<BarChartOutlined />} aria-label={t("app.stats")} onClick={() => useUi.setState({ statsOpen: true })} />
        </Tooltip>
        <Tooltip title={t("app.settings")}>
          <Button type="text" size="small" icon={<SettingOutlined />} aria-label={t("app.settings")} onClick={() => useUi.getState().showSettings()} />
        </Tooltip>
        <Tooltip title={t("app.about")}>
          <Button type="text" size="small" icon={<InfoCircleOutlined />} aria-label={t("app.about")} onClick={() => useUi.setState({ aboutOpen: true })} />
        </Tooltip>
      </div>
    </div>
  );
}

/** 应用外壳：三栏布局（左栏导航 / 中间聊天+Composer / 右栏）+ 自绘标题栏 + 全局弹窗与通知堆栈；
 *  负责挂载初始化（配置/会话加载 + 事件绑定）、系统通知回跳、macOS 菜单动作、
 *  右键菜单抑制与全局快捷键（Esc 停止 / Cmd+W 关 Tab / Cmd+L 聚焦输入 / Cmd+←→ 切 Tab）。 */
export default function AppShell() {
  const { t } = useTranslation();
  // 自绘标题栏激活（[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)）：挂载即触发；mode/平台标记写入 <html data-*> 供 CSS 分支
  useTitlebarActivation();
  const activeKey = useSessions((s) => s.activeKey);
  const explorerOpen = useSessions((s) => s.explorerOpen);
  const activeWorkspace = useActiveWorkspace();

  // 挂载初始化：配置/会话/项目加载 + 事件绑定（M-5：unlisten 必须在卸载时清理，防 HMR 后重复注册）
  useEffect(() => {
    let unlistens: (() => void)[] = [];
    let cancelled = false;
    void (async () => {
      await useSettings.getState().load();
      await useSessions.getState().refresh();
      await useSessions.getState().loadProjects();
      const fns = await bindEvents(useRun.getState().bindGlobalHandlers());
      if (cancelled) {
        for (const fn of fns) fn();
      } else {
        unlistens = fns;
      }
    })();
    return () => {
      cancelled = true;
      for (const fn of unlistens) fn();
    };
  }, []);

  // 系统通知点击回跳（Windows/macOS 原生通知 -> 后端发 notify:activate）
  useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    void listen<{ session_id?: string }>("notify:activate", (e) => {
      void handleNotifyActivate(e.payload?.session_id);
    }).then((fn) => {
      if (disposed) fn();
      else un = fn;
    });
    return () => {
      disposed = true;
      un?.();
    };
  }, []);

  // macOS 应用菜单动作（macOS app-menu 批次）：后端把自定义菜单项
  // （关于 / 设置 / 检查更新）经 menu:action 路由至此；三项均打开应用内界面
  // （AboutModal / SettingsModal / 更新检查 toast）。检查更新走共享流程
  // utils/updateCheck（check → 下载安装 → 重启；Windows/Linux 从「关于」弹框触发）。
  useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    void listen<{ action: string }>("menu:action", (e) => {
      if (e.payload?.action === "menu-about") {
        useUi.setState({ aboutOpen: true });
      } else if (e.payload?.action === "menu-settings") {
        useUi.getState().showSettings();
      } else if (e.payload?.action === "menu-check-updates") {
        void checkForUpdates();
      }
    }).then((fn) => {
      if (disposed) fn();
      else un = fn;
    });
    return () => {
      disposed = true;
      un?.();
    };
  }, []);

  // 抑制 WebView 默认右键菜单：桌面应用语义，去掉刷新/检查等浏览器条目；
  // 调试不受影响（debug 构建中 devtools 快捷键仍可用）
  useEffect(() => {
    const onContextMenu = (e: MouseEvent) => e.preventDefault();
    window.addEventListener("contextmenu", onContextMenu);
    return () => window.removeEventListener("contextmenu", onContextMenu);
  }, []);

  // 全局快捷键
  useEffect(() => {
    const onKeydown = (e: KeyboardEvent) => {
      if (e.isComposing || e.defaultPrevented) return;
      const mod = e.metaKey || e.ctrlKey;
      if (e.key === "Escape") {
        // 弹窗/抽屉/浮层/输入框内的 Esc 交给组件库；勿误停运行
        const target = e.target as HTMLElement | null;
        if (target?.closest(".ant-modal, .ant-drawer, .ant-popover, .ant-select-dropdown, .ant-input, textarea, input")) return;
        const st = useRun.getState();
        const key = useSessions.getState().activeKey ?? "";
        if (st.tabs[key]?.running) void st.cancel();
      } else if (mod && e.key.toLowerCase() === "w") {
        e.preventDefault();
        const key = useSessions.getState().activeKey;
        if (key) useSessions.getState().closeTab(key);
      } else if (mod && e.key.toLowerCase() === "l") {
        // Cmd/Ctrl+L：聚焦输入框（与浏览器地址栏同习惯）；Composer 监听该事件后聚焦
        e.preventDefault();
        window.dispatchEvent(new CustomEvent("ws:focus-composer"));
      } else if (mod && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        e.preventDefault();
        useSessions.getState().cycleTab(e.key === "ArrowRight" ? 1 : -1);
      }
    };
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  }, []);

  const notifications = useUi((s) => s.notifications);
  const settingsOpen = useUi((s) => s.settingsOpen);
  const tasksOpen = useUi((s) => s.tasksOpen);
  const statsOpen = useUi((s) => s.statsOpen);
  const aboutOpen = useUi((s) => s.aboutOpen);

  return (
    <Layout style={{ height: "100%", background: "var(--ws-bg)" }}>
      <Header
        className="toolbar"
        style={{ background: "var(--ws-panel)", borderBottom: "1px solid var(--ws-border)" }}
      >
        {/* 标题栏拖拽层（docs/custom-font-and-titlebar）：铺满顶栏底部；Tauri drag.js 只认目标自身标记，
            内容以 z-1 悬于其上——空白处拖动窗口、按钮照常可用。
            两段式内容（docs/titlebar-content-batch）已自带 drag-region 标记覆盖整个顶栏，本层只兜底
            drag.js 的行为缝隙（如段间瞬态接缝），不再承担主拖拽职责。 */}
        <div className="titlebar-drag-zone" data-tauri-drag-region />
        <TopBar />
      </Header>

      <Layout style={{ height: "calc(100% - var(--ws-titlebar-h))" }}>
        {/* Sider 外壳两态恒保留：antd 以真实 Sider 子组件判定 has-sider 水平布局，
            换成普通 div 会让内层 Layout 翻成垂直排布、内容区被压成 0（docs/sidebar-toggle-buttons 缺陷修复）。
            docs/sidebar-collapse-animation-and-titlebar-blend 窄轨退役：折叠宽 0 = 完全隐藏——antd 0.2s 缓动宽度，
            -zero-width 修饰类裁切子内容，内容恒挂载、折叠动画平滑 */}
        <Sider
          width={explorerOpen ? SIDER_W_OPEN : SIDER_W_CLOSED}
          style={{
            background: "var(--ws-bg-nav)",
            borderRight: explorerOpen ? "1px solid var(--ws-border)" : "1px solid transparent",
          }}
        >
          {/* 左栏开合入口收口到标题栏 Logo（悬停同位交换箭头，docs/titlebar-logo-toggle）；侧栏内不再有开关按钮 */}
          <div className="sider-body">
            <div className="sider-nav">
              <ProjectNav />
            </div>
            <SiderFooter />
          </div>
        </Sider>
        <Content style={{ height: "100%", display: "flex", background: "var(--ws-bg-main)" }}>
          <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
            <ChatMessages />
            {/* 无活跃会话时隐藏（不占布局）；空态引导接管中栏 */}
            {activeKey && <Composer />}
          </div>
          <RightBar />
        </Content>
      </Layout>

      {settingsOpen && <SettingsModal />}
      {aboutOpen && <AboutModal />}
      {tasksOpen && <TaskCenterPanel />}
      {statsOpen && <TokenStatsModal />}
      {/* docs/subagent-interaction-drawer：子代理过程抽屉（每 Tab 态；关闭不销毁，经 Portal 渲入 body） */}
      <SubagentDrawer />

      <div className="notify-stack">
        {notifications.map((n) => (
          <Button key={n.id} onClick={() => void handleNotifyActivate(n.sessionId)} style={{ minWidth: 240, textAlign: "left", display: "block", height: "auto", padding: "8px 12px" }}>
            <b>{n.title}</b>
            <div className="dim" style={{ fontSize: 12 }}>{n.body}</div>
          </Button>
        ))}
      </div>
    </Layout>
  );
}
