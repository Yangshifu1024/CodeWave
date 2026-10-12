import type { ComposerDraft, PendingImage } from "./run.types";
import { addRefs, mergeRefs } from "../features/chat/composerRefs";
import { mergeDraftImages, type AppendResult } from "./attachmentPolicy";

export interface DraftTarget { sessionId: string; instanceId: string; attachmentEpoch: number }
interface DraftIdentity { instanceId: string; attachmentEpoch: number; textRevision: number; refsRevision: number; imports: string[] }
export interface SendSnapshot extends DraftTarget {
  sendId: string; textRevision: number; refsRevision: number; imageIds: string[];
  payload: { text: string; images: { mime: string; data: string }[] }; pendingImports: boolean;
}
export interface DraftState {
  drafts: Record<string, ComposerDraft>;
  draftIdentities: Record<string, DraftIdentity>;
  draftSends: Record<string, SendSnapshot>;
}
export interface DraftActions {
  captureAttachmentTarget(sessionId: string): DraftTarget | null;
  isAttachmentTargetCurrent(target: DraftTarget): boolean;
  appendDraftImages(target: DraftTarget, items: PendingImage[]): AppendResult;
  appendDraftRefs(target: DraftTarget, refs: string[]): boolean;
  startAttachmentImport(target: DraftTarget): string | null;
  finishAttachmentImport(target: DraftTarget, importId: string): void;
  removeDraftImage(id: string, sessionId: string): void;
  beginSend(sessionId: string): SendSnapshot | null;
  completeSend(sendId: string, accepted: boolean): { textConsumed: boolean; target: DraftTarget | null };
}
export function newDraftIdentity(): DraftIdentity {
  return { instanceId: crypto.randomUUID(), attachmentEpoch: 0, textRevision: 0, refsRevision: 0, imports: [] };
}
export function ensureDraftIdentity(state: DraftState, key: string) {
  return state.draftIdentities[key] ?? (state.draftIdentities[key] = newDraftIdentity());
}
function matches(state: DraftState, target: DraftTarget) {
  const identity = state.draftIdentities[target.sessionId];
  return identity?.instanceId === target.instanceId && identity.attachmentEpoch === target.attachmentEpoch;
}
export function createDraftActions(
  set: (fn: (state: DraftState) => void) => void,
  get: () => DraftState,
  isOpen: (key: string) => boolean,
): DraftActions {
  return {
    captureAttachmentTarget(sessionId) {
      if (!isOpen(sessionId)) return null;
      set((state) => { ensureDraftIdentity(state, sessionId); });
      const identity = get().draftIdentities[sessionId]!;
      return { sessionId, instanceId: identity.instanceId, attachmentEpoch: identity.attachmentEpoch };
    },
    isAttachmentTargetCurrent: (target) => matches(get(), target),
    appendDraftImages(target, items) {
      let result: AppendResult = { status: "stale" };
      set((state) => {
        if (!matches(state, target)) return;
        const draft = state.drafts[target.sessionId] ?? (state.drafts[target.sessionId] = { text: "", images: [], refs: [] });
        const merged = mergeDraftImages(draft.images, items);
        draft.images = merged.images;
        result = merged.result;
      });
      return result;
    },
    appendDraftRefs(target, refs) {
      let appended = false;
      set((state) => {
        if (!matches(state, target)) return;
        const draft = state.drafts[target.sessionId] ?? (state.drafts[target.sessionId] = { text: "", images: [], refs: [] });
        draft.refs = addRefs(draft.refs ?? [], refs);
        state.draftIdentities[target.sessionId]!.refsRevision++;
        appended = true;
      });
      return appended;
    },
    startAttachmentImport(target) {
      const id = crypto.randomUUID();
      let started = false;
      set((state) => {
        if (!matches(state, target)) return;
        state.draftIdentities[target.sessionId]!.imports.push(id);
        started = true;
      });
      return started ? id : null;
    },
    finishAttachmentImport(target, importId) {
      set((state) => {
        if (!matches(state, target)) return;
        const identity = state.draftIdentities[target.sessionId]!;
        identity.imports = identity.imports.filter((id) => id !== importId);
      });
    },
    removeDraftImage(id, sessionId) {
      set((state) => {
        const draft = state.drafts[sessionId];
        if (draft) draft.images = draft.images.filter((image) => image.id !== id);
      });
    },
    beginSend(sessionId) {
      if (!isOpen(sessionId)) return null;
      let snapshot: SendSnapshot | null = null;
      set((state) => {
        const draft = state.drafts[sessionId];
        if (!draft || !mergeRefs(draft.text, draft.refs ?? [])) return;
        const identity = ensureDraftIdentity(state, sessionId);
        // One admission per draft instance; repeated Enter must not duplicate a pending send.
        if (Object.values(state.draftSends).some((send) => send.instanceId === identity.instanceId)) return;
        snapshot = {
          sessionId, instanceId: identity.instanceId, attachmentEpoch: identity.attachmentEpoch,
          sendId: crypto.randomUUID(), textRevision: identity.textRevision, refsRevision: identity.refsRevision,
          imageIds: draft.images.map((image) => image.id), pendingImports: identity.imports.length > 0,
          payload: { text: mergeRefs(draft.text, draft.refs ?? []), images: draft.images.map(({ mime, data }) => ({ mime, data })) },
        };
        state.draftSends[snapshot.sendId] = snapshot;
      });
      return snapshot;
    },
    completeSend(sendId, accepted) {
      let textConsumed = false;
      let target: DraftTarget | null = null;
      set((state) => {
        const snapshot = state.draftSends[sendId];
        if (!snapshot) return;
        delete state.draftSends[sendId];
        if (!accepted || !matches(state, snapshot)) return;
        const draft = state.drafts[snapshot.sessionId];
        const identity = state.draftIdentities[snapshot.sessionId];
        if (!draft || !identity) return;
        target = { sessionId: snapshot.sessionId, instanceId: snapshot.instanceId, attachmentEpoch: snapshot.attachmentEpoch };
        if (identity.textRevision === snapshot.textRevision) {
          draft.text = "";
          identity.textRevision++;
          textConsumed = true;
        }
        if (identity.refsRevision === snapshot.refsRevision) {
          draft.refs = [];
          identity.refsRevision++;
        }
        const sent = new Set(snapshot.imageIds);
        draft.images = draft.images.filter((image) => !sent.has(image.id));
      });
      return { textConsumed, target };
    },
  };
}
