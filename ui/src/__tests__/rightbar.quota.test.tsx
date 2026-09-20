// 额度与余额段（[docs/quota-from-provider-config](../../../docs/quota-from-provider-config.md)）：
// 行集合 = CodeWave 供应商配置。用例覆盖五类行（ok / error / invalid / rejected / no_key / unsupported）、
// unsupported > 3 家折叠、标题消歧、窗口状态标签、灰行「去设置」跳转、空态文案、配置变化 debounce 重拉、
// 可见性门控，以及全部纯函数（风险分级 / 摘要 / 倒计时 / 消歧 / 折叠判定 / 状态归一 / 相对时间）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import type { ConfigState, QuotaEntry, QuotaSnapshot } from "../ipc/types";
import { EXPANDED_QUOTA_KEY } from "../utils/rightbarPrefs";
import { activeProviderIdOf } from "../utils/models";
import {
  countdown,
  entryLabel,
  riskLevel,
  summaryEntry,
  summaryRemaining,
  windowLabelKey,
} from "../features/quota/quotaFormat";
import {
  agoKind,
  disambiguateTitles,
  entryStatusKind,
  hostOf,
  shouldCollapseUnsupported,
  splitBySupport,
  unsupportedReason,
} from "../features/quota/quotaRow";

/** 供应商 uuid（行 id = uuid，不再是 kind 串） */
const P = {
  opencode: "11111111-1111-4111-8111-111111111111",
  deepseek: "22222222-2222-4222-8222-222222222222",
  kimi: "33333333-3333-4333-8333-333333333333",
  minimax: "44444444-4444-4444-8444-444444444444",
  glm: "55555555-5555-4555-8555-555555555555",
  extra: (n: number) => `99999999-9999-4999-8999-00000000000${n}`,
};

const soon = new Date(Date.now() + 2 * 86400_000 + 3600_000).toISOString();
const nowIso = () => new Date().toISOString();
/**
 * 「上次成功时间」构造：now - offsetMs。
 * 断言相对文案的固定值必须在**模块加载时**求值：fixture 是在请求返回时才构造的，而组件的 `now` 来自挂载那一刻，
 * 挂载早于构造 → 差值会比偏移量小几毫秒，`floor` 后会掉到「1 小时前」这种边界值上。
 */
const agoIso = (offsetMs: number) => new Date(Date.now() - offsetMs).toISOString();
const LAST_OK_2H = agoIso(2 * 3600_000);
const LAST_OK_5H = agoIso(5 * 3600_000);
const LAST_OK_25H = agoIso(25 * 3600_000);

const calls: { cmd: string; args: any }[] = [];
/** 每次 quota_snapshots 的响应（用例可整体替换） */
let respond: () => QuotaSnapshot[] = () => [];
let fail = false;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "list_skills" || cmd === "list_editors") return [];
    if (cmd === "quota_snapshots") {
      if (fail) throw new Error("network down");
      return respond();
    }
    return null;
  }),
}));

const quotaCalls = () => calls.filter((c) => c.cmd === "quota_snapshots");

function entry(over: Partial<QuotaEntry> = {}): QuotaEntry {
  return {
    key: "weekly",
    label: null,
    used_percent: 69,
    remaining_percent: 31,
    value_text: null,
    resets_at: soon,
    status: null,
    ...over,
  };
}

function snapshot(over: Partial<QuotaSnapshot> & { provider_id: string }): QuotaSnapshot {
  return {
    display_name: "供应商",
    status: "ok",
    reason: null,
    entries: [],
    error: null,
    key_source: "config",
    last_ok_at: null,
    fetched_at: nowIso(),
    ...over,
  };
}

/** 五类行的标准快照集合（顺序：可查询类在前、unsupported 殿后，与后端约定一致） */
const fiveKinds = (): QuotaSnapshot[] => [
  snapshot({
    provider_id: P.opencode,
    display_name: "OpenCode Go",
    status: "ok",
    key_source: "keyring",
    entries: [entry(), entry({ key: "monthly", used_percent: 34, remaining_percent: 66 })],
    // ok 行后端也会给 last_ok_at（= 本次成功时间），但界面**不展示**（避免与「X 分钟前更新」重复）
    last_ok_at: nowIso(),
  }),
  snapshot({
    provider_id: P.deepseek,
    display_name: "DeepSeek",
    status: "error",
    error: "HTTP 500",
    // error + last_ok_at = null：从未成功过 → 展开不补时间行（失败原因本身已足够）
    last_ok_at: null,
  }),
  snapshot({
    provider_id: P.kimi,
    display_name: "Kimi Code",
    status: "invalid",
    error: "key unreadable",
    last_ok_at: LAST_OK_25H,
  }),
  snapshot({
    provider_id: P.minimax,
    display_name: "MiniMax",
    status: "rejected",
    error: "403 forbidden",
    last_ok_at: null, // 配额文件里没有成功记录 → 展开回显「从未成功查询」
  }),
  snapshot({ provider_id: P.glm, display_name: "Zhipu GLM", status: "no_key", key_source: null }),
];

function provider(over: Partial<ConfigState["providers"][number]> & { id: string }) {
  return {
    name: "供应商",
    api_format: "openai_chat" as const,
    base_url: "https://api.example.com/v1",
    keys: ["sk-test"],
    models: [
      {
        id: `m-${over.id}`,
        model: "gpt-5",
        max_tokens: 4096,
        context_window: 128000,
        reasoning_effort: null,
        vision: false,
        video: false,
      },
    ],
    headers: [],
    ...over,
  };
}

const baseConfig: ConfigState = {
  schema_version: 2,
  providers: [
    provider({ id: P.opencode, name: "OpenCode Go", base_url: "https://api.opencode.ai/v1" }),
    provider({ id: P.deepseek, name: "DeepSeek", base_url: "https://api.deepseek.com/v1" }),
    provider({ id: P.kimi, name: "Kimi Code", base_url: "https://api.moonshot.cn/v1" }),
    provider({ id: P.minimax, name: "MiniMax", base_url: "https://api.minimax.chat/v1" }),
    provider({ id: P.glm, name: "Zhipu GLM", base_url: "https://open.bigmodel.cn/api/paas/v4" }),
  ],
  active_model_id: `m-${P.opencode}`,
  proxy: null,
  network: { allow_private_network: false },
  compact_threshold: 0.6,
  compact_timeout_seconds: 180,
  approval: {
    enabled: true,
    confirm_outside_create: true,
    confirm_git_push: true,
    auto_confirm: false,
    command_allowlist: [],
  },
  post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
  ui: { font_size: 15, accent: "ink", language: "zh-CN", font_sans: "", font_mono: "" },
  custom_prompt: null,
  disabled_skills: [],
  log: { level: "info", session_verbose: false },
  shell: { selection: null },
  sessions: { retention_days: null },
};

function seed(modelId: string | null = null) {
  useSessions.setState({
    tabs: [
      {
        key: "s1",
        sessionId: "s1",
        workspace: "D:/demo/project",
        title: "额度",
        projectId: null,
        createdAt: "2026-09-08T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: modelId, reasoning_effort: null },
      },
    ],
    activeKey: "s1",
    projects: [],
  });
  useSettings.setState({ config: baseConfig });
  useUi.setState({ rightBarOpen: true, rbTab: "info", settingsOpen: false, settingsHit: null });
}

const renderBar = () =>
  render(
    <AntApp>
      <RightBar />
    </AntApp>,
  );

/** 行所在的块（展开内容与行按钮同属 .rb-quota-provider） */
const blockOf = (el: HTMLElement): HTMLElement =>
  (el.closest(".rb-quota-provider") ?? el.closest(".rb-quota-row") ?? el) as HTMLElement;

afterEach(() => {
  cleanup();
  calls.length = 0;
  respond = () => [];
  fail = false;
  localStorage.clear();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useSettings.setState({ config: null });
  useUi.setState({ settingsOpen: false, settingsHit: null });
});

describe("额度段：五类行", () => {
  it("ok 行给最紧张窗口的摘要，点开显示全部窗口与重置倒计时", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const summary = await screen.findByRole("button", { name: /OpenCode Go/ });
    expect(summary.textContent).toContain("31%");
    expect(summary.textContent).not.toContain("66%");
    expect(summary.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(summary);
    await waitFor(() => expect(summary.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(summary);
    expect(block.textContent).toContain("本月");
    expect(block.textContent).toContain("66%");
    expect(block.textContent).toMatch(/\d+ 天 \d+ 小时后重置/);
    // 展开态记忆：落 uuid（kind 串不再进 localStorage）
    expect(JSON.parse(localStorage.getItem(EXPANDED_QUOTA_KEY)!)).toContain(P.opencode);
  });

  it("error / invalid 行折叠态带 `!`，展开显示原因与重试", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const failed = await screen.findByRole("button", { name: /DeepSeek/ });
    expect(within(failed).getByText("!")).toBeTruthy();
    fireEvent.click(failed);
    await waitFor(() => expect(failed.getAttribute("aria-expanded")).toBe("true"));
    expect(blockOf(failed).textContent).toContain("额度请求失败：HTTP 500");
    expect(within(blockOf(failed)).getByText("重试")).toBeTruthy();

    const invalid = await screen.findByRole("button", { name: /Kimi Code/ });
    fireEvent.click(invalid);
    await waitFor(() => expect(invalid.getAttribute("aria-expanded")).toBe("true"));
    expect(blockOf(invalid).textContent).toContain("密钥读取失败：key unreadable");
  });

  it("rejected 是降级灰行：可展开看原因 + 时间行（按定义从未成功过）+ 重试", async () => {
    // rejected = 401/403/404 **且从未成功过** → 后端 last_ok_at 恒为 null（只有这个组合产得出来）
    respond = () => [
      snapshot({
        provider_id: P.minimax,
        display_name: "MiniMax",
        status: "rejected",
        error: "403 forbidden",
        last_ok_at: null,
      }),
    ];
    seed();
    renderBar();

    const rejected = await screen.findByRole("button", { name: /MiniMax/ });
    await waitFor(() => expect(within(rejected).getByText(/查询被拒/)).toBeTruthy());
    expect(rejected.className).toContain("rb-quota-degraded");
    expect(rejected.getAttribute("aria-expanded")).toBe("false");
    // 折叠态不带时间行（明细只在展开内容里）
    expect(blockOf(rejected).textContent).not.toContain("上次成功");
    expect(blockOf(rejected).textContent).not.toContain("从未成功查询");

    fireEvent.click(rejected);
    await waitFor(() => expect(rejected.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(rejected);
    expect(block.textContent).toContain("额度请求失败：403 forbidden");
    // 按定义从未成功过 → 时间行给「从未成功查询」（不显示「上次成功」）
    expect(block.textContent).toContain("从未成功查询");
    expect(block.textContent).not.toContain("上次成功");
    expect(within(block).getByText("重试")).toBeTruthy();
  });

  it("rejected 意外带历史 last_ok_at 时优先显示「上次成功」（防御性；后端按定义产不出来）", async () => {
    respond = () => [
      snapshot({
        provider_id: P.minimax,
        display_name: "MiniMax",
        status: "rejected",
        error: "403 forbidden",
        last_ok_at: LAST_OK_2H,
      }),
    ];
    seed();
    renderBar();
    const rejected = await screen.findByRole("button", { name: /MiniMax/ });
    fireEvent.click(rejected);
    await waitFor(() => expect(rejected.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(rejected);
    expect(block.textContent).toContain("上次成功：2 小时前");
    expect(block.textContent).not.toContain("从未成功查询");
  });

  it("刷新两次仍显示后端给的 last_ok_at（数据源不再是会话内记忆）", async () => {
    // 第一次 ok（若走会话记忆，就会记成「刚刚」）；第二次 error（曾成功过 → 后端带历史 last_ok_at），
    // 后端持久化的事实是 5 小时前
    const okAt = agoIso(30_000);
    const persisted = LAST_OK_5H;
    let seq = 0;
    respond = () => {
      const first = seq++ === 0;
      return [
        first
          ? snapshot({
              provider_id: P.minimax,
              display_name: "MiniMax",
              status: "ok",
              entries: [entry()],
              fetched_at: okAt,
              last_ok_at: okAt,
            })
          : snapshot({
              provider_id: P.minimax,
              display_name: "MiniMax",
              status: "error",
              error: "HTTP 500",
              last_ok_at: persisted,
            }),
      ];
    };
    seed();
    renderBar();
    await screen.findByRole("button", { name: /MiniMax/ });

    fireEvent.click(screen.getByLabelText("刷新额度"));
    await waitFor(() => expect(quotaCalls().length).toBeGreaterThan(1));

    const row = await screen.findByRole("button", { name: /MiniMax/ });
    fireEvent.click(row);
    await waitFor(() => expect(row.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(row);
    expect(block.textContent).toContain("上次成功：5 小时前");
    expect(block.textContent).not.toContain("刚刚");
  });

  it("error 行带历史 last_ok_at → 展开显示「上次成功：X 小时前」（判据按数据而非行态）", async () => {
    // 曾成功过、这次 401/403/404 被后端判为 error 并带历史 last_ok_at：
    // 这是真实数据里最常见的一类时间行（旧实现只认 rejected/invalid，这行几乎永不出现）
    respond = () => [
      snapshot({
        provider_id: P.deepseek,
        display_name: "DeepSeek",
        status: "error",
        error: "HTTP 401",
        last_ok_at: LAST_OK_2H,
      }),
    ];
    seed();
    renderBar();
    const failed = await screen.findByRole("button", { name: /DeepSeek/ });
    fireEvent.click(failed);
    await waitFor(() => expect(failed.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(failed);
    expect(block.textContent).toContain("额度请求失败：HTTP 401");
    expect(block.textContent).toContain("上次成功：2 小时前");
    expect(block.textContent).not.toContain("从未成功查询");
  });

  it("ok 行不显示「上次成功」行（避免与「X 分钟前更新」重复）", async () => {
    respond = fiveKinds; // ok 行的 last_ok_at = 本次成功时刻，界面仍不展示
    seed();
    renderBar();
    const ok = await screen.findByRole("button", { name: /OpenCode Go/ });
    fireEvent.click(ok);
    await waitFor(() => expect(ok.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(ok);
    expect(block.querySelector(".rb-quota-lastok")).toBeNull();
    expect(block.textContent).not.toContain("上次成功");
    expect(block.textContent).not.toContain("从未成功查询");
    expect(document.querySelectorAll(".rb-quota-lastok")).toHaveLength(0);
  });

  it("invalid 行有 last_ok_at 时回显；error 行 last_ok_at 为 null 时不显示这行", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const invalid = await screen.findByRole("button", { name: /Kimi Code/ });
    fireEvent.click(invalid);
    await waitFor(() => expect(invalid.getAttribute("aria-expanded")).toBe("true"));
    expect(blockOf(invalid).textContent).toContain("上次成功：1 天前");

    // 从未成功过（last_ok_at = null）的 error 行：不补时间行，也不误报「从未成功查询」
    const failed = await screen.findByRole("button", { name: /DeepSeek/ });
    fireEvent.click(failed);
    await waitFor(() => expect(failed.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(failed);
    expect(block.querySelector(".rb-quota-lastok")).toBeNull();
    expect(block.textContent).not.toContain("上次成功");
    expect(block.textContent).not.toContain("从未成功查询");
  });

  it("rejected 从未成功过（后端 last_ok_at = null）时回显「从未成功查询」", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const rejected = await screen.findByRole("button", { name: /MiniMax/ });
    fireEvent.click(rejected);
    await waitFor(() => expect(rejected.getAttribute("aria-expanded")).toBe("true"));
    expect(blockOf(rejected).textContent).toContain("从未成功查询");
  });

  it("no_key 是独立只读行（无展开语义），带「未配置密钥」标签", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const label = await screen.findByText("Zhipu GLM");
    const row = label.closest(".rb-quota-row") as HTMLElement;
    expect(within(row).getByText("未配置密钥")).toBeTruthy();
    // 只读行不是 button（不可展开），也没有 aria-expanded
    const summary = row.querySelector(".rb-quota-summary") as HTMLElement;
    expect(summary.tagName).toBe("DIV");
    expect(summary.getAttribute("aria-expanded")).toBeNull();
  });

  it("unsupported 灰行显示「不支持额度查询」与原因标签（空 base URL 走另一文案）", async () => {
    respond = () => [
      snapshot({
        provider_id: P.glm,
        display_name: "Zhipu GLM",
        status: "unsupported",
        reason: "no_adapter",
      }),
      snapshot({
        provider_id: P.opencode,
        display_name: "OpenCode Go",
        status: "unsupported",
        reason: "empty_base_url",
      }),
    ];
    seed();
    renderBar();
    const rows = await screen.findAllByText("不支持额度查询");
    expect(rows).toHaveLength(2);
    expect(screen.getByText("域名不在支持范围")).toBeTruthy();
    expect(screen.getByText("未填 base URL")).toBeTruthy();
  });

  it("旧版本写的 kind 串被过滤：合法 uuid 的展开态照常生效，且不回写脏值", async () => {
    respond = fiveKinds;
    localStorage.setItem(EXPANDED_QUOTA_KEY, JSON.stringify(["opencode-go", P.opencode]));
    seed();
    renderBar();
    const row = await screen.findByRole("button", { name: /OpenCode Go/ });
    // 磁盘里的合法 uuid 仍被恢复为展开态
    await waitFor(() => expect(row.getAttribute("aria-expanded")).toBe("true"));
    expect(blockOf(row).textContent).toContain("66%");
    // 脏值（kind 串）只被忽略，不被写回、也不被清掉
    expect(JSON.parse(localStorage.getItem(EXPANDED_QUOTA_KEY)!)).toEqual(["opencode-go", P.opencode]);
  });

  it("窗口级 status：受限类橙标、未知值原样灰显、ok 不显示标签；折叠摘要行不带标签", async () => {
    respond = () => [
      snapshot({
        provider_id: P.opencode,
        display_name: "OpenCode Go",
        entries: [
          entry({ key: "five_hour", remaining_percent: 3, status: "rate-limited" }),
          entry({ key: "monthly", remaining_percent: 66, status: "quota_paused" }),
          entry({ key: "weekly", remaining_percent: 50, status: "ok" }),
        ],
      }),
    ];
    seed();
    renderBar();
    const summary = await screen.findByRole("button", { name: /OpenCode Go/ });
    // 折叠态只给最紧张窗口的摘要，不带状态标签
    expect(summary.querySelectorAll(".rb-quota-status")).toHaveLength(0);

    fireEvent.click(summary);
    await waitFor(() => expect(summary.getAttribute("aria-expanded")).toBe("true"));
    const block = blockOf(summary);
    const limited = within(block).getByText("已限流");
    expect(limited.className).toContain("risk-warn");
    // 未知值 forward-compatible：原样显示（灰显、不崩）
    const unknown = within(block).getByText("quota_paused");
    expect(unknown.className).toBe("rb-quota-status");
    // status = ok 的窗口不显示标签
    expect(block.querySelectorAll(".rb-quota-status")).toHaveLength(2);
  });
});

describe("额度段：unsupported 折叠", () => {
  const unsupportedPayload = (n: number): QuotaSnapshot[] => [
    ...Array.from({ length: n }, (_, i) =>
      snapshot({
        provider_id: P.extra(i + 1),
        display_name: `Unsupported ${i + 1}`,
        status: "unsupported",
        reason: "no_adapter",
      }),
    ),
    snapshot({ provider_id: P.glm, display_name: "Zhipu GLM", status: "no_key", key_source: null }),
    snapshot({ provider_id: P.minimax, display_name: "MiniMax", status: "rejected", error: "403" }),
  ];

  it("多于 3 家时默认折叠为一行汇总，点击展开/收起全部", async () => {
    respond = () => unsupportedPayload(4);
    seed();
    renderBar();
    const collapsed = await screen.findByRole("button", { name: /另有 4 家不支持额度查询/ });
    expect(collapsed.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("Unsupported 1")).toBeNull();
    // no_key / rejected 不参与折叠，各自独立成行
    expect(screen.getByText("Zhipu GLM")).toBeTruthy();
    expect(screen.getByText("MiniMax")).toBeTruthy();

    fireEvent.click(collapsed);
    await waitFor(() => expect(collapsed.getAttribute("aria-expanded")).toBe("true"));
    expect(screen.getByText("Unsupported 1")).toBeTruthy();
    expect(screen.getByText("Unsupported 4")).toBeTruthy();

    fireEvent.click(collapsed);
    await waitFor(() => expect(collapsed.getAttribute("aria-expanded")).toBe("false"));
    expect(screen.queryByText("Unsupported 1")).toBeNull();
  });

  it("3 家以内不折叠（逐条列出，没有汇总行）", async () => {
    respond = () => unsupportedPayload(3);
    seed();
    renderBar();
    await screen.findByText("Unsupported 3");
    expect(screen.queryByText(/另有 \d+ 家不支持额度查询/)).toBeNull();
  });
});

describe("额度段：标题消歧与来源 tooltip", () => {
  it("同名两家追加 base_url 主机名；同名同主机再追加序号", async () => {
    respond = () => [
      snapshot({ provider_id: P.extra(1), display_name: "OpenAI", entries: [entry()] }),
      snapshot({ provider_id: P.extra(2), display_name: "OpenAI", entries: [entry()] }),
      snapshot({ provider_id: P.extra(3), display_name: "MiniMax", entries: [entry()] }),
      snapshot({ provider_id: P.extra(4), display_name: "MiniMax", entries: [entry()] }),
    ];
    useSessions.setState({
      tabs: [
        {
          key: "s1",
          sessionId: "s1",
          workspace: "D:/demo/project",
          title: "额度",
          projectId: null,
          createdAt: "2026-09-08T00:00:00Z",
          prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
        },
      ],
      activeKey: "s1",
      projects: [],
    });
    useSettings.setState({
      config: {
        ...baseConfig,
        providers: [
          provider({ id: P.extra(1), name: "OpenAI", base_url: "https://api.a.example.com/v1" }),
          provider({ id: P.extra(2), name: "OpenAI", base_url: "https://api.b.example.com/v1" }),
          provider({ id: P.extra(3), name: "MiniMax", base_url: "https://api.minimax.chat/v1" }),
          provider({ id: P.extra(4), name: "MiniMax", base_url: "https://api.minimax.chat/v1" }),
        ],
      },
    });
    useUi.setState({ rightBarOpen: true, rbTab: "info", settingsOpen: false, settingsHit: null });
    renderBar();

    expect(await screen.findByText("OpenAI · api.a.example.com")).toBeTruthy();
    expect(screen.getByText("OpenAI · api.b.example.com")).toBeTruthy();
    expect(screen.getByText("MiniMax · api.minimax.chat")).toBeTruthy();
    expect(screen.getByText("MiniMax · api.minimax.chat (2)")).toBeTruthy();
  });

  it("名字悬浮显示密钥来源；两种来源都不再出现 env:/auth.json/jsonc 字样", async () => {
    respond = () => [
      snapshot({ provider_id: P.opencode, display_name: "OpenCode Go", key_source: "keyring", entries: [entry()] }),
      snapshot({ provider_id: P.deepseek, display_name: "DeepSeek", key_source: "config", entries: [entry()] }),
    ];
    seed();
    renderBar();
    const name = await screen.findByText("OpenCode Go");
    fireEvent.mouseEnter(name);
    expect(await screen.findByText("密钥来源：系统钥匙串")).toBeTruthy();

    fireEvent.mouseEnter(screen.getByText("DeepSeek"));
    expect(await screen.findByText("密钥来源：配置文件")).toBeTruthy();

    expect(document.body.textContent).not.toMatch(/auth\.json|jsonc|env:/);
  });
});

describe("额度段：去设置跳转", () => {
  it("no_key / rejected / unsupported 三类灰行给行内跳转按钮（锚点 = 该供应商）", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const noKeyRow = (await screen.findByText("Zhipu GLM")).closest(".rb-quota-row") as HTMLElement;
    const go = within(noKeyRow).getByLabelText("去设置");
    // 行本体是 button，跳转按钮必须是独立元素（同级、不被嵌套）
    expect(go.closest("button.rb-quota-summary")).toBeNull();
    fireEvent.click(go);
    expect(useUi.getState().settingsHit?.anchorId).toBe(`providers.${P.glm}`);
    expect(useUi.getState().settingsOpen).toBe(true);
  });

  it("unsupported 灰行的跳转锚点指向该供应商", async () => {
    respond = () => [
      snapshot({ provider_id: P.glm, display_name: "Zhipu GLM", status: "unsupported", reason: "no_adapter" }),
    ];
    seed();
    renderBar();
    const row = (await screen.findByText("Zhipu GLM")).closest(".rb-quota-row") as HTMLElement;
    fireEvent.click(within(row).getByLabelText("去设置"));
    expect(useUi.getState().settingsHit?.anchorId).toBe(`providers.${P.glm}`);
  });

  it("一家供应商都没配置时给空态文案 + 去设置按钮（不再列举凭证链）", async () => {
    respond = () => [];
    seed();
    renderBar();
    await screen.findByText(/额度按你在 CodeWave 中配置的供应商显示/);
    expect(document.body.textContent).not.toMatch(/auth\.json|jsonc|OPENCODE_API_KEY/);
    fireEvent.click(screen.getByLabelText("去设置"));
    expect(useUi.getState().settingsHit?.anchorId).toBe("providers");
  });
});

describe("额度段：刷新时机", () => {
  it("IPC 入参 = 当前会话生效模型所属供应商 id", async () => {
    respond = fiveKinds;
    seed(`m-${P.kimi}`);
    renderBar();
    await waitFor(() => expect(quotaCalls().length).toBeGreaterThan(0));
    expect(quotaCalls()[0].args.activeProviderId).toBe(P.kimi);
    // 纯函数口径一致：会话覆盖优先于全局活跃模型
    expect(activeProviderIdOf(baseConfig, `m-${P.kimi}`)).toBe(P.kimi);
    expect(activeProviderIdOf(baseConfig, null)).toBe(P.opencode);
  });

  it("供应商配置变化 debounce 重拉；无关配置变化不重拉", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    await waitFor(() => expect(quotaCalls().length).toBe(1));
    const before = quotaCalls().length;

    // 无关配置（代理）不该触发额度重拉
    useSettings.setState({ config: { ...baseConfig, proxy: { mode: "manual", url: "http://127.0.0.1:7890" } } });
    await new Promise((r) => setTimeout(r, 700));
    expect(quotaCalls().length).toBe(before);

    // base_url 变化 → debounce 后重拉
    useSettings.setState({
      config: {
        ...baseConfig,
        providers: baseConfig.providers.map((p) =>
          p.id === P.opencode ? { ...p, base_url: "https://api.opencode.ai/v2" } : p,
        ),
      },
    });
    await waitFor(() => expect(quotaCalls().length).toBeGreaterThan(before), { timeout: 2000 });
  });

  it("右栏关闭时不派发额度请求（可见性门控）", async () => {
    respond = fiveKinds;
    seed();
    useUi.setState({ rightBarOpen: false });
    renderBar();
    await new Promise((r) => setTimeout(r, 0));
    expect(quotaCalls()).toHaveLength(0);
  });

  it("请求失败显示错误与重试入口", async () => {
    fail = true;
    seed();
    renderBar();
    await screen.findByText(/额度请求失败/);
    const before = quotaCalls().length;
    fireEvent.click(screen.getByText("重试"));
    await waitFor(() => expect(quotaCalls().length).toBeGreaterThan(before));
  });

  it("「X 分钟前更新」贴在刷新按钮左侧（同一标签行）", async () => {
    respond = fiveKinds;
    seed();
    renderBar();
    const row = (await screen.findByText("额度与余额")).closest(".rb-label-row")!;
    const stamp = row.querySelector(".rb-quota-updated") as HTMLElement;
    const refresh = row.querySelector("button") as HTMLElement;
    expect(stamp.textContent).toMatch(/更新$/);
    expect(refresh.getAttribute("aria-label")).toBe("刷新额度");
    expect(stamp.compareDocumentPosition(refresh) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(document.querySelectorAll(".rb-quota-updated")).toHaveLength(1);
  });
});

describe("额度展示纯函数", () => {
  const base = Date.parse("2026-09-18T12:00:00Z");

  it("风险分级：<5% 红、<20% 橙、其余中性；数值行不参与分级", () => {
    expect(riskLevel(90)).toBe("normal");
    expect(riskLevel(19.9)).toBe("warn");
    expect(riskLevel(4.9)).toBe("danger");
    expect(riskLevel(null)).toBe("normal");
  });

  it("摘要取剩余最小的百分比行；纯数值行取第一条", () => {
    const entries = [
      entry({ key: "weekly", remaining_percent: 31 }),
      entry({ key: "monthly", remaining_percent: 66 }),
    ];
    expect(summaryEntry(entries)!.key).toBe("weekly");
    expect(summaryRemaining(entries)).toBe(31);
    const balance = [entry({ key: "balance_cny", remaining_percent: null, value_text: "CNY 12.50" })];
    expect(summaryEntry(balance)!.key).toBe("balance_cny");
    expect(summaryRemaining(balance)).toBeNull();
  });

  it("倒计时口径：天/小时/分钟/不足一分钟，过期或无值返回 null", () => {
    expect(countdown("2026-09-20T13:00:00Z", base)).toEqual({
      key: "rightbar.quotaResetDays",
      params: { d: 2, h: 1 },
    });
    expect(countdown("2026-09-18T15:30:00Z", base)).toEqual({
      key: "rightbar.quotaResetHours",
      params: { h: 3, m: 30 },
    });
    expect(countdown("2026-09-18T12:45:00Z", base)).toEqual({
      key: "rightbar.quotaResetMinutes",
      params: { m: 45 },
    });
    expect(countdown("2026-09-18T12:00:30Z", base)).toEqual({
      key: "rightbar.quotaResetSoon",
      params: {},
    });
    expect(countdown("2026-09-18T11:00:00Z", base)).toBeNull();
    expect(countdown(null, base)).toBeNull();
    expect(countdown("not-a-date", base)).toBeNull();
  });

  it("窗口标签：已知键走 i18n 映射，余额前缀归一，未知键原样显示", () => {
    expect(windowLabelKey("weekly")).toBe("weekly");
    expect(windowLabelKey("week")).toBe("weekly");
    expect(windowLabelKey("five_hour")).toBe("rolling");
    expect(windowLabelKey("balance_usd")).toBe("balance");
    expect(windowLabelKey("mystery")).toBeNull();
    expect(entryLabel(entry({ label: "Weekly limit", key: "limit_1" }), (k) => `t:${k}`)).toBe(
      "Weekly limit",
    );
    expect(entryLabel(entry({ key: "mystery" }), (k) => `t:${k}`)).toBe("mystery");
  });

  it("主机名提取容忍缺 scheme 的写法，拿不到返回 null", () => {
    expect(hostOf("https://api.example.com/v1")).toBe("api.example.com");
    expect(hostOf("api.example.com/v1")).toBe("api.example.com");
    expect(hostOf("  https://api.example.com  ")).toBe("api.example.com");
    expect(hostOf("")).toBeNull();
    expect(hostOf(null)).toBeNull();
    expect(hostOf("not a url")).toBeNull();
  });

  it("标题消歧：名字唯一原样；同名加主机名；同名同主机追加序号；无主机名不编号", () => {
    const hosts = new Map<string, string | null>([
      ["a", "api.a.com"],
      ["b", "api.b.com"],
      ["c", "api.a.com"],
      ["d", null],
      ["e", null],
    ]);
    const titles = disambiguateTitles(
      [
        snapshot({ provider_id: "only", display_name: "Solo" }),
        snapshot({ provider_id: "a", display_name: "Dup" }),
        snapshot({ provider_id: "b", display_name: "Dup" }),
        snapshot({ provider_id: "c", display_name: "Dup" }),
        snapshot({ provider_id: "d", display_name: "NoHost" }),
        snapshot({ provider_id: "e", display_name: "NoHost" }),
      ],
      (id) => hosts.get(id) ?? null,
    );
    expect(titles.get("only")).toBe("Solo");
    expect(titles.get("a")).toBe("Dup · api.a.com");
    expect(titles.get("b")).toBe("Dup · api.b.com");
    // 名字与主机名都相同（同家两账号）→ 按后端顺序追加序号
    expect(titles.get("c")).toBe("Dup · api.a.com (2)");
    // 拿不到主机名：跳过后缀，同组内仍按序号区分
    expect(titles.get("d")).toBe("NoHost");
    expect(titles.get("e")).toBe("NoHost (2)");
  });

  it("标题消歧：display_name 为空时回落到主机名 / provider_id", () => {
    const titles = disambiguateTitles(
      [
        snapshot({ provider_id: "x", display_name: "" }),
        snapshot({ provider_id: "y", display_name: "  " }),
      ],
      (id) => (id === "x" ? "api.x.com" : null),
    );
    expect(titles.get("x")).toBe("api.x.com");
    expect(titles.get("y")).toBe("y");
  });

  it("unsupported 折叠判定：3 家以内不折叠，4 家起折叠", () => {
    expect(shouldCollapseUnsupported(0)).toBe(false);
    expect(shouldCollapseUnsupported(3)).toBe(false);
    expect(shouldCollapseUnsupported(4)).toBe(true);
  });

  it("unsupported 原因：empty_base_url 单独一类，其余（含未知）按域名不在支持范围", () => {
    expect(unsupportedReason({ reason: "empty_base_url" })).toBe("empty_base_url");
    expect(unsupportedReason({ reason: "no_adapter" })).toBe("no_adapter");
    expect(unsupportedReason({ reason: null })).toBe("no_adapter");
    expect(unsupportedReason({ reason: "mystery" })).toBe("no_adapter");
  });

  it("窗口状态归一：受限类（大小写 / 下划线 / 连字符）归一，ok 与空值不显示，未知值算其它", () => {
    expect(entryStatusKind(null)).toBe("none");
    expect(entryStatusKind("")).toBe("none");
    expect(entryStatusKind("ok")).toBe("none");
    expect(entryStatusKind("OK")).toBe("none");
    expect(entryStatusKind("rate-limited")).toBe("limited");
    expect(entryStatusKind("rate_limited")).toBe("limited");
    expect(entryStatusKind("Rate-Limited")).toBe("limited");
    expect(entryStatusKind("exceeded")).toBe("limited");
    expect(entryStatusKind("blocked")).toBe("limited");
    expect(entryStatusKind("limited")).toBe("limited");
    expect(entryStatusKind("quota_paused")).toBe("other");
  });

  it("相对时间分级：无值 / 非法值 → null（后端用 null 表达「从未成功过」），其余按分钟/小时/天", () => {
    expect(agoKind(null, base)).toBeNull();
    expect(agoKind(undefined, base)).toBeNull();
    expect(agoKind("", base)).toBeNull();
    expect(agoKind("not-a-date", base)).toBeNull();
    expect(agoKind("2026-09-18T12:00:30Z", base)).toEqual({ kind: "just_now" });
    expect(agoKind("2026-09-18T11:30:00Z", base)).toEqual({ kind: "minutes", value: 30 });
    expect(agoKind("2026-09-18T02:00:00Z", base)).toEqual({ kind: "hours", value: 10 });
    expect(agoKind("2026-09-15T12:00:00Z", base)).toEqual({ kind: "days", value: 3 });
  });

  it("splitBySupport 保序：可查询类在前、unsupported 单独成段", () => {
    const split = splitBySupport([
      snapshot({ provider_id: "a", status: "ok" }),
      snapshot({ provider_id: "b", status: "unsupported" }),
      snapshot({ provider_id: "c", status: "no_key" }),
      snapshot({ provider_id: "d", status: "unsupported" }),
    ]);
    expect(split.main.map((s) => s.provider_id)).toEqual(["a", "c"]);
    expect(split.unsupported.map((s) => s.provider_id)).toEqual(["b", "d"]);
  });
});
