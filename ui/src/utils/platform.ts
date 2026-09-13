// 平台检测（[docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)）：CSS 依据 <html data-os="macos"> 分支（macOS 红绿灯左侧让位）
export function isMacOS(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent;
  return /Macintosh|Mac OS X/i.test(ua);
}
