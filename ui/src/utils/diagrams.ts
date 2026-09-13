// mermaid / katex 占位符升级：markdown.ts 只产出占位 DOM，本模块按需懒加载渲染库并替换。
// 渲染结果按内容缓存。mermaid 只处理已定稿消息（.md 无 data-streaming）：流式中的代码不完整，
// 每帧异步 render 会造成 DOM 高度抖动 → 滚动风暴；定稿后由内容变化 effect 重跑补渲染。

let katexMod: Promise<typeof import("katex")> | null = null;
function loadKatex() {
  katexMod ??= Promise.all([import("katex"), import("katex/dist/katex.min.css")]).then(([m]) => m);
  return katexMod;
}

let mermaidMod: Promise<typeof import("mermaid")> | null = null;
/** 已 initialize 的主题档（"dark" | "default"）；与请求主题不一致时重新 initialize（主题切换后新图立即用对配色） */
let mermaidTheme: "dark" | "default" | null = null;
function loadMermaid() {
  const want: "dark" | "default" = document.documentElement.classList.contains("dark") ? "dark" : "default";
  if (mermaidMod && mermaidTheme === want) return mermaidMod;
  mermaidMod ??= import("mermaid");
  return mermaidMod.then((m) => {
    // initialize 幂等合并配置，运行期按当前主题重入安全（html.dark 由 App.tsx 随 effectiveDark 同步）；
    // 记账在 then 内以自身 want 闭包为准：加载途中切换主题时，后到者的 initialize 后写入胜出，记录与实际一致
    m.default.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      suppressErrorRendering: true, // 失败不向 DOM 注入 mermaid 错误 bomb，错误 UI 由本模块呈现
      theme: want,
    });
    mermaidTheme = want;
    return m;
  });
}

const CACHE_MAX = 240;
const cache = new Map<string, string>();
function cachePut(key: string, html: string) {
  if (cache.size >= CACHE_MAX) {
    // FIFO 淘汰最旧一页
    const first = cache.keys().next().value;
    if (first !== undefined) cache.delete(first);
  }
  cache.set(key, html);
}

// 并发去重：同一代码的渲染进行中时共享同一 Promise，避免重复触发 mermaid.render
const inflight = new Map<string, Promise<string>>();
function renderDedup(key: string, run: () => Promise<string>): Promise<string> {
  const exist = inflight.get(key);
  if (exist) return exist;
  const p = run().finally(() => inflight.delete(key));
  inflight.set(key, p);
  return p;
}

let seq = 0;

/** 扫描 root 下未处理的 .ws-math / .ws-diagram 占位符并渲染。幂等，可随每次流式渲染反复调用。 */
export async function upgradeDiagrams(root: HTMLElement | null): Promise<void> {
  if (!root) return;
  const maths = root.querySelectorAll<HTMLElement>(".ws-math:not([data-done])");
  if (maths.length > 0) {
    const katex = await loadKatex();
    for (const el of maths) {
      el.dataset.done = "1";
      const tex = el.textContent ?? "";
      const key = `k:${el.dataset.kind}:${tex}`;
      const hit = cache.get(key);
      if (hit !== undefined) {
        el.innerHTML = hit;
        continue;
      }
      try {
        const html = katex.renderToString(tex, {
          throwOnError: false,
          displayMode: el.dataset.kind === "block",
          output: "html",
        });
        cachePut(key, html);
        el.innerHTML = html;
      } catch {
        // 语法错误：回退为原文（保底可读）
        el.classList.add("ws-math-raw");
      }
    }
  }

  // 只升级已定稿消息的占位符（.md 无 data-streaming）；流式中的由定稿后的 effect 重跑补渲染
  const diagrams = root.querySelectorAll<HTMLElement>(".md:not([data-streaming]) .ws-diagram:not([data-done])");
  if (diagrams.length > 0) {
    // 同一图表亮暗两套主题分别缓存（key 前缀区分），主题切换后新渲染的图直接命中当前主题的缓存
    const dark = document.documentElement.classList.contains("dark");
    const mermaid = (await loadMermaid()).default;
    for (const el of diagrams) {
      el.dataset.done = "1";
      const code = el.textContent ?? "";
      const key = `m:${dark ? "d" : "l"}:${code}`;
      const hit = cache.get(key);
      if (hit !== undefined) {
        el.innerHTML = hit;
        continue;
      }
      try {
        // 先 parse 校验：半成品代码在此直接报错，不触发异步 render（suppressErrors 下合法返回 ParseResult 对象，非法返回 false）
        const parsed = await mermaid.parse(code, { suppressErrors: true });
        if (!parsed) throw new Error("语法错误（parse 未通过）");
        const svg = await renderDedup(key, async () => (await mermaid.render(`ws-mermaid-${++seq}`, code)).svg);
        cachePut(key, svg);
        el.innerHTML = svg;
      } catch (e) {
        // 错误只写一行摘要（不回写全文，避免错误文本本身成为高度突变源）；不进缓存，下次可重试
        el.classList.add("ws-diagram-error");
        el.textContent = `[mermaid] ${e instanceof Error ? e.message : String(e)}`;
      }
    }
  }
}
