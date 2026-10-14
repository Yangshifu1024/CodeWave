/** 全局聊天展示偏好；只影响渲染，不修改模型回复或历史。 */
export type OutputDisplay = "compact" | "detailed";
export const OUTPUT_DISPLAY_KEY = "ws_output_display";

export function readStoredOutputDisplay(): OutputDisplay {
  try {
    return typeof localStorage !== "undefined" && localStorage.getItem(OUTPUT_DISPLAY_KEY) === "compact"
      ? "compact" : "detailed";
  } catch {
    return "detailed";
  }
}
