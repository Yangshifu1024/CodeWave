// 外部链接打开链路（markdown.ts target=_blank + linkhandler.ts 委托 → ipc.openUrl）
import { describe, it, expect, vi, afterEach } from "vitest";
import { renderMarkdown } from "../utils/markdown";
import { bindExternalLinkDelegate, isHttpUrl } from "../utils/linkhandler";
import { ipc } from "../ipc/client";

// ipc 是模块级单例：直接 spy 其 openUrl 方法
vi.spyOn(ipc, "openUrl").mockResolvedValue(undefined);

afterEach(() => {
  vi.mocked(ipc.openUrl).mockClear();
  document.body.innerHTML = "";
});

function clickOn(el: HTMLElement) {
  el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
}

describe("isHttpUrl", () => {
  it("http/https 通过；其他协议与非法串拒绝", () => {
    expect(isHttpUrl("https://git.example.com/a/b/pull/12")).toBe(true);
    expect(isHttpUrl("http://example.com")).toBe(true);
    expect(isHttpUrl("file:///etc/passwd")).toBe(false);
    expect(isHttpUrl("javascript:alert(1)")).toBe(false);
    expect(isHttpUrl("not a url")).toBe(false);
  });
});

describe("外部链接点击委托", () => {
  it("markdown linkify 出的 PR 链接：点击被拦截并走 openUrl 用系统浏览器打开", () => {
    const unbind = bindExternalLinkDelegate();
    // linkify: true——裸 URL 自动成链（聊天中模型贴 PR 链接的典型形态）
    document.body.innerHTML = renderMarkdown(
      "PR 已创建：https://git.example.com/a/b/-/merge_requests/12",
    );
    const a = document.querySelector("a[href]") as HTMLAnchorElement;
    expect(a).toBeTruthy();
    // markdown.ts 对链接统一加 target=_blank（WebView 中该属性本身无效，由委托接管）
    expect(a.getAttribute("target")).toBe("_blank");

    const e = new MouseEvent("click", { bubbles: true, cancelable: true });
    a.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(true);
    expect(ipc.openUrl).toHaveBeenCalledWith(
      "https://git.example.com/a/b/-/merge_requests/12",
    );
    unbind();
  });

  it("非 http 协议链接不拦截，交回默认行为", () => {
    const unbind = bindExternalLinkDelegate();
    document.body.innerHTML = `<a href="file:///D:/doc.md">本地文档</a>`;
    const a = document.querySelector("a[href]") as HTMLAnchorElement;

    const e = new MouseEvent("click", { bubbles: true, cancelable: true });
    a.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(false);
    expect(ipc.openUrl).not.toHaveBeenCalled();
    unbind();
  });

  it("非链接区域点击不触发；解绑后恢复默认", () => {
    const unbind = bindExternalLinkDelegate();
    document.body.innerHTML = "<p>纯文本，无链接</p>";
    clickOn(document.querySelector("p") as HTMLElement);
    expect(ipc.openUrl).not.toHaveBeenCalled();
    unbind();

    // 解绑后点击链接不再拦截
    document.body.innerHTML = `<a href="https://example.com">x</a>`;
    const a = document.querySelector("a[href]") as HTMLAnchorElement;
    const e = new MouseEvent("click", { bubbles: true, cancelable: true });
    a.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(false);
    expect(ipc.openUrl).not.toHaveBeenCalled();
  });
});
