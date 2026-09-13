// 自绘标题栏激活（[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)）：
// 窗口以 visible:false 起动，前端挂载后调 activate_and_show——
// 成功 → "custom"（插件注入 Windows/Linux HTML 控制按钮，macOS 红绿灯 Overlay）；
// 失败 → "native"（后端已回退原生标题栏并显示窗口，顶栏让位 padding 随之撤销）。
// mode 与平台标记写入 <html data-*>，供 CSS 分支。
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { isMacOS } from "../../utils/platform";

export type TitlebarMode = "custom" | "native" | "pending";

/** 自绘标题栏激活 hook：挂载后调用 activate_and_show 命令，返回三态模式（pending/custom/native）；
 *  同时把平台与标题栏模式写入 `<html data-os/data-titlebar-mode>`，供 CSS 按平台/模式分支。 */
export function useTitlebarActivation(): TitlebarMode {
  const [mode, setMode] = useState<TitlebarMode>("pending");

  useEffect(() => {
    void invoke<"custom" | "native">("activate_and_show")
      .then((result) => setMode(result === "native" ? "native" : "custom"))
      // 命令 Err：后端 restore_and_show 已尽力恢复原生框（旧版后端缺该命令时同理），
      // 撤销让位预留配合原生布局更安全（custom 态下插件本就没注入，clearance 0 也无害）
      .catch(() => setMode("native"));
  }, []);

  useEffect(() => {
    document.documentElement.dataset.os = isMacOS() ? "macos" : "std";
    if (mode !== "pending") document.documentElement.dataset.titlebarMode = mode;
  }, [mode]);

  return mode;
}
