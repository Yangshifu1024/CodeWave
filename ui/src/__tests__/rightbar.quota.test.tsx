// 订阅额度段（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）：
// 多提供商摘要行 → 点开全部窗口（百分比行 / 数值行）、无凭证指引、失败态、面板关闭不发请求。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { EXPANDED_QUOTA_KEY } from "../utils/rightbarPrefs";
import {
  countdown,
  entryLabel,
  riskLevel,
  summaryEntry,
  summaryRemaining,
  windowLabelKey,
} from "../features/quota/quotaFormat";

const calls: { cmd: string; args: any }[] = [];
let mode: "ok" | "empty" | "error" = "ok";

const soon = new Date(Date.now() + 2 * 86400_000 + 3600_000).toISOString();

const payload = () => [
  {
    provider_id: "opencode-go",
    display_name: "OpenCode Go",
    status: "ok",
    entries: [
      { key: "weekly", label: null, used_percent: 69, remaining_percent: 31, value_text: null, resets_at: soon },
      { key: "monthly", label: null, used_percent: 34, remaining_percent: 66, value_text: null, resets_at: soon },
    ],
    error: null,
    credential_source: "auth.json",
    fetched_at: new Date().toISOString(),
  },
  {
    provider_id: "deepseek",
    display_name: "DeepSeek",
    status: "ok",
    entries: [
      { key: "balance_cny", label: null, used_percent: null, remaining_percent: null, value_text: "CNY 12.50", resets_at: null },
    ],
    error: null,
    credential_source: "env:DEEPSEEK_API_KEY",
    fetched_at: new Date().toISOString(),
  },
  {
    provider_id: "kimi-code",
    display_name: "Kimi Code",
    status: "ok",
    entries: [
      { key: "five_hour", label: null, used_percent: 97, remaining_percent: 3, value_text: null, resets_at: soon },
    ],
    error: null,
    credential_source: "auth.json",
    fetched_at: new Date().toISOString(),
  },
];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "list_skills" || cmd === "list_editors") return [];
    if (cmd === "quota_snapshots") {
      if (mode === "empty") return [];
      if (mode === "error") throw new Error("network down");
      return payload();
    }
    return null;
  }),
}));

function seed(open = true) {
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
  useUi.setState({ rightBarOpen: open, rbTab: "info" });
}

const renderBar = () =>
  render(
    <AntApp>
      <RightBar />
    </AntApp>,
  );

afterEach(() => {
  cleanup();
  calls.length = 0;
  mode = "ok";
  localStorage.clear();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

describe("右栏订阅额度段", () => {
  it("摘要行给最紧张窗口；点开显示全部窗口与重置倒计时", async () => {
    seed();
    renderBar();
    const summary = await screen.findByRole("button", { name: /OpenCode Go/ });
    // 摘要 = 剩余最小的窗口（weekly 31%）
    expect(summary.textContent).toContain("31%");
    expect(summary.textContent).not.toContain("66%");
    expect(summary.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(summary);
    await waitFor(() => expect(summary.getAttribute("aria-expanded")).toBe("true"));
    // 展开后两个窗口都在，且带倒计时
    const provider = summary.closest(".rb-quota-provider")!;
    expect(provider.textContent).toContain("本月");
    expect(provider.textContent).toContain("66%");
    // 倒计时格式（具体数值由纯函数用例钉住，此处只验「已渲染并可读」）
    expect(provider.textContent).toMatch(/\d+ 天 \d+ 小时后重置/);
    // 展开态记忆
    expect(JSON.parse(localStorage.getItem(EXPANDED_QUOTA_KEY)!)).toContain("opencode-go");
  });

  it("数值行（余额型提供商）直接给文本，不画进度条", async () => {
    seed();
    renderBar();
    const row = await screen.findByRole("button", { name: /DeepSeek/ });
    expect(row.textContent).toContain("CNY 12.50");
    expect(row.querySelector(".ant-progress")).toBeNull();
  });

  it("风险等级落到进度条包裹类，且填充元素是 antd 6 的 .ant-progress-track", async () => {
    seed();
    renderBar();
    // <5% → risk-danger（色彩由 app.css token 决定；这里钉住类名与真实填充元素，
    // 防「CSS 选择器写成已不存在的 .ant-progress-bg」这类死规则再次出现）
    const danger = await screen.findByRole("button", { name: /Kimi Code/ });
    const dangerBar = danger.querySelector(".rb-quota-bar")!;
    expect(dangerBar.classList.contains("risk-danger")).toBe(true);
    expect(danger.querySelector(".ant-progress-track")).toBeTruthy();
    // 正常区间（31%）不带风险类
    const normal = await screen.findByRole("button", { name: /OpenCode Go/ });
    expect(normal.querySelector(".rb-quota-bar")!.className).toBe("rb-quota-bar");
  });

  it("一家都没检测到凭证时显示配置指引（单行 + 悬浮）", async () => {
    mode = "empty";
    seed();
    renderBar();
    await screen.findByText(/未检测到订阅凭证/);
  });

  it("请求失败显示错误与重试入口", async () => {
    mode = "error";
    seed();
    renderBar();
    await screen.findByText(/额度请求失败/);
    expect(screen.getByText("重试")).toBeTruthy();
    // 重试会再次派发请求
    const before = calls.filter((c) => c.cmd === "quota_snapshots").length;
    fireEvent.click(screen.getByText("重试"));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "quota_snapshots").length).toBeGreaterThan(before),
    );
  });

  it("右栏关闭时不派发额度请求（可见性门控）", async () => {
    seed(false);
    renderBar();
    await new Promise((r) => setTimeout(r, 0));
    expect(calls.find((c) => c.cmd === "quota_snapshots")).toBeUndefined();
  });
});

describe("额度展示纯函数", () => {
  const base = Date.parse("2026-09-18T12:00:00Z");
  const entry = (over: Partial<import("../ipc/types").QuotaEntry>) => ({
    key: "rolling",
    label: null,
    used_percent: 10,
    remaining_percent: 90,
    value_text: null,
    resets_at: null,
    ...over,
  });

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
    // 厂商自带 label 优先于映射
    expect(
      entryLabel(entry({ label: "Weekly limit", key: "limit_1" }), (k) => `t:${k}`),
    ).toBe("Weekly limit");
    expect(entryLabel(entry({ key: "mystery" }), (k) => `t:${k}`)).toBe("mystery");
  });
});
