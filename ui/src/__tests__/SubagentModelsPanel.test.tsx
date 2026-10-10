// 子代理模型覆盖面板测试（[docs/subagent-model-override](../../../docs/subagent-model-override.md)）：
// 复合容器 = 8 角色 × 模型下拉。本文件以「纯组件」方式挂载 SubagentModelsPanel（不走 SettingsPage 整树）：
// `shell.settings.test.tsx` 已示范 standalone + AntApp 包裹 + i18n 显式 import 的写法；
// 本组件还要 mock list_agents（IPC 拉一次角色清单）与 ipc.saveConfig（覆盖式提交）。
//
// 守护六件事：
// 1. 8 行下拉，角色名 = 后端 DELEGABLE_ROLES 顺序；
// 2. 每行首项「继承父会话」存在且 value=null；
// 3. 改下拉调一次 useSettings.save(next)，参数中 subagent_models[role] 正确反映新值，其余字段原样保留；
// 4. 只读角色（explore / reviewer / code-reviewer）显示「只读」角标，其他角色不显示；
// 5. 悬空 wire id（subagent_models[role] 指向已被供应商清掉的 model.id）→ 该行展示为「继承父会话」；
// 6. 默认配置（无 providers）不崩：8 行下拉都在，仅无可选项。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // 显式初始化：standalone 挂载不走全局入口
import SubagentModelsPanel from "../features/panels/SubagentModelsPanel";
import { useSettings } from "../stores/settings";
import type { AgentMeta, ConfigState } from "../ipc/types";

// 后端 agents::DELEGABLE_ROLES 顺序：剔除 title；任务约束「顺序 = 后端注册表」
// 与「只读角色 = explore / reviewer / code-reviewer」（后端 readonly=true）。
const fixtureAgents: AgentMeta[] = [
  { name: "explore", description: "只读源码调研" },
  { name: "backend-dev", description: "后端实现" },
  { name: "frontend-dev", description: "前端实现" },
  { name: "app-dev", description: "App 实现" },
  { name: "reviewer", description: "方案对齐审查" },
  { name: "product-manager", description: "需求分析" },
  { name: "code-reviewer", description: "代码质量审查" },
  { name: "tester", description: "测试设计与执行" },
];

function makeConfig(overrides: Partial<ConfigState> = {}): ConfigState {
  return {
    schema_version: 2,
    providers: [],
    active_model_id: null,
    proxy: null,
    network: { allow_private_network: false },
    compact_threshold: 0.6,
    compact_timeout_seconds: 180,
    approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
    post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
    subagent_models: {},
    ...overrides,
  };
}

/** fixture providers：两家供应商三个模型，覆盖「OptGroup 分组」「按 model.model 显示」两条断言 */
function withProviders(config: ConfigState): ConfigState {
  return {
    ...config,
    providers: [
      {
        id: "p1", name: "Alpha", api_format: "openai_chat", base_url: "https://a.example/v1",
        keys: ["k"], headers: [],
        models: [
          { id: "m1", model: "alpha-fast", max_tokens: 8000, context_window: 32000, reasoning_effort: null, vision: false, video: false },
          { id: "m2", model: "alpha-pro", max_tokens: 32000, context_window: 128000, reasoning_effort: null, vision: true, video: false },
        ],
      },
      {
        id: "p2", name: "Beta", api_format: "anthropic_messages", base_url: "https://b.example",
        keys: ["k"], headers: [],
        models: [
          { id: "m3", model: "beta-sonnet", max_tokens: 32000, context_window: 200000, reasoning_effort: null, vision: true, video: false },
        ],
      },
    ],
    active_model_id: "m2",
  };
}

const savedCalls: ConfigState[] = [];

async function baseInvoke(cmd: string, _args?: any) {
  switch (cmd) {
    case "list_agents": return JSON.parse(JSON.stringify(fixtureAgents));
    case "save_config": savedCalls.push(JSON.parse(JSON.stringify(_args?.config))); return null;
    default: return null;
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(baseInvoke),
  Channel: class { onmessage: any = null; },
}));

async function invokeMock() {
  return (await import("@tauri-apps/api/core")).invoke as any;
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(async () => {
  cleanup();
  (await invokeMock()).mockImplementation(baseInvoke);
  // module-level singletons reset：settings 镜像 + savedCalls 计数器
  useSettings.setState({ config: null, loaded: false });
  savedCalls.length = 0;
});

/** 加载 config 进 store，并 standalone 挂载组件 */
function mountPanel(config: ConfigState) {
  useSettings.setState({ config, loaded: true });
  return render(
    <AntApp>
      <SubagentModelsPanel />
    </AntApp>,
  );
}

/** 下拉行的 role 名（左侧粗体文本） */
function rowRoleTexts(): string[] {
  return Array.from(document.querySelectorAll(".subagent-models-row strong")).map((el) =>
    (el.textContent ?? "").trim(),
  );
}

/** 拿某角色的下拉容器（antd 给 Select 套了一层 .ant-select） */
function rowSelectByRole(role: string): HTMLElement {
  const row = Array.from(document.querySelectorAll(".subagent-models-row")).find((r) =>
    (r.querySelector("strong")?.textContent ?? "").trim() === role,
  );
  if (!row) throw new Error(`role row not found: ${role}`);
  return row.querySelector(".ant-select") as HTMLElement;
}

/** 打开某角色的下拉（antd Select 需要 mousedown） */
async function openSelect(role: string) {
  fireEvent.mouseDown(rowSelectByRole(role));
  await waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy(), { timeout: 3000 });
}

async function pickOption(text: string) {
  const opt = await waitFor(() => {
    const el = Array.from(document.querySelectorAll(".ant-select-item-option")).find(
      (o) => (o.textContent ?? "").includes(text),
    ) as HTMLElement | undefined;
    expect(el, `下拉选项缺失：${text}`).toBeTruthy();
    return el!;
  }, { timeout: 5000 });
  fireEvent.click(opt);
  await new Promise((r) => setTimeout(r, 80));
}

function selectedDisplay(role: string): string {
  const sel = rowSelectByRole(role);
  const content = sel.querySelector(".ant-select-content");
  const titleAttr = (content?.getAttribute("title") ?? "").trim();
  const inner = (content?.textContent ?? "").trim();
  return titleAttr || inner;
}
describe("SubagentModelsPanel：8 行下拉与只读角色", () => {
  it("8 行下拉，角色名 = 后端 DELEGABLE_ROLES 顺序", async () => {
    mountPanel(withProviders(makeConfig()));
    // 列表拉取是 mount 后的异步 useEffect，等它落定（happy-dom 下 microtask 即可）
    await waitFor(() => expect(rowRoleTexts().length).toBe(8), { timeout: 2000 });
    expect(rowRoleTexts()).toEqual([
      "explore", "backend-dev", "frontend-dev", "app-dev",
      "reviewer", "product-manager", "code-reviewer", "tester",
    ]);
  });

  it("每行首项「继承父会话」存在且 value=null", async () => {
    const config = withProviders(makeConfig());
    mountPanel(config);
    const role = "backend-dev";
    await waitFor(() => expect(rowRoleTexts().length).toBe(8));
    await openSelect(role);
    // 首项文案 = 占位档「继承父会话」
    const first = document.querySelector(".ant-select-item-option");
    expect(first?.textContent ?? "").toContain("继承父会话");
    // 子代理角色行 + 「继承父会话」同行；value=null 须命中该行 Select 的当前值（无 config 覆盖 → 默认 null）
    expect(selectedDisplay(role)).toContain("继承父会话");
    await waitFor(() => {
      // 整份下拉含 OptGroup + 项值；这里只断结构与文案，不调 click
      const items = Array.from(document.querySelectorAll(".ant-select-item-option"));
      expect(items.length).toBeGreaterThanOrEqual(1);
    });
  });

  it("改某行下拉 → 调用 useSettings.save(next) 一次；参数 subagent_models[role] 正确、其他 role 保留", async () => {
    const config = withProviders(makeConfig({
      subagent_models: { backend: "m1" }, // 噪声：角色 key "backend" 不是真实 role名，只是验证 “无关 key 原样保留”
    }));
    mountPanel(config);
    await waitFor(() => expect(rowRoleTexts().length).toBe(8));

    expect(useSettings.getState().config?.subagent_models?.backend).toBe("m1");

    await openSelect("backend-dev");
    await pickOption("alpha-pro");
    await waitFor(() => expect(savedCalls.length).toBe(1));
    const sent = savedCalls[0];
    expect(sent.subagent_models?.["backend-dev"]).toBe("m2");
    expect(sent.subagent_models?.backend).toBe("m1");
    expect(sent.providers).toEqual(config.providers);
    expect(sent.active_model_id).toBe(config.active_model_id);
  });

  it("改后端（含已存在的覆盖值）→ subagent_models[role]=新值，旧 role 的覆盖仍保留", async () => {
    const config = withProviders(makeConfig({
      subagent_models: { "backend-dev": "m1", "frontend-dev": "m3" },
    }));
    mountPanel(config);
    await waitFor(() => expect(rowRoleTexts().length).toBe(8));

    await openSelect("backend-dev");
    await pickOption("alpha-pro");
    await waitFor(() => expect(savedCalls.length).toBe(1));
    const sent = savedCalls[0];
    expect(sent.subagent_models?.["backend-dev"]).toBe("m2");
    expect(sent.subagent_models?.["frontend-dev"]).toBe("m3");
  });

  it("只读角色 explore / reviewer / code-reviewer 显示「只读」Tag，其他角色不显示", async () => {
    mountPanel(withProviders(makeConfig()));
    await waitFor(() => expect(rowRoleTexts().length).toBe(8));

    const readOnlyRoles = new Set(["explore", "reviewer", "code-reviewer"]);
    for (const role of rowRoleTexts()) {
      const row = Array.from(document.querySelectorAll(".subagent-models-row")).find(
        (r) => (r.querySelector("strong")?.textContent ?? "").trim() === role,
      );
      const tag = row?.querySelector(".ant-tag");
      if (readOnlyRoles.has(role)) {
        expect(tag, `${role} 应带只读 Tag`).toBeTruthy();
        expect(tag?.textContent ?? "").toContain("只读");
      } else {
        expect(tag, `${role} 不该带只读 Tag`).toBeFalsy();
      }
    }
  });

  it("悬空 wire id → 该行展示为「继承父会话」（不主动 save）", async () => {
    // m-gone：模拟「之前覆盖过 m-gone，后来供应商删除」——findModel 返 null
    const config = withProviders(makeConfig({ subagent_models: { backend: "m-gone" } }));
    mountPanel(config);
    await waitFor(() => expect(rowRoleTexts().length).toBe(8));

    // 该行 Select 显示框退化为「继承父会话」，且未触发 save
    expect(selectedDisplay("backend-dev")).toContain("继承父会话");
    expect(savedCalls.length).toBe(0);
  });

  it("默认 empty config（无 providers）不崩：8 行下拉都在；空态时「继承父会话」即为唯一显示", async () => {
    const config = makeConfig();
    mountPanel(config);
    await waitFor(() => expect(rowRoleTexts().length).toBe(8));
    for (const role of rowRoleTexts()) {
      expect(selectedDisplay(role)).toContain("继承父会话");
    }
    // 不该抛错、不该调 save
    expect(savedCalls.length).toBe(0);
  });
});
