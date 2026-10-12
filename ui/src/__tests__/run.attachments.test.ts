import { beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), Channel: class {} }));
import { useRun, type PendingImage } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { IMAGE_MAX_TOTAL_BASE64, mergeDraftImages } from "../stores/attachmentPolicy";

const image = (id: string, data = "AAAA"): PendingImage => ({ id, name: `${id}.png`, data, mime: "image/png", dataUrl: `data:image/png;base64,${data}` });
beforeEach(() => {
  useRun.setState({ tabs: {}, drafts: {}, draftIdentities: {}, draftSends: {} });
  useSessions.setState({ tabs: [], activeKey: "A" });
  useRun.getState().initTab("A");
  useRun.getState().initTab("B");
});
const run = () => useRun.getState();
const target = () => run().captureAttachmentTarget("A")!;

describe("attachment identity and atomic commit", () => {
  it("preserves imports captured on an open skeleton when runtime initialization finishes", () => {
    const tabs = useSessions.getState().tabs;
    useSessions.setState({ tabs: [...tabs, { key: "loading", sessionId: "loading" } as (typeof tabs)[number]] });
    const ticket = run().captureAttachmentTarget("loading")!;
    expect(ticket).not.toBeNull();
    run().startAttachmentImport(ticket);
    run().initTab("loading");
    expect(run().appendDraftImages(ticket, [image("loaded")])).toMatchObject({ accepted: 1 });
    expect(run().drafts.loading.images[0].id).toBe("loaded");
  });
  it("routes a late import to A without altering any B fields", () => {
    const ticket = target();
    run().setDraftText("B", "B"); run().setDraftRefs(["b"], "B"); run().setDraftImages([image("b")], "B");
    const before = run().drafts.B;
    useSessions.setState({ activeKey: "B" });
    expect(run().appendDraftImages(ticket, [image("a")])).toMatchObject({ accepted: 1 });
    expect(run().drafts.A.images[0].id).toBe("a");
    expect(run().drafts.B).toBe(before);
  });
  it("merges reversed batches and never resurrects a removed image", () => {
    run().setDraftImages([image("old")], "A"); const x = target(), y = target();
    run().appendDraftImages(y, [image("y")]); run().removeDraftImage("old", "A");
    run().appendDraftImages(x, [image("x")]);
    expect(run().drafts.A.images.map((i) => i.id)).toEqual(["y", "x"]);
    run().appendDraftImages(x, [image("x")]);
    expect(run().drafts.A.images).toHaveLength(2);
  });
  it("rechecks final count for competing batches", () => {
    run().setDraftImages([image("1"), image("2"), image("3")], "A"); const a = target(), b = target();
    expect(run().appendDraftImages(b, [image("4")])).toMatchObject({ accepted: 1 });
    expect(run().appendDraftImages(a, [image("5")])).toMatchObject({ accepted: 0, rejected: 1, reason: "count" });
    expect(run().drafts.A.images).toHaveLength(4);
  });
  it("accepts exact total boundary and rejects one-character overflow atomically", () => {
    const old = [image("1", "x".repeat(IMAGE_MAX_TOTAL_BASE64 - 1))];
    expect(mergeDraftImages(old, [image("2", "x")]).result).toMatchObject({ accepted: 1 });
    const result = mergeDraftImages(old, [image("2", "xx")]);
    expect(result.images).toBe(old); expect(result.result).toMatchObject({ accepted: 0, reason: "total" });
  });
  it.each(["clear", "replace", "dispose", "reopen"])("invalidates import after %s without recreating a closed bucket", (kind) => {
    const ticket = target();
    if (kind === "clear") run().clearDraft("A");
    if (kind === "replace") run().setDraftImages([image("new")], "A");
    if (kind === "dispose" || kind === "reopen") run().dispose("A");
    if (kind === "reopen") run().initTab("A");
    const before = run().drafts.A;
    expect(run().appendDraftImages(ticket, [image("late")])).toEqual({ status: "stale" });
    expect(run().appendDraftRefs(ticket, ["late.pdf"])).toBe(false);
    expect(run().drafts.A).toBe(before);
  });
  it("does not invalidate imports for text edits, image removal or switching", () => {
    const ticket = target(); run().setDraftText("new", "A"); run().removeDraftImage("absent", "A");
    useSessions.setState({ activeKey: "B" }); expect(run().isAttachmentTargetCurrent(ticket)).toBe(true);
  });
});

describe("send snapshot consumption", () => {
  const seed = () => { run().setDraftText("old", "A"); run().setDraftRefs(["old.pdf"], "A"); run().setDraftImages([image("old")], "A"); };
  it.each(["append", "replace", "delete"])("preserves %s text edits and new refs/images while consuming only sent ids", (kind) => {
    seed(); const ticket = target(); const snap = run().beginSend("A")!;
    const next = kind === "append" ? "old new" : kind === "replace" ? "new" : "";
    run().setDraftText(next, "A"); run().appendDraftRefs(ticket, ["new.pdf"]); run().appendDraftImages(ticket, [image("new")]);
    expect(snap.payload.images).toEqual([{ mime: "image/png", data: "AAAA" }]);
    expect(snap.payload.text).not.toContain("new");
    run().completeSend(snap.sendId, true);
    expect(run().drafts.A).toMatchObject({ text: next, refs: ["old.pdf", "new.pdf"] });
    expect(run().drafts.A.images.map((i) => i.id)).toEqual(["new"]);
  });
  it("consumes unchanged fields, preserves legal in-flight imports and ignores duplicate admission", () => {
    seed(); const ticket = target(); const importId = run().startAttachmentImport(ticket)!;
    const snap = run().beginSend("A")!; expect(snap.pendingImports).toBe(true);
    expect(run().beginSend("A")).toBeNull(); run().completeSend(snap.sendId, true);
    expect(run().drafts.A).toEqual({ text: "", refs: [], images: [] });
    run().appendDraftImages(ticket, [image("late")]); run().finishAttachmentImport(ticket, importId);
    expect(run().completeSend(snap.sendId, true).textConsumed).toBe(false);
    expect(run().drafts.A.images[0].id).toBe("late");
  });
  it("failed admission consumes nothing and imports remain valid", () => {
    seed(); const ticket = target(); const snap = run().beginSend("A")!;
    run().appendDraftImages(ticket, [image("new")]); const before = run().drafts.A;
    run().completeSend(snap.sendId, false); expect(run().drafts.A).toBe(before);
    expect(run().isAttachmentTargetCurrent(ticket)).toBe(true); expect(run().beginSend("A")).not.toBeNull();
  });
  it.each(["clear", "replace", "dispose", "reopen"])("late accepted does not consume a %s draft", (kind) => {
    seed(); const snap = run().beginSend("A")!;
    if (kind === "clear") run().clearDraft("A");
    if (kind === "replace") run().setDraftImages([image("replacement")], "A");
    if (kind === "dispose" || kind === "reopen") run().dispose("A");
    if (kind === "reopen") { run().initTab("A"); run().setDraftText("reopened", "A"); }
    const before = run().drafts.A; expect(run().completeSend(snap.sendId, true).textConsumed).toBe(false);
    expect(run().drafts.A).toBe(before);
  });
  it("switching to B consumes A only", () => {
    seed(); const snap = run().beginSend("A")!; run().setDraftText("B", "B"); useSessions.setState({ activeKey: "B" });
    run().completeSend(snap.sendId, true); expect(run().drafts.A.text).toBe(""); expect(run().drafts.B.text).toBe("B");
  });
});
