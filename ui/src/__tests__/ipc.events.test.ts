// bindEvents: handler-keys → tauri listen registration, payload passthrough ([docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect, vi, beforeEach } from "vitest";

const registered: { name: string; handler: (e: { payload: any }) => void }[] = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (e: { payload: any }) => void) => {
    registered.push({ name, handler });
    return () => {};
  }),
}));

import { bindEvents } from "../ipc/events";

beforeEach(() => {
  registered.length = 0;
  vi.clearAllMocks();
});

describe("ipc/events bindEvents", () => {
  it("registers one tauri listener per handler key", async () => {
    const handlers = {
      "run:start": () => {},
      "run:done": () => {},
      "sub:spawn": () => {},
    };
    const unlistens = await bindEvents(handlers);
    expect(unlistens).toHaveLength(3);
    expect(registered.map((r) => r.name).sort()).toEqual(["run:done", "run:start", "sub:spawn"]);
    expect(unlistens.every((u) => typeof u === "function")).toBe(true);
  });

  it("routes tauri event payload into the matching handler", async () => {
    const runStart = vi.fn();
    const runDone = vi.fn();
    await bindEvents({ "run:start": runStart, "run:done": runDone });
    const byName = Object.fromEntries(registered.map((r) => [r.name, r.handler]));
    byName["run:start"]({ payload: { session: "s1" } });
    byName["run:done"]({ payload: { session: "s1", run_id: "r1" } });
    expect(runStart).toHaveBeenCalledWith({ session: "s1" });
    expect(runDone).toHaveBeenCalledWith({ session: "s1", run_id: "r1" });
    expect(runStart).toHaveBeenCalledTimes(1);
  });

  it("returns an unlisten function per registration", async () => {
    const unlistens = await bindEvents({ "a:x": () => {}, "b:y": () => {} });
    const spies = unlistens.map((u) => vi.fn(u));
    spies.forEach((s) => s());
    expect(spies.every((s) => s.mock.calls.length === 1)).toBe(true);
  });
});
