import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const probe = spawnSync('pwsh', ['-NoProfile', '-Command', '$PSVersionTable.PSVersion.ToString()'], {
  encoding: 'utf8',
});

test('Windows CMake reuse, fallback, version verification and PATH handling', {
  skip: probe.error?.code === 'ENOENT' && process.platform !== 'win32'
    ? 'PowerShell is unavailable on this platform; exercised by Windows CI'
    : false,
}, () => {
  assert.ifError(probe.error);
  assert.equal(probe.status, 0, probe.stderr);
  const result = spawnSync('pwsh', [
    '-NoProfile', '-NonInteractive', '-File',
    fileURLToPath(new URL('./ensure-cmake.tests.ps1', import.meta.url)),
  ], { encoding: 'utf8' });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});
