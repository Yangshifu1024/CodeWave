// 配置状态（设置中心的镜像）
import { create } from "zustand";
import { ipc } from "../ipc/client";
import type { ConfigState, FlatModel } from "../ipc/types";
import { DEFAULT_LSP_SETTINGS } from "../ipc/types";
import { findModel } from "../utils/models";
import { reconcileFontsFromConfig } from "../utils/fonts";

// IPC 不可用（如纯浏览器调试）时回退默认配置，保证 UI 仍可渲染
const DEFAULT_CONFIG: ConfigState = {
  schema_version: 2,
  providers: [],
  active_model_id: null,
  proxy: null,
  network: { allow_private_network: false },
  compact_threshold: 0.6,
  compact_timeout_seconds: 180,
  approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
  validation: {
    python: true,
    rust: true,
    typescript: true,
    go: true,
    json: true,
    // java 默认关闭（jdtls 需 JDK 21+ 与依赖树索引，首次启用需确认）；dart 默认开启——与后端 ValidationSettings::default() 同源
    java: false,
    dart: true,
    lsp: DEFAULT_LSP_SETTINGS,
  },
  ui: { font_size: 15, accent: "cyan", language: "zh-CN", font_sans: "", font_mono: "" },
  custom_prompt: null,
  disabled_skills: [],
  log: { level: "info", session_verbose: false },
  shell: { selection: null },
};

/** 设置 store 契约：加载 / 保存全局配置 */
interface SettingsState {
  config: ConfigState | null;
  /** 是否已完成首次加载 */
  loaded: boolean;
  load(): Promise<void>;
  save(next: ConfigState): Promise<void>;
}

export const useSettings = create<SettingsState>((set) => ({
  config: null,
  loaded: false,
  async load() {
    const config = await ipc.getConfig().catch(() => DEFAULT_CONFIG);
    set({ config, loaded: true });
    // 字体真源在后端配置：配置到手后与 localStorage 缓存对账（后端优先；
    // 老版本只把字体存在 localStorage 的用户，这里会把缓存值自动迁移回后端，
    // 见 utils/fonts.ts::reconcileFontsFromConfig）
    const { prefs, migrate } = reconcileFontsFromConfig({
      sans: config.ui.font_sans,
      mono: config.ui.font_mono,
    });
    if (migrate) {
      void ipc.setFontPrefs(prefs.sans, prefs.mono).catch(() => null);
    }
  },
  async save(next) {
    await ipc.saveConfig(next);
    set({ config: next });
  },
}));

/** 活跃模型显示名，未配置时回退「未配置模型」。 */
export function selectActiveModelName(s: SettingsState): string {
  return findModel(s.config, s.config?.active_model_id ?? null)?.model ?? "未配置模型";
}

/** 活跃模型的摊平视图，未配置返回 null。 */
export function selectActiveModel(s: SettingsState): FlatModel | null {
  return findModel(s.config, s.config?.active_model_id ?? null);
}

/** 是否已配置活跃模型。 */
export function selectHasModel(s: SettingsState): boolean {
  return selectActiveModel(s) != null;
}
