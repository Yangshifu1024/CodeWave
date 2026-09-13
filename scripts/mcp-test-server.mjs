#!/usr/bin/env node
// 真实 MCP 验证服务器（docs/10 §A3）：官方 @modelcontextprotocol/sdk 实现。
// 双模式：
//   stdio（默认）：node scripts/mcp-test-server.mjs
//   streamable-http：node scripts/mcp-test-server.mjs --http <port>
// 提供 echo / add 两个工具，供 src-tauri/src/mcp/mod.rs 的真实 server 集成测试使用。

import http from "node:http";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { z } from "zod";

const SERVER_INFO = { name: "codewave-test", version: "1.0.0" };

function makeServer() {
  const server = new McpServer(SERVER_INFO);
  server.registerTool(
    "echo",
    { description: "回显输入文本", inputSchema: { text: z.string() } },
    async ({ text }) => ({ content: [{ type: "text", text: `echo: ${text}` }] }),
  );
  server.registerTool(
    "add",
    { description: "两个整数相加", inputSchema: { a: z.number(), b: z.number() } },
    async ({ a, b }) => ({ content: [{ type: "text", text: String(a + b) }] }),
  );
  return server;
}

const args = process.argv.slice(2);

if (args[0] === "--http") {
  const port = Number(args[1]);
  // 无状态模式（sessionIdGenerator: undefined）：每个请求新建 server + transport
  const httpServer = http.createServer(async (req, res) => {
    if (req.method === "POST") {
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const raw = Buffer.concat(chunks).toString("utf8");
      let body;
      try {
        body = raw ? JSON.parse(raw) : undefined;
      } catch {
        body = raw;
      }
      const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined });
      const server = makeServer();
      await server.connect(transport);
      await transport.handleRequest(req, res, body);
      return;
    }
    if (req.method === "GET") {
      const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined });
      const server = makeServer();
      await server.connect(transport);
      await transport.handleRequest(req, res);
      return;
    }
    res.writeHead(405, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "method not allowed" }));
  });
  httpServer.listen(port, () => console.error(`mcp-test-server http listening on :${port}`));
} else {
  const server = makeServer();
  await server.connect(new StdioServerTransport());
}
