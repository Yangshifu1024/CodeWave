import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";

const workflow = readFileSync(new URL("../../.github/workflows/test.yml", import.meta.url), "utf8").replace(/\r\n?/g, "\n");
const jobs = Object.fromEntries([...workflow.matchAll(/^  ([\w-]+):\n([\s\S]*?)(?=^  [\w-]+:\n|$(?![\s\S]))/gm)].map((match) => [match[1], match[2]]));

test("frontend/backend matrices retain all platforms and run independently", () => {
  for (const name of ["test", "frontend"]) {
    const job = jobs[name];
    assert.ok(job, name);
    for (const platform of ["macos-14", "ubuntu-24.04", "windows-2022"]) assert.ok(job.includes(platform));
    assert.match(job, /fail-fast: false/);
    assert.doesNotMatch(job, /^    (?:needs|if):/m);
    assert.doesNotMatch(job, /continue-on-error:/);
    assert.match(job, /node-version: "22"/);
    assert.match(job, /pnpm install --frozen-lockfile/);
  }
  assert.match(jobs.frontend, /run: node scripts\/ci\/metrics.mjs run -- pnpm --dir ui test/);
  assert.match(jobs.frontend, /run: node --test "scripts\/\*\*\/\*.test.mjs"/);
  assert.match(jobs.test, /run: node scripts\/ci\/metrics.mjs run -- pnpm --dir ui build/);
  assert.match(jobs.test, /working-directory: src-tauri\n\s+run: node ..\/scripts\/ci\/metrics.mjs run -- cargo test --workspace\n/);
  assert.match(jobs.test, /uses: swatinem\/rust-cache@v2/);
  assert.doesNotMatch(jobs.test, /shared-key:|prefix-key:|CARGO_PROFILE_|--lib|--no-default-features|--ignored/);
});

test("existing check names are always-run gates for both entire matrices", () => {
  const gate = jobs.validation;
  assert.match(gate, /name: Test \(\$\{\{ matrix.os \}\}\)/);
  assert.match(gate, /if: \$\{\{ always\(\) \}\}/);
  assert.match(gate, /needs: \[test, frontend\]/);
  for (const platform of ["macos-14", "ubuntu-24.04", "windows-2022"]) assert.ok(gate.includes(platform));
  assert.match(gate, /BACKEND_RESULT: \$\{\{ needs.test.result \}\}/);
  assert.match(gate, /FRONTEND_RESULT: \$\{\{ needs.frontend.result \}\}/);
  const command = /^        run: (.+)$/m.exec(gate)?.[1];
  assert.equal(command, 'test "$BACKEND_RESULT" = success && test "$FRONTEND_RESULT" = success');
  assert.doesNotMatch(gate, /continue-on-error:/);
  // The aggregation runner is Ubuntu. Every backend/frontend status pair must fail closed.
  if (process.platform !== "win32") {
    assert.match(command, /&&/);
    for (const key of ["BACKEND_RESULT", "FRONTEND_RESULT"]) {
      assert.ok(command.includes(`test "$${key}" = success`), key);
    }
    for (const backend of ["success", "failure", "cancelled", "skipped", ""]) {
      for (const frontend of ["success", "failure", "cancelled", "skipped", ""]) {
        const result = spawnSync("sh", ["-c", command], {
          env: { ...process.env, BACKEND_RESULT: backend, FRONTEND_RESULT: frontend },
        });
        const expected = backend === "success" && frontend === "success";
        assert.equal(result.status === 0, expected, `${backend}/${frontend}`);
      }
    }
  }
});

test("Dependabot groups only minor/patch version updates in each existing ecosystem", () => {
  const text = readFileSync(new URL("../../.github/dependabot.yml", import.meta.url), "utf8").replace(/\r\n?/g, "\n");
  const entries = text.split("  - package-ecosystem: ").slice(1);
  assert.equal(entries.length, 3);
  assert.deepEqual(entries.map((entry) => entry.split("\n")[0]), ["cargo", "npm", "github-actions"]);
  for (const entry of entries) {
    assert.match(entry, /interval: weekly/);
    assert.match(entry, /applies-to: version-updates/);
    assert.match(entry, /patterns: \["\*"\]/);
    assert.match(entry, /update-types: \[minor, patch\]/);
    assert.doesNotMatch(entry, /major|security-updates/);
    if (entry.startsWith("npm\n")) {
      // Only the TS 7 compatibility hold is allowed; TS 6 and other dependencies stay enabled.
      const ignore = /^    ignore:\n([\s\S]*?)(?=^    \S)/m.exec(entry)?.[1];
      assert.ok(ignore);
      assert.equal(
        ignore.replace(/^\s*#.*\n/gm, ""),
        '      - dependency-name: typescript\n        versions: [">=7.0.0 <8.0.0"]\n',
      );
      assert.equal(entry.match(/^    ignore:/gm)?.length, 1);
    } else {
      assert.doesNotMatch(entry, /ignore:/);
    }
  }
});
