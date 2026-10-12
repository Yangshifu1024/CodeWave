import type { ConfigState } from "../ipc/types";

/** Rebase a successful receipt while retaining edits made after submission.
 * Arrays are intentionally indivisible: provider/model order is meaningful.
 * This is not a conflict-resolution mechanism for another page writer. */
export function rebaseSubmittedDraft(submitted: ConfigState, current: ConfigState, saved: ConfigState): ConfigState {
  function merge(before: unknown, now: unknown, committed: unknown): unknown {
    if (JSON.stringify(before) === JSON.stringify(now)) return committed;
    if (before && now && committed && typeof before === "object" && typeof now === "object" &&
        typeof committed === "object" && !Array.isArray(before) && !Array.isArray(now) && !Array.isArray(committed)) {
      const base = before as Record<string, unknown>;
      const local = now as Record<string, unknown>;
      const remote = committed as Record<string, unknown>;
      return Object.fromEntries([...new Set([...Object.keys(local), ...Object.keys(remote)])]
        .map((key) => [key, merge(base[key], local[key], remote[key])]));
    }
    return now;
  }
  const rebased = merge(submitted, current, saved) as ConfigState;
  const providers = new Map(saved.providers.map((provider) => [provider.id, provider]));
  return {
    ...rebased,
    subagent_models: saved.subagent_models,
    ui: { ...rebased.ui, font_sans: saved.ui.font_sans, font_mono: saved.ui.font_mono },
    providers: rebased.providers.map((provider) => {
      const committed = providers.get(provider.id);
      if (!committed) return provider;
      const updated = { ...provider, keys: committed.keys };
      if ("credential_ref" in committed) updated.credential_ref = committed.credential_ref;
      else delete updated.credential_ref;
      if ("credential_count" in committed) updated.credential_count = committed.credential_count;
      else delete updated.credential_count;
      return updated;
    }),
  };
}
