// Composer 引用「文本 ↔ chip」工具（[docs/composer-file-ref-chips](../../../docs/composer-file-ref-chips.md)）：
// 判定启发式（宁可漏判不误判）+ 往返无损（split → merge 对引用在末尾的文本逐字节还原）。
import { describe, it, expect } from "vitest";
import { addRefs, isRefToken, mergeRefs, recoverRefs, splitRefs } from "../features/chat/composerRefs";

describe("isRefToken 判定", () => {
  it("认：相对/绝对/带扩展名的文件路径", () => {
    for (const t of [
      "@report.xlsx",
      "@src/components/Chat.tsx",
      "@docs\\office-and-pdf-support.md",
      "@/tmp/ws/report.xlsx",
      "@D:\\Work\\x\\支付订单_10W.xlsx",
      "@\\\\srv\\share\\a.pdf",
      "@~/Documents/a.xlsx",
    ]) {
      expect(isRefToken(t), t).toBe(true);
    }
  });

  it("不认：正文里像提及的普通词与目录", () => {
    for (const t of ["@types/node", "@src/components", "@用户名", "@", "@a@b", "@scope/pkg", "a.xlsx", "你好@"]) {
      expect(isRefToken(t), t).toBe(false);
    }
  });
});

describe("splitRefs", () => {
  it("抽出引用，其余文本与内部空白原样保留", () => {
    expect(splitRefs("我的问题 @report.xlsx")).toEqual({ text: "我的问题", refs: ["report.xlsx"] });
    expect(splitRefs("@report.xlsx")).toEqual({ text: "", refs: ["report.xlsx"] });
    expect(splitRefs("看一下 @a.xlsx 然后给我结论")).toEqual({
      text: "看一下 然后给我结论",
      refs: ["a.xlsx"],
    });
  });

  it("多行正文：引用抽走、换行保留", () => {
    expect(splitRefs("第一行\n第二行 @a/b.ts")).toEqual({ text: "第一行\n第二行", refs: ["a/b.ts"] });
  });

  it("重复引用只留一项（保首次顺序）", () => {
    expect(splitRefs("@a.xlsx @b.xlsx @a.xlsx").refs).toEqual(["a.xlsx", "b.xlsx"]);
  });

  it("不像引用的 token 留在正文", () => {
    expect(splitRefs("装 @types/node 和 @report.xlsx")).toEqual({
      text: "装 @types/node 和",
      refs: ["report.xlsx"],
    });
  });
});

describe("mergeRefs", () => {
  it("引用追加末尾，空格相连", () => {
    expect(mergeRefs("我的问题", ["report.xlsx"])).toBe("我的问题 @report.xlsx");
    expect(mergeRefs("", ["report.xlsx"])).toBe("@report.xlsx");
    expect(mergeRefs("我的问题", [])).toBe("我的问题");
  });

  it("正文已写着同一引用时不重复追加", () => {
    expect(mergeRefs("读 @report.xlsx 给我结论", ["report.xlsx"])).toBe("读 @report.xlsx 给我结论");
  });

  it("内部去重：同一 ref 只合成一次", () => {
    expect(mergeRefs("问题", ["a.xlsx", "a.xlsx"])).toBe("问题 @a.xlsx");
  });
});

describe("往返无损与回填门禁", () => {
  it("引用在末尾：split → merge 逐字节等于原文", () => {
    for (const t of [
      "我的问题 @report.xlsx",
      "看一下\n第二行 @b/c.ts",
      "@report.xlsx",
    ]) {
      const { text, refs } = splitRefs(t);
      expect(mergeRefs(text, refs), t).toBe(t);
    }
  });

  it("引用在中间：位置规范化到末尾，引用集合与正文不变", () => {
    const { text, refs } = splitRefs("@a.xlsx 请读一下");
    expect(refs).toEqual(["a.xlsx"]);
    expect(mergeRefs(text, refs)).toBe("请读一下 @a.xlsx");
    // 多行同理：行内引用被搬到末尾，行序不变、不留双空格
    const mid = splitRefs("看一下 @a.xlsx\n第二行 @b/c.ts");
    expect(mid.text).toBe("看一下\n第二行");
    expect(mergeRefs(mid.text, mid.refs)).toBe("看一下\n第二行 @a.xlsx @b/c.ts");
  });

  it("不像引用的 token 永不被搬走", () => {
    const t = "用 @types/node 解释 @report.xlsx";
    const { text, refs } = splitRefs(t);
    expect(mergeRefs(text, refs)).toBe("用 @types/node 解释 @report.xlsx");
  });
});

describe("recoverRefs 回填门禁", () => {
  it("app 产出形态（引用在末尾、单空格分隔）：解析且可逐字节还原", () => {
    for (const t of ["@report.xlsx", "看一下 @report.xlsx", "看一下\n第二行 @a/b.ts"]) {
      const p = recoverRefs(t);
      expect(mergeRefs(p.text, p.refs), t).toBe(t);
    }
    expect(recoverRefs("看一下 @report.xlsx")).toEqual({ text: "看一下", refs: ["report.xlsx"] });
  });

  it("引用在句子中间：不解析（不搬动用户的行文）", () => {
    expect(recoverRefs("@a.xlsx 请读一下")).toEqual({ text: "@a.xlsx 请读一下", refs: [] });
  });

  it("路径含空格（token 会被截断）：不解析", () => {
    const t = "看一下 @C:\\Users\\Me\\My Documents\\a.xlsx";
    expect(recoverRefs(t)).toEqual({ text: t, refs: [] });
  });

  it("引用独占一行：不解析（否则会凭空多出一个空行）", () => {
    const t = "第一行\n@a.xlsx\n第三行";
    expect(recoverRefs(t)).toEqual({ text: t, refs: [] });
  });

  it("本来就没有引用：原样返回", () => {
    expect(recoverRefs("就是一段普通的话 @types/node")).toEqual({
      text: "就是一段普通的话 @types/node",
      refs: [],
    });
  });
});

describe("addRefs", () => {
  it("去重且保首次顺序，空串丢弃", () => {
    expect(addRefs(["a.xlsx"], ["b.xlsx", "a.xlsx", ""])).toEqual(["a.xlsx", "b.xlsx"]);
    expect(addRefs([], ["a.xlsx"])).toEqual(["a.xlsx"]);
  });
});
