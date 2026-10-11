import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { ConfigProvider, theme } from "antd";
import ThemeBridge from "../theme/bridge";
import WidgetPreviewModal from "../features/tools/WidgetPreviewModal";
import "../i18n";

afterEach(cleanup);

describe("真实 antd 主题桥", () => {
  it("亮暗切换同步背景、文字和代码高亮，再切回亮色", async () => {
    const light = theme.getDesignToken({ algorithm: theme.defaultAlgorithm });
    const dark = theme.getDesignToken({ algorithm: theme.darkAlgorithm });
    const style = document.documentElement.style;
    const { rerender } = render(<ConfigProvider><ThemeBridge /></ConfigProvider>);
    await waitFor(() => expect(style.getPropertyValue("--ws-bg-main")).toBe("#FFFFFF"));
    expect(style.getPropertyValue("--ws-text-1")).toBe(light.colorText);

    rerender(<ConfigProvider theme={{ algorithm: theme.darkAlgorithm }}><ThemeBridge /></ConfigProvider>);
    await waitFor(() => expect(style.getPropertyValue("--ws-bg-main")).toBe(dark.colorBgLayout));
    expect(style.getPropertyValue("--ws-bg-nav")).toBe(dark.colorBgContainer);
    expect(style.getPropertyValue("--ws-text-1")).toBe(dark.colorText);
    expect(style.getPropertyValue("--ws-code-block-text")).toBe("#e6edf3");

    rerender(<ConfigProvider><ThemeBridge /></ConfigProvider>);
    await waitFor(() => expect(style.getPropertyValue("--ws-bg-main")).toBe("#FFFFFF"));
    expect(style.getPropertyValue("--ws-bg-nav")).toBe("#eef4f9");
    expect(style.getPropertyValue("--ws-text-1")).toBe(light.colorText);
    expect(style.getPropertyValue("--ws-code-block-text")).toBe("#1f2328");
  });

  it("暗色应用打开 HTML 预览时默认使用暗色画布", async () => {
    render(<ConfigProvider theme={{ algorithm: theme.darkAlgorithm }}>
      <WidgetPreviewModal open title="预览" html="<p>hello</p>" onClose={() => {}} />
    </ConfigProvider>);
    await waitFor(() => expect(document.querySelector<HTMLIFrameElement>(".widget-preview-frame")?.style.colorScheme).toBe("dark"));
  });
});
