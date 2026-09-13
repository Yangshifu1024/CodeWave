#!/usr/bin/env node
//! 本地伪造更新源（端到端验证 updater 链路用，[docs/version-bump-and-release](../docs/version-bump-and-release.md) §4）。
//! 扫描 tauri 产物目录里的安装包 + .sig 签名对，生成 latest.json（版本号可指定为高于
//! 当前安装版的假版本）并在 8080 端口提供下载；配合「临时把 endpoints 指向
//! http://127.0.0.1:8080/latest.json 后重打包」的本地模拟流程使用。
//!
//! 用法：node scripts/mock-updater-endpoint.mjs [产物目录] [假版本号]
//!   例：node scripts/mock-updater-endpoint.mjs src-tauri/target/debug/bundle 9.9.9
import { createServer } from "node:http";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { extname, join } from "node:path";

const root = process.argv[2] ?? "src-tauri/target/debug/bundle";
const fakeVersion = process.argv[3] ?? "9.9.9";
const PORT = 8080;

// 平台键 → 安装包扩展名（updater v2：NSIS setup.exe / msi / dmg 直接签名，无 zip 壳）
const PLATFORMS = [
  { key: "windows-x86_64", exts: [".exe", ".msi"] },
  { key: "darwin-aarch64", exts: [".dmg", ".app.tar.gz"] },
  { key: "darwin-x86_64", exts: [".dmg", ".app.tar.gz"] },
  { key: "linux-x86_64", exts: [".AppImage", ".deb"] },
];

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

const files = walk(root);
const platforms = {};
for (const { key, exts } of PLATFORMS) {
  const installer = files.find((f) => exts.some((e) => f.endsWith(e)) && !f.endsWith(".sig"));
  if (!installer) continue;
  const sig = files.find((f) => f === installer + ".sig");
  if (!sig) continue;
  platforms[key] = {
    signature: readFileSync(sig, "utf8"),
    url: `http://127.0.0.1:${PORT}/${installer.replaceAll("\\", "/").split("/bundle/")[1]}`,
  };
}

if (Object.keys(platforms).length === 0) {
  console.error(`[mock] ${root} 下未找到「安装包 + .sig」对——请先用 TAURI_SIGNING_PRIVATE_KEY 构建带签名产物`);
  process.exit(1);
}

const latest = JSON.stringify(
  {
    version: fakeVersion,
    notes: `本地模拟更新源（假版本 ${fakeVersion}，实际为当前构建产物）`,
    pub_date: new Date().toISOString(),
    platforms,
  },
  null,
  2,
);

createServer((req, res) => {
  const path = decodeURIComponent(new URL(req.url, "http://x").pathname);
  if (path === "/latest.json") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(latest);
    console.log(`[mock] latest.json → ${req.socket.remoteAddress}`);
    return;
  }
  try {
    const file = files.find((f) => f.replaceAll("\\", "/").split("/bundle/")[1] === path.slice(1));
    if (!file) throw new Error("not found");
    res.writeHead(200);
    readFileSync(file).pipe(res);
    console.log(`[mock] 安装包下载：${path}`);
  } catch {
    res.writeHead(404).end("not found");
  }
}).listen(PORT, () => {
  console.log(`[mock] 更新源已就绪：http://127.0.0.1:${PORT}/latest.json（版本 ${fakeVersion}）`);
  console.log("[mock] Ctrl+C 停止");
});
