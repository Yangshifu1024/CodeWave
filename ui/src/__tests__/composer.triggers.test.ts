// 触发符光标感知判定（[docs/composer-trigger-caret](../../../docs/composer-trigger-caret.md)）：
// 「正文里已有内容时拉不起菜单」的根因是判定锚定整段末尾且不读光标——这里守护新口径：
// `/`、`$` 限消息开头（模型侧「以 … 开头」契约），`@` 放宽到行首/空白后（正文任意位置）。
import { describe, it, expect } from "vitest";
import { clampCaret, detectTrigger, fragmentBefore } from "../features/chat/composerTriggers";

describe("clampCaret / fragmentBefore", () => {
  it("光标越界与缺省按文末处理", () => {
    expect(clampCaret("abc", 99)).toBe(3);
    expect(clampCaret("abc", -5)).toBe(0);
    expect(clampCaret("abc", null)).toBe(3);
    expect(clampCaret("abc", undefined)).toBe(3);
    expect(clampCaret("abc", 2.7)).toBe(2);
  });

  it("片段 = 光标前最后一段不含空白的文本，起点即行首/空白后", () => {
    expect(fragmentBefore("你好 @sr", 6)).toEqual({ start: 3, text: "@sr" });
    expect(fragmentBefore("你好 @sr", 2)).toEqual({ start: 0, text: "你好" });
    expect(fragmentBefore("第一行\n@a", 7)).toEqual({ start: 4, text: "@a" });
    expect(fragmentBefore("@a 后文", 2)).toEqual({ start: 0, text: "@a" });
    expect(fragmentBefore("", 0)).toEqual({ start: 0, text: "" });
  });
});

describe("detectTrigger · @ 提及（放宽到行首/空白后）", () => {
  it("正文任意位置可用：句中", () => {
    expect(detectTrigger("看看 @src", 7)).toEqual({ kind: "at", query: "src", start: 3, end: 7 });
  });

  it("正文任意位置可用：开头（后面已有正文）——改造前打不开的场景", () => {
    // 光标停在 `@a` 之后、正文还在后面
    expect(detectTrigger("@a后面还有正文", 2)).toEqual({ kind: "at", query: "a", start: 0, end: 2 });
  });

  it("行首/空白后之外不触发：邮箱与紧贴的 @", () => {
    expect(detectTrigger("me@x.com", 8)).toBeNull();
    expect(detectTrigger("看这个@a", 5)).toBeNull();
  });

  it("全角空格（U+3000）也是片段边界（中日文排版）", () => {
    expect(fragmentBefore("你好\u3000@sr", 6)).toEqual({ start: 3, text: "@sr" });
    expect(detectTrigger("你好\u3000@sr", 6)).toEqual({ kind: "at", query: "sr", start: 3, end: 6 });
  });

  it("制表符与 CRLF 同为边界；caret 是 UTF-16 索引（emoji 前也能正确回填范围）", () => {
    expect(detectTrigger("a\t@x", 4)?.start).toBe(2);
    expect(detectTrigger("第一行\r\n@y", 7)?.start).toBe(5);
    expect(detectTrigger("😀 @z", 4)?.start).toBe(3); // emoji 占两个 UTF-16 单元
  });

  it("片段内出现第二个 @ 不触发（与原 [^@\\s]* 口径一致）", () => {
    expect(detectTrigger("@a@b", 4)).toBeNull();
  });

  it("查询词只取到光标为止，不含光标之后的字", () => {
    expect(detectTrigger("@src 后文", 4)).toEqual({ kind: "at", query: "src", start: 0, end: 4 });
  });
});

describe("detectTrigger · / 技能与 $ 子代理（限消息开头）", () => {
  it("消息开头打、后面已有正文 → 可触发（改造前因整段锚定而失效）", () => {
    expect(detectTrigger("/repo-index 分析 X", 11)).toEqual({
      kind: "slash", query: "repo-index", start: 0, end: 11,
    });
    expect(detectTrigger("$tester 帮我看看", 7)).toEqual({
      kind: "dollar", query: "tester", start: 0, end: 7,
    });
  });

  it("正文中间不触发（模型侧只认「消息以 … 开头」）", () => {
    expect(detectTrigger("分析一下 /repo", 9)).toBeNull();
    expect(detectTrigger("帮我 $tester", 9)).toBeNull();
  });

  it("空查询：刚打出触发符即列全部候选", () => {
    expect(detectTrigger("/", 1)).toEqual({ kind: "slash", query: "", start: 0, end: 1 });
    expect(detectTrigger("$", 1)).toEqual({ kind: "dollar", query: "", start: 0, end: 1 });
    expect(detectTrigger("@", 1)).toEqual({ kind: "at", query: "", start: 0, end: 1 });
  });

  it("空格后合并菜单收起（Enter 正常发送）", () => {
    expect(detectTrigger("/repo-index 分析 X", 15)).toBeNull();
    expect(detectTrigger("$tester 帮我看看", 10)).toBeNull();
  });

  it("片段内第二个 $ 不触发（与原 [^$\\s]* 口径一致）", () => {
    expect(detectTrigger("$a$b", 4)).toBeNull();
  });

  it("光标位置越界时按文末处理", () => {
    expect(detectTrigger("你好 @a", 999)).toEqual({ kind: "at", query: "a", start: 3, end: 5 });
  });
});
