// CI-only observation: preserve the command, stream its output, and never run tests twice.
import { spawn, spawnSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { cpus, totalmem } from "node:os";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

export function parseCargoDuration(value) {
  const match = /^(?:(\d+)m\s+)?(\d+(?:\.\d+)?)s$/.exec(value.trim());
  return match ? Number(match[1] ?? 0) * 60 + Number(match[2]) : null;
}

export function cargoMetrics(lines) {
  const builds = [];
  const suites = [];
  for (const line of lines) {
    const finished = /Finished `test` profile .* in (.+)$/.exec(line);
    if (finished) {
      const seconds = parseCargoDuration(finished[1]);
      if (seconds !== null) builds.push(seconds);
    }
    const suite = /^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;.*finished in ([\d.]+)s/.exec(line);
    if (suite) suites.push({ result: suite[1], passed: +suite[2], failed: +suite[3], ignored: +suite[4], seconds: +suite[5] });
  }
  return { builds, suites };
}

// Escape even allowlisted values before putting them in the GitHub Markdown summary.
export const cell = (value) => String(value ?? "unknown").replace(/[\r\n|`<>]/g, " ");

export function timingSummary(command, seconds, code, metrics) {
  const lines = [
    `### ${cell(command)}`,
    `- Wall time: ${seconds.toFixed(2)}s; exit code: ${cell(code)}`,
  ];
  if (metrics.builds.length) lines.push(`- Cargo test-profile build (Cargo reported): ${metrics.builds.reduce((a, b) => a + b, 0).toFixed(2)}s`);
  if (metrics.suites.length) {
    const sum = (key) => metrics.suites.reduce((n, suite) => n + suite[key], 0);
    lines.push(`- Test-suite execution (reported sum): ${sum("seconds").toFixed(2)}s; passed: ${sum("passed")}; failed: ${sum("failed")}; ignored: ${sum("ignored")}`);
    lines.push("- Suite sum includes reported unit/integration/doc-test results; it excludes Cargo overhead and doc-test compilation. Missing metrics are not zero.");
  }
  return `${lines.join("\n")}\n\n`;
}

function writeSummary(text) {
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, text);
}

function version(command) {
  const result = spawnSync(command, ["--version"], { encoding: "utf8", shell: process.platform === "win32" });
  return result.status === 0 ? result.stdout.trim().split(/\r?\n/)[0] : "unavailable";
}

export function environmentSummary(env, versions, hardware) {
  // Never dump env, action contexts or command output wholesale: these can contain secrets.
  const rows = [
    ["Runner", `${env.RUNNER_OS ?? "unknown"} / ${env.RUNNER_ARCH ?? "unknown"}`],
    ["Image", `${env.ImageOS ?? "unknown"} / ${env.ImageVersion ?? "unknown"}`],
    ["Logical CPUs / RAM GiB", `${hardware.cpus} / ${hardware.ramGiB}`],
    ["Rust cache exact hit", env.CI_RUST_CACHE_HIT || "not reported / not applicable"],
    ...Object.entries(versions),
  ];
  return `### CI environment\n\n| Metric | Value |\n|---|---|\n${rows.map(([key, value]) => `| ${cell(key)} | ${cell(value)} |`).join("\n")}\n\nA non-exact cache hit does not distinguish a partial restore from a miss; see the Rust cache step log for keys and restore/save sizes.\n\n`;
}

export async function run(command, args) {
  // The workflow uses fixed commands/arguments, not PR-controlled expression interpolation.
  // Windows pnpm.cmd needs a shell; reject metacharacters rather than re-quoting arbitrary input.
  if ([command, ...args].some((arg) => !arg || /[\s"'&|<>^`$;()]/.test(arg))) throw new Error("Expected fixed, shell-safe command arguments");
  const start = performance.now();
  const observations = [];
  const child = spawn(command, args, { stdio: ["inherit", "pipe", "pipe"], shell: process.platform === "win32" });
  for (const [stream, target] of [[child.stdout, process.stdout], [child.stderr, process.stderr]]) {
    stream.on("data", (chunk) => target.write(chunk));
    const reader = createInterface({ input: stream });
    reader.on("line", (line) => {
      if (/Finished `test` profile|^test result:/.test(line)) observations.push(line);
    });
  }
  // Forward cancellation; a killed command must never become a green wrapper step.
  let interrupted = false;
  const interrupt = () => { interrupted = true; child.kill("SIGINT"); };
  const terminate = () => { interrupted = true; child.kill("SIGTERM"); };
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", terminate);
  let spawnError;
  child.on("error", (error) => { spawnError = error; console.error(error.message); });
  const status = await new Promise((resolve) => child.on("close", (status) => resolve(status ?? 1)));
  process.off("SIGINT", interrupt);
  process.off("SIGTERM", terminate);
  const code = interrupted || spawnError ? 1 : status;
  writeSummary(timingSummary([command, ...args].join(" "), (performance.now() - start) / 1000, code, cargoMetrics(observations)));
  return code;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [mode, separator, command, ...args] = process.argv.slice(2);
  if (mode === "environment") {
    writeSummary(environmentSummary(process.env, Object.fromEntries(["node", "pnpm", "rustc", "cargo", "cmake"].map((tool) => [tool, version(tool)])), { cpus: cpus().length, ramGiB: (totalmem() / 1024 ** 3).toFixed(1) }));
  } else if (mode === "run" && separator === "--" && command) {
    process.exitCode = await run(command, args);
  } else {
    throw new Error("Usage: metrics.mjs environment | run -- <command> [args]");
  }
}
