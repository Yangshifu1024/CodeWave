// 开 PR 前的本地门禁：逐条跑完 CI 会跑的全部检查（.github/workflows/lint.yml + test.yml），
// 全绿才推 PR——这样「CI 才发现的问题」不再占用一轮 PR 反馈。
//
// 设计要点：
// - 步骤清单是可导出的纯数据（PRE_PR_STEPS），pre-pr.test.mjs 用它对照两个 workflow 的 run:
//   语句做双向断言：CI 加了检查而这里没跟上 → 测试红。
// - shell: false 逐参数执行（跨平台一致；node 自行展开 `scripts/**/*.test.mjs` 的 glob，
//   与 CI 的做法一致——CI 注释记录了不交给 shell 展开的原因）。
// - 唯一软步骤：clippy。CI 里它是 continue-on-error（存量测试告警未清），本地同样只报告不阻断。
// - 本地只能覆盖当前平台；CI 的三平台矩阵（macos-14 / ubuntu-24.04 / windows-2022）里
//   平台特有差异仍需 CI 兜底，故「本地全绿 → CI 全绿」是预期而非保证。

import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 门禁步骤：name = 报告用短名；soft = CI 侧 continue-on-error（失败不阻断） */
export const PRE_PR_STEPS = [
  {
    name: "lockfile",
    title: "依赖锁定（pnpm install --frozen-lockfile）",
    cmd: "pnpm",
    args: ["install", "--frozen-lockfile"],
    cwd: ROOT,
    soft: false,
  },
  {
    name: "rust-fmt",
    title: "Rust 格式（cargo fmt --all -- --check）",
    cmd: "cargo",
    args: ["fmt", "--all", "--", "--check"],
    cwd: resolve(ROOT, "src-tauri"),
    soft: false,
  },
  {
    name: "rust-clippy",
    title: "Rust lint（cargo clippy --all-targets，CI 侧软门槛）",
    cmd: "cargo",
    args: ["clippy", "--all-targets"],
    cwd: resolve(ROOT, "src-tauri"),
    soft: true,
  },
  {
    name: "rust-test",
    title: "Rust 测试（cargo test --workspace）",
    cmd: "cargo",
    args: ["test", "--workspace"],
    cwd: resolve(ROOT, "src-tauri"),
    soft: false,
  },
  {
    name: "ui-lint",
    title: "前端 lint（pnpm --dir ui run lint）",
    cmd: "pnpm",
    args: ["--dir", "ui", "run", "lint"],
    cwd: ROOT,
    soft: false,
  },
  {
    name: "ui-test",
    title: "前端测试（pnpm --dir ui test）",
    cmd: "pnpm",
    args: ["--dir", "ui", "test"],
    cwd: ROOT,
    soft: false,
  },
  {
    name: "ui-build",
    title: "前端构建（pnpm --dir ui build）",
    cmd: "pnpm",
    args: ["--dir", "ui", "build"],
    cwd: ROOT,
    soft: false,
  },
  {
    name: "scripts-test",
    title: "脚本测试（node --test scripts/**/*.test.mjs）",
    cmd: "node",
    args: ["--test", "scripts/**/*.test.mjs"],
    cwd: ROOT,
    soft: false,
  },
];

/** 结果汇总：任一硬步骤失败 → ok=false（软步骤失败只进 softFailures） */
export function summarize(results) {
  const softFailures = results.filter((r) => !r.ok && r.soft).map((r) => r.name);
  const hardFailures = results.filter((r) => !r.ok && !r.soft).map((r) => r.name);
  return { ok: hardFailures.length === 0, hardFailures, softFailures };
}

/** 按名筛选步骤；names 为空表示全选 */
export function selectSteps(steps, names) {
  if (!names || names.length === 0) return steps;
  return steps.filter((s) => names.includes(s.name));
}

function runStep(step) {
  process.stdout.write(`\n▶ ${step.title}\n`);
  const res = spawnSync(step.cmd, step.args, { cwd: step.cwd, stdio: "inherit" });
  const ok = res.status === 0;
  process.stdout.write(`${ok ? "✓" : step.soft ? "⚠" : "✗"} ${step.name}\n`);
  return { name: step.name, soft: step.soft, ok };
}

function parseArgs(argv) {
  const names = [];
  const opts = { only: names, skipSoft: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--skip-soft") opts.skipSoft = true;
    else if (a.startsWith("--only=")) names.push(...a.slice(7).split(",").filter(Boolean));
    else if (a === "--only") names.push(...(argv[++i] ?? "").split(",").filter(Boolean));
    else if (a === "-h" || a === "--help") opts.help = true;
  }
  return opts;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log(`用法：pnpm prepr [--only=a,b] [--skip-soft]

步骤名：${PRE_PR_STEPS.map((s) => s.name).join(", ")}
--only=<a,b>  只跑指定步骤
--skip-soft   跳过软步骤（clippy）`);
    return 0;
  }
  let steps = selectSteps(PRE_PR_STEPS, opts.only);
  if (opts.skipSoft) steps = steps.filter((s) => !s.soft);
  if (steps.length === 0) {
    console.error("没有可跑的步骤（检查 --only 的名称）");
    return 2;
  }

  const started = Date.now();
  const results = [];
  for (const step of steps) {
    const r = runStep(step);
    results.push(r);
    // 硬步骤失败即停：后续步骤大多依赖同一份代码，继续跑只是浪费时间与噪音
    if (!r.ok && !r.soft) break;
  }

  const { ok, hardFailures, softFailures } = summarize(results);
  const seconds = ((Date.now() - started) / 1000).toFixed(0);
  console.log(`\n──────── 本地门禁汇总（${seconds}s） ────────`);
  for (const r of results) {
    console.log(`${r.ok ? "✓" : r.soft ? "⚠" : "✗"} ${r.name}${r.ok ? "" : r.soft ? "（软门槛，CI 亦不阻断）" : "（失败）"}`);
  }
  if (softFailures.length) console.log(`\n软失败：${softFailures.join(", ")}`);
  if (!ok) {
    console.log(`\n硬失败：${hardFailures.join(", ")}`);
    console.log("先修掉再开 PR——CI 会跑同一套检查。");
    return 1;
  }
  console.log("\n全部硬步骤通过：可以开 PR（本地为当前平台，三平台差异仍以 CI 为准）。");
  return 0;
}

// 仅直接执行时跑 main（被 import 时不执行）：pre-pr.test.mjs 要读 PRE_PR_STEPS
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exit(main());
}
