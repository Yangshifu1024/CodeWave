// 配置状态（设置中心的镜像）
import { create } from "zustand";
import { ipc } from "../ipc/client";
import type { ConfigSaveResult, ConfigSnapshot, ConfigState, ConfigVersion, CredentialEdit, FlatModel } from "../ipc/types";
import { DEFAULT_POST_WRITE_CHECK } from "../ipc/types";
import { findModel } from "../utils/models";
import { useUi } from "./ui";
import { i18n } from "../i18n";
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
  // 写入后检查默认关闭且命令留空——与后端 PostWriteCheckSettings::default() 同源
  post_write_check: DEFAULT_POST_WRITE_CHECK,
  ui: { font_size: 15, accent: "cyan", language: "zh-CN", font_sans: "", font_mono: "" },
  custom_prompt: null,
  disabled_skills: [],
  log: { level: "info", session_verbose: false },
  shell: { selection: null },
  // 会话保留期与清理：null = 不清理（与后端默认值同源，绝不把「删数据」当默认）
  sessions: { retention_days: null },
  // 内置子代理按角色模型覆盖：缺省全部跟随父会话（与后端 ConfigState.subagent_models 默认空 HashMap 同源）
  subagent_models: {},
};

export function configErrorMessage(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) return String(error.message);
  return String(error);
}

interface SaveOptions {
  skipCleanup?: boolean;
  expectedVersion?: ConfigVersion;
  credentialEdits?: Record<string, CredentialEdit>;
  operationId?: string;
}

/** Only backend receipts are authoritative. Drafts never become saved state. */
interface SettingsState {
  config: ConfigState | null;
  snapshot: ConfigSnapshot | null;
  loaded: boolean;
  rolePending: Record<string, boolean>;
  roleErrors: Record<string, string | undefined>;
  roleRetry: Record<string, string | null | undefined>;
  load(): Promise<boolean>;
  adoptSnapshot(snapshot: ConfigSnapshot): boolean;
  save(next: ConfigState, opts?: SaveOptions): Promise<ConfigSaveResult>;
  setSubagentModel(role: string, modelId: string | null): Promise<void>;
}

let loadRequest = 0;
const notifiedWarnings = new Set<string>();

// Only these backend rejections prove that this attempt did not commit. Unknown
// errors, including OperationUnknown/OperationUncertain, retain the operation ID.
const PRECOMMIT_REJECTION_CODES = new Set([
  "ConfigConflict", "ConfigUnreadable", "ConfigWriteFailed", "CredentialConflict",
  "CredentialIntentRequired", "CredentialReadFailed", "CredentialVerificationFailed",
  "CredentialWriteFailed", "InvalidConfiguration", "InvalidCredential", "InvalidCredentialIntent",
  "InvalidCredentialReference", "InvalidHeaders", "InvalidModel", "InvalidOperation", "InvalidProvider",
  "InvalidRole", "JournalUnreadable", "JournalWriteFailed", "ModelNotFound", "RecoveryQueueFull",
  "UnsafeCredentialStorage", "KeyringUnavailable",
]);

export function isDefiniteConfigRejection(error: unknown): boolean {
  return !!error && typeof error === "object" && "code" in error &&
    typeof error.code === "string" && PRECOMMIT_REJECTION_CODES.has(error.code);
}

/** A lost or uncertain response may follow a commit; reconcile before retry. */
export async function reconcileConfigOperation(operationId: string, error: unknown): Promise<ConfigSaveResult> {
  if (isDefiniteConfigRejection(error)) throw error;
  try {
    return await ipc.getConfigOperation(operationId);
  } catch {
    throw error;
  }
}

export const useSettings = create<SettingsState>((set, get) => ({
  config: null,
  snapshot: null,
  loaded: false,
  rolePending: {},
  roleErrors: {},
  roleRetry: {},
  adoptSnapshot(snapshot) {
    const current = get().snapshot;
    // Only load() can introduce a boot ID. A late save from a closed backend
    // must never replace the newly loaded generation.
    if (!current || current.version.boot_id !== snapshot.version.boot_id ||
        current.version.sequence > snapshot.version.sequence) return false;
    set({ config: snapshot.config, snapshot });
    return true;
  },
  async load() {
    const request = ++loadRequest;
    let snapshot: ConfigSnapshot;
    try {
      snapshot = await ipc.getConfig();
    } catch {
      if (request === loadRequest) set((state) => ({ config: state.config ?? DEFAULT_CONFIG, loaded: true }));
      return false;
    }
    if (request !== loadRequest) return false;
    const previous = get().snapshot;
    if (previous && previous.version.boot_id === snapshot.version.boot_id &&
        previous.version.sequence > snapshot.version.sequence) return false;
    set({ config: snapshot.config, snapshot, loaded: true,
      ...(previous?.version.boot_id !== snapshot.version.boot_id
        ? { rolePending: {}, roleErrors: {}, roleRetry: {} } : {}),
    });
    for (const warning of snapshot.warnings) {
      const key = `${snapshot.version.boot_id}:${warning}`;
      if (!notifiedWarnings.has(key)) {
        notifiedWarnings.add(key);
        useUi.getState().notify(i18n.t("settings.title"), warning);
      }
    }
    const { prefs, migrate } = reconcileFontsFromConfig({
      sans: snapshot.config.ui.font_sans,
      mono: snapshot.config.ui.font_mono,
    });
    if (migrate) {
      void ipc.setFontPrefs(prefs.sans, prefs.mono, crypto.randomUUID())
        .then((result) => { get().adoptSnapshot(result); }).catch(() => null);
    }
    return true;
  },
  async save(next, opts) {
    const snapshot = get().snapshot;
    const expectedVersion = opts?.expectedVersion ?? (snapshot && {
      boot_id: snapshot.version.boot_id, sequence: snapshot.page_version,
    });
    if (!expectedVersion) throw new Error("Configuration must be loaded before saving.");
    const operationId = opts?.operationId ?? crypto.randomUUID();
    const result = await ipc.saveConfig(next, {
      expectedVersion, operationId, credentialEdits: opts?.credentialEdits ?? {},
      skipCleanup: opts?.skipCleanup ?? false,
    }).catch((error: unknown) => reconcileConfigOperation(operationId, error));
    get().adoptSnapshot(result);
    return result;
  },
  async setSubagentModel(role, modelId) {
    const state = get();
    if (state.rolePending[role]) return;
    const snapshot = state.snapshot;
    if (!snapshot) throw new Error("Configuration must be loaded before saving.");
    const bootId = snapshot.version.boot_id;
    set({
      rolePending: { ...state.rolePending, [role]: true },
      roleErrors: { ...state.roleErrors, [role]: undefined },
      roleRetry: { ...state.roleRetry, [role]: modelId },
    });
    const operationId = crypto.randomUUID();
    try {
      const result = await ipc.setSubagentModel(role, modelId, {
        boot_id: bootId, sequence: snapshot.role_versions[role] ?? 0,
      }, operationId).catch((error: unknown) => reconcileConfigOperation(operationId, error));
      get().adoptSnapshot(result);
      if (get().snapshot?.version.boot_id === bootId) {
        set((current) => ({ roleRetry: { ...current.roleRetry, [role]: undefined } }));
      }
    } catch (error) {
      const code = error && typeof error === "object" && "code" in error ? String(error.code) : "";
      if (/conflict|model/i.test(code)) await get().load();
      if (get().snapshot?.version.boot_id === bootId) {
        set((current) => ({ roleErrors: { ...current.roleErrors, [role]: configErrorMessage(error) } }));
      }
      throw error;
    } finally {
      if (get().snapshot?.version.boot_id === bootId) {
        set((current) => ({ rolePending: { ...current.rolePending, [role]: false } }));
      }
    }
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
