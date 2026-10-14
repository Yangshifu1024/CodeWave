import { describe, expect, it } from "vitest";
import { md, renderMarkdown, renderUserMarkdown } from "../utils/markdown";

const broken = "## 两点提示1. **登录日志级别问题**：`CustomerMemberServiceImpl.java:165` 用 `log.info` 记录密码错误。\n2. **月度统计的 P0 隐患仍在**：只会在 06:00 复现。";
const renderAssistant = (source: string) => renderMarkdown(source, undefined, { repairAssistantBlocks: true });
const dom = (html: string) => {
  const node = document.createElement("div");
  node.innerHTML = html;
  return node;
};

describe("assistant heading/list formatting repair", () => {
  it("separates the real malformed heading from both numbered items", () => {
    const node = dom(renderAssistant(broken));
    expect(node.querySelector("h2")?.textContent).toBe("两点提示");
    const list = node.querySelector("ol")!;
    expect(list.hasAttribute("start")).toBe(false);
    expect(list.querySelectorAll("li")).toHaveLength(2);
    expect(list.querySelector("li > strong")?.textContent).toBe("登录日志级别问题");
    expect([...list.querySelectorAll("code")].map(code => code.textContent)).toEqual(["CustomerMemberServiceImpl.java:165", "log.info"]);
    expect(broken).toContain("## 两点提示1.");
  });

  it("handles multiple confirmed collisions and blank lines before the second item", () => {
    const node = dom(renderAssistant(broken.replace("\n2.", "\n\n2.") + "\n\n" + broken.replace("##", "###")));
    expect(node.querySelectorAll("h2, h3")).toHaveLength(2);
    expect([...node.querySelectorAll("h2, h3")].map(heading => heading.textContent)).toEqual(["两点提示", "两点提示"]);
    expect(node.querySelectorAll("li")).toHaveLength(4);
  });

  it.each([
    "## 两点提示\n\n1. **登录**：正文\n2. **统计**：正文",
    "## 版本 1.2 发布说明\n2. **事项**：正文",
    "## 提示1. **登录**：正文", // No following list confirms the missing boundary yet.
    "## 1. **登录**：正文\n2. **统计**：正文", // The numbered heading has no separate title.
    "## 示例 `1. **登录**：正文`\n2. **统计**：正文",
    "> ## 两点提示1. **登录**：正文\n> 2. **统计**：正文",
    "    ## 两点提示1. **登录**：正文\n    2. **统计**：正文",
    "```markdown\n" + broken + "\n```",
  ])("leaves valid Markdown, ambiguous text and code examples intact: %s", source => {
    expect(renderAssistant(source)).toBe(md.render(source));
  });

  it("does not rewrite user messages/file previews or enable raw HTML", () => {
    expect(renderMarkdown(broken)).toBe(md.render(broken));
    expect(dom(renderUserMarkdown(broken)).querySelector("h2")?.textContent).toContain("两点提示1.");
    const node = dom(renderAssistant(broken + "\n\n<script>alert(1)</script>"));
    expect(node.querySelector("script")).toBeNull();
    expect(node.textContent).toContain("<script>alert(1)</script>");
  });
});
