import { ipc } from "../../ipc/client";
import type { ActivitySession, ActivityWaiting } from "../../ipc/types";
import { useSessions } from "../../stores/sessions";
import { useRun } from "../../stores/run";
import { useActivity } from "../../stores/activity";
import { watchAskClosed } from "../../stores/runHandlers";

/** 先完成恢复，再校验当前 run/ask；只回填原 AskPanel，永不提交或切档。 */
export async function openActivityWaiting(session: ActivitySession, waiting: ActivityWaiting): Promise<boolean> {
  await useSessions.getState().openSessionReady(session.session_id);
  const before = useRun.getState().tabs[session.session_id];
  const epoch = before?.runEpoch;
  let closed = false;
  const stop = watchAskClosed(session.session_id, waiting.ask_id, () => { closed = true; });
  try {
    const payload = await ipc.getActivityAsk(session.session_id, session.run_id, waiting.ask_id);
    // IPC 完成时用户可能切走或关 Tab；不把恢复内容塞进其他会话。
    if (!useSessions.getState().tabs.some((t) => t.sessionId === session.session_id)) return false;
    const current = useRun.getState().tabs[session.session_id];
    if (closed || !current?.running || current.runId !== session.run_id || current.runEpoch !== epoch) return false;
    if (!payload || payload.session !== session.session_id || payload.ask_id !== waiting.ask_id) {
      const old = useRun.getState().tabs[session.session_id]?.ask;
      if (old?.askId === waiting.ask_id) {
        useRun.getState().bindGlobalHandlers()["ask:closed"]({ session: session.session_id });
      }
      void useActivity.getState().refresh();
      return false;
    }
    useRun.getState().restoreAsk(payload);
    return true;
  } finally { stop(); }
}
