#!/usr/bin/env node
//! 统一更新全仓库版本号，并刷新两个锁文件。
//!
//! 版本号声明位置：
//!   - package.json（根）          —— pnpm 工作区根
//!   - ui/package.json             —— 前端包
//!   - src-tauri/tauri.conf.json   —— 桌面安装包版本（tauri-action 打包产物取此处）
//!   - src-tauri/Cargo.toml        —— Rust crate（[package] version）
//! 锁文件：src-tauri/Cargo.lock 由 `cargo update -w` 刷新（只同步工作区成员自身版本，不动三方依赖）；
//!         pnpm-lock.yaml 由 `pnpm install --lockfile-only` 刷新（只改锁文件，不装依赖）。
//!
//! 完整发版流程见 .agents/skills/codewave-release/SKILL.md（门禁 → bump → commit → 确认后推 tag）。
//! 用法：pnpm bump 0.2.1   （或 node scripts/bump-version.mjs 0.2.1）
import { execSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const arg = process.argv[2];
if (!arg || !/^v?\d+\.\d+\.\d+(-[\w.]+)?$/.test(arg)) {
  console.error("用法: pnpm bump <x.y.z>（如 0.2.1；支持 -预发布 后缀）");
  process.exit(1);
}
const version = arg.replace(/^v/, "");

const files = [
  "package.json",
  "ui/package.json",
  "src-tauri/tauri.conf.json",
];

// 守卫看「正则是否命中」而非「结果串是否变化」：同版本 bump 是合法 no-op，
// 结果串必然等于原文，不能当失败处理
const versionField = /("version":\s*)"[^"]+"/;

for (const file of files) {
  const s = readFileSync(file, "utf8");
  if (!versionField.test(s)) {
    console.error(`[bump] ${file}: 未找到 version 字段`);
    process.exit(1);
  }
  writeFileSync(file, s.replace(versionField, `$1"${version}"`));
  console.log(`[bump] ${file} -> ${version}`);
}

const cargo = readFileSync("src-tauri/Cargo.toml", "utf8");
// [package] version 是全文件第一个行首 version = 行（依赖均为 foo = { version = ... } 行内形态，不会误命中）
const cargoVersion = /^version = "[^"]+"/m;
if (!cargoVersion.test(cargo)) {
  console.error("[bump] src-tauri/Cargo.toml: 未找到 [package] version");
  process.exit(1);
}
writeFileSync("src-tauri/Cargo.toml", cargo.replace(cargoVersion, `version = "${version}"`));
console.log(`[bump] src-tauri/Cargo.toml -> ${version}`);

execSync("cargo update -w", { cwd: "src-tauri", stdio: "inherit" });
execSync("pnpm install --lockfile-only", { stdio: "inherit" });
console.log(`[bump] 锁文件已刷新，全部版本 -> ${version}`);
