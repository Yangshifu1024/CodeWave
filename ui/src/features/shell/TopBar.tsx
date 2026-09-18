// 顶栏内容（[docs/titlebar-content-batch](../../../../docs/titlebar-content-batch.md) 两段式标题栏；[docs/titlebar-logo-right-segment](../../../../docs/titlebar-logo-right-segment.md)：Logo 左栏开关位于右段段首、标题之前）。
// 左段与左侧栏同色同宽（280 展开 / 0 折叠跟随 explorerOpen，[docs/sidebar-collapse-animation-and-titlebar-blend](../../../../docs/sidebar-collapse-animation-and-titlebar-blend.md) 窄轨退役；
// 0.2s 过渡与 Sider 同步），是纯背景带；折叠时完全消失——其背景融入内容色、
// macOS 红绿灯让位迁移至右段段首（.tb-left-closed / .tb-main-cleared，见 app.css）；
// Logo 悬停时图标淡出、折叠/展开箭头淡入（[docs/titlebar-logo-toggle](../../../../docs/titlebar-logo-toggle.md) 同位交换；[docs/titlebar-content-batch](../../../../docs/titlebar-content-batch.md) 的 ←/→ 会话历史导航已移除）。
// 右段承载 Logo 开关 / 会话标题 / 工作目录胶囊（Git worktree basename）/ 分支胶囊（只读）/ 右栏开关。
// 平台差异复用 [docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md) 让位机制：macOS 红绿灯 Overlay 展开时落在左段（CSS 让位），
// Windows 控制条由右段 padding-right 清出，原生回退全部撤销（见 app.css）。
// 拖拽：drag.js 只认目标自身的 data-tauri-drag-region——两段容器与中部弹性区
// 均带标记（空白处拖动窗口），按钮/胶囊以自身为目标、不受影响。
import { Button, Tooltip } from "antd";
import {
  BranchesOutlined,
  FolderOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import storeLogo from "../../assets/store-logo.png";
import { baseName } from "../../utils/path";
import { NAV_W_DEFAULT } from "../../utils/layout";
import { useRun } from "../../stores/run";
import { useActiveTab, useSessions } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import { useDisplayWidths } from "./useDisplayWidths";

/**
 * 左栏默认宽（未拖拽过时）与折叠宽。
 * 左段宽度现在跟随用户拖出的宽度（与 AppShell 的 Sider 读同一份显示宽度，保证分隔线连续；
 * [docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）；
 * 折叠 0 = 与 Sider 一起完全隐藏（[docs/sidebar-collapse-animation-and-titlebar-blend](../../../../docs/sidebar-collapse-animation-and-titlebar-blend.md)，窄轨退役）：
 * 段收为零宽、背景融入内容色，红绿灯让位经 .tb-main-cleared 迁至右段段首（app.css）。
 */
export const SIDER_W_OPEN = NAV_W_DEFAULT;
export const SIDER_W_CLOSED = 0;

/** 自绘标题栏顶栏：左段纯背景带（与左栏同色同宽）+ 右段 Logo 开关/标题/胶囊/右栏开关；
 *  空白区域拖动窗口，按钮与胶囊各自响应点击。 */
export default function TopBar() {
  const { t } = useTranslation();
  const tab = useActiveTab();
  const explorerOpen = useSessions((s) => s.explorerOpen);
  const rightBarOpen = useUi((s) => s.rightBarOpen);
  // 左段宽度 = 左栏当前显示宽度（与 AppShell 的 Sider 同源，分隔线不断开）
  const { nav: navWidth } = useDisplayWidths();
  const activeKey = useSessions((s) => s.activeKey);
  // gitEntries 上的窄选择器：流式 delta 更新不会改变该引用，避免顶栏逐帧重渲染
  const git = useRun((s) => s.tabs[activeKey ?? ""]?.gitEntries ?? null);

  return (
    <>
      {/* 左段：纯背景带——与左栏同色同宽（宽度随 explorerOpen 内联，0.2s 过渡与 Sider 同步）；
          Logo 开关位于右段段首（docs/titlebar-logo-right-segment） */}
      <div
        className={explorerOpen ? "tb-left-seg" : "tb-left-seg tb-left-closed"}
        data-tauri-drag-region
        style={{ width: explorerOpen ? `${navWidth}px` : `${SIDER_W_CLOSED}px` }}
      />

      {/* 右段：Logo 开关（左栏入口，docs/titlebar-logo-right-segment）+ 标题簇 + flex 中部（拖动窗口）+ 右簇；尾部 padding 为 Windows 控制条让位 */}
      <div
        className={explorerOpen ? "tb-main-seg" : "tb-main-seg tb-main-cleared"}
        data-tauri-drag-region
      >
        <Tooltip title={explorerOpen ? t("app.collapseLeft") : t("app.expandLeft")}>
          <button
            type="button"
            className="tb-logo-toggle"
            aria-label={explorerOpen ? t("app.collapseLeft") : t("app.expandLeft")}
            onClick={() => useSessions.getState().setExplorerOpen(!explorerOpen)}
          >
            <img className="tb-logo" src={storeLogo} alt="CodeWave" draggable={false} />
            {explorerOpen ? (
              <MenuFoldOutlined className="tb-logo-swap" />
            ) : (
              <MenuUnfoldOutlined className="tb-logo-swap" />
            )}
          </button>
        </Tooltip>
        {tab ? (
          <>
            <span className="tb-title" title={tab.title}>
              {tab.title}
            </span>
            <span className="tb-pill" title={tab.workspace}>
              <FolderOutlined className="tb-pill-icon" />
              <span className="tb-pill-text">
                {tab.projectId ? baseName(tab.workspace) : t("titlebar.tempSession")}
              </span>
            </span>
            {git?.repo && !!git.branch && (
              <span className="tb-pill" title={git.branch}>
                <BranchesOutlined className="tb-pill-icon" />
                <span className="tb-pill-text">{git.branch}</span>
              </span>
            )}
          </>
        ) : (
          <span className="tb-title tb-title-empty">CodeWave</span>
        )}
        <div className="tb-flex" data-tauri-drag-region />
        <Tooltip title={rightBarOpen ? t("app.collapseRight") : t("app.expandRight")}>
          <Button
            type="text"
            size="small"
            /* 图标随状态切换（与左栏 Logo 开关同约定）：
               展开 → MenuFold（收起）；折叠 → MenuUnfold（展开）。此前固定 PicRightOutlined 不随状态变化（缺陷修复） */
            icon={rightBarOpen ? <MenuFoldOutlined /> : <MenuUnfoldOutlined />}
            aria-label={rightBarOpen ? t("app.collapseRight") : t("app.expandRight")}
            onClick={() => useUi.getState().setRightBarOpen(!rightBarOpen)}
          />
        </Tooltip>
      </div>
    </>
  );
}
