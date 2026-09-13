import { createRoot } from "react-dom/client";
import App from "./App";
import "./theme/native.css";
import "./theme/app.css";
import { applyInitialFonts } from "./utils/fonts";
import "./utils/markdown"; // 顺带引入 hljs 主题（代码块恒为深色背景，与 Vue 版一致）

// React 挂载前先应用字体偏好（避免字体闪烁）
applyInitialFonts();
createRoot(document.getElementById("root")!).render(<App />);
