// isMacOS: UA-based platform detection driving <html data-os="macos"> CSS branches ([docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md), [docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect, afterEach } from "vitest";
import { isMacOS } from "../utils/platform";

function setUA(ua: string) {
  Object.defineProperty(window.navigator, "userAgent", { value: ua, configurable: true });
}

afterEach(() => {
  setUA("");
});

describe("utils/platform isMacOS", () => {
  it("detects macOS via Macintosh UA", () => {
    setUA("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15");
    expect(isMacOS()).toBe(true);
  });

  it("detects macOS via Mac OS X UA token", () => {
    setUA("Mozilla/5.0 (Mac OS X 14_0) AppleWebKit/605.1.15");
    expect(isMacOS()).toBe(true);
  });

  it("returns false for Windows UA", () => {
    setUA("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36");
    expect(isMacOS()).toBe(false);
  });

  it("returns false for Linux UA", () => {
    setUA("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36");
    expect(isMacOS()).toBe(false);
  });
});
