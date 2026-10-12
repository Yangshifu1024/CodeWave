import type { CleanupOutcome, ConfigSaveResult, ConfigSnapshot, ConfigState } from "../ipc/types";

/** Shared versioned IPC fixtures. Tests may omit unrelated optional config fields. */
export function configSnapshot(config: unknown, overrides: Partial<ConfigSnapshot> = {}): ConfigSnapshot {
  return { config: config as ConfigState, version: { boot_id: "test-boot", sequence: 0 },
    page_version: 0, role_versions: {}, warnings: [], ...overrides };
}

export function configSaved(config: unknown, cleanup: CleanupOutcome | null = null): ConfigSaveResult {
  return { ...configSnapshot(config, { version: { boot_id: "test-boot", sequence: 1 }, page_version: 1 }), cleanup, replayed: false };
}
