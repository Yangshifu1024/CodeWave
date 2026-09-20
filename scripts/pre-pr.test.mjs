// pre-pr.mjs 的守门测试：本地门禁必须覆盖 CI 会跑的**每一条**检查。
// 这里不重复 workflow 的命令字符串，而是从 .github/workflows/*.yml 里解析 run: 语句做双向断言——
// 将来 CI 加了新检查而本地门禁没跟上，本测试就会红。
//
// 只依赖 node 内置模块（仓库 scripts/ 的既有约定）。

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { PRE_PR_STEPS, summarize, selectSteps } from "./pre-pr.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WORKFLOWS = ["lint.yml", "test.yml"].map((f) => resolve(ROOT, ".github/workflows", f));

/** 从 workflow 文本里取所有 `run:` 命令（单行 scalar 与块 scalar；块内按行拆分，反斜杠续行合并） */
function runCommands(text) {
  const out = [];
  const lines = text.split("\n");
  // YAML 块 scalar 指示符（`run: |` / `run: >-`）本身不是命令
  const BLOCK_MARKER = /^(\||>|-|\+)+$/;
  for (let i = 0; i < lines.length; i++) {
    const m = /^(\s*)run:\s*(.*)$/.exec(lines[i]);
    if (!m) continue;
    const [, indent, inlineHead] = m;
    const inline = inlineHead.trim();
    if (inline && !BLOCK_MARKER.test(inline)) {
      out.push(inline);
      continue;
    }
    // 块 scalar：收集比 run: 更深缩进的行，续行（反斜杠结尾）与下一行合并成一条
    const block = [];
    for (let j = i + 1; j < lines.length; j++) {
      const line = lines[j];
      if (!line.trim()) continue;
      if (line.length - line.trimStart().length <= indent.length) break;
      const prev = block[block.length - 1];
      if (prev && prev.endsWith("\\")) block[block.length - 1] = `${prev} ${line.trim()}`;
      else block.push(line.trim());
    }
    out.push(...block);
  }
  return out;
}

/** 不是「检查」的步骤：装依赖/系统包等环境准备，本地无需复现；多行脚本的续行片段也在此滤掉 */
const NON_CHECK = /^(sudo\s|choco\s|cmake\s|lib|#)/;

/** 把命令归一化为可比较的形态（去引号、压空白） */
const norm = (s) => s.replace(/["']/g, "").replace(/\s+/g, " ").trim();

test("本地门禁覆盖 CI 两个 workflow 里的每条检查命令", () => {
  const ciCommands = new Set();
  for (const file of WORKFLOWS) {
    for (const cmd of runCommands(readFileSync(file, "utf8"))) {
      if (NON_CHECK.test(cmd)) continue;
      ciCommands.add(norm(cmd));
    }
  }
  assert.ok(ciCommands.size >= 5, `解析出的 CI 命令过少，解析器可能失效：${[...ciCommands]}`);

  const local = new Set(PRE_PR_STEPS.map((s) => norm([s.cmd, ...s.args].join(" "))));
  const uncovered = [...ciCommands].filter((c) => !local.has(c));
  assert.deepEqual(
    uncovered,
    [],
    `CI 跑了但本地门禁没覆盖（请在 scripts/pre-pr.mjs 的 PRE_PR_STEPS 补上）：\n${uncovered.join("\n")}`,
  );
});

test("软/硬门槛与 CI 的 continue-on-error 一致", () => {
  const lint = readFileSync(WORKFLOWS[0], "utf8");
  // CI 里唯一带 continue-on-error 的是 clippy；本地也必须只有 clippy 是软步骤
  const softInCi = /- name:\s*cargo clippy\n(?:\s+.*\n)*?\s+continue-on-error:\s*true/.test(lint);
  assert.ok(softInCi, "lint.yml 的 clippy 不再是 continue-on-error：需同步调整本测试与 pre-pr.mjs 的 soft 标记");

  const soft = PRE_PR_STEPS.filter((s) => s.soft).map((s) => s.name);
  assert.deepEqual(soft, ["rust-clippy"], "本地软步骤集合应与 CI 的 continue-on-error 集合一致");
});

test("步骤名唯一且无空字段（汇总与 --only 依赖名字）", () => {
  const names = PRE_PR_STEPS.map((s) => s.name);
  assert.equal(new Set(names).size, names.length, "步骤名必须唯一");
  for (const s of PRE_PR_STEPS) {
    assert.ok(s.name && s.title && s.cmd, `步骤字段不全：${JSON.stringify(s)}`);
    assert.ok(Array.isArray(s.args), `args 必须是数组：${s.name}`);
  }
});

test("summarize：软失败不阻断，硬失败阻断", () => {
  assert.deepEqual(summarize([{ name: "a", soft: false, ok: true }]), {
    ok: true,
    hardFailures: [],
    softFailures: [],
  });
  assert.deepEqual(summarize([{ name: "clippy", soft: true, ok: false }]), {
    ok: true,
    hardFailures: [],
    softFailures: ["clippy"],
  });
  const hard = summarize([{ name: "rust-test", soft: false, ok: false }]);
  assert.equal(hard.ok, false);
  assert.deepEqual(hard.hardFailures, ["rust-test"]);
});

test("selectSteps：空筛选返回全部，按名筛选保序", () => {
  assert.equal(selectSteps(PRE_PR_STEPS, []).length, PRE_PR_STEPS.length);
  const picked = selectSteps(PRE_PR_STEPS, ["rust-fmt", "ui-test"]);
  assert.deepEqual(picked.map((s) => s.name), ["rust-fmt", "ui-test"]);
});
