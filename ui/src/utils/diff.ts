// 轻量行级 diff（供工具卡展示）：LCS 动态规划，输出行序列
/** 单行 diff 结果：same 不变 / add 新增 / del 删除 */
export type DiffLine = { type: "same" | "add" | "del"; text: string };

/** 计算新旧文本的行级 diff，输出按原顺序交织的行序列 */
export function lineDiff(oldText: string, newText: string): DiffLine[] {
  const a = oldText.split("\n");
  const b = newText.split("\n");
  const n = a.length;
  const m = b.length;

  // LCS 表（尺寸设限防卡顿：超限退化为简单前后缀匹配）
  const LIMIT = 400;
  const out: DiffLine[] = [];
  if (n * m > LIMIT * LIMIT) {
    let head = 0;
    while (head < n && head < m && a[head] === b[head]) {
      out.push({ type: "same", text: a[head] });
      head++;
    }
    for (let i = head; i < n; i++) out.push({ type: "del", text: a[i] });
    for (let j = head; j < m; j++) out.push({ type: "add", text: b[j] });
    return out;
  }

  const dp: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ type: "same", text: a[i] });
      i++; j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.push({ type: "del", text: a[i] });
      i++;
    } else {
      out.push({ type: "add", text: b[j] });
      j++;
    }
  }
  while (i < n) { out.push({ type: "del", text: a[i] }); i++; }
  while (j < m) { out.push({ type: "add", text: b[j] }); j++; }
  return out;
}

/** 折叠 diff：变更区域前后各保留 `context` 行不变行，其余以「…」行示意省略 */
export function collapseDiff(lines: DiffLine[], context = 2): DiffLine[] {
  const keep = new Array(lines.length).fill(false);
  lines.forEach((l, idx) => {
    if (l.type !== "same") {
      for (let k = Math.max(0, idx - context); k <= Math.min(lines.length - 1, idx + context); k++) {
        keep[k] = true;
      }
    }
  });
  const out: DiffLine[] = [];
  let skipping = false;
  lines.forEach((l, idx) => {
    if (keep[idx]) {
      if (skipping) {
        out.push({ type: "same", text: "…" });
        skipping = false;
      }
      out.push(l);
    } else {
      skipping = true;
    }
  });
  return out;
}
