import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { cargoMetrics, environmentSummary, parseCargoDuration, timingSummary } from "./metrics.mjs";

test("Cargo duration accepts seconds/minutes and does not invent missing measurements", () => {
  assert.equal(parseCargoDuration("6m 33s"), 393);
  assert.equal(parseCargoDuration("53.69s"), 53.69);
  assert.equal(parseCargoDuration("unknown"), null);
  assert.equal(parseCargoDuration("1h 3m 4s"), null);
  assert.deepEqual(cargoMetrics(["a build error", "running 10 tests"]), { builds: [], suites: [] });
});

test("Cargo parser preserves failed/ignored counts, multiple suites and doctests", () => {
  const metrics = cargoMetrics([
    "    Finished `test` profile [optimized + debuginfo] target(s) in 6m 33s",
    "test result: ok. 1408 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 38.42s",
    "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s",
    "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.04s",
  ]);
  assert.deepEqual(metrics.builds, [393]);
  assert.equal(metrics.suites.length, 3);
  const summary = timingSummary("cargo test --workspace", 434, 101, metrics);
  assert.match(summary, /exit code: 101/);
  assert.match(summary, /393.00s/);
  assert.match(summary, /38.47s; passed: 1410; failed: 1; ignored: 6/);
  assert.doesNotMatch(timingSummary("cargo test", 1, 1, cargoMetrics([])), /build .*0.00s/);
});

test("Cargo ANSI colors are ignored by parsing and streamed unchanged by the wrapper", () => {
  const lines = [
    "\u001b[1m\u001b[92m    Finished\u001b[0m `test` profile [unoptimized + debuginfo] target(s) in 1m 00s",
    "\u001b[32mtest result: ok. 1408 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 39.08s\u001b[0m",
  ];
  assert.deepEqual(cargoMetrics(lines).builds, [60]);
  assert.equal(cargoMetrics(lines).suites[0].passed, 1408);
  const dir = mkdtempSync(join(tmpdir(), "codewave-ci-ansi-"));
  try {
    writeFileSync(join(dir, "colored.mjs"), `console.error(${JSON.stringify(lines.join("\n"))});`);
    const script = fileURLToPath(new URL("./metrics.mjs", import.meta.url));
    const summary = join(dir, "summary.md");
    const result = spawnSync(process.execPath, [script, "run", "--", "node", "colored.mjs"], { cwd: dir, encoding: "utf8", env: { ...process.env, GITHUB_STEP_SUMMARY: summary } });
    assert.equal(result.status, 0, result.stderr);
    assert.ok(result.stderr.includes(lines[0]), "raw command output must retain its colors");
    const text = readFileSync(summary, "utf8");
    assert.match(text, /Cargo test-profile build \(Cargo reported\): 60.00s/);
    assert.match(text, /39.08s; passed: 1408; failed: 0; ignored: 2/);
    assert.doesNotMatch(text, /\u001b/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("environment summary uses an allowlist and sanitizes Markdown cells", () => {
  const summary = environmentSummary({ RUNNER_OS: "Windows", RUNNER_ARCH: "X64", ImageVersion: "20261012", CI_RUST_CACHE_HIT: "false", SECRET_TOKEN: "must-not-leak", PATH: "private-path" }, { rustc: "rustc 1.99.0\n|<script>" }, { cpus: 4, ramGiB: 16 });
  assert.match(summary, /Windows \/ X64/);
  assert.match(summary, /Rust cache exact hit \| false/);
  assert.doesNotMatch(summary, /must-not-leak|private-path|<script>/);
  assert.match(summary, /partial restore/);
});

test("wrapper streams output, writes timing and preserves failures or spawn errors", () => {
  const dir = mkdtempSync(join(tmpdir(), "codewave-ci-metrics-"));
  try {
    const script = fileURLToPath(new URL("./metrics.mjs", import.meta.url));
    const summary = join(dir, "summary.md");
    const env = { ...process.env, GITHUB_STEP_SUMMARY: summary };
    const ok = spawnSync(process.execPath, [script, "run", "--", "node", "--version"], { env, encoding: "utf8" });
    assert.equal(ok.status, 0, ok.stderr);
    assert.match(ok.stdout, /v\d+\./);
    const failure = spawnSync(process.execPath, [script, "run", "--", "node", "--codewave-invalid-flag"], { env, encoding: "utf8" });
    assert.notEqual(failure.status, 0);
    const missing = spawnSync(process.execPath, [script, "run", "--", "codewave-nonexistent-command"], { env, encoding: "utf8" });
    assert.notEqual(missing.status, 0);
    const text = readFileSync(summary, "utf8");
    assert.match(text, /node --version/);
    assert.match(text, /codewave-nonexistent-command/);
    assert.match(text, /Wall time:/);
    const unsafe = spawnSync(process.execPath, [script, "run", "--", "node", "a&b"], { env, encoding: "utf8" });
    assert.notEqual(unsafe.status, 0);
    assert.match(unsafe.stderr, /shell-safe/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("cancellation cannot succeed even when the child handles the signal and exits zero", { skip: process.platform === "win32" ? "POSIX signal handling regression" : false, timeout: 10000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), "codewave-ci-signal-"));
  let child;
  try {
    writeFileSync(join(dir, "wait.mjs"), 'process.on("SIGINT", () => process.exit(0)); console.log("ready"); setInterval(() => {}, 1000);');
    const script = fileURLToPath(new URL("./metrics.mjs", import.meta.url));
    child = spawn(process.execPath, [script, "run", "--", "node", "wait.mjs"], { cwd: dir, env: { ...process.env, GITHUB_STEP_SUMMARY: join(dir, "summary.md") }, stdio: ["ignore", "pipe", "pipe"] });
    const closed = new Promise((resolve, reject) => { child.on("close", resolve); child.on("error", reject); });
    let sent = false;
    child.stdout.on("data", (chunk) => {
      if (!sent && chunk.toString().includes("ready")) { sent = true; child.kill("SIGINT"); }
    });
    const code = await closed;
    assert.equal(sent, true);
    assert.equal(code, 1);
    assert.match(readFileSync(join(dir, "summary.md"), "utf8"), /exit code: 1/);
  } finally {
    child?.kill("SIGKILL");
    rmSync(dir, { recursive: true, force: true });
  }
});
