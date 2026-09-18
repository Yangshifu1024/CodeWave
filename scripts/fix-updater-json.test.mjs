//! scripts/fix-updater-json.mjs 的单测：改写语义、幂等、报错边界，以及 CLI 双形态。
//! 零第三方依赖，仅 node 内置（node:test + node:assert/strict + node:fs/os/child_process）。
//! 运行：node --test "scripts/**/*.test.mjs"
//!   —— glob 必须用双引号包裹，交给 node 自行展开（Node 官方要求）：node --test 不认目录参数，
//!      `node --test scripts/` 在 Node 22 与 24 上都会 Cannot find module。
//!      CI 见 .github/workflows/test.yml 的「Scripts tests」。
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  buildDownloadUrl,
  matchAssetApiUrl,
  resolveRepoFromEnv,
  rewriteUpdaterUrls,
} from "./fix-updater-json.mjs";

const README_OWNER = "Yangshifu1024";
const README_REPO = "CodeWave";
const README_OPTIONS = { tag: "v0.3.8", owner: README_OWNER, repo: README_REPO };
const SCRIPT_PATH = fileURLToPath(new URL("./fix-updater-json.mjs", import.meta.url));

const apiUrl = (id, { owner = README_OWNER, repo = README_REPO } = {}) =>
  `https://api.github.com/repos/${owner}/${repo}/releases/assets/${id}`;

// v0.3.8 真实资产清单（REST API 数字 id + 文件名；取自 gh api repos/.../releases/tags/v0.3.8）
const README_ASSETS = [
  { id: 572428501, name: "CodeWave-0.3.8-1.x86_64.rpm" },
  { id: 572428541, name: "CodeWave-0.3.8-1.x86_64.rpm.sig" },
  { id: 572426046, name: "CodeWave_0.3.8_aarch64.app.tar.gz" },
  { id: 572426076, name: "CodeWave_0.3.8_aarch64.app.tar.gz.sig" },
  { id: 572426812, name: "CodeWave_0.3.8_aarch64.dmg" },
  { id: 572428553, name: "CodeWave_0.3.8_amd64.AppImage" },
  { id: 572428634, name: "CodeWave_0.3.8_amd64.AppImage.sig" },
  { id: 572428456, name: "CodeWave_0.3.8_amd64.deb" },
  { id: 572428487, name: "CodeWave_0.3.8_amd64.deb.sig" },
  { id: 572436251, name: "CodeWave_0.3.8_x64-setup.exe" },
  { id: 572436280, name: "CodeWave_0.3.8_x64-setup.exe.sig" },
  { id: 572436176, name: "CodeWave_0.3.8_x64_en-US.msi" },
  { id: 572436235, name: "CodeWave_0.3.8_x64_en-US.msi.sig" },
  { id: 572436322, name: "latest.json" },
];

/** 真实 v0.3.8 清单形状：9 个平台条目、url 全为 api 资产端点、同 id 被多条共用 */
function realShapeManifest() {
  const platforms = {
    "darwin-aarch64": { signature: "sig-darwin", url: apiUrl(572426046) },
    "darwin-aarch64-app": { signature: "sig-darwin", url: apiUrl(572426046) },
    "linux-x86_64": { signature: "sig-appimage", url: apiUrl(572428553) },
    "linux-x86_64-appimage": { signature: "sig-appimage", url: apiUrl(572428553) },
    "linux-x86_64-deb": { signature: "sig-deb", url: apiUrl(572428456) },
    "linux-x86_64-rpm": { signature: "sig-rpm", url: apiUrl(572428501) },
    "windows-x86_64": { signature: "sig-msi", url: apiUrl(572436176) },
    "windows-x86_64-msi": { signature: "sig-msi", url: apiUrl(572436176) },
    "windows-x86_64-nsis": { signature: "sig-nsis", url: apiUrl(572436251) },
  };
  return {
    version: "0.3.8",
    notes: "",
    pub_date: "2026-09-18T10:57:30.617Z",
    platforms,
  };
}

const platformUrl = (manifest, key) => manifest.platforms[key].url;
const downloadUrl = (name) => `https://github.com/${README_OWNER}/${README_REPO}/releases/download/v0.3.8/${name}`;

test("同一 asset id 被多个平台条目共用 → 改写后 url 完全一致", () => {
  const manifest = realShapeManifest();
  const rewritten = rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS);

  // darwin 两条同 id 572426046
  assert.equal(platformUrl(rewritten, "darwin-aarch64"), downloadUrl("CodeWave_0.3.8_aarch64.app.tar.gz"));
  assert.equal(
    platformUrl(rewritten, "darwin-aarch64-app"),
    platformUrl(rewritten, "darwin-aarch64"),
    "同 id 的两个 darwin 条目必须改写为同一 url",
  );
  // linux appimage 两条同 id 572428553
  assert.equal(platformUrl(rewritten, "linux-x86_64"), downloadUrl("CodeWave_0.3.8_amd64.AppImage"));
  assert.equal(platformUrl(rewritten, "linux-x86_64-appimage"), platformUrl(rewritten, "linux-x86_64"));
  // windows 的 x86_64 与 msi 两条同 id 572436176
  assert.equal(platformUrl(rewritten, "windows-x86_64"), downloadUrl("CodeWave_0.3.8_x64_en-US.msi"));
  assert.equal(platformUrl(rewritten, "windows-x86_64-msi"), platformUrl(rewritten, "windows-x86_64"));
});

test("asset name 含空格 / + / # / 中文 → 正确 percent-encode", () => {
  const trickyName = "CodeWave 1.0+fix#1_中文.dmg";
  const manifest = {
    version: "1.0.0",
    platforms: {
      "darwin-aarch64": { signature: "s", url: apiUrl(7, { owner: "me", repo: "my.repo" }) },
    },
  };
  const rewritten = rewriteUpdaterUrls(manifest, [{ id: 7, name: trickyName }], {
    tag: "v1.0.0",
    owner: "me",
    repo: "my.repo",
  });
  const url = rewritten.platforms["darwin-aarch64"].url;

  assert.equal(url, `https://github.com/me/my.repo/releases/download/v1.0.0/${encodeURIComponent(trickyName)}`);
  assert.ok(url.includes("%20"), "空格应编码为 %20");
  assert.ok(url.includes("%2B"), "+ 应编码为 %2B");
  assert.ok(url.includes("%23"), "# 应编码为 %23");
  assert.ok(url.includes("%E4%B8%AD%E6%96%87"), "中文应 percent-encode（# 之后的片段不会被当 fragment 丢掉）");
  assert.ok(!url.includes(" "), "url 里不得出现裸空格");
  assert.ok(!url.includes("#"), "url 里不得出现裸 #（否则 # 之后会被当 fragment 丢掉）");
});

test("已是 releases/download/ 前缀 → 幂等不改写", () => {
  const already = downloadUrl("CodeWave_0.3.8_x64_en-US.msi");
  const manifest = {
    version: "0.3.8",
    platforms: {
      "windows-x86_64": { signature: "s", url: already },
      "windows-x86_64-msi": { signature: "s", url: `https://example.com/self-hosted/CodeWave.msi` },
      "windows-x86_64-nsis": { signature: "s", url: apiUrl(999999999) },
    },
  };
  const rewritten = rewriteUpdaterUrls(manifest, [{ id: 999999999, name: "CodeWave_0.3.8_x64-setup.exe" }], README_OPTIONS);

  assert.equal(platformUrl(rewritten, "windows-x86_64"), already, "已是 CDN url 必须原样保留");
  assert.equal(
    platformUrl(rewritten, "windows-x86_64-msi"),
    "https://example.com/self-hosted/CodeWave.msi",
    "非 api 资产端点的 url 必须原样保留",
  );
  assert.equal(platformUrl(rewritten, "windows-x86_64-nsis"), downloadUrl("CodeWave_0.3.8_x64-setup.exe"));
});

test("未知 asset id → 抛错而非写坏 url", () => {
  const manifest = {
    version: "0.3.8",
    platforms: { "darwin-aarch64": { signature: "s", url: apiUrl(123456789) } },
  };
  assert.throws(
    () => rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS),
    /123456789/,
    "查不到的 id 必须抛错",
  );
  // 缺必需参数也要抛
  assert.throws(() => rewriteUpdaterUrls(manifest, README_ASSETS, { owner: "a", repo: "b" }), /tag/);
  assert.throws(() => rewriteUpdaterUrls(manifest, README_ASSETS, { tag: "v1", repo: "b" }), /owner/);
  assert.throws(() => rewriteUpdaterUrls(manifest, null, README_OPTIONS), /assets/);
});

// R1：asset name 空值加固（空串 / 全空白 / 非字符串 → 报错，不得拼出 `.../<tag>/` 死链）
test("asset name 为空串 / 全空白 / 非字符串 → 抛错而非拼出无文件名的死链", () => {
  const manifest = {
    version: "0.3.8",
    platforms: { "darwin-aarch64": { signature: "s", url: apiUrl(572426046) } },
  };

  for (const badName of ["", "   ", "\t\n", 42, null, undefined]) {
    assert.throws(
      () => rewriteUpdaterUrls(manifest, [{ id: 572426046, name: badName }], README_OPTIONS),
      (error) =>
        /没有可用的文件名/.test(error.message) && /572426046/.test(error.message),
      `name=${JSON.stringify(badName) ?? "undefined"} 必须抛错（不得拼出无文件名的 url）`,
    );
  }

  // 对照：合法名字仍能正常改写（加固不得误伤正常路径）
  const ok = rewriteUpdaterUrls(
    manifest,
    [{ id: 572426046, name: "CodeWave_0.3.8_aarch64.app.tar.gz" }],
    README_OPTIONS,
  );
  assert.equal(platformUrl(ok, "darwin-aarch64"), downloadUrl("CodeWave_0.3.8_aarch64.app.tar.gz"));
});

test("tag 含版本号而 asset name 也含同版本号 → 只替换 repo 前缀，asset name 不被误改", () => {
  const name = "CodeWave_0.3.8_aarch64.app.tar.gz";
  const manifest = { version: "0.3.8", platforms: { "darwin-aarch64": { signature: "s", url: apiUrl(572426046) } } };
  const url = platformUrl(rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS), "darwin-aarch64");

  assert.equal(url, downloadUrl(name));
  assert.ok(url.endsWith(`/v0.3.8/${name}`), "tag 段之后的 asset name 保持原样");
  assert.equal(url.split("/").at(-1), name, "asset name 必须与 release 资产名逐字相同");
});

test("真实 v0.3.8 清单整体改写：无 api.github.com 残留，url 全为 CDN 形态", () => {
  const manifest = realShapeManifest();
  const rewritten = rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS);
  const serialized = JSON.stringify(rewritten);

  assert.ok(!serialized.includes("api.github.com"), "改写结果不得残留 api.github.com");
  assert.deepEqual(Object.keys(rewritten.platforms), Object.keys(manifest.platforms), "平台条目集合不变");
  for (const [key, entry] of Object.entries(rewritten.platforms)) {
    assert.ok(entry.url.startsWith("https://github.com/Yangshifu1024/CodeWave/releases/download/v0.3.8/"), `${key} url 形态不符`);
    assert.equal(entry.signature, manifest.platforms[key].signature, `${key} signature 不得改动`);
  }
  // 头部字段一律不动
  assert.equal(rewritten.version, manifest.version);
  assert.equal(rewritten.notes, manifest.notes);
  assert.equal(rewritten.pub_date, manifest.pub_date);
  // 入参不被修改
  assert.ok(JSON.stringify(manifest).includes("api.github.com"), "原清单对象不得被就地修改");
});

test("幂等：对已改写结果再跑一次 → 结果不变", () => {
  const manifest = realShapeManifest();
  const once = rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS);
  const twice = rewriteUpdaterUrls(once, README_ASSETS, README_OPTIONS);
  assert.deepEqual(twice, once);
  assert.equal(JSON.stringify(twice), JSON.stringify(once));
});

// R2：平台键聚合容器加固（__proto__ 键不得被静默丢弃）
test("平台条目键为 __proto__ 时不得被静默丢弃", () => {
  // 必须经 JSON.parse 构造：对象字面量里的 __proto__ 只改原型、不是自身属性
  const manifest = JSON.parse(`{
    "version": "0.3.8",
    "platforms": {
      "darwin-aarch64": { "signature": "sig-darwin", "url": "${apiUrl(572426046)}" },
      "__proto__": { "signature": "sig-nsis", "url": "${apiUrl(572436251)}" }
    }
  }`);
  assert.deepEqual(
    Object.keys(manifest.platforms),
    ["darwin-aarch64", "__proto__"],
    "前置条件：输入确实含 __proto__ 自身属性",
  );

  const rewritten = rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS);

  assert.deepEqual(
    Object.keys(rewritten.platforms),
    ["darwin-aarch64", "__proto__"],
    "__proto__ 条目必须仍在，不得被静默丢弃",
  );
  assert.equal(rewritten.platforms["__proto__"].url, downloadUrl("CodeWave_0.3.8_x64-setup.exe"));
  assert.equal(rewritten.platforms["__proto__"].signature, "sig-nsis", "条目其余字段原样保留");
  assert.ok(!JSON.stringify(rewritten).includes("api.github.com"));
  // 聚合容器无原型，不会把 Object.prototype 之类混进序列化结果
  assert.equal(Object.getPrototypeOf(rewritten.platforms), null);
});

// R3：正则宽松判据（大写 host / 尾随斜杠 / ?query / #frag 均命中，且改写生效而非静默透传）
test("R3 宽松判据：大写 host / 尾随斜杠 / ?query / #frag 均命中且改写生效", () => {
  const trailingSlash = `${apiUrl(572426046)}/`;
  const upperHost = apiUrl(572436251).replace("api.github.com", "API.GitHub.com");
  const upperHostQuery = `${apiUrl(572428456).replace("api.github.com", "API.GitHub.com")}/?page=2`;

  assert.equal(matchAssetApiUrl(trailingSlash, README_OWNER, README_REPO), "572426046", "尾随斜杠应命中");
  assert.equal(matchAssetApiUrl(upperHost, README_OWNER, README_REPO), "572436251", "大写 host 应命中");
  assert.equal(
    matchAssetApiUrl(`${trailingSlash}?per_page=1`, README_OWNER, README_REPO),
    "572426046",
    "尾随斜杠 + ?query 应命中",
  );
  assert.equal(matchAssetApiUrl(`${upperHost}#frag`, README_OWNER, README_REPO), "572436251", "大写 host + #frag 应命中");
  // 边界：host 必须完整匹配 api.github.com，不得被近似域名 / 其他 repo 误伤
  assert.equal(
    matchAssetApiUrl(
      `https://api.github.com.evil.com/repos/${README_OWNER}/${README_REPO}/releases/assets/7`,
      README_OWNER,
      README_REPO,
    ),
    null,
    "api.github.com.<其他域名> 不得命中",
  );
  assert.equal(
    matchAssetApiUrl(
      `https://api.github.com/repos/${README_OWNER}/other-repo/releases/assets/7`,
      README_OWNER,
      README_REPO,
    ),
    null,
    "其他 repo 的 api 端点不得命中",
  );

  // 改写生效（而非静默透传写盘）
  const manifest = {
    version: "0.3.8",
    platforms: {
      "darwin-aarch64": { signature: "sig-darwin", url: trailingSlash },
      "windows-x86_64-nsis": { signature: "sig-nsis", url: upperHost },
      "linux-x86_64-deb": { signature: "sig-deb", url: upperHostQuery },
    },
  };
  const rewritten = rewriteUpdaterUrls(manifest, README_ASSETS, README_OPTIONS);

  assert.equal(platformUrl(rewritten, "darwin-aarch64"), downloadUrl("CodeWave_0.3.8_aarch64.app.tar.gz"));
  assert.equal(platformUrl(rewritten, "windows-x86_64-nsis"), downloadUrl("CodeWave_0.3.8_x64-setup.exe"));
  assert.equal(platformUrl(rewritten, "linux-x86_64-deb"), downloadUrl("CodeWave_0.3.8_amd64.deb"));
  assert.ok(!/api\.github\.com/i.test(JSON.stringify(rewritten)), "改写结果不得残留 api.github.com（大小写无关）");
});

test("matchAssetApiUrl / buildDownloadUrl 契约", () => {
  assert.equal(matchAssetApiUrl(apiUrl(572426046), README_OWNER, README_REPO), "572426046");
  assert.equal(matchAssetApiUrl(`${apiUrl(572426046)}?per_page=1`, README_OWNER, README_REPO), "572426046");
  assert.equal(matchAssetApiUrl(`${apiUrl(572426046)}#frag`, README_OWNER, README_REPO), "572426046");
  assert.equal(matchAssetApiUrl(downloadUrl("a.dmg"), README_OWNER, README_REPO), null);
  assert.equal(matchAssetApiUrl(apiUrl(5, { owner: "other", repo: README_REPO }), README_OWNER, README_REPO), null);
  assert.equal(matchAssetApiUrl(undefined, README_OWNER, README_REPO), null);
  assert.equal(
    buildDownloadUrl({ owner: "o", repo: "r", tag: "v1.0.0", assetName: "a b+c.dmg" }),
    "https://github.com/o/r/releases/download/v1.0.0/a%20b%2Bc.dmg",
  );
});

test("resolveRepoFromEnv：解析 GITHUB_REPOSITORY，缺失/形态不对回落默认仓库", () => {
  assert.deepEqual(resolveRepoFromEnv({ GITHUB_REPOSITORY: "acme/widget" }), { owner: "acme", repo: "widget" });
  assert.deepEqual(resolveRepoFromEnv({}), { owner: README_OWNER, repo: README_REPO });
  assert.deepEqual(resolveRepoFromEnv({ GITHUB_REPOSITORY: "no-slash" }), { owner: README_OWNER, repo: README_REPO });
});

// ---- CLI 形态（--assets-json 离线兜底，避开 gh 与网络）----

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "fix-updater-json-"));
  return fn(dir);
}

function runCli(args) {
  return execFileSync(process.execPath, [SCRIPT_PATH, ...args], { encoding: "utf8" });
}

test("CLI：--dry-run 只打印对照不写文件", () => {
  withTempDir((dir) => {
    const inFile = join(dir, "latest.json");
    const assetsFile = join(dir, "assets.json");
    const original = JSON.stringify(realShapeManifest(), null, 2);
    writeFileSync(inFile, original);
    writeFileSync(assetsFile, JSON.stringify(README_ASSETS));

    const stdout = runCli([
      "--tag", "v0.3.8", "--owner", README_OWNER, "--repo", README_REPO,
      "--in", inFile, "--out", inFile, "--assets-json", assetsFile, "--dry-run",
    ]);

    assert.ok(stdout.includes("dry-run"), "应提示 dry-run");
    // 对照格式："    - <旧 url>" / "    + <新 url>"
    assert.ok(
      stdout.includes(`    + ${downloadUrl("CodeWave_0.3.8_aarch64.app.tar.gz")}`),
      "应打印改写后的 CDN url",
    );
    assert.ok(stdout.includes(`    - ${apiUrl(572426046)}`), "应打印改写前的 api url 作为对照");
    assert.equal(readFileSync(inFile, "utf8"), original, "dry-run 不得写文件");
  });
});

test("CLI：默认写入 --out，同 id 多平台条目落盘后 url 一致且无 api 残留", () => {
  withTempDir((dir) => {
    const inFile = join(dir, "latest.json");
    const outFile = join(dir, "out.json");
    const assetsFile = join(dir, "assets.json");
    writeFileSync(inFile, JSON.stringify(realShapeManifest(), null, 2));
    writeFileSync(assetsFile, JSON.stringify({ assets: README_ASSETS }));

    const stdout = runCli([
      "--tag", "v0.3.8", "--owner", README_OWNER, "--repo", README_REPO,
      "--in", inFile, "--out", outFile, "--assets-json", assetsFile,
    ]);
    const written = JSON.parse(readFileSync(outFile, "utf8"));

    assert.ok(stdout.includes("已写入"), "应打印写入提示");
    assert.ok(!JSON.stringify(written).includes("api.github.com"));
    assert.equal(written.platforms["darwin-aarch64"].url, written.platforms["darwin-aarch64-app"].url);
    assert.equal(written.platforms["linux-x86_64"].url, written.platforms["linux-x86_64-appimage"].url);
  });
});

test("CLI：缺 --tag / 未知参数 → 退出码 1", () => {
  const cases = [[], ["--tag"], ["--nope"]];
  for (const args of cases) {
    assert.throws(
      () => runCli(args),
      (error) => error.status === 1,
      `参数 ${JSON.stringify(args)} 应退出码 1`,
    );
  }
});
