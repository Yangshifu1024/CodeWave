// 后台服务（service 工具）状态修复 —— store / handler 层回归测试。
//
// 缺陷背景：`ToolCallCard` 曾用 `data.tail ? "运行中" : "已停止"` 判存活，而 `tail` 只是日志内容：
//   ① `start` 出参根本不含 tail（只有 {id,pid,purpose,owner_root_id,note,running}）；
//   ② 无输出的服务（如 `sleep 300`）日志恒空 → tail 恒空。
// 于是进程明明在跑，界面永久误报「已停止」，「停止服务」按钮一并消失 → 形成占端口的僵尸服务。
//
// 本组用例钉死三条契约：
//   B1 `restoreFromMessages` 恢复出来的落盘 `running: true` 必须降级为 `undefined`（过期快照不信任）；
//   B2 `service:update` 的属主查找必须同时认主会话（tabs 顶层 assistant 项的 toolsMap）与
//      子代理流（`subStreams[sub_id]` 的**平铺** toolsMap——工具卡不在 timeline 里，
//      见 stores/runFrames.ts 的 closeRunningTools 遍历口径）；子代理启动的服务其
//      session = sub_id、不在 tabs 顶层，早期只查 tabs 会让这类事件永久静默丢弃；
//   B3 `running` 是进程存活事实，与 tail 有无内容无关（无输出服务也必须能翻到「运行中」）。
//
// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）；
// zustand store 是模块级单例，逐用例重建态桶（踩坑清单）；runHandlers 有模块级计数与防抖定时器，
// 每个用例前调 `__resetServiceReconcileForTest()`。
import { beforeEach, describe, expect, it, vi } from "vitest";

const ipcMock = vi.hoisted(() => ({
  listServices: vi.fn(async (): Promise<any[]> => []),
  loadToolOutcomes: vi.fn(async (): Promise<any[]> => []),
  loadSubagentHistory: vi.fn(async (): Promise<any[]> => []),
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import { useRun } from "../stores/run";
import {
  __resetServiceReconcileForTest,
  droppedServiceUpdateCount,
  reconcileServices,
  scheduleServiceReconcile,
} from "../stores/runHandlers";

const SESSION = "s-svc";

/** 工具卡：immer 每次 set 都会换对象，用取值函数而非缓存引用 */
function card(key: string): any {
  const item = useRun.getState().tabs[SESSION]!.items.find((i) => i.kind === "assistant") as any;
  return item?.toolsMap?.[key];
}

/** 子代理过程流内的工具卡 */
function subCard(subId: string, key: string): any {
  const st = useRun.getState().tabs[SESSION]!.subStreams[subId];
  return st?.toolsMap?.[key];
}

function handlers() {
  return useRun.getState().bindGlobalHandlers();
}

/**
 * 造一张 service 工具卡。
 *
 * ⚠️ `data` 里**刻意不放 `tail`**：无输出服务的 tail 恒空，早期用 tail 当存活代理正是缺陷根因，
 * 放进去会让本组用例「顺带」通过而钉不住修复点。
 */
function seedServiceCard(key: string, id: string, extra: Record<string, unknown> = {}) {
  handlers()["tool:start"]({
    session: SESSION,
    call_key: key,
    tool: "service",
    args_preview: JSON.stringify({ action: "start", name: id }),
    phase: "running",
  });
  handlers()["tool:result"]({
    session: SESSION,
    call_key: key,
    tool: "service",
    args_preview: JSON.stringify({ action: "start", name: id }),
    outcome: { ok: true, data: { id, pid: 4321, purpose: "dev server", running: true, ...extra } },
    duration_ms: 8,
  });
}

/** 在会话流里再放两张 id 不同的 service 卡（供「不串场」用例） */
function seedSecondServiceCard(key: string, id: string) {
  handlers()["tool:start"]({
    session: SESSION,
    call_key: key,
    tool: "service",
    args_preview: "",
    phase: "running",
  });
  handlers()["tool:result"]({
    session: SESSION,
    call_key: key,
    tool: "service",
    args_preview: "",
    outcome: { ok: true, data: { id, pid: 9999, running: true } },
    duration_ms: 3,
  });
}

/**
 * 把一张已存在的 service 卡挂进子代理过程流。
 *
 * 子代理启动的服务：事件 session = sub_id、不在 tabs 顶层，工具卡住在所属 Tab 的
 * `subStreams[subId]`（`{ timeline: TimelineSeg[], toolsMap: Record<callKey, ToolView> }`，
 * 见 run.types.ts 的 SubStream —— 工具卡在**平铺**的 toolsMap 上，不是 assistant 项）。
 */
function mountServiceCardInSubStream(subId: string, key: string, id: string) {
  useRun.setState((s) => {
    const tb = s.tabs[SESSION]!;
    tb.subStreams[subId] = {
      timeline: [{ kind: "tool", callKey: key }],
      toolsMap: {
        [key]: {
          callKey: key,
          tool: "service",
          status: "ok",
          outcome: { ok: true, data: { id, pid: 555, running: true } },
          argsPreview: "",
          progressTail: "",
        },
      },
      status: "done",
      gen: 0,
      loaded: true,
    } as any;
  });
}

beforeEach(() => {
  __resetServiceReconcileForTest();
  ipcMock.listServices.mockClear();
  ipcMock.listServices.mockResolvedValue([]);
  useRun.setState((s) => {
    s.tabs = {};
    s.tabs[SESSION] = blankTab();
  });
});

/** blank() 的镜像（含 usage，逐字段照抄 runFrames.blank 的默认态） */
function blankTab(): any {
  return {
    items: [],
    running: false,
    streamGen: 0,
    ask: null,
    breakdown: null,
    todos: [],
    suggestions: [],
    subs: [],
    subStreams: {},
    subDrawer: { open: false, subId: null },
    gitEntries: null,
    writeTick: 0,
    queue: [],
    pendingItemId: null,
    draftFromQueue: null,
    lastDoneRunId: null,
    compacting: false,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  };
}

describe("service:update 事件落到工具卡", () => {
  it("带 tail 的推送落到 id 匹配的那张 service 卡的 data.tail", () => {
    seedServiceCard("c-tail", "svc_a");
    expect(card("c-tail").outcome.data.tail).toBeUndefined();

    handlers()["service:update"]({ session: SESSION, id: "svc_a", tail: "listening on :5173" });

    expect(card("c-tail").outcome.data.tail).toBe("listening on :5173");
    // tail 是日志内容，与存活无关：这条不带 running，卡片原有的 running 不被覆盖
    expect(card("c-tail").outcome.data.running).toBe(true);
  });

  it("exited 事件：目标卡 running=false 且 tail 清空", () => {
    seedServiceCard("c-exit", "svc_b", { tail: "booting…" });
    expect(card("c-exit").outcome.data.tail).toBe("booting…");

    handlers()["service:update"]({ session: SESSION, id: "svc_b", exited: true });

    expect(card("c-exit").outcome.data.running).toBe(false);
    expect(card("c-exit").outcome.data.tail).toBe("");
  });

  it("removed 事件：目标卡 running=false（且 tail 清空）", () => {
    seedServiceCard("c-rm", "svc_c", { tail: "log line" });

    handlers()["service:update"]({ session: SESSION, removed: "svc_c" });

    expect(card("c-rm").outcome.data.running).toBe(false);
    expect(card("c-rm").outcome.data.tail).toBe("");
  });

  // B3：无输出服务（如 `sleep 300`）的 tail 恒空 —— tail 为空不得被读成「已停止」
  it("running:true 的普通推送：tail 为空串也照样把 running 翻成 true", () => {
    seedServiceCard("c-quiet", "svc_quiet");

    handlers()["service:update"]({ session: SESSION, id: "svc_quiet", tail: "", running: true });

    expect(card("c-quiet").outcome.data.running).toBe(true);
    expect(card("c-quiet").outcome.data.tail).toBe("");
  });

  // B2：子代理启动的服务，session = sub_id、工具卡挂在所属 Tab 的 subStreams 下，
  // 不在 tabs 顶层 —— 只查 tabs 的话这类事件会永久静默丢弃（界面停在旧状态且无任何报错）。
  //
  // 关键形状差异：子代理的工具卡住在 SubStream **平铺**的 `toolsMap` 上，而不是 timeline
  // （SubStream = { timeline: TimelineSeg[], toolsMap: Record<callKey, ToolView> }，
  //  实时路径 ensureToolAnchorIm 与恢复路径 messagesToSubStream 都写这个 map），
  // timeline 里只有 tool/text/thinking 等 seg。因此 serviceToolsOf 必须对两种容器形状
  // 分别处理：数组按 assistant 项取 toolsMap，SubStream 直接取自身 toolsMap。
  // 把 SubStream 当数组遍历、筛 kind === "assistant"，会让属主查找对子代理恒返回空集。
  it("子代理流内的 service 卡也能收到事件（session = sub_id，不在 tabs 顶层）", () => {
    const SUB = "sub_1";
    mountServiceCardInSubStream(SUB, "s-c1", "svc_sub");
    expect(subCard(SUB, "s-c1").outcome.data.tail).toBeUndefined();

    handlers()["service:update"]({ session: SUB, id: "svc_sub", tail: "sub log", running: true });

    expect(subCard(SUB, "s-c1").outcome.data.tail).toBe("sub log");
    expect(subCard(SUB, "s-c1").outcome.data.running).toBe(true);
    // 属主查找不得凭空造桶：tabs 顶层没有 sub_1
    expect(useRun.getState().tabs[SUB]).toBeUndefined();
  });

  // 防回归护栏：确保子代理路径的修复没有把主会话路径带坏（两种形状共存）。
  it("同一事件同时更新主会话卡与子代理流卡（两种挂载形状都得认）", () => {
    seedServiceCard("c-main", "svc_main");
    const SUB = "sub_3";
    mountServiceCardInSubStream(SUB, "s-c1", "svc_main");

    handlers()["service:update"]({ session: SESSION, id: "svc_main", tail: "main log" });
    handlers()["service:update"]({ session: SUB, id: "svc_main", tail: "sub log" });

    expect(card("c-main").outcome.data.tail).toBe("main log");
    expect(subCard(SUB, "s-c1").outcome.data.tail).toBe("sub log");
  });

  it("未知 session 的事件：不抛错、不新建 tabs 桶，丢帧计数递增", () => {
    seedServiceCard("c-ok", "svc_ok");
    expect(droppedServiceUpdateCount()).toBe(0);

    expect(() => handlers()["service:update"]({ session: "ghost", id: "svc_ok", tail: "x" })).not.toThrow();

    expect(droppedServiceUpdateCount()).toBe(1);
    expect(useRun.getState().tabs["ghost"]).toBeUndefined();
    // 已知会话的卡不受影响
    expect(card("c-ok").outcome.data.tail).toBeUndefined();
  });

  it("同一会话两张不同 id 的卡：只更新 id 命中的那张，不串场", () => {
    seedServiceCard("c-1", "svc_1");
    seedSecondServiceCard("c-2", "svc_2");
    expect(card("c-2")).toBeTruthy();

    handlers()["service:update"]({ session: SESSION, id: "svc_1", tail: "hit-1", running: true });

    expect(card("c-1").outcome.data.tail).toBe("hit-1");
    expect(card("c-1").outcome.data.running).toBe(true);
    // 没被命中的那张：两个字段都不得被动
    expect(card("c-2").outcome.data.tail).toBeUndefined();
    expect(card("c-2").outcome.data.running).toBe(true);
    expect(card("c-2").outcome.data.id).toBe("svc_2");
  });
});

describe("会话恢复：落盘 running 快照必须降级", () => {
  // B1：落盘的 `running: true` 只是**启动当时**的快照。进程可能已退出 / 被 stop / 随应用重启被回收。
  // 直接沿用 = 永久误报运行中（比误报已停止更坏：用户会相信服务还活着）→ 一律先降为未知。
  it("restoreFromMessages 恢复出的 running:true 被降级为 undefined（不是 true）", () => {
    useRun.getState().restoreFromMessages(SESSION, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-svc", name: "service", args: { action: "start", name: "api" } }],
      },
      {
        role: "tool",
        content: [
          {
            type: "tool_result",
            tool_use_id: "call-svc",
            // 落盘的模型侧 JSON：`running: true` 是启动当时的快照
            content: JSON.stringify({
              id: "svc_hist",
              pid: 7777,
              purpose: "dev server",
              running: true,
            }),
            is_error: false,
          },
        ],
      },
    ] as any);

    const data = card("call-svc").outcome.data;
    expect(data.id).toBe("svc_hist");
    // 断言「未知」而不是「未赋值」：卡仍在，data 其它字段照常保留
    expect("running" in data).toBe(true);
    expect(data.running).toBeUndefined();
  });

  it("恢复出的 service 卡降级后，对账定时器排定（静默期到了才真拉 list_services）", () => {
    useRun.getState().restoreFromMessages(SESSION, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-svc2", name: "service", args: {} }],
      },
      {
        role: "tool",
        content: [
          {
            type: "tool_result",
            tool_use_id: "call-svc2",
            content: JSON.stringify({ id: "svc_hist2", running: true }),
            is_error: false,
          },
        ],
      },
    ] as any);

    expect(card("call-svc2").outcome.data.running).toBeUndefined();

    // 防抖 3s：立刻断言 listServices 未被调用；不代表拉取失败（拉取是兵底路）
    expect(ipcMock.listServices).not.toHaveBeenCalled();
  });

  it("非 service 卡的 running 字段不受降级影响", () => {
    useRun.getState().restoreFromMessages(SESSION, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-cmd", name: "command", args: {} }],
      },
      {
        role: "tool",
        content: [
          {
            type: "tool_result",
            tool_use_id: "call-cmd",
            content: JSON.stringify({ id: "svc_hist3", running: true }),
            is_error: false,
          },
        ],
      },
    ] as any);

    expect(card("call-cmd").outcome.data.running).toBe(true);
  });
});

describe("list_services 对账（推送通道的拉取兜底）", () => {
  it("listServices 返回实况后，卡的 running 被覆写为权威值", async () => {
    ipcMock.listServices.mockResolvedValue([
      { id: "svc_a", running: true },
      { id: "svc_dead", running: false },
    ]);
    seedServiceCard("c-a", "svc_a");
    seedSecondServiceCard("c-dead", "svc_dead");
    expect(card("c-dead").outcome.data.running).toBe(true);

await reconcileServices(useRun.setState as any, useRun.getState as any);
    expect(ipcMock.listServices).toHaveBeenCalled();
    // 进程还活着 → true
    expect(card("c-a").outcome.data.running).toBe(true);
    // 后端已报 running:false 的同名 id：live.has → 但 running 集不含 → false
    expect(card("c-dead").outcome.data.running).toBe(false);
  });

  it("对账把「不在快照里」的服务一律判为已停止（覆盖过期的落盘 running:true）", async () => {
    ipcMock.listServices.mockResolvedValue([{ id: "svc_a", running: true }]);
    seedServiceCard("c-a", "svc_a");
    seedSecondServiceCard("c-zombie", "svc_zombie");
    // 手动把 zombie 刷成「误报运行中」的形态：落盘快照被直接沿用
    useRun.setState((s) => {
      const item = s.tabs[SESSION]!.items.find((i) => i.kind === "assistant") as any;
      item.toolsMap["c-zombie"].outcome = {
        ok: true,
        data: { id: "svc_zombie", pid: 1, running: true },
      };
    });

    await reconcileServices(useRun.setState as any, useRun.getState as any);
    expect(card("c-a").outcome.data.running).toBe(true);
    // 僵尸（快照里查无此服务）→ 不得继续谎报运行中
    expect(card("c-zombie").outcome.data.running).toBe(false);
  });

  // 先把夹具刷成「未知/错报」态，否则 `expect(running).toBe(true)` 会空转（fixture 本来就是 true）
  it("对账也覆盖子代理流内的 service 卡", async () => {
    ipcMock.listServices.mockResolvedValue([{ id: "svc_sub", running: true }]);
    mountServiceCardInSubStream("sub_2", "s-c1", "svc_sub");
    // 刷成「误报已停止」：对账必须把它拉回 true（而不是拿 fixture 的初值糊过去）
    useRun.setState((s) => {
      const tool = (s.tabs[SESSION]!.subStreams["sub_2"].toolsMap as any)["s-c1"];
      tool.outcome = { ok: true, data: { id: "svc_sub", pid: 555, running: false } };
    });
    expect(subCard("sub_2", "s-c1").outcome.data.running).toBe(false);

    await reconcileServices(useRun.setState as any, useRun.getState as any);

    expect(subCard("sub_2", "s-c1").outcome.data.running).toBe(true);
  });

  it("对账覆盖子代理流内的「僵尸服务」（快照里查无此 id → 不得继续谎报运行中）", async () => {
    ipcMock.listServices.mockResolvedValue([{ id: "svc_other", running: true }]);
    mountServiceCardInSubStream("sub_4", "s-c1", "svc_zombie");
    expect(subCard("sub_4", "s-c1").outcome.data.running).toBe(true);

    await reconcileServices(useRun.setState as any, useRun.getState as any);

    expect(subCard("sub_4", "s-c1").outcome.data.running).toBe(false);
  });

  // 钉死「自维持重排在 early-return 之前」这个修复点：若重排被挪到 return 之后，
  // 「本轮跳过拉取 → 无人再排」会让拉取兜底永久失效（而它正是事件全丢时唯一的纠错手段）。
  // 关键性质是**连续多轮都还会排定**，故连推三轮并断言拉取被调用多次——若重排被挪到
  // early-return 之后，第一轮之后链就断了，调用次数会停在 1。
  it("链式重排：跳过的轮次也会排下一轮（链不中断）", async () => {
    vi.useFakeTimers();
    try {
      ipcMock.listServices.mockResolvedValue([{ id: "svc_a", running: true }]);
      seedServiceCard("c-a", "svc_a");

      scheduleServiceReconcile(useRun.setState as any, useRun.getState as any);
      await vi.advanceTimersByTimeAsync(3000);
      await vi.advanceTimersByTimeAsync(3000);
      await vi.advanceTimersByTimeAsync(3000);

      // 三轮 ⇒ 至少两次拉取（若重排被挪到 early-return 之后，次数会停在 1）
      expect(ipcMock.listServices.mock.calls.length).toBeGreaterThanOrEqual(2);
      // 对账把卡片拉回进程实况（快照为 running:true）
      expect(card("c-a").outcome.data.running).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});
