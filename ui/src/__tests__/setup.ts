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
