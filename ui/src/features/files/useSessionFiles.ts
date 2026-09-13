// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：会话产物列表数据源——后端登记边车是唯一事实源；
// writeTick（create/edit 成功信号）触发防抖刷新，页签未激活时计数也能保持新鲜
import { useCallback, useEffect, useRef, useState } from "react";
import { ipc } from "../../ipc/client";
import type { SessionFileEntry } from "../../ipc/types";
import { useActiveRun } from "../../stores/run";

/** 会话产物列表 hook：按 sessionId 拉取产物登记；writeTick 变化时 300ms 防抖刷新，
 *  内置过期响应守卫（切换会话后迟到的旧响应不得覆盖新列表）。 */
export function useSessionFiles(sessionId: string | null): {
  files: SessionFileEntry[];
  refresh: () => void;
} {
  const active = useActiveRun();
  const writeTick = active.writeTick;
  const [files, setFiles] = useState<SessionFileEntry[]>([]);
  // 过期响应守卫：切换会话后迟到的旧响应不得覆盖新列表
  const reqIdRef = useRef(0);

  const refresh = useCallback(() => {
    const my = ++reqIdRef.current;
    if (!sessionId) {
      setFiles([]);
      return;
    }
    ipc
      .listSessionFiles(sessionId)
      .then((r) => {
        if (reqIdRef.current === my) setFiles(Array.isArray(r) ? r : []);
      })
      .catch(() => {
        if (reqIdRef.current === my) setFiles([]);
      });
  }, [sessionId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!writeTick) return;
    const t = setTimeout(refresh, 300);
    return () => clearTimeout(t);
  }, [writeTick, refresh]);

  return { files, refresh };
}
