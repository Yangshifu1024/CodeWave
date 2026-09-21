// [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：表格预览的数据解析。
// 后端把表格区域渲染成制表符分隔的文本（见 src-tauri/src/tools/document/xlsx.rs 的 `render_tsv`），
// 单元格里的制表符与换行已在那一步换成空格，所以按行按列切回来是无损的。

/** 制表符分隔文本 → 二维数组（末尾空行丢弃；空文本给空表）。 */
export function parseTsv(text: string): string[][] {
  const lines = text.split(/\r?\n/);
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines.map((line) => line.split("\t"));
}

/**
 * 带引号的字段切分（逗号与制表符共用一套规则）：
 * 双引号包裹的字段里，分隔符与换行都是字段内容，`""` 表示一个字面双引号。
 */
function parseQuoted(text: string, delim: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;
    if (quoted) {
      if (ch !== '"') {
        cell += ch;
      } else if (text[i + 1] === '"') {
        cell += '"';
        i++;
      } else {
        quoted = false;
      }
      continue;
    }
    if (ch === '"' && cell === "") {
      quoted = true;
    } else if (ch === delim) {
      row.push(cell);
      cell = "";
    } else if (ch === "\n" || ch === "\r") {
      if (ch === "\r" && text[i + 1] === "\n") i++;
      row.push(cell);
      rows.push(row);
      row = [];
      cell = "";
    } else {
      cell += ch;
    }
  }
  // 末尾没有换行时补最后一行；文本以换行结尾时不额外留一行空行
  if (cell !== "" || row.length > 0) {
    row.push(cell);
    rows.push(row);
  }
  return rows;
}

/** 逗号分隔文本（.csv）→ 二维数组。 */
export function parseCsv(text: string): string[][] {
  return parseQuoted(text, ",");
}

/** 制表符分隔文本（.tsv 文件）→ 二维数组：与后端产出的 TSV 不同，文件里的字段可能带引号。 */
export function parseTsvFile(text: string): string[][] {
  return parseQuoted(text, "\t");
}

/** 二维数组里的最大列数（空表为 0）。 */
export function maxCols(rows: string[][]): number {
  return rows.reduce((m, r) => Math.max(m, r.length), 0);
}

/** 0 基列号 → 表头字母：0→A、25→Z、26→AA（表头用它，替代表格里的行号列）。 */
export function columnLabel(index: number): string {
  let n = index;
  let out = "";
  do {
    out = String.fromCharCode(65 + (n % 26)) + out;
    n = Math.floor(n / 26) - 1;
  } while (n >= 0);
  return out;
}
