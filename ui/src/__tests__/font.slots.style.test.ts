// Verify independent font slots on shared CSS surfaces. No GUI automation:
// happy-dom resolves font variables, while calc() is evaluated here.
import { beforeEach, afterEach, describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..");
const nativeCss = readFileSync(join(SRC, "theme/native.css"), "utf8");
const appCss = readFileSync(join(SRC, "theme/app.css"), "utf8");
const settingsCss = readFileSync(join(SRC, "features/panels/settings/settings-theme.css"), "utf8");
let style: HTMLStyleElement;
let fixture: HTMLDivElement;

beforeEach(() => {
  document.documentElement.removeAttribute("style");
  style = document.createElement("style");
  style.textContent = `${nativeCss}\n${appCss}\n${settingsCss}`;
  document.head.append(style);
  fixture = document.createElement("div");
  fixture.innerHTML = `
    <div class="project-row" data-test="project">Project</div>
    <div class="session-nav-row" data-test="session">Session</div>
    <div class="chat-messages">
      <div class="user-bubble md" data-test="user">User <code data-test="user-code">a()</code></div>
      <div class="assistant"><div class="md" data-test="assistant">
        <h1 data-test="heading">Heading</h1><p>Answer <code data-test="inline">b()</code></p>
        <div class="code-wrap"><pre data-test="pre"><code data-test="code">let a = 1</code></pre><button class="code-copy" data-test="copy">Copy</button></div>
      </div></div>
      <div class="tool-card" data-test="tool"><code class="tool-name" data-test="tool-name">Tool name</code>Tool status</div>
      <div class="thinking-block"><div class="thinking-body" data-test="thinking">Reasoning</div></div>
    </div>
    <div class="sub-drawer-stream"><div class="md" data-test="sub">Report</div></div>
    <div class="plan-body md" data-test="plan">Plan</div>
    <div class="updater-notes-body md" data-test="notes">Release notes</div>
    <div class="skill-body md" data-test="skill">Skill</div>
    <div class="viewer-md md" data-test="document">Document</div>
    <pre class="code-block viewer-text" data-test="text">Text preview</pre>
    <div class="tool-card"><div class="diff-line" data-test="diff">+ source</div></div>
    <pre class="rb-log" data-test="log">Log</pre>
    <div class="sub-card"><span class="sub-card-meta" data-test="sub-meta">3s · 120 tokens</span></div>
    <span class="ctx-popover-value" data-test="context">120 / 1000</span>
    <span class="rb-quota-value rb-mono" data-test="quota">25%</span>
    <div class="settings-theme-root" style="--settings-field-title-size: calc(14px + var(--ws-font-size-sans-offset)); --settings-metadata-size: calc(12px + var(--ws-font-size-sans-offset))">
      <span class="settings-provider-model-limits" data-test="model-limits">128k / 16k</span>
      <span class="settings-provider-model-name" data-test="model-name">Model name</span>
      <span class="settings-provider-endpoint" data-test="endpoint">https://provider.example</span>
      <span class="mcp-tool-name" data-test="mcp-name">list_files</span>
      <div class="proxy-mode-echo" data-test="proxy">Proxy detected</div>
    </div>`;
  document.body.append(fixture);
});

afterEach(() => {
  fixture.remove();
  style.remove();
  document.documentElement.removeAttribute("style");
});

function element(name: string): HTMLElement {
  return fixture.querySelector<HTMLElement>(`[data-test="${name}"]`)!;
}

function size(name: string): number {
  const css = getComputedStyle(element(name)).fontSize.replace(/calc|px|[()]/g, "");
  expect(css).toMatch(/^[\d.\s+-]+$/);
  let sign = 1;
  return (css.match(/[+-]|\d+(?:\.\d+)?/g) ?? []).reduce((sum, part) => {
    if (part === "+" || part === "-") { sign = part === "+" ? 1 : -1; return sum; }
    return sum + sign * Number(part);
  }, 0);
}

const CONTENT = ["user", "assistant", "thinking", "sub", "plan", "notes", "skill", "document", "text"];
const CODE = ["user-code", "inline", "pre", "code", "diff", "log"];
const METADATA = [
  ["sub-meta", 12], ["context", 12], ["quota", 11.5], ["model-limits", 12],
  ["model-name", 14], ["endpoint", 12], ["mcp-name", 14], ["proxy", 12],
] as const;

describe("independent interface/content/code typography", () => {
  it("uses 14px interface/content and 12px code without implicit navigation or chat offsets", () => {
    for (const name of ["project", "session", ...CONTENT]) expect(size(name), name).toBe(14);
    expect(size("tool")).toBe(12.5);
    expect(size("tool-name")).toBe(12.5);
    for (const name of CODE) expect(size(name), name).toBe(12);
    for (const [name, expected] of METADATA) expect(size(name), name).toBe(expected);
    expect(appCss).not.toContain("--ws-font-size-sans-offset-nav");
    expect(nativeCss).not.toContain("--ws-font-size-sans-offset-nav");
  });

  it("content changes affect all prose surfaces and headings while controls and code retain their own sizes", () => {
    document.documentElement.style.setProperty("--ws-font-size-content", "20px");
    for (const name of CONTENT) expect(size(name), name).toBe(20);
    expect(size("heading")).toBeGreaterThan(20);
    for (const name of CODE) expect(size(name), name).toBe(12);
    expect(size("tool")).toBe(12.5);
    expect(size("project")).toBe(14);
    expect(size("copy")).toBe(11);
    expect(size("tool-name")).toBe(12.5);
    for (const [name, expected] of METADATA) expect(size(name), name).toBe(expected);
  });

  it("interface changes affect navigation, tool status and copy controls without resizing prose or code", () => {
    document.documentElement.style.setProperty("--ws-font-size-sans", "18px");
    expect(size("project")).toBe(18);
    expect(size("session")).toBe(18);
    expect(size("tool")).toBe(16.5);
    expect(size("tool-name")).toBe(16.5);
    expect(size("copy")).toBe(15);
    for (const [name, expected] of METADATA) expect(size(name), name).toBe(expected + 4);
    for (const name of CONTENT) expect(size(name), name).toBe(14);
    for (const name of CODE) expect(size(name), name).toBe(12);
  });

  it("code changes include inline code, fenced blocks, diff and logs without resizing prose", () => {
    document.documentElement.style.setProperty("--ws-font-size-mono", "19px");
    for (const name of CODE) expect(size(name), name).toBe(19);
    for (const name of CONTENT) expect(size(name), name).toBe(14);
    expect(size("tool")).toBe(12.5);
    expect(size("tool-name")).toBe(12.5);
    for (const [name, expected] of METADATA) expect(size(name), name).toBe(expected);
  });

  it("separate font families do not leak content fonts into code-copy controls or tool titles", () => {
    document.documentElement.style.setProperty("--ws-font-sans", '"Interface Family"');
    document.documentElement.style.setProperty("--ws-font-content", '"Content Family"');
    document.documentElement.style.setProperty("--ws-font-mono", '"Code Family"');
    for (const name of CONTENT) expect(getComputedStyle(element(name)).fontFamily, name).toBe('"Content Family"');
    for (const name of CODE) expect(getComputedStyle(element(name)).fontFamily, name).toBe('"Code Family"');
    for (const name of ["tool", "tool-name", "copy", "project"]) expect(getComputedStyle(element(name)).fontFamily, name).toBe('"Interface Family"');
    for (const [name] of METADATA) expect(getComputedStyle(element(name)).fontFamily, name).toBe('"Interface Family"');
  });
});
