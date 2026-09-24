// Event contract test: every event name emitted by the backend must have a frontend handler (regression guard against silent C1-type breaks)
import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { useRun } from "../stores/run";

function collectRs(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) collectRs(full, out);
    else if (name.endsWith(".rs")) out.push(full);
  }
  return out;
}

function collectTs(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) collectTs(full, out);
    else if (name.endsWith(".ts") || name.endsWith(".tsx")) out.push(full);
  }
  return out;
}

const uiSrc = join(dirname(fileURLToPath(import.meta.url)), "../../src");

const srcTauri = join(dirname(fileURLToPath(import.meta.url)), "../../../src-tauri/src");

describe("事件契约：后端 emit ↔ 前端 handler", () => {
  it("每个后端 emit 的事件都有前端 handler", () => {
    const emitted = new Set<string>();
    // The emit_to bypass (host/notify.rs system-notification click event) is included in the contract scan too
    const re = /\.(?:emit|emit_to)\([^;]*?"([a-z]+:[a-z_]+)"/g;
    for (const file of collectRs(srcTauri)) {
      const text = readFileSync(file, "utf8");
      for (const m of text.matchAll(re)) emitted.add(m[1]);
    }
    expect(emitted.size).toBeGreaterThan(10);

    const handled = new Set(Object.keys(useRun.getState().bindGlobalHandlers()));

    // Events consumed via direct listen calls (not in the run.ts handler table, e.g. notify:activate listened by AppShell):
    // scan only ui/src and match event-name literals inside listen call arguments exactly (avoid false passes from node_modules noise)
    const directListened = new Set<string>();
    for (const text of collectTs(uiSrc).map((f) => readFileSync(f, "utf8"))) {
      for (const m of text.matchAll(/listen(?:<[^>]*>)?\(\s*["']([a-z]+:[a-z_]+)["']/g)) {
        directListened.add(m[1]);
      }
    }

    const missing = [...emitted].filter((e) => !handled.has(e) && !directListened.has(e));
    expect(missing, `后端 emit 但前端无 handler: ${missing.join(", ")}`).toEqual([]);
  });

  it("前端 handler 的每个事件后端确实会 emit（防拼写漂移）", () => {
    const handled = Object.keys(useRun.getState().bindGlobalHandlers());

    let allRust = "";
    for (const file of collectRs(srcTauri)) allRust += readFileSync(file, "utf8");

    const whitelist = new Set(["run:suggestions"]); // carried in the run:done payload + also emitted as a standalone event
    const fake = handled.filter((h) => !whitelist.has(h) && !allRust.includes(`"${h}"`));
    expect(fake, `前端监听但后端从不 emit: ${fake.join(", ")}`).toEqual([]);
  });

  // 键数硬锚点：每次新增事件必须在此同步改数（且上面两条双向扫描都要过），防止“加了 handler 忘了 emit”或反过来
  it("事件面键数为 30（含 tool:start 与 goal:update；lsp:server_missing 已随写入后检查删除）", () => {
    const handled = Object.keys(useRun.getState().bindGlobalHandlers());
    expect(handled).not.toContain("lsp:server_missing");
    expect(handled).toContain("tool:start");
    // 目标模式（`ApprovalMode::Goal`）新增的事件键：后端 emit + 前端 handler 双向都要在
    expect(handled).toContain("goal:update");
    expect(handled.length).toBe(30);
  });

  // 载荷形状断言（键名之外的第二层守护）：键数硬锚点只守「有没有」，
  // 这里走一遍 handler，守 `goal:update` 的载荷被真正读进 run store（字段改名/嵌套改名会红）。
  it("goal:update 载荷形状：session 定位 Tab、三关键字段读入、goal=null 整体清空", () => {
    const prevTabs = useRun.getState().tabs;
    try {
      useRun.setState({ tabs: { s1: { goal: null, todos: [], goalRev: 0 } } } as any);
      const h = useRun.getState().bindGlobalHandlers();
      h["goal:update"]({
        session: "s1",
        goal: {
          text: "把登录改成 OAuth",
          criteria: [{ title: "单测全绿", done: true }],
          ledger: { paths: ["src/auth.rs"], programs: ["cargo"] },
          status: "executing",
          decisions: [],
          pending: [],
          blocked: [],
          rounds: 1,
          stall_streak: 0,
          ledger_denials: 0,
        },
      });
      const g = useRun.getState().tabs["s1"].goal!;
      expect(g.text).toBe("把登录改成 OAuth");
      expect(g.criteria).toEqual([{ title: "单测全绿", done: true }]);
      expect(g.ledger).toEqual({ paths: ["src/auth.rs"], programs: ["cargo"] });
      expect(g.status).toBe("executing");
      expect(useRun.getState().tabs["s1"].goalRev).toBe(1); // 推送代际 +1（作废在途回读）
      // goal = null = 目标已清除 / 已回落前档：整体覆盖，不留旧目标
      h["goal:update"]({ session: "s1", goal: null });
      expect(useRun.getState().tabs["s1"].goal).toBeNull();
    } finally {
      useRun.setState({ tabs: prevTabs } as any); // 不把状态泄露给本文件其余用例
    }
  });
});
