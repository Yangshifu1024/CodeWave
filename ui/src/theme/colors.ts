/** antd colorBgBase 的亮度判定；算法默认使用 #000/#fff 简写。 */
export function isDarkBase(colorBgBase: string): boolean {
  let hex = colorBgBase.replace(/^#/, "");
  if (/^[0-9a-f]{3}$/i.test(hex)) {
    hex = hex.split("").map((c) => c + c).join("");
  }
  if (!/^[0-9a-f]{6}$/i.test(hex)) return false;
  const n = parseInt(hex, 16);
  return 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255) < 128;
}
