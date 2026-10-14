// [docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)：会话产物列表数据源——后端登记边车是唯一事实源；
// writeTick（create/edit 成功信号）触发防抖刷新，页签未激活时计数也能保持新鲜
import { useCallback, useEffect, useRef, useState } from "react";
import { ipc } from "../../ipc/client";
import type { SessionFileEntry } from "../../ipc/types";
import { useRun } from "../../stores/run";

/** 会话产物列表 hook：按 sessionId 拉取产物登记；writeTick 变化时 300ms 防抖刷新，
 *  内置过期响应守卫（切换会话后迟到的旧响应不得覆盖新列表）。 */
export function useSessionFiles(sessionId: string | null): {
  files: SessionFileEntry[];
  refresh: () => void;
} {
  const writeTick = useRun(state => sessionId ? state.tabs[sessionId]?.writeTick ?? 0 : 0);
  const [result, setResult] = useState<{ sessionId: string | null; files: SessionFileEntry[] }>({ sessionId: null, files: [] });
  // 过期响应守卫：切换会话后迟到的旧响应不得覆盖新列表
  const requests = useRef({ id: 0 }).current;

  const refresh = useCallback(() => {
    const my = ++requests.id;
    if (!sessionId) {
      setResult({ sessionId, files: [] });
      return;
    }
    ipc
      .listSessionFiles(sessionId)
      .then((r) => {
        if (requests.id === my) setResult({ sessionId, files: Array.isArray(r) ? r : [] });
      })
      .catch(() => {
        if (requests.id === my) setResult({ sessionId, files: [] });
      });
  }, [sessionId, requests]);

  useEffect(() => {
    refresh();
    return () => { ++requests.id; };
  }, [refresh, requests]);

  useEffect(() => {
    if (!writeTick) return;
    const t = setTimeout(refresh, 300);
    return () => clearTimeout(t);
  }, [writeTick, refresh]);

  return { files: result.sessionId === sessionId ? result.files : [], refresh };
}
