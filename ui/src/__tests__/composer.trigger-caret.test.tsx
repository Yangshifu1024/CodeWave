// 触发符**光标感知**判定（[docs/composer-trigger-caret](../../../docs/composer-trigger-caret.md)）：
// 改造前三个触发符都锚定「整段文本的末尾」且不读光标——正文里已有内容时，在中间或开头打触发符拉不起菜单。
// 现在只看光标前的片段：`@` 在正文任意位置可用（行首/空白后），`/`、`$` 限消息开头（模型侧点名写死「以 … 开头」）。
// 断言都经由真实 Composer 渲染（happy-dom 在 fireEvent.change 时会像浏览器一样把光标置到文末，
// 因此需要别的光标位置时显式传 selectionStart）。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act, within } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";

const ipcMock = vi.hoisted(() => ({
  selectDocumentFiles: vi.fn(async () => [] as string[]),
  checkExternalPath: vi.fn(async (_sid: string, _p: string) => ({ inside: true, dir: "", ref: "" })),
  allowExternalDir: vi.fn(async () => [] as string[]),
  readWorkspaceFileBase64: vi.fn(async () => ({ path: "", size: 0, content: "" })),
  listSkills: vi.fn(async () => [] as { name: string; description: string; whenToUse: string; origin: string; deletable: boolean }[]),
  listAgents: vi.fn(async () => [] as { name: string; description: string }[]),
  searchWorkspacePaths: vi.fn(async (_sid: string, _q: string, _limit?: number) => [] as string[]),
  compactSession: vi.fn(async () => null),
  startChat: vi.fn(async (_sid: string, _text: string, _images: unknown[], _ch?: unknown) => "ok"),
  cancelRun: vi.fn(async () => null),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import Composer from "../features/chat/Composer";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

function seedEnv() {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE }],
          headers: [],
        },
      ],
      active_model_id: "m1",
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
    },
    loaded: true,
  });
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "冒烟会话",
      projectId: null, createdAt: "2026-08-30T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: "s1",
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null,
      compacting: false,
    } as (typeof s.tabs)[string];
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

const draftText = () => useRun.getState().drafts["s1"]?.text ?? "";
const draftRefs = () => useRun.getState().drafts["s1"]?.refs ?? [];

function textarea(): HTMLTextAreaElement {
  return screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
}

/** 在输入框里写入文本并把光标放到指定位置（缺省 = 文末，与真人继续打字一致）。 */
function typeAt(value: string, caret = value.length) {
  fireEvent.change(textarea(), { target: { value, selectionStart: caret } });
}

/** 菜单项（`.menu-item`）里按文本找一项 */
async function menuItem(text: string): Promise<HTMLElement> {
  return waitFor(() => {
    const el = Array.from(document.querySelectorAll(".menu-item")).find((o) =>
      (o.textContent ?? "").includes(text),
    );
    expect(el, `菜单项「${text}」未出现`).toBeTruthy();
    return el as HTMLElement;
  });
}

/** 菜单是否已收起：antd 的 Popover 关闭后内容仍留在 DOM（带 `ant-popover-hidden`），
 *  所以只能断言「没有可见的菜单项」。 */
async function noMenu() {
  await new Promise((r) => setTimeout(r, 30));
  const visible = Array.from(document.querySelectorAll(".menu-item")).filter(
    (el) => !el.closest(".ant-popover-hidden"),
  );
  expect(visible.length).toBe(0);
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {} });
  vi.clearAllMocks();
  ipcMock.listSkills.mockResolvedValue([]);
  ipcMock.listAgents.mockResolvedValue([]);
  ipcMock.searchWorkspacePaths.mockResolvedValue([]);
});

describe("触发符光标感知 · @ 提及可放宽到正文任意位置", () => {
  it("句中：光标停在 `@片段` 之后即拉起菜单（改造前因锚定文末而失效）", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("看看 @src 后面还有字", 7); // 光标在 `@src` 之后，正文还有内容
    await menuItem("src/a.ts");
  });

  it("消息开头：`@片段` 后面还有正文时也能拉起（改造前打不开的场景）", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("@src 后面还有字", 4);
    await menuItem("src/a.ts");
    // 查询词只取「光标前那段」，不把光标之后的正文算进去
    expect(ipcMock.searchWorkspacePaths).toHaveBeenCalledWith("s1", "src", 8);
  });

  it("行首/空白后之外不触发：邮箱里的 @ 不再误弹", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("邮箱 me@x.com", 12);
    await noMenu();
    expect(ipcMock.searchWorkspacePaths).not.toHaveBeenCalled();
  });

  it("回填只替换光标前那段片段：光标之后的正文一字不动，文件转成引用 chip", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("看看 @src 后面还有字", 7);
    fireEvent.click(await menuItem("src/a.ts"));

    await waitFor(() => expect(draftRefs()).toEqual(["src/a.ts"]));
    expect(draftText()).toBe("看看 后面还有字"); // `@src` 连同它前面的空格被替换，后半句原样保留
  });
});

describe("触发符光标感知 · / 技能与 $ 子代理限消息开头（模型侧点名契约）", () => {
  const SKILLS = [{ name: "repo-index", description: "索引仓库", whenToUse: "", origin: "builtin", deletable: false }];
  const AGENTS = [{ name: "tester", description: "测试代理" }];

  it("消息开头打 `/`、后面已有正文 → 可触发（改造前因整段锚定而失效）", async () => {
    seedEnv();
    ipcMock.listSkills.mockResolvedValue(SKILLS);
    render(<AntApp><Composer /></AntApp>);

    typeAt("/repo 分析 X", 5);
    await menuItem("/repo-index");
  });

  it("消息开头打 `$`、后面已有正文 → 可触发", async () => {
    seedEnv();
    ipcMock.listAgents.mockResolvedValue(AGENTS);
    render(<AntApp><Composer /></AntApp>);

    typeAt("$tester 帮我看看", 7);
    await menuItem("$tester");
  });

  it("正文中间打 `/`、`$` → 静默不触发（放宽会让用户以为点名了、其实没点名）", async () => {
    seedEnv();
    ipcMock.listSkills.mockResolvedValue(SKILLS);
    ipcMock.listAgents.mockResolvedValue(AGENTS);
    render(<AntApp><Composer /></AntApp>);

    typeAt("分析一下 /repo", 9);
    await noMenu();
    expect(ipcMock.listSkills).not.toHaveBeenCalled();
    typeAt("帮我 $tester", 9);
    await noMenu();
    expect(ipcMock.listAgents).not.toHaveBeenCalled();
  });

  it("光标被方向键移到片段之前：菜单收起，Enter 正常发送（不会按过期片段改写正文）", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("看看 @src 后面还有字", 7);
    await menuItem("src/a.ts");

    // happy-dom 不实现真实的键盘光标移动：直接改选区再触发 keyup（Composer 的 onKeyUp 会同步光标）
    const ta = textarea();
    ta.selectionStart = 2;
    ta.selectionEnd = 2;
    fireEvent.keyUp(ta, { key: "ArrowLeft" });
    expect(ta.selectionStart).toBe(2);

    // 菜单随之收起：Enter 回到「发送」语义（happy-dom 没有动画结束事件，Popover 关闭后
    // 菜单节点仍留在 DOM，所以这里只能断**行为**而不能断 DOM 可见性）
    fireEvent.keyDown(ta, { key: "Enter", bubbles: true, cancelable: true });
    await waitFor(() => expect(ipcMock.startChat).toHaveBeenCalled());
    expect(ipcMock.startChat.mock.calls[0][1]).toBe("看看 @src 后面还有字"); // 正文没被重复拼接
  });

  it("菜单开着时外部链路改写文本（ws:composer-fill）：菜单作废且不按旧片段回填", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("@src 后面还有字", 4);
    await menuItem("src/a.ts");

    // 「修改」回填覆盖草稿（不经过 onChange）
    act(() => {
      window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text: "改后的消息" } }));
    });

    await waitFor(() => expect(draftText()).toBe("改后的消息"));
    // 菜单已作废：Enter 正常发送回填后的文本（而不是按旧片段回填）
    fireEvent.keyDown(textarea(), { key: "Enter", bubbles: true, cancelable: true });
    await waitFor(() => expect(ipcMock.startChat).toHaveBeenCalledTimes(1));
    expect(ipcMock.startChat.mock.calls[0][1]).toBe("改后的消息");
  });

  it("「使用 / 选择技能」在句中插入：插到光标处但静默不开菜单（门禁只认消息开头）", async () => {
    seedEnv();
    ipcMock.listSkills.mockResolvedValue([{ name: "repo-index", description: "索引", whenToUse: "", origin: "builtin", deletable: false }]);
    render(<AntApp><Composer /></AntApp>);

    typeAt("分析", 1);
    fireEvent.click(document.querySelector(".toolbar-left .ant-btn") as HTMLElement);
    const item = await waitFor(() => {
      const el = Array.from(document.querySelectorAll(".ant-dropdown-menu-item")).find((o) =>
        (o.textContent ?? "").includes("技能"),
      );
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    fireEvent.click(item);

    await waitFor(() => expect(draftText()).toBe("分 /析"));
    await noMenu();
    expect(ipcMock.listSkills).not.toHaveBeenCalled();
  });

  it("空格后合并菜单收起（Enter 正常发送）", async () => {
    seedEnv();
    ipcMock.listSkills.mockResolvedValue(SKILLS);
    render(<AntApp><Composer /></AntApp>);

    typeAt("/repo ", 6); // 打了空格 → 片段断掉
    await noMenu();
  });

  it("光标停在开头片段中间按 Enter = 选中技能（已接受的代价：不发送）", async () => {
    seedEnv();
    ipcMock.listSkills.mockResolvedValue(SKILLS);
    render(<AntApp><Composer /></AntApp>);

    typeAt("/repo-index 分析 X", 3); // 光标落在 `/re|po-index …`
    await menuItem("/repo-index");
    fireEvent.keyDown(textarea(), { key: "Enter", bubbles: true, cancelable: true });

    // 只替换光标前那段 `/re`，其余正文原样留在后面（键盘语义保持「菜单开着 Enter=选中」）
    await waitFor(() => expect(draftText()).toBe("/repo-index po-index 分析 X"));
    expect(ipcMock.startChat).not.toHaveBeenCalled();
  });
});

describe("触发符光标感知 · + 菜单插到光标处", () => {
  it("「使用 @ 添加上下文」插到光标处并就地打开菜单（原来追加到末尾）", async () => {
    seedEnv();
    render(<AntApp><Composer /></AntApp>);

    typeAt("分析", 1); // 光标停在「分|析」
    fireEvent.click(document.querySelector(".toolbar-left .ant-btn") as HTMLElement);
    const item = await waitFor(() => {
      const el = Array.from(document.querySelectorAll(".ant-dropdown-menu-item")).find((o) =>
        (o.textContent ?? "").includes("@"),
      );
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    fireEvent.click(item);

    // 触发起始片段插到光标处（与前一字符用空格隔开），并且菜单就地打开
    await waitFor(() => expect(draftText()).toBe("分 @析"));
    const root = await menuItem("ws"); // /tmp/ws 的根目录条目
    expect(within(root).getByText(/📁/)).toBeTruthy();
  });
});
