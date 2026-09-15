// 左栏会话树展开/折叠态的持久化与恢复（会话保存与恢复优化 · 批1）：
// 事实源在 useUi store，而不是组件 useState / uiState 的模块内存 —— 后者会踩「hydrate（异步读盘）晚于 ProjectNav 首渲染」的时序陷阱：
// 首渲染时快照还没到货，挂载时读一次就永远拿不到恢复值；store 的 setState 才能把后到的值推给已挂载的订阅者。
// vitest 不加载 CSS：直接渲染组件 + 用 store setState 铺数据；invoke 全量 mock（覆盖挂载时的 list_scheduled_tasks）。
// 落盘观测口径：uiState 的防抖落盘最终走 ipc.setUiState → invoke("set_ui_state", { state })。
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, cleanup, act, fireEvent, screen } from "@testing-library/react";
import { App } from "antd";
import ProjectNav from "../features/shell/ProjectNav";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { invoke } from "@tauri-apps/api/core";
import { type ProjectEntry, type SessionMeta } from "../ipc/types";
import {
  FLUSH_DEBOUNCE_MS,
  UI_STATE_SCHEMA,
  applyUiStateToStores,
  buildSnapshot,
  getTreeCollapsed,
  getTreeExpanded,
  loadUiState,
  reset,
} from "../utils/uiState";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));

const project: ProjectEntry = {
  id: "p1",
  name: "项目A",
  directory: "/tmp/ws",
  created_at: "2026-08-01T00:00:00Z",
};

/** 分组 key 由 ProjectNav 按 `proj:<项目 id>` 生成；快照里的 tree.expanded 用的就是它 */
const GROUP_KEY = "proj:p1";
/** PREVIEW_COUNT = 5：6 条会话才有多出来的「显示更多」开关 */
const TOTAL = 6;

function meta(partial: Partial<SessionMeta> & { id: string; title: string }): SessionMeta {
  return {
    workspace: "/tmp/ws",
    model_id: null,
    created_at: "",
    updated_at: "",
    message_count: 0,
    project_id: null,
    roots: ["/tmp/ws"],
    running: false,
    interrupted: null,
    ...partial,
  };
}

/** 6 条老会话，全在 p1 下（按最后活跃倒序排列） */
function sixSessions(): SessionMeta[] {
  return Array.from({ length: TOTAL }, (_, i) =>
    meta({
      id: `s${i}`,
      title: `会话${i}`,
      project_id: "p1",
      created_at: `2026-08-2${i}T09:00:00Z`,
      updated_at: `2026-08-2${i}T10:00:00Z`,
    }),
  );
}

/** 铺数据：会话 + 项目 + 左栏态（左栏态直接写 store，模拟 hydrate 的产物） */
function seed(tree: { expanded?: Record<string, boolean>; collapsed?: boolean } = {}): void {
  useSessions.setState({ tabs: [], sessions: sixSessions(), projects: [project], activeKey: null });
  useUi.setState({ treeExpand: tree.expanded ?? {}, treeCollapsed: tree.collapsed ?? false });
}

function renderNav() {
  return render(
    <App>
      <ProjectNav />
    </App>,
  );
}

/** 项目分组里渲染出的会话行数（不含临时会话区） */
function projectRows(): number {
  return document.querySelectorAll(".project-sessions .session-nav-row").length;
}

function projectGroups(): number {
  return document.querySelectorAll(".project-group").length;
}

/** 落盘链路上有若干 await（动态 import + invoke）：推进计时器后把微任务与零延迟计时器都走干净 */
async function advance(ms: number): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
  for (let i = 0; i < 10; i++) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

function flushedPayloads(): { tree: { expanded: Record<string, boolean>; collapsed: boolean } }[] {
  return vi
    .mocked(invoke)
    .mock.calls.filter(([cmd]) => cmd === "set_ui_state")
    .map(([, args]) => (args as { state: { tree: { expanded: Record<string, boolean>; collapsed: boolean } } }).state);
}

/** 让出若干轮（计时器 + 微任务）直到落盘真的发生——避开对内部 await 次数做假设 */
async function untilFlushed(): Promise<void> {
  for (let i = 0; i < 40 && flushedPayloads().length === 0; i++) await advance(0);
}

beforeEach(async () => {
  vi.useRealTimers();
  // 预热：buildSnapshot 要动态 import 窗口 API（读窗口几何），首次加载走真实异步 I/O；
  // 不预热的话「本文件头一个 fake timer 用例」的落盘会卡在模块加载上，断言变成比谁跑得快（不稳）
  await buildSnapshot();
  reset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [], unread: {} });
  // uiState 的 reset 会一并清掉 store 里的左栏态与待落盘计时器（reset 放在最后，免得上面几次 setState 又挂上新计时器）
  reset();
  vi.mocked(invoke).mockClear();
  vi.mocked(invoke).mockImplementation(async () => []);
});

describe("左栏会话树展开态：store 为事实源 + 落盘链路同步", () => {
  it("点「显示更多」→ useUi.treeExpand 立即反映，uiState.getTreeExpanded() 同步（写的就是它）", () => {
    seed();
    renderNav();
    expect(projectRows()).toBe(5);
    expect(screen.getByText("显示更多")).toBeTruthy();

    act(() => {
      fireEvent.click(document.querySelector(".show-more")!);
    });
    expect(projectRows()).toBe(TOTAL);
    expect(useUi.getState().treeExpand).toEqual({ [GROUP_KEY]: true });
    expect(getTreeExpanded()).toEqual({ [GROUP_KEY]: true });
    expect(screen.getByText("收起")).toBeTruthy();

    act(() => {
      fireEvent.click(document.querySelector(".show-more")!);
    });
    expect(projectRows()).toBe(5);
    expect(useUi.getState().treeExpand[GROUP_KEY]).toBe(false);
    expect(getTreeExpanded()).toEqual({ [GROUP_KEY]: false });
  });

  it("hydrate 场景：store 里先有快照值 → 渲染出的会话列表直接是展开的（时序陷阱已解）", () => {
    // AppShell 首渲染之前就拿到快照的等价情形（渲染时机任意，值不会丢）
    seed({ expanded: { [GROUP_KEY]: true } });
    renderNav();
    expect(projectRows()).toBe(TOTAL);
    expect(document.querySelector(".show-more")?.textContent).toBe("收起");
  });

  it("真实时序回归：先渲染（快照未到货）→ hydrate 的 setState 到达 → 左栏自动展开", () => {
    seed();
    renderNav();
    expect(projectRows()).toBe(5); // 首渲染时快照还没到货：全折叠

    act(() => {
      // 等价于 applyUiStateToStores 里的那一次推送（组件 useState 方案在这里永远收不到信号）
      useUi.setState({ treeExpand: { [GROUP_KEY]: true } });
    });
    expect(projectRows()).toBe(TOTAL);
    expect(document.querySelector(".show-more")?.textContent).toBe("收起");
  });

  it("端到端 hydrate：loadUiState → applyUiStateToStores 把快照推进 store，已挂载的左栏立即展开", async () => {
    seed();
    renderNav();
    expect(projectRows()).toBe(5);

    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === "get_ui_state"
        ? { schema: UI_STATE_SCHEMA, tabs: { order: [], items: {} }, tree: { expanded: { [GROUP_KEY]: true }, collapsed: false, unread: {} } }
        : [],
    );
    await loadUiState();
    act(() => {
      applyUiStateToStores();
    });

    expect(useUi.getState().treeExpand).toEqual({ [GROUP_KEY]: true });
    expect(projectRows()).toBe(TOTAL);
  });
});

describe("左栏项目区折叠态：同样住 store 并随快照恢复", () => {
  it("treeCollapsed 快照 true → 项目分组不渲染；恢复 false 后回来，点头部再折叠写回 store", () => {
    seed({ collapsed: true });
    renderNav();
    expect(projectGroups()).toBe(0);
    expect(getTreeCollapsed()).toBe(true);

    act(() => {
      useUi.setState({ treeCollapsed: false });
    });
    expect(projectGroups()).toBe(1);
    expect(getTreeCollapsed()).toBe(false);

    act(() => {
      fireEvent.click(screen.getByText("项目"));
    });
    expect(projectGroups()).toBe(0);
    expect(useUi.getState().treeCollapsed).toBe(true);
    expect(getTreeCollapsed()).toBe(true); // uiState 侧同步：落盘读到的就是它
  });
});

describe("展开/折叠变更触发防抖落盘", () => {
  it("setTreeGroupExpanded → 防抖窗口内不写盘，期满后 set_ui_state 带上 tree.expanded", async () => {
    vi.useFakeTimers();
    seed();
    act(() => {
      useUi.getState().setTreeGroupExpanded(GROUP_KEY, true);
    });
    expect(flushedPayloads()).toHaveLength(0); // 防抖未到期

    await advance(FLUSH_DEBOUNCE_MS);
    await untilFlushed();
    const payloads = flushedPayloads();
    expect(payloads.length).toBeGreaterThan(0);
    expect(payloads.at(-1)!.tree.expanded).toEqual({ [GROUP_KEY]: true });
  });

  it("折叠态变更同样落盘（tree.collapsed 进快照）", async () => {
    vi.useFakeTimers();
    seed();
    act(() => {
      useUi.getState().setTreeCollapsed(true);
    });
    await advance(FLUSH_DEBOUNCE_MS);
    await untilFlushed();
    const payloads = flushedPayloads();
    expect(payloads.length).toBeGreaterThan(0);
    expect(payloads.at(-1)!.tree.collapsed).toBe(true);
  });
});
