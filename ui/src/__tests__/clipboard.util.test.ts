// 统一复制工具（utils/clipboard.ts）三级降级链路的测试。
//
// 本文件锁死的是本次缺陷的病根：旧写法 `navigator.clipboard?.writeText(t).then(...)`
// 在 clipboard 不存在时 `?.` 短路得到 undefined，紧接的 .then 同步抛 TypeError，
// 而空 catch 又把它吃掉——界面上「点了没反应」。所以必须逐条钉死：
// 不存在 → 降级、被拒 → 降级、全失败 → 返回 false（供调用方提示用户）、兜底清理干净。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { copyText } from "../utils/clipboard";

// 原生插件封装（ipc/clipboard.ts）——主路径单独 mock，才能测到「插件失败是否降级」。
const writeClipboardText = vi.fn(async () => true);
vi.mock("../ipc/clipboard", () => ({
  writeClipboardText: (...args: unknown[]) => writeClipboardText(...(args as [])),
}));

const execCommand = vi.fn(() => true);
const originalExecCommand = document.execCommand;

function setClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", { value, configurable: true });
}

beforeEach(() => {
  writeClipboardText.mockReset().mockResolvedValue(true);
  execCommand.mockReset().mockReturnValue(true);
  document.execCommand = execCommand as unknown as typeof document.execCommand;
  setClipboard({ writeText: vi.fn(async () => {}) });
});

afterEach(() => {
  document.execCommand = originalExecCommand;
  setClipboard(undefined);
  vi.restoreAllMocks();
});

describe("copyText 三级降级链路", () => {
  it("主路径：原生插件写入成功即返回 true，不碰浏览器接口与兜底", async () => {
    const writeText = vi.fn(async () => {});
    setClipboard({ writeText });

    await expect(copyText("内容")).resolves.toBe(true);

    expect(writeClipboardText).toHaveBeenCalledWith("内容");
    expect(writeText).not.toHaveBeenCalled(); // 主路径成功就不该再降级
    expect(execCommand).not.toHaveBeenCalled();
  });

  it("插件失败（非 Tauri 环境 / ACL 未授予）时降级到浏览器接口", async () => {
    writeClipboardText.mockResolvedValue(false);
    const writeText = vi.fn(async () => {});
    setClipboard({ writeText });

    await expect(copyText("降级")).resolves.toBe(true);

    expect(writeText).toHaveBeenCalledWith("降级");
    expect(execCommand).not.toHaveBeenCalled();
  });

  it("clipboard 不存在时降级到 execCommand 兜底（不抛 TypeError——旧写法的病根）", async () => {
    writeClipboardText.mockResolvedValue(false);
    setClipboard(undefined); // 关键：模拟打包版 navigator.clipboard 缺失

    await expect(copyText("兜底")).resolves.toBe(true);

    expect(execCommand).toHaveBeenCalledWith("copy");
  });

  it("浏览器接口被拒绝时继续降级到 execCommand", async () => {
    writeClipboardText.mockResolvedValue(false);
    const writeText = vi.fn(async () => {
      throw new Error("NotAllowedError");
    });
    setClipboard({ writeText });

    await expect(copyText("被拒")).resolves.toBe(true);

    expect(writeText).toHaveBeenCalled();
    expect(execCommand).toHaveBeenCalledWith("copy");
  });

  it("三级全失败时返回 false（调用方据此提示用户，不静默）", async () => {
    writeClipboardText.mockResolvedValue(false);
    const writeText = vi.fn(async () => {
      throw new Error("NotAllowedError");
    });
    setClipboard({ writeText });
    execCommand.mockReturnValue(false);

    await expect(copyText("全失败")).resolves.toBe(false);
  });

  it("空文本直接返回 false，不触发任何写入", async () => {
    await expect(copyText("")).resolves.toBe(false);
    expect(writeClipboardText).not.toHaveBeenCalled();
    expect(execCommand).not.toHaveBeenCalled();
  });

  it("兜底用的 textarea 用完即删，不残留 DOM", async () => {
    writeClipboardText.mockResolvedValue(false);
    setClipboard(undefined);

    await copyText("清理检查");

    // 兜底期间会短暂挂一个 textarea，结束后必须消失
    expect(document.querySelectorAll("textarea")).toHaveLength(0);
  });

  it("兜底恢复原焦点元素，不抢走聊天输入框的光标", async () => {
    writeClipboardText.mockResolvedValue(false);
    setClipboard(undefined);
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    expect(document.activeElement).toBe(input);

    await copyText("焦点检查");

    expect(document.activeElement).toBe(input);
    input.remove();
  });

  it("兜底写入的文本正确放进 textarea（多行内容也不截断）", async () => {
    writeClipboardText.mockResolvedValue(false);
    setClipboard(undefined);
    const text = "第一行\n第二行\n第三行";
    let captured = "";
    execCommand.mockImplementation(() => {
      captured = (document.querySelector("textarea") as HTMLTextAreaElement)?.value ?? "";
      return true;
    });

    await expect(copyText(text)).resolves.toBe(true);
    expect(captured).toBe(text);
  });
});

describe("复制入口唯一化守护", () => {
  it("全仓除 utils/clipboard.ts 与 ipc/clipboard.ts 外不得出现 navigator.clipboard", async () => {
    const { readdirSync, readFileSync } = await import("node:fs");
    const { join } = await import("node:path");
    const root = join(process.cwd(), "src");
    const allowed = new Set([join(root, "utils", "clipboard.ts"), join(root, "ipc", "clipboard.ts")]);

    const offenders: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, entry.name);
        if (entry.isDirectory()) {
          // 跳过 node_modules 与 __tests__：测试用例本身会故意断言这个字符串
          if (entry.name === "node_modules" || entry.name === "__tests__") continue;
          walk(p);
        } else if (/\.tsx?$/.test(entry.name) && !allowed.has(p)) {
          if (readFileSync(p, "utf8").includes("navigator.clipboard")) offenders.push(p);
        }
      }
    };
    walk(root);

    expect(offenders).toEqual([]);
  });
});
