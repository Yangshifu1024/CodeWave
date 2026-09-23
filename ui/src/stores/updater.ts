// 应用内自动更新的 UI 状态机（与 GitWave 同款的完整升级体验）：
// 检查 → 发现新版本（含发布说明）→ 下载（进度条）→ 安装完成待重启；失败可重试，Linux deb/rpm 降级手动下载。
//
// 分工：状态归本 store，流程在 utils/updateCheck，视觉在 features/panels/UpdateModal。
// 弹窗渲染在 AppShell、流程在 util、触发点散在菜单/关于弹框——store 是三者唯一交点
// （与 ui.exitRequest / ui.closeTabRequest 同一模式）。
//
// 关键设计：phase 与 modalOpen 解耦。downloading 允许被隐藏（下载继续、后台完成），
// ready 必须能重新弹出（用户可能在下载途中关过弹窗，重启入口不能被吞掉）。
import { create } from "zustand";

/** 流程阶段。idle/up-to-date 为「无弹窗」态，error 可再重试，其余均伴随弹窗 */
export type UpdaterPhase =
  | "idle"
  | "checking"
  | "available"
  | "manual-download"
  | "downloading"
  | "ready"
  | "up-to-date"
  | "error";

/** 本 store 的对外状态与方法（流程侧只经这些方法改状态，组件侧只读字段） */
export interface UpdaterState {
  phase: UpdaterPhase;
  /** 弹窗可见性（与 phase 解耦，见文件头） */
  modalOpen: boolean;
  /** 当前安装的版本（检查到更新时由后端返回值回填，用于「当前版本 x.y.z」一行） */
  currentVersion: string | null;
  /** 可安装的新版本；error / up-to-date 时清空 */
  newVersion: string | null;
  /** 发布说明（latest.json 的 notes，markdown 源文；由 UpdateModal 经 utils/markdown 的 renderMarkdown 渲染） */
  notes: string | null;
  downloadedBytes: number;
  /** 总字节数；null = 服务端未给 content-length（进度条不显示百分比） */
  totalBytes: number | null;
  /** 失败文案（已按 i18n 本地化，含 403 配额耗尽的专用指引） */
  error: string | null;
  /** 检查代次：每次检查递增。流程用它判定「我是否仍是最新一次检查」，陈旧结果一律丢弃
   *  （启动静默检查与用户手动检查可能重叠，旧结果不得把 available 回滚成 up-to-date） */
  checkEpoch: number;
  /** 静默检查入口：只递增代次，不动 phase（启动自动检查保持不可见） */
  startCheckEpoch(): number;
  /** 显式检查入口：phase=checking + 递增代次，返回本次代次 */
  beginCheck(): number;
  /** 已是最新（显式检查才有反馈；静默检查直接返回、不动状态） */
  markUpToDate(currentVersion: string): void;
  /** 有可用更新：manual=true（Linux 非 AppImage）→ manual-download，需打开发布页手动装 */
  markAvailable(info: {
    currentVersion: string;
    newVersion: string;
    notes: string | null;
    manual: boolean;
  }): void;
  /** 进入下载：清零进度并显示弹窗 */
  beginDownload(): void;
  /** 进度上报（downloaded/total 一律原样落库，非法值兜底为 0/null） */
  setProgress(downloaded: number, total: number | null): void;
  /** 下载并安装完成，等待重启（弹窗强制打开：重启入口不能被吞掉） */
  markReady(): void;
  /** 失败：清掉版本/说明/进度，只留错误文案 */
  fail(error: string): void;
  /** 弹窗开合（下载中途隐藏、稍后再说、点 X 关闭都走这里） */
  setModalOpen(open: boolean): void;
}

const INITIAL = {
  phase: "idle" as UpdaterPhase,
  modalOpen: false,
  currentVersion: null,
  newVersion: null,
  notes: null,
  downloadedBytes: 0,
  totalBytes: null,
  error: null,
  checkEpoch: 0,
};

export const useUpdater = create<UpdaterState>((set, get) => ({
  ...INITIAL,

  startCheckEpoch() {
    const epoch = get().checkEpoch + 1;
    set({ checkEpoch: epoch });
    return epoch;
  },

  beginCheck() {
    const epoch = get().checkEpoch + 1;
    set({ checkEpoch: epoch, phase: "checking", error: null });
    return epoch;
  },

  markUpToDate(currentVersion) {
    set({ ...INITIAL, phase: "up-to-date", currentVersion, checkEpoch: get().checkEpoch });
  },

  markAvailable({ currentVersion, newVersion, notes, manual }) {
    set({
      phase: manual ? "manual-download" : "available",
      modalOpen: true,
      currentVersion,
      newVersion,
      notes: notes && notes.trim() ? notes : null,
      error: null,
      downloadedBytes: 0,
      totalBytes: null,
    });
  },

  beginDownload() {
    set({ phase: "downloading", modalOpen: true, downloadedBytes: 0, totalBytes: null, error: null });
  },

  setProgress(downloaded, total) {
    set({
      // 非法值（NaN / 负数）兜底：进度条宁可停在 0 也不能渲染出 NaN%
      downloadedBytes: Number.isFinite(downloaded) && downloaded > 0 ? downloaded : 0,
      totalBytes: total !== null && Number.isFinite(total) && total > 0 ? total : null,
    });
  },

  markReady() {
    set({ phase: "ready", modalOpen: true, error: null });
  },

  fail(error) {
    // modalOpen 置真：显式检查失败时弹窗本就没开（设置页「关于」的手动检查按钮 / macOS 菜单触发），
    // 不打开就等于把失败吞掉——原先的 toast 反馈由这个错误弹窗接管
    set({
      phase: "error",
      modalOpen: true,
      error,
      newVersion: null,
      notes: null,
      downloadedBytes: 0,
      totalBytes: null,
    });
  },

  setModalOpen(open) {
    set({ modalOpen: open });
  },
}));
