import { useEffect, useState } from "react";
import { Avatar, Button, Layout, Modal, Tooltip } from "antd";
import { BarChartOutlined, ClockCircleOutlined, InfoCircleOutlined, SettingOutlined, WarningOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { useRun } from "../../stores/run";
import { useActiveTab, useActiveWorkspace, useSessions } from "../../stores/sessions";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import { bindEvents } from "../../ipc/events";
import { ipc } from "../../ipc/client";
import { checkForUpdates, useStartupUpdateCheck } from "../../utils/updateCheck";
import {
  applyUiStateToStores,
  initUiStatePersistence,
  loadUiState,
  respondExitRequest,
} from "../../utils/uiState";
import ChatMessages from "../chat/ChatMessages";
import Composer from "../chat/Composer";
import SubagentDrawer from "../subagent/SubagentDrawer";
import SettingsPage from "../panels/SettingsPage";
import AboutModal from "../panels/AboutModal";
import UpdateModal from "../panels/UpdateModal";
import TaskCenterPanel from "../panels/TaskCenterPanel";
import TokenStatsModal from "../panels/TokenStatsModal";
import RightBar from "./RightBar";
import ProjectNav from "./ProjectNav";
import ResizeHandle from "./ResizeHandle";
import TopBar, { SIDER_W_CLOSED } from "./TopBar";
import { useDisplayWidths } from "./useDisplayWidths";
import {
  dragLimit,
  NAV_W_DEFAULT,
  NAV_W_MAX,
  NAV_W_MIN,
  RB_W_DEFAULT,
  RB_W_MAX,
  RB_W_MIN,
} from "../../utils/layout";
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

/** 顶部中断提示条（会话保存与恢复优化 · 批1）：活跃会话上次运行被中断（崩溃 / 正常退出前中止）时出现，
 *  带的「清除标记」入口就地更新列表项——左栏会话行已有同类徽标（ProjectNav），此处是「顶部」补充：
 *  用户不必先定位到左栏那一行才知道「上次发生了什么」。 */
function InterruptBanner() {
  const { t } = useTranslation();
  const activeKey = useSessions((s) => s.activeKey);
  const meta = useSessions((s) => s.sessions.find((m) => m.id === s.activeKey) ?? null);
  const [busy, setBusy] = useState(false);
  const interrupted = meta?.interrupted ?? null;
  if (!activeKey || !interrupted) return null;
  const reason = interrupted.kind === "crash" ? t("nav.interruptedCrash") : t("nav.interruptedQuit");
  const clear = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await ipc.clearSessionInterrupt(activeKey);
      // 就地更新列表项（左栏徽标同步消失）；不为一次点击重拉整份会话列表
      useSessions.setState((s) => ({
        sessions: s.sessions.map((m) => (m.id === activeKey ? { ...m, interrupted: null } : m)),
      }));
      useUi.getState().toast(t("nav.interruptCleared"));
    } catch (e) {
      // 失败保留标记、不静默（与后端 E9 约定一致：写失败必须可见）
      useUi.getState().toast(t("nav.clearInterruptFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="interrupt-banner" data-kind={interrupted.kind}>
      <WarningOutlined style={{ color: "var(--ws-warn)", flex: "none" }} />
      <span className="interrupt-banner-text">{reason}</span>
      <Button size="small" type="text" loading={busy} onClick={() => void clear()}>
        {t("nav.clearInterrupt")}
      </Button>
    </div>
  );
}

/** 关 Tab 二次确认（会话保存与恢复优化 · 批1）：有草稿/未发队列时 store 不再直接关 Tab，
 *  而是置 `closeTabRequest`；Cmd+W / 顶栏 / 左栏三个入口共用这一处弹窗。 */
function CloseTabConfirm() {
  const { t } = useTranslation();
  const request = useUi((s) => s.closeTabRequest);
  const exists = useSessions((s) => (request ? s.tabs.some((x) => x.key === request) : false));
  // 防御：目标 Tab 已不存在（会话被删除、项目级联删除等）→ 直接清状态，不弹幽灵框
  useEffect(() => {
    if (request && !exists) useUi.getState().setCloseTabRequest(null);
  }, [request, exists]);
  const answer = (choice: "discard" | "keep" | null) => {
    if (!request) return;
    if (choice) useSessions.getState().resolveCloseTab(request, choice);
    else useUi.getState().setCloseTabRequest(null);
  };
  return (
    <Modal
      open={!!request && exists}
      title={t("closeTab.title")}
      closable={false}
      // antd 6 废弃 maskClosable，等价写法是 mask.closable；与 ExitConfirm 同口径：
      // 两个弹窗都不得被遮罩 / Esc 绕过（Esc 关闭不丢数据，但口径必须自洽）
      mask={{ closable: false }}
      keyboard={false}
      onCancel={() => answer(null)}
      footer={[
        <Button key="cancel" onClick={() => answer(null)}>
          {t("closeTab.cancel")}
        </Button>,
        <Button key="keep" onClick={() => answer("keep")}>
          {t("closeTab.keep")}
        </Button>,
        <Button key="discard" danger onClick={() => answer("discard")}>
          {t("closeTab.discard")}
        </Button>,
      ]}
    >
      <div>{t("closeTab.desc")}</div>
    </Modal>
  );
}

/** 退出拦截（会话保存与恢复优化 · 批1）：后端在 ExitRequested 下不可退、下发在跑会话后等应答；
 *  三个选项都必须走 `respondExitRequest`（先 flushNow 再回后端），直接调 ipc 会丢现场态。
 *  不得用 Esc / 遮罩绕过：一旦弹窗被意外关掉而后端仍在等，应用会卡在「退不出去」。 */
function ExitConfirm() {
  const { t } = useTranslation();
  const request = useUi((s) => s.exitRequest);
  // 设置页有未保存改动时，设置侧的三选弹框先接管（同一份文案/行为，第四条路径；处置完 save/discard 后
  // 脏标记清零，本弹框自然接管运行中会话的确认）——[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md)
  const settingsDirty = useUi((s) => s.settingsDirty);
  const settingsOpen = useUi((s) => s.settingsOpen);
  const answer = (action: "wait" | "abort" | "cancel") => {
    void respondExitRequest(action).finally(() => {
      // 应答完成（或通道已关而失败）后再清 store：respondExitRequest 只回后端、不动 store，
      // 不清会留幽灵弹窗；过早清又让重试/改选无处可点
      useUi.setState({ exitRequest: null });
    });
  };
  return (
    <Modal
      open={!!request && !(settingsOpen && settingsDirty)}
      title={t("exitApp.title")}
      closable={false}
      // antd 6 废弃了 maskClosable，等价写法是 mask.closable；两个弹窗都不得被遮罩/Esc 绕过
      mask={{ closable: false }}
      keyboard={false}
      footer={[
        <Button key="cancel" onClick={() => answer("cancel")}>
          {t("exitApp.cancel")}
        </Button>,
        <Button key="abort" danger onClick={() => answer("abort")}>
          {t("exitApp.abort")}
        </Button>,
        <Button key="wait" type="primary" onClick={() => answer("wait")}>
          {t("exitApp.wait")}
        </Button>,
      ]}
    >
      <div>{t("exitApp.desc", { n: request?.running.length ?? 0 })}</div>
      <ul style={{ margin: "8px 0", paddingLeft: 20 }}>
        {(request?.running ?? []).map((id) => (
          <li key={id}>
            <code>{id}</code>
          </li>
        ))}
      </ul>
      <div className="dim" style={{ fontSize: 12 }}>
        {t("exitApp.waitHint")}
      </div>
    </Modal>
  );
}

/** 应用外壳：三栏布局（左栏导航 / 中间聊天+Composer / 右栏）+ 自绘标题栏 + 全局弹窗与通知堆栈；
 *  负责挂载初始化（配置/会话加载 + 事件绑定）、系统通知回跳、macOS 菜单动作、
 *  右键菜单抑制与全局快捷键（Esc 停止 / Cmd+W 关 Tab / Cmd+L 聚焦输入 / Cmd+←→ 切 Tab）。 */
export default function AppShell() {
  const { t } = useTranslation();
  // 自绘标题栏激活（[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)）：挂载即触发；mode/平台标记写入 <html data-*> 供 CSS 分支
  useTitlebarActivation();
  // 启动静默检查更新（延迟 3s，可用 ws_auto_update=false 关闭；有更新才弹窗）
  useStartupUpdateCheck();
  const activeKey = useSessions((s) => s.activeKey);
  const explorerOpen = useSessions((s) => s.explorerOpen);
  // 可拖拽栏宽（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）：
  // 显示宽度按当前窗口夹取（只夹显示、不改记忆值）；折叠态宽度仍为 0。
  const { nav: navWidth, rightBar: rightBarWidth, windowWidth } = useDisplayWidths();
  const rightBarOpen = useUi((s) => s.rightBarOpen);
  const setNavWidth = useUi((s) => s.setNavWidth);
  const setRightBarWidth = useUi((s) => s.setRightBarWidth);
  // 记忆值（未夹取）与显示值分开拿：显示值 < 记忆值 说明当前窗口放不下，此时禁用拖动
  // （拖动会把夹取后的显示宽写回记忆值，静默抹掉用户原来的宽度）
  const storedNavWidth = useUi((s) => s.navWidth);
  const storedRightBarWidth = useUi((s) => s.rightBarWidth);
  const activeWorkspace = useActiveWorkspace();

  // 挂载初始化：配置/会话/项目加载 + ui-state 现场态恢复 + 事件绑定
  // （M-5：unlisten 必须在卸载时清理，防 HMR 后重复注册）
  useEffect(() => {
    let unlistens: (() => void)[] = [];
    let cancelled = false;
    void (async () => {
      // 事件绑定必须先于挂载链上任何 await（🔴2）：后端 `app:exit_requested` 是**单次下发**、不重放
      // （host/commands/ui_state.rs 的 handle_exit_requested 只在 ExitRequested 那一刻 emit），
      // 而下面几步（load 配置 / refresh 会话与项目 / 读 ui-state 落 store）每一步都是 await，
      // 期间到达的退出请求在订阅到位前会永久丢失；叠加后端「有 run 在跑就等前端决定」，
      // 表现为应用退不出去。绑定提到最前，等于把这个失联窗口从「挂载 → hydrate 完成」
      // 压缩到「挂载 → 首个 listen 注册完成」。
      const fns = await bindEvents(useRun.getState().bindGlobalHandlers());
      if (cancelled) {
        // 绑定期间已被卸载：立即解绑并退出，不再继续后续初始化
        for (const fn of fns) fn();
        return;
      }
      unlistens = fns;
      await useSettings.getState().load();
      // 顺序要点：restoreTabs 要用 listSessions / listProjects 的结果校验引用（已删的会话 /
      // 已删项目下的会话一律静默剔除），所以两份列表必须先到位——否则恢复会被全量剔除、
      // 表现为「重启后 Tab 全没了」（本批最容易踩的坑）
      await Promise.all([
        useSessions.getState().refresh(),
        useSessions.getState().loadProjects(),
      ]);
      // ui-state 现场态：读盘 → 落到各 store（Tab 骨架/树态/未读/草稿/面板态）→ 急切加载活跃 Tab
      // 读盘失败或结构损坏一律降级为「无快照启动」，不阻断启动（loadUiState 内部已兜底，
      // 这里只防意外；后端也已在读取时把损坏文件备份为 .corrupt/.v<N>.bak）
      let restoredKey: string | null = null;
      try {
        await loadUiState();
        restoredKey = applyUiStateToStores().activeKey;
      } catch (e) {
        console.warn("ui-state 恢复失败，按无快照启动", e);
      }
      // 落盘订阅独立于恢复成败：即便恢复失败，本次会话的现场态也要能落盘
      initUiStatePersistence();
      // 只急切加载活跃 Tab；其余保持骨架（loaded=false），首次激活时由 sessions.activate 惰性加载
      if (restoredKey) useSessions.getState().activate(restoredKey);
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
  // （AboutModal / SettingsPage / 更新检查弹窗）。检查更新走共享流程
  // utils/updateCheck（check → 弹窗展示发布说明 → 下载安装 → 重启；Windows/Linux 从「关于」弹框触发）。
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
        // 设置页打开时不在此处置：Esc 归设置页（先关页内浮层，否则走「返回工作区」拦截），
        // 事件目标可能是 body 而不在 .settings-shell 内，光靠下面的 closest 白名单会漏掉
        // （[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md)）。
        // 任何情况下都不得因设置页而停止运行中会话。
        if (useUi.getState().settingsOpen) return;
        // 弹窗/抽屉/浮层/输入框内的 Esc 交给组件库；勿误停运行（.settings-shell 同列：全屏页里也归自己处置）
        const target = e.target as HTMLElement | null;
        if (target?.closest(".settings-shell, .ant-modal, .ant-drawer, .ant-popover, .ant-select-dropdown, .ant-input, textarea, input")) return;
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

      <Layout style={{ height: "calc(100% - var(--ws-titlebar-h))", position: "relative" }}>
        {/* Sider 外壳两态恒保留：antd 以真实 Sider 子组件判定 has-sider 水平布局，
            换成普通 div 会让内层 Layout 翻成垂直排布、内容区被压成 0（docs/sidebar-toggle-buttons 缺陷修复）。
            docs/sidebar-collapse-animation-and-titlebar-blend 窄轨退役：折叠宽 0 = 完全隐藏——antd 0.2s 缓动宽度，
            -zero-width 修饰类裁切子内容，内容恒挂载、折叠动画平滑 */}
        <Sider
          className={settingsOpen ? "workspace-covered" : undefined}
          aria-hidden={settingsOpen || undefined}
          width={explorerOpen ? navWidth : SIDER_W_CLOSED}
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
        <Content
          className={settingsOpen ? "workspace-covered" : undefined}
          aria-hidden={settingsOpen || undefined}
          style={{ height: "100%", display: "flex", background: "var(--ws-bg-main)" }}
        >
          <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
            <InterruptBanner />
            <ChatMessages />
            {/* 无活跃会话时隐藏（不占布局）；空态引导接管中栏 */}
            {activeKey && <Composer />}
          </div>
          <RightBar />
        </Content>

        {/* 栏宽分隔条（绝对定位在栏边界；折叠态不渲染 = 拖动不抢折叠入口）。
            设置页打开时 covered：z-index 比覆盖层低只挡得住指针，键盘焦点照旧能落到分隔条上
            （Tab 序残留 + ←/→ 静默改栏宽），所以同步加 .workspace-covered 与 tabIndex=-1。 */}
        {explorerOpen && (
          <ResizeHandle
            side="nav"
            width={navWidth}
            min={NAV_W_MIN}
            max={dragLimit("nav", rightBarOpen ? rightBarWidth : 0, windowWidth)}
            offset={navWidth}
            label={t("app.resizeLeft")}
            disabled={navWidth < storedNavWidth}
            covered={settingsOpen}
            onWidth={setNavWidth}
            onReset={() => setNavWidth(NAV_W_DEFAULT)}
          />
        )}
        {rightBarOpen && (
          <ResizeHandle
            side="right"
            width={rightBarWidth}
            min={RB_W_MIN}
            max={dragLimit("right", explorerOpen ? navWidth : 0, windowWidth)}
            offset={rightBarWidth}
            label={t("app.resizeRight")}
            disabled={rightBarWidth < storedRightBarWidth}
            covered={settingsOpen}
            onWidth={setRightBarWidth}
            onReset={() => setRightBarWidth(RB_W_DEFAULT)}
          />
        )}

        {/* 设置全屏页（[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md)）：覆盖式贴在内层 Layout 上。
            工作区（Sider/Content/ChatMessages/Composer/RightBar）全程挂载，只是 .workspace-covered 隐藏可见性——
            绝不用 display:none（ResizeObserver 会测到 0 尺寸、滚动容器错乱，从而影响运行中任务）。 */}
        {settingsOpen && <SettingsPage />}
      </Layout>

      {aboutOpen && <AboutModal />}
      {/* 自动更新弹窗：常驻挂载（可见性取自 store 的 modalOpen），phase 驱动标题/正文/页脚 */}
      <UpdateModal />
      {tasksOpen && <TaskCenterPanel />}
      {statsOpen && <TokenStatsModal />}
      {/* 退出拦截 / 关 Tab 二次确认：两者都挂在 store 请求位上，只在 AppShell 渲染这一处 */}
      <ExitConfirm />
      <CloseTabConfirm />
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
