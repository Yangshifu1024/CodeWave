/**
 * MCP 配置纯函数（`utils/mcpConfig.ts`）的无损往返测试。
 *
 * 这些用例守护的是原先内联在 SettingsPage 里、**零测试**的那套有损逻辑：
 * 含空格参数、env 值空白、未知键、未知 transport、顶层其它键。
 */
import { describe, expect, it } from "vitest";
import {
  draftTransport,
  emptyDraft,
  extraKeysOf,
  normalizeMcpDoc,
  parseMcpDoc,
  serializeMcpDoc,
  transportIsExplicit,
  type McpServerDraft,
} from "../utils/mcpConfig";

/** 解析一份文本，断言可结构化编辑。 */
function doc(raw: string) {
  const d = parseMcpDoc(raw);
  expect(d).not.toBeNull();
  return d!;
}

/** 取某个 server 的草稿。 */
function srv(raw: string, name: string): McpServerDraft {
  const found = doc(raw).servers.find((s) => s.name === name);
  expect(found).toBeDefined();
  return found!;
}

/** 解析→序列化→解析，断言语义等价（键顺序与格式不参与比较）。 */
function roundTrip(raw: string): unknown {
  const once = normalizeMcpDoc(raw);
  const twice = normalizeMcpDoc(once);
  expect(twice).toBe(once); // 幂等
  return JSON.parse(once);
}

describe("parseMcpDoc 的输入容忍度", () => {
  it("非法 JSON 返回 null（调用方回退原文模式，不拿空配置覆盖）", () => {
    expect(parseMcpDoc("{ not json")).toBeNull();
    expect(normalizeMcpDoc("{ not json")).toBe("{ not json");
  });

  it("空文本按空文档处理（没有内容可丢，不进兜底模式）", () => {
    expect(parseMcpDoc("")).toEqual({ servers: [], extraTop: {} });
    expect(parseMcpDoc("  ")).toEqual({ servers: [], extraTop: {} });
    expect(normalizeMcpDoc("")).toBe(JSON.stringify({ mcpServers: {} }, null, 2));
  });

  it("顶层不是对象返回 null", () => {
    expect(parseMcpDoc("[1,2]")).toBeNull();
    expect(parseMcpDoc('"str"')).toBeNull();
  });

  it("mcpServers 不是对象返回 null", () => {
    expect(parseMcpDoc('{"mcpServers":[]}')).toBeNull();
    expect(parseMcpDoc('{"mcpServers":"x"}')).toBeNull();
  });

  it("某个条目不是对象也返回 null（不把用户手写的怪形状改成对象）", () => {
    expect(parseMcpDoc('{"mcpServers":{"a":"just-a-string"}}')).toBeNull();
  });

  it("合法 JSON 但没有 mcpServers 段 → 空文档，且顶层其它键进 extraTop", () => {
    const d = doc('{"$schema":"https://x/s"}');
    expect(d.servers).toEqual([]);
    expect(d.extraTop).toEqual({ $schema: "https://x/s" });
    // 保存后顶层键仍在（旧实现在这里会丢）
    expect(roundTrip('{"$schema":"https://x/s"}')).toEqual({
      $schema: "https://x/s",
      mcpServers: {},
    });
  });
});

describe("args 表格化：含空格参数不再被拆坏", () => {
  const raw = JSON.stringify({
    mcpServers: {
      fs: { command: "npx", args: ["-y", "pkg", "D:/我的 项目/dir", ""] },
    },
  });

  it("解析出逐行参数（空串参数被丢弃，符合「空行不算参数」的约定）", () => {
    expect(srv(raw, "fs").args.map((a) => a.value)).toEqual([
      "-y",
      "pkg",
      "D:/我的 项目/dir",
      "",
    ]);
  });

  it("往返后含空格参数完整保留（旧实现会拆成两个参数）", () => {
    const out = roundTrip(raw) as any;
    expect(out.mcpServers.fs.args).toEqual(["-y", "pkg", "D:/我的 项目/dir"]);
  });
});

describe("env / headers 表格化：值不被 trim", () => {
  const raw = JSON.stringify({
    mcpServers: {
      fs: {
        command: "npx",
        env: { A: " padded ", B: "x=y", C: "" },
        headers: { Authorization: "Bearer abc def" },
      },
    },
  });

  it("值两侧空白与内含 = 原样保留", () => {
    const d = srv(raw, "fs");
    expect(d.env).toEqual([
      { key: "A", value: " padded " },
      { key: "B", value: "x=y" },
      { key: "C", value: "" },
    ]);
    expect(d.headers).toEqual([{ key: "Authorization", value: "Bearer abc def" }]);
  });

  it("往返后仍是同样的值", () => {
    const out = roundTrip(raw) as any;
    expect(out.mcpServers.fs.env).toEqual({ A: " padded ", B: "x=y", C: "" });
    expect(out.mcpServers.fs.headers).toEqual({ Authorization: "Bearer abc def" });
  });

  it("键为空的行被丢弃（点了「＋」没填的行）", () => {
    const d = doc('{"mcpServers":{"fs":{"command":"x"}}}');
    d.servers[0].env = [{ key: "  ", value: "v" }, { key: "K", value: "v" }];
    const out = JSON.parse(serializeMcpDoc(d)) as any;
    expect(out.mcpServers.fs.env).toEqual({ K: "v" });
  });
});

describe("未知键透传（保存不丢）", () => {
  it("条目级未识别键进 extra 并写回", () => {
    const raw = JSON.stringify({
      mcpServers: {
        fs: { command: "npx", experimental: { nested: [1, 2] }, custom: "keep" },
      },
    });
    expect(extraKeysOf(srv(raw, "fs"))).toEqual(["custom", "experimental"]);
    const out = roundTrip(raw) as any;
    expect(out.mcpServers.fs.experimental).toEqual({ nested: [1, 2] });
    expect(out.mcpServers.fs.custom).toBe("keep");
  });

  it("顶层未识别键（$schema）写回", () => {
    const raw = JSON.stringify({
      $schema: "https://x/s",
      mcpServers: { fs: { command: "npx" } },
    });
    const out = roundTrip(raw) as any;
    expect(out.$schema).toBe("https://x/s");
  });

  it("未识别的 transport 取值原样保留，不被静默改写成 stdio", () => {
    const raw = JSON.stringify({
      mcpServers: { legacy: { transport: "sse", url: "https://h/sse" } },
    });
    expect(srv(raw, "legacy").transportRaw).toBe("sse");
    const out = roundTrip(raw) as any;
    expect(out.mcpServers.legacy.transport).toBe("sse");
    // 表单按 stdio 呈现（错误由后端定向报出），但不谎称是显式声明
    expect(draftTransport(srv(raw, "legacy"))).toBe("stdio");
    expect(transportIsExplicit(srv(raw, "legacy"))).toBe(false);
  });

  it("`type` 别名读得进来，写回时统一成 transport", () => {
    const raw = JSON.stringify({
      mcpServers: { h: { type: "streamable_http", url: "https://h/mcp" } },
    });
    expect(srv(raw, "h").transportRaw).toBe("streamable_http");
    const out = roundTrip(raw) as any;
    expect(out.mcpServers.h.transport).toBe("streamable_http");
    expect(out.mcpServers.h.type).toBeUndefined();
  });
});

describe("字段往返", () => {
  it("enabled 缺省 true，显式 false 往返保留", () => {
    expect(srv('{"mcpServers":{"a":{"command":"x"}}}', "a").enabled).toBe(true);
    expect(srv('{"mcpServers":{"a":{"command":"x","enabled":false}}}', "a").enabled).toBe(false);
    const out = roundTrip('{"mcpServers":{"a":{"command":"x","enabled":false}}}') as any;
    expect(out.mcpServers.a.enabled).toBe(false);
    // 默认 true 时不写该键，保持文件干净
    const on = roundTrip('{"mcpServers":{"a":{"command":"x"}}}') as any;
    expect(on.mcpServers.a.enabled).toBeUndefined();
  });

  it("timeout_ms 读入（含 camelCase）与写回", () => {
    expect(srv('{"mcpServers":{"a":{"command":"x","timeout_ms":5000}}}', "a").timeoutMs).toBe(
      "5000",
    );
    expect(srv('{"mcpServers":{"a":{"command":"x","timeoutMs":7000}}}', "a").timeoutMs).toBe(
      "7000",
    );
    const out = roundTrip('{"mcpServers":{"a":{"command":"x","timeout_ms":5000}}}') as any;
    expect(out.mcpServers.a.timeout_ms).toBe(5000);
    expect(out.mcpServers.a.timeoutMs).toBeUndefined();
  });

  it("非数字超时被忽略（不写入非法值）", () => {
    const d = doc('{"mcpServers":{"a":{"command":"x"}}}');
    d.servers[0].timeoutMs = "abc";
    const out = JSON.parse(serializeMcpDoc(d)) as any;
    expect(out.mcpServers.a.timeout_ms).toBeUndefined();
  });

  it("read_only / always_allow 往返（含 camelCase 读入）", () => {
    expect(srv('{"mcpServers":{"a":{"command":"x","readOnly":true}}}', "a").readOnly).toBe(true);
    expect(
      srv('{"mcpServers":{"a":{"command":"x","alwaysAllow":true}}}', "a").alwaysAllow,
    ).toBe(true);
    const out = roundTrip(
      '{"mcpServers":{"a":{"command":"x","read_only":true,"always_allow":true}}}',
    ) as any;
    expect(out.mcpServers.a.read_only).toBe(true);
    expect(out.mcpServers.a.always_allow).toBe(true);
  });

  it("工具过滤往返；默认 all 且名单为空时不写该键", () => {
    const raw = '{"mcpServers":{"a":{"command":"x","tools":{"mode":"allow","list":["read_*"]}}}}';
    const d = srv(raw, "a");
    expect(d.toolsMode).toBe("allow");
    expect(d.toolsList).toBe("read_*");
    const out = roundTrip(raw) as any;
    expect(out.mcpServers.a.tools).toEqual({ mode: "allow", list: ["read_*"] });
    const plain = roundTrip('{"mcpServers":{"a":{"command":"x"}}}') as any;
    expect(plain.mcpServers.a.tools).toBeUndefined();
  });

  it("cwd / url 空值不写入（避免产出无意义字段）", () => {
    const d = doc('{"mcpServers":{"a":{"command":"x"}}}');
    const out = JSON.parse(serializeMcpDoc(d)) as any;
    expect(out.mcpServers.a.cwd).toBeUndefined();
    expect(out.mcpServers.a.url).toBeUndefined();
    expect(out.mcpServers.a.env).toBeUndefined();
  });

  it("未命名条目被跳过（与旧行为一致）", () => {
    const d = doc('{"mcpServers":{"a":{"command":"x"}}}');
    d.servers.push({ ...emptyDraft(), name: "   " });
    const out = JSON.parse(serializeMcpDoc(d)) as any;
    expect(Object.keys(out.mcpServers)).toEqual(["a"]);
  });
});

describe("draftTransport / emptyDraft", () => {
  it("未声明 transport 时按 command / url 推导", () => {
    expect(draftTransport(srv('{"mcpServers":{"a":{"command":"npx"}}}', "a"))).toBe("stdio");
    expect(draftTransport(srv('{"mcpServers":{"a":{"url":"https://h"}}}', "a"))).toBe(
      "streamable_http",
    );
    expect(transportIsExplicit(srv('{"mcpServers":{"a":{"command":"npx"}}}', "a"))).toBe(false);
  });

  it("显式 transport 优先于推导", () => {
    const d = srv(
      '{"mcpServers":{"a":{"transport":"stdio","command":"npx","url":"https://h"}}}',
      "a",
    );
    expect(draftTransport(d)).toBe("stdio");
    expect(transportIsExplicit(d)).toBe(true);
  });

  it("emptyDraft 是干净默认值", () => {
    const d = emptyDraft();
    expect(d.enabled).toBe(true);
    expect(d.toolsMode).toBe("all");
    expect(d.args).toEqual([]);
    expect(d.extra).toEqual({});
  });
});
