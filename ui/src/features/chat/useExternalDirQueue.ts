import { useCallback, useEffect, useRef, useState } from "react";
import { useRun } from "../../stores/run";
import type { DraftTarget } from "../../stores/runDrafts";
import type { DirDecision } from "./useComposerAttachments";

type Question = { dir: string; target: DraftTarget; resolve: (choice: DirDecision) => void };

/** A single visible authorization question; queued/invalidated imports always settle. */
export function useExternalDirQueue() {
  const queue = useRef<Question[]>([]);
  const [extSessionId, setExtSessionId] = useState<string | null>(null);
  const [extDir, setExtDir] = useState<string | null>(null);
  const refresh = useCallback(() => {
    const valid: Question[] = [];
    for (const question of queue.current) {
      if (useRun.getState().isAttachmentTargetCurrent(question.target)) valid.push(question);
      else question.resolve("cancel");
    }
    queue.current = valid;
    setExtDir(valid[0]?.dir ?? null);
    setExtSessionId(valid[0]?.target.sessionId ?? null);
  }, []);
  const askExternalDir = useCallback((dir: string, target: DraftTarget) => new Promise<DirDecision>((resolve) => {
    queue.current.push({ dir, target, resolve });
    refresh();
  }), [refresh]);
  const decideExternalDir = useCallback((choice: DirDecision) => {
    const question = queue.current.shift();
    question?.resolve(useRun.getState().isAttachmentTargetCurrent(question.target) ? choice : "cancel");
    refresh();
  }, [refresh]);
  useEffect(() => {
    const unsubscribe = useRun.subscribe(refresh);
    return () => {
      unsubscribe();
      for (const question of queue.current.splice(0)) question.resolve("cancel");
    };
  }, [refresh]);
  return { extDir, extSessionId, askExternalDir, decideExternalDir };
}
