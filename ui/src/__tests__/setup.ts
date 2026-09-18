// Test environment shims: browser APIs missing in happy-dom (antd depends on matchMedia / ResizeObserver)
import { beforeAll } from "vitest";

beforeAll(() => {
  if (!window.matchMedia) {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: (query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      }),
    });
  }
  // 窗口尺寸 shim：默认 1440×900（桌面常态）。可拖拽栏宽按窗口夹取（utils/layout），
  // 不定则 happy-dom 的 1024 会把默认左栏 280 夹到 216 —— 需要窄窗行为的用例自行
  // Object.defineProperty 覆盖 innerWidth。
  if (window.innerWidth !== 1440) {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1440 });
  }
  if (window.innerHeight !== 900) {
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 900 });
  }
  const g = globalThis as any;
  if (!g.ResizeObserver) {
    g.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  if (!Element.prototype.scrollTo) {
    (Element.prototype as any).scrollTo = () => {};
  }
  // happy-dom's crypto lacks randomUUID (present in WebView/browsers)
  if (!crypto.randomUUID) {
    (crypto as any).randomUUID = () =>
      "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
        const r = (Math.random() * 16) | 0;
        return (c === "x" ? r : (r & 0x3) | 0x8).toString(16);
      });
  }
});
