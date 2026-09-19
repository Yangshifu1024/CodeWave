// UI store: panel toggles, right bar persistence, notification stack lifecycle ([docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useUi } from "../stores/ui";

describe("stores/ui", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.useFakeTimers();
    useUi.setState({
      language: "zh-CN",
      settingsOpen: false,
      settingsTab: "general",
      tasksOpen: false,
      statsOpen: false,
      rightBarOpen: true,
      rbTab: "info",
      createProjectRequested: false,
      notifications: [],
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("panel toggles update state independently", () => {
    useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false });
    useUi.setState({ settingsOpen: true });
    expect(useUi.getState().settingsOpen).toBe(true);
    useUi.setState({ tasksOpen: true, statsOpen: true });
    expect(useUi.getState().tasksOpen).toBe(true);
    expect(useUi.getState().statsOpen).toBe(true);
  });

  it("setRightBarOpen persists to localStorage", () => {
    useUi.getState().setRightBarOpen(false);
    expect(useUi.getState().rightBarOpen).toBe(false);
    expect(localStorage.getItem("ws_right_bar_open")).toBe("0");
    useUi.getState().setRightBarOpen(true);
    expect(localStorage.getItem("ws_right_bar_open")).toBe("1");
  });

  it("default rightBarOpen is open when localStorage has no record", () => {
    expect(useUi.getState().rightBarOpen).toBe(true);
  });

  it("rbTab switches and showChanges opens the right bar on the changes tab", () => {
    useUi.getState().setRightBarOpen(false);
    useUi.getState().showChanges();
    expect(useUi.getState().rightBarOpen).toBe(true);
    expect(useUi.getState().rbTab).toBe("changes");
  });

  it("showSettings：带页签跳到该页，无参保持当前页不重置（docs/settings-fullscreen-shell）", () => {
    // 带参深链：错误卡「打开模型设置」→ providers、LSP 引导卡 → 所在页靠它落地（auth-error-guidance）
    useUi.setState({ settingsTab: "general", settingsOpen: false });
    useUi.getState().showSettings("providers");
    expect(useUi.getState().settingsOpen).toBe(true);
    expect(useUi.getState().settingsTab).toBe("providers");
    // 无参 = 保持当前页（有意变更：旧实现无参一律回 general，设置改成常驻全屏页后会把用户甩回第一页）
    useUi.setState({ settingsOpen: false });
    useUi.getState().showSettings();
    expect(useUi.getState().settingsOpen).toBe(true);
    expect(useUi.getState().settingsTab).toBe("providers");
  });

  it("setLanguage persists choice", () => {
    useUi.getState().setLanguage("en-US");
    expect(useUi.getState().language).toBe("en-US");
    expect(localStorage.getItem("ws_lang")).toBe("en-US");
  });

  it("notify appends a notification and auto-dismisses it after 6s", () => {
    useUi.getState().notify("t1", "b1", "sess-1");
    expect(useUi.getState().notifications).toHaveLength(1);
    expect(useUi.getState().notifications[0]).toMatchObject({ title: "t1", body: "b1", sessionId: "sess-1" });
    vi.advanceTimersByTime(6000);
    expect(useUi.getState().notifications).toHaveLength(0);
  });

  it("toast uses the app title and carries no sessionId", () => {
    useUi.getState().toast("hello");
    const n = useUi.getState().notifications[0];
    expect(n).toMatchObject({ title: "CodeWave", body: "hello" });
    expect(n.sessionId).toBeUndefined();
  });

  it("dismiss removes a single notification by id; dismissBySession clears all of a session", () => {
    useUi.getState().notify("a", "a", "s1");
    useUi.getState().notify("b", "b", "s1");
    useUi.getState().notify("c", "c");
    const first = useUi.getState().notifications[0];
    useUi.getState().dismiss(first.id);
    expect(useUi.getState().notifications).toHaveLength(2);
    expect(useUi.getState().notifications.every((n) => n.id !== first.id)).toBe(true);
    useUi.getState().dismissBySession("s1");
    expect(useUi.getState().notifications).toHaveLength(1);
    expect(useUi.getState().notifications[0].sessionId).toBeUndefined();
  });
});
