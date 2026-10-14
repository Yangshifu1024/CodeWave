import { useCallback, useEffect, useRef, useState } from "react";
import { ipc } from "../../ipc/client";
import type { GitDiffFile } from "../../ipc/types";
import { i18n } from "../../i18n";

export interface WorkspaceChanges {
  files: GitDiffFile[] | null;
  loading: boolean;
  error: string;
  truncated: boolean;
  refresh(): void;
}

/** One request source for the sidebar overview and full diff. No background polling. */
export function useWorkspaceChanges(sessionId: string | null, visible: boolean): WorkspaceChanges {
  const [state, setState] = useState<{
    sessionId: string | null; files: GitDiffFile[] | null; loading: boolean; error: string; truncated: boolean;
  }>({ sessionId: null, files: null, loading: false, error: "", truncated: false });
  const requests = useRef({ id: 0, inFlight: false }).current;

  const refresh = useCallback(() => {
    if (!visible || !sessionId || requests.inFlight) return;
    const id = ++requests.id;
    requests.inFlight = true;
    setState(previous => ({
      sessionId, files: previous.sessionId === sessionId ? previous.files : null,
      loading: true, error: "", truncated: previous.sessionId === sessionId && previous.truncated,
    }));
    void ipc.gitDiff(sessionId).then(result => {
      if (!result || !Array.isArray(result.files)) throw new Error(i18n.t("rightbar.changesInvalid"));
      if (requests.id === id) setState({ sessionId, files: result.files, loading: false, error: "", truncated: !!result.truncated });
    }).catch(error => {
      if (requests.id === id) setState(previous => ({
        ...previous, loading: false, error: String(error).replace(/^Error[:\s]*/i, ""),
      }));
    }).finally(() => {
      if (requests.id === id) requests.inFlight = false;
    });
  }, [sessionId, visible, requests]);

  useEffect(() => {
    refresh();
    return () => {
      ++requests.id;
      requests.inFlight = false;
    };
  }, [refresh, requests]);

  // Hide old-session results synchronously, before the effect or next IPC response runs.
  const current = state.sessionId === sessionId;
  return {
    files: current ? state.files : null,
    error: current ? state.error : "",
    loading: !!sessionId && visible && (!current || state.loading),
    truncated: current && state.truncated,
    refresh,
  };
}
