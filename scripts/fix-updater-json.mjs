#!/usr/bin/env node
//! 把更新器清单 latest.json 里指向 GitHub REST 资产 API 的 url 改写为 CDN 下载 url。
//!
//! 背景（v0.3.8 实测）：tauri-action（action-v1.0.0，src/upload-version-json.ts）写清单时，
//! 每个平台的 url 用的是 REST 资产 API 端点
//!   https://api.github.com/repos/<owner>/<repo>/releases/assets/<id>
//! 该端点**匿名配额只有 60 次/小时/出口 IP**，更新检查一多就 403 Forbidden
//! （实测响应头 x-ratelimit-remaining: 0），应用内「检查更新」的下载阶段因此直接失败。
//! 而 release 资产另有 CDN 通道
//!   https://github.com/<owner>/<repo>/releases/download/<tag>/<assetName>
//! 由 CDN 承载、不计 API 配额，能正确 302 到 Blob（文件名保持原始形态，含空格亦可）。
//! 因此发布流程收尾时把清单里的 url 统一改写成后者。
//!
//! 注意：**同一 asset id 会被多个平台条目共用**（v0.3.8：id 572426046 同时是
//! darwin-aarch64 与 darwin-aarch64-app 的 url；id 572428553 同为 linux-x86_64 与
//! linux-x86_64-appimage），所以必须「按 id 建映射统一替换」，不能逐条按平台名猜文件名。
//!
//! 双形态：既可当 CLI 跑（pnpm 无关，仅 node 内置模块），也可 import 纯函数供单测使用。
//! 用法：node scripts/fix-updater-json.mjs --tag v0.3.8 [--owner X] [--repo Y]
//!                                      [--in latest.json] [--out latest.json] [--dry-run]
//!                                      [--assets-json <file>]
//! 测试：node --test "scripts/**/*.test.mjs"（glob 必须用双引号包裹，交由 node 自行展开——
//! node --test 不认目录参数，`node --test scripts/` 在 Node 22 与 24 上都会 Cannot find module）。
//! 相关：.github/workflows/release.yml 的 finalize-updater-json 作业；测试 scripts/fix-updater-json.test.mjs。

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** 未显式提供 owner/repo 且环境变量 GITHUB_REPOSITORY 缺失时的回落值 */
const DEFAULT_OWNER = "Yangshifu1024";
const DEFAULT_REPO = "CodeWave";
/** --in / --out 缺省文件名（CI 里在仓库根执行，gh 下载的同名文件） */
const DEFAULT_MANIFEST = "latest.json";

const USAGE = `用法: node scripts/fix-updater-json.mjs --tag <vX.Y.Z> [选项]

把 latest.json 里的 GitHub REST 资产 API url 改写为 CDN 下载 url。

选项:
  --tag <tag>           release tag（必填，如 v0.3.8；CI 里传 $GITHUB_REF_NAME）
  --owner <owner>       仓库 owner（默认取 GITHUB_REPOSITORY 的 owner，缺失回落到 ${DEFAULT_OWNER}）
  --repo <repo>         仓库名（默认取 GITHUB_REPOSITORY 的 repo，缺失回落到 ${DEFAULT_REPO}）
  --in <file>           输入清单（默认 ${DEFAULT_MANIFEST}）
  --out <file>          输出清单（默认等于 --in）
  --dry-run             只打印 url 前后对照，不写文件
  --assets-json <file>  离线兜底：从 JSON 文件读资产列表（数组，或 { "assets": [...] }），
                        跳过 gh api 调用（用于 gh 不可用 / 本地验证）
  -h, --help            显示本帮助
`;

/** 正则元字符转义（owner/repo 可能含 `.`、`-` 等） */
function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * 资产 API 端点的**唯一**判据：主匹配与「改写后兜底断言」共用同一份，避免两处松紧不一致。
 * 形态：https://api.github.com/repos/<owner>/<repo>/releases/assets/<id>
 *   - host 大小写不敏感（`i` 标志；GitHub 的 owner/repo 本身也大小写不敏感）
 *   - 允许尾随斜杠：/assets/7/
 *   - 允许 ?query / #frag 结尾：/assets/7?per_page=1、/assets/7#frag
 * host 段由 `api\.github\.com/repos/` 锚定，`api.github.com.evil.com` 之类不会被误命中。
 */
function assetApiPattern(owner, repo) {
  return new RegExp(
    `^https?://api\\.github\\.com/repos/${escapeRegExp(owner)}/${escapeRegExp(repo)}/releases/assets/(\\d+)/?(?:$|[?#])`,
    "i",
  );
}

/**
 * 判断 url 是否为 GitHub REST 资产 API 端点，是则返回其中的数字 asset id（字符串），否则 null。
 */
export function matchAssetApiUrl(url, owner, repo) {
  if (typeof url !== "string") return null;
  const matched = assetApiPattern(owner, repo).exec(url);
  return matched ? matched[1] : null;
}

/** 拼 CDN 下载 url（asset name 必须 percent-encode，否则含空格/`#`/中文的名字会拼出坏 URL） */
export function buildDownloadUrl({ owner, repo, tag, assetName }) {
  return `https://github.com/${owner}/${repo}/releases/download/${tag}/${encodeURIComponent(assetName)}`;
}

/**
 * 把清单里所有 api 资产端点 url 改写为 CDN 下载 url，返回**新对象**（不修改入参）。
 *
 * @param {object} latestJson 解析后的 latest.json：{ version, notes, pub_date, platforms: { name: { signature, url } } }
 * @param {Array<{id: number|string, name: string}>} assets release 资产列表（REST API .assets，id 为数字）
 * @param {{tag: string, owner: string, repo: string}} options
 * @returns {object} 改写后的新清单；signature / version / notes / pub_date 等字段原样保留
 *
 * 幂等：已是 releases/download/ 前缀（或任何非 api 资产端点）的 url 原样保留，重复执行结果不变。
 * 报错而非静默写坏：id 在 assets 里查不到 → 抛错；asset name 为空/全空白/非字符串 → 抛错
 * （否则会拼出 `.../<tag>/` 这种无文件名的死链）；改写后仍有 url 命中 api 资产端点 → 抛错。
 */
export function rewriteUpdaterUrls(latestJson, assets, { tag, owner, repo } = {}) {
  if (!latestJson || typeof latestJson !== "object" || Array.isArray(latestJson)) {
    throw new TypeError("latestJson 必须是对象");
  }
  if (!Array.isArray(assets)) {
    throw new TypeError("assets 必须是 [{ id, name }] 数组");
  }
  for (const [key, value] of [["tag", tag], ["owner", owner], ["repo", repo]]) {
    if (typeof value !== "string" || value === "") {
      throw new TypeError(`缺少必需参数 ${key}`);
    }
  }

  // 按 id 建映射：同一 id 被多个平台条目共用（darwin-aarch64 / darwin-aarch64-app 等），
  // 统一走同一张表才保证同 id 条目改写后 url 完全一致
  const nameById = new Map();
  for (const asset of assets) {
    // 只按 id 建表；name 的形态（空串/全空白/非字符串）留给「查到该 id 时」统一校验报错
    if (asset && asset.id !== undefined && asset.id !== null) {
      nameById.set(String(asset.id), asset.name);
    }
  }

  const platforms = latestJson.platforms;
  if (!platforms || typeof platforms !== "object" || Array.isArray(platforms)) {
    throw new TypeError("latestJson.platforms 必须是对象");
  }

  // 用无原型容器聚合：在普通对象字面量上写 `__proto__` 键只会改原型、被静默丢弃（平台条目凭空消失），
  // Object.create(null) 下任何字符串键（含 __proto__）都是普通自身属性
  const nextPlatforms = Object.create(null);
  for (const [key, entry] of Object.entries(platforms)) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      // 形状异常的条目原样带走（本脚本只负责 url 改写，不做结构校验）
      nextPlatforms[key] = entry;
      continue;
    }
    const assetId = matchAssetApiUrl(entry.url, owner, repo);
    if (assetId === null) {
      nextPlatforms[key] = { ...entry };
      continue;
    }
    if (!nameById.has(assetId)) {
      throw new Error(
        `平台 ${key} 的 url 指向 asset id ${assetId}，但资产列表里没有该 id：${entry.url}\n` +
          "（清单与 release 资产不匹配，拒绝改写以免写坏下载地址）",
      );
    }
    const assetName = nameById.get(assetId);
    // 空 / 全空白 / 非字符串的 name 会拼出 `.../releases/download/<tag>/` 这种无文件名的死链，必须报错
    if (typeof assetName !== "string" || assetName.trim() === "") {
      throw new Error(
        `平台 ${key} 的 url 指向 asset id ${assetId}，但该资产没有可用的文件名` +
          `（name=${JSON.stringify(assetName) ?? "undefined"}）：${entry.url}\n` +
          "（缺文件名的 CDN 地址是死链，拒绝改写）",
      );
    }
    nextPlatforms[key] = {
      ...entry,
      url: buildDownloadUrl({ owner, repo, tag, assetName }),
    };
  }

  const rewritten = { ...latestJson, platforms: nextPlatforms };

  // 兜底断言：改写后不得残留任何 api 资产端点（防止正则/映射出现遗漏而静默交付坏清单）。
  // 与主匹配共用 matchAssetApiUrl（内部即 assetApiPattern），松紧一致——
  // 放宽主匹配（大写 host / 尾随斜杠）后兜底同步放宽，不会出现「主匹配放宽、兜底仍严」的矛盾静默路径
  for (const [key, entry] of Object.entries(nextPlatforms)) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    if (matchAssetApiUrl(entry.url, owner, repo) !== null) {
      throw new Error(`平台 ${key} 改写后仍指向 api.github.com 资产端点：${entry.url}`);
    }
  }

  return rewritten;
}

/** 从 GITHUB_REPOSITORY（owner/repo 形态）解析 owner/repo，缺失或形态不对时回落默认值 */
export function resolveRepoFromEnv(env = process.env) {
  const raw = env.GITHUB_REPOSITORY;
  if (typeof raw === "string" && raw.includes("/")) {
    const [owner, ...rest] = raw.split("/");
    const repo = rest.join("/");
    if (owner && repo) return { owner, repo };
  }
  return { owner: DEFAULT_OWNER, repo: DEFAULT_REPO };
}

function parseArgs(argv) {
  const opts = { dryRun: false, assetsJson: null };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const takeValue = () => {
      const value = argv[++i];
      if (value === undefined) {
        console.error(`[fix-updater-json] 参数 ${arg} 缺少取值\n\n${USAGE}`);
        process.exit(1);
      }
      return value;
    };
    if (arg === "--tag") opts.tag = takeValue();
    else if (arg === "--owner") opts.owner = takeValue();
    else if (arg === "--repo") opts.repo = takeValue();
    else if (arg === "--in") opts.inFile = takeValue();
    else if (arg === "--out") opts.outFile = takeValue();
    else if (arg === "--assets-json") opts.assetsJson = takeValue();
    else if (arg === "--dry-run") opts.dryRun = true;
    else if (arg === "-h" || arg === "--help") {
      console.log(USAGE);
      process.exit(0);
    } else {
      console.error(`[fix-updater-json] 未知参数：${arg}\n\n${USAGE}`);
      process.exit(1);
    }
  }
  return opts;
}

/**
 * 经 gh api 取 release 资产列表（id + name）。
 * 用 execFileSync 传数组参数，不做 shell 字符串拼接；gh 自行读 GITHUB_TOKEN/GH_TOKEN，全程不打印 token。
 */
export function fetchAssetsFromGh({ owner, repo, tag }) {
  let raw;
  try {
    raw = execFileSync(
      "gh",
      ["api", `repos/${owner}/${repo}/releases/tags/${tag}`, "--jq", ".assets[] | {id, name}"],
      { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
    );
  } catch (error) {
    throw new Error(
      `无法获取 ${owner}/${repo} 的 tag ${tag} 的资产列表（gh api 调用失败）：${error.message}\n` +
        "（gh 未安装 / 未登录 / 网络不通时可先用 --assets-json <file> 从文件读取资产列表）",
    );
  }
  // gh --jq 逐行输出紧凑 JSON（NDJSON）
  const lines = raw.split("\n").map((line) => line.trim()).filter((line) => line !== "");
  if (lines.length === 0) {
    throw new Error(`${owner}/${repo} 的 tag ${tag} 没有任何资产，无法改写清单`);
  }
  return lines.map((line, index) => {
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      throw new Error(`gh api 输出第 ${index + 1} 行不是合法 JSON：${line}`);
    }
    return parsed;
  });
}

/** 离线兜底：从本地 JSON 文件读资产列表（数组本身，或 { assets: [...] } 形态） */
function loadAssetsFromFile(file) {
  const parsed = JSON.parse(readFileSync(file, "utf8"));
  const assets = Array.isArray(parsed) ? parsed : parsed?.assets;
  if (!Array.isArray(assets)) {
    throw new Error(`${file} 里没有资产数组（需为 [{ id, name }] 或 { "assets": [...] }）`);
  }
  return assets;
}

/** 打印改写前后对照（dry-run / 正常写盘的摘要都用它） */
function printDiff(before, after) {
  const beforePlatforms = before.platforms ?? {};
  const afterPlatforms = after.platforms ?? {};
  let rewritten = 0;
  for (const [key, entry] of Object.entries(afterPlatforms)) {
    const oldUrl = beforePlatforms[key]?.url;
    const newUrl = entry?.url;
    const changed = oldUrl !== newUrl;
    if (changed) rewritten += 1;
    console.log(`  ${key}:`);
    console.log(`    - ${oldUrl}`);
    console.log(`    + ${newUrl}${changed ? "" : "   (未改写)"}`);
  }
  return { total: Object.keys(afterPlatforms).length, rewritten };
}

function main(argv) {
  const opts = parseArgs(argv);
  if (!opts.tag) {
    console.error(`[fix-updater-json] 缺少必填参数 --tag\n\n${USAGE}`);
    process.exit(1);
  }

  const fromEnv = resolveRepoFromEnv();
  const owner = opts.owner ?? fromEnv.owner;
  const repo = opts.repo ?? fromEnv.repo;
  const inFile = opts.inFile ?? DEFAULT_MANIFEST;
  const outFile = opts.outFile ?? inFile;

  const latestJson = JSON.parse(readFileSync(inFile, "utf8"));
  const assets = opts.assetsJson ? loadAssetsFromFile(opts.assetsJson) : fetchAssetsFromGh({ owner, repo, tag: opts.tag });

  // 先完整算出结果（内部校验通过才返回），再决定是否落盘——不留半成品
  const rewritten = rewriteUpdaterUrls(latestJson, assets, { tag: opts.tag, owner, repo });
  const { total, rewritten: changed } = printDiff(latestJson, rewritten);

  if (opts.dryRun) {
    console.log(`[fix-updater-json] dry-run：${changed}/${total} 个平台 url 将被改写，未写任何文件`);
    return;
  }

  writeFileSync(outFile, `${JSON.stringify(rewritten, null, 2)}\n`);
  console.log(`[fix-updater-json] 已写入 ${outFile}：${changed}/${total} 个平台 url 改写为 CDN 下载形态`);
}

// 仅在被直接执行时跑 CLI；被 import（单测）时不执行
const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    // 以可读信息失败退出（而非吐栈），退出码 1 让 CI 变红
    console.error(`[fix-updater-json] 失败：${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  }
}
