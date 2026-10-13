import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Button, theme } from "antd";
import { invoke } from "@tauri-apps/api/core";
import App from "../App";
import { AppearanceSettings } from "../features/panels/FontSettings";
import { SettingsThemeProvider } from "../features/panels/settings/SettingsTheme";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { applyInitialFonts, commitFontSizeSlot, reconcileFontsFromConfig, type FontSlot } from "../utils/fonts";
import { configSaved, configSnapshot } from "./configFixtures";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), Channel: class {} }));
// Keep production containers so CSS inheritance and antd token boundaries are exercised.
vi.mock("../features/shell/AppShell", () => ({ default: () => <>
  <TokenProbe name="app-size" />
  <Button data-testid="ui-button">Interface button</Button>
  <div className="chat-messages">
    <div className="assistant"><div className="md"><h1>Heading</h1><p>Body text</p><pre><code data-testid="code">let n = 1;</code></pre></div></div>
    <div className="tool-card"><div className="diff-line" data-testid="diff">+ line</div></div>
    <div className="user-bubble" data-testid="user-prose">User prose</div>
  </div>
  <div className="rb-log" data-testid="log">Log text</div>
  <SettingsThemeProvider><TokenProbe name="settings-size" /><AppearanceSettings /></SettingsThemeProvider>
</> }));

function TokenProbe({ name }: { name: string }) {
  return <output data-testid={name}>{theme.useToken().token.fontSize}</output>;
}

let stylesheet: HTMLStyleElement;
beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("style");
  stylesheet = document.createElement("style");
  stylesheet.textContent = ["native.css", "app.css"].map((file) =>
    readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme", file), "utf8")).join("\n");
  document.head.append(stylesheet);
  const snapshot = configSnapshot({ providers: [], ui: {
    font_sans: "", font_content: "", font_mono: "", font_size_sans: 14, font_size_content: 14, font_size_mono: 12, language: "zh-CN",
  } });
  useSettings.setState({ snapshot, config: snapshot.config, loaded: true });
  useUi.setState({ theme: "light", language: "zh-CN" });
  vi.mocked(invoke).mockImplementation(async (_command, args) => {
    const prefs = args as { fontSizeSans: number; fontSizeContent: number; fontSizeMono: number };
    const current = useSettings.getState().config!;
    return configSaved({ ...current, ui: { ...current.ui,
      font_size_sans: prefs.fontSizeSans, font_size_content: prefs.fontSizeContent, font_size_mono: prefs.fontSizeMono,
    } });
  });
});
afterEach(() => { cleanup(); stylesheet.remove(); vi.restoreAllMocks(); });

async function chooseSize(slot: FontSlot, label: string) {
  const input = document.querySelector(`[data-setting-id="ui.font_size_${slot}"] input`)!;
  fireEvent.mouseDown(input);
  const option = await waitFor(() => {
    const options = Array.from(document.querySelectorAll<HTMLElement>(".ant-select-item-option"));
    const found = options.find((el) => el.textContent === label);
    expect(found).toBeTruthy();
    return found!;
  });
  fireEvent.click(option);
}

// happy-dom resolves custom properties but preserves calc() rather than doing
// layout. Evaluate only the additive pixel expression in these typography rules.
function textSize(element: Element): number {
  const css = getComputedStyle(element).fontSize.replace(/calc|px|[()]/g, "");
  expect(css).toMatch(/^[\d.\s+-]+$/);
  let sign = 1;
  return (css.match(/[+-]|\d+(?:\.\d+)?/g) ?? []).reduce((sum, part) => {
    if (part === "+" || part === "-") { sign = part === "+" ? 1 : -1; return sum; }
    return sum + sign * Number(part);
  }, 0);
}

describe("font sizes in the runtime UI", () => {
  it("uses startup cache in the first App and settings tokens, then reconciles authoritative sizes", () => {
    // 旧语义键仍需被认得（向后兼容）：老用户 localStorage 里就是 small/large/xlarge
    localStorage.setItem("ws_font_size_sans", "xlarge");
    localStorage.setItem("ws_font_size_mono", "small");
    localStorage.setItem("ws_font_sans", "Cached Sans");
    localStorage.setItem("ws_font_mono", "Cached Mono");
    applyInitialFonts();
    render(<App />);
    expect(screen.getByTestId("app-size").textContent).toBe("19");
    expect(screen.getByTestId("settings-size").textContent).toBe("19");
    expect(document.documentElement.style.getPropertyValue("--ws-font-size-sans")).toBe("19px");
    act(() => { reconcileFontsFromConfig({ sans: "Saved Sans", content: "Saved Content", mono: "Saved Mono", font_size_sans: 17, font_size_content: 16, font_size_mono: 19 }); });
    expect(screen.getByTestId("app-size").textContent).toBe("17");
    expect(screen.getByTestId("settings-size").textContent).toBe("17");
    for (const [slot, label, px, family] of [["sans", "17px", "17px", "Saved Sans"], ["content", "16px", "16px", "Saved Content"], ["mono", "19px", "19px", "Saved Mono"]] as const) {
      const sizeField = document.querySelector(`[data-setting-id="ui.font_size_${slot}"]`)!;
      expect(sizeField.querySelector(".ant-select")?.textContent).toContain(label);
      const preview = sizeField.querySelector<HTMLElement>(".font-preview-row")!;
      expect(preview.style.fontSize).toBe(px);
      expect(preview.style.fontFamily).toContain(family);
      expect(document.querySelector<HTMLInputElement>(`[data-setting-id="ui.font_${slot}"] input`)?.value).toBe(family);
    }
  });

  it("sizes interface, prose and code independently, including code within content", () => {
    applyInitialFonts();
    render(<App />);
    const code = screen.getByTestId("code");
    const diff = screen.getByTestId("diff");
    const log = screen.getByTestId("log");
    const heading = screen.getByText("Heading");
    const sizes = () => [code, diff, log, heading].map(textSize);
    expect(sizes()).toEqual([12, 12, 12, 18.5]);
    expect(textSize(screen.getByTestId("user-prose"))).toBe(14);
    expect(textSize(screen.getByTestId("ui-button"))).toBe(14);
    act(() => { commitFontSizeSlot("mono", "13"); });
    expect(sizes()).toEqual([13, 13, 13, 18.5]);
    act(() => { commitFontSizeSlot("mono", "19"); });
    expect(sizes()).toEqual([19, 19, 19, 18.5]);
    act(() => { commitFontSizeSlot("sans", "19"); });
    expect(sizes()).toEqual([19, 19, 19, 18.5]);
    expect(textSize(screen.getByTestId("ui-button"))).toBe(19);
    expect(textSize(screen.getByTestId("user-prose"))).toBe(14);
    act(() => { commitFontSizeSlot("content", "20"); });
    expect(sizes()).toEqual([19, 19, 19, 24.5]);
    expect(textSize(screen.getByTestId("user-prose"))).toBe(20);
    expect(textSize(screen.getByTestId("ui-button"))).toBe(19);
    act(() => { commitFontSizeSlot("mono", "12"); commitFontSizeSlot("sans", "14"); commitFontSizeSlot("content", "14"); });
    expect(sizes()).toEqual([12, 12, 12, 18.5]);
  });

  it("keeps the latest size applied through a delayed older receipt, newer failure and retry", async () => {
    let releaseFirst!: (result: ReturnType<typeof configSaved>) => void;
    const first = new Promise<ReturnType<typeof configSaved>>((resolve) => { releaseFirst = resolve; });
    const notification = vi.spyOn(useUi.getState(), "notify").mockImplementation(() => {});
    vi.mocked(invoke).mockImplementationOnce(() => first)
      .mockRejectedValueOnce({ code: "InvalidConfiguration", message: "Save unavailable" });
    applyInitialFonts();
    render(<App />);
    await chooseSize("sans", "17px");
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(screen.getByTestId("app-size").textContent).toBe("17");
    expect(useSettings.getState().config?.ui.font_size_sans).toBe(14);
    await chooseSize("sans", "19px");
    expect(screen.getByTestId("app-size").textContent).toBe("19");
    const current = useSettings.getState().config!;
    await act(async () => { releaseFirst(configSaved({ ...current, ui: { ...current.ui, font_size_sans: 17 } })); });
    await screen.findByRole("button", { name: "重试・界面字号" });
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(notification).toHaveBeenCalled();
    expect(useSettings.getState().config?.ui.font_size_sans).toBe(17);
    expect(screen.getByTestId("app-size").textContent).toBe("19");
    expect(screen.getByTestId("settings-size").textContent).toBe("19");
    expect(document.documentElement.style.getPropertyValue("--ws-font-size-sans")).toBe("19px");
    fireEvent.click(screen.getByRole("button", { name: "重试・界面字号" }));
    await waitFor(() => expect(useSettings.getState().config?.ui.font_size_sans).toBe(19));
    expect(invoke).toHaveBeenLastCalledWith("set_font_prefs", expect.objectContaining({ fontSizeSans: 19, fontSizeContent: 14, fontSizeMono: 12 }));
    fireEvent.click(screen.getByRole("button", { name: "恢复默认・界面字号" }));
    expect(screen.getByTestId("app-size").textContent).toBe("14");
    await waitFor(() => expect(useSettings.getState().config?.ui.font_size_sans).toBe(14));
  });
});
