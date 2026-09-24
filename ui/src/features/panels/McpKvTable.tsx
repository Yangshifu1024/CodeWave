// MCP 配置里的表格编辑器：参数（单列）与环境变量 / 请求头（键值两列）。
//
// 为什么是表格而不是文本框：文本框只能表达「一行一条」的**行投影**，
// 于是含空格的 Windows 路径会被按空白切坏、值里含换行根本写不出来。
// 表格直接编辑行模型（`McpArgRow` / `McpKeyValueRow`），零编解码、零有损。
//
// 纯展示 + 回调：增删改都由调用方落到草稿（本组件不持有状态）。
import { Button, Input } from "antd";

const { TextArea } = Input;
import { DeleteOutlined } from "@ant-design/icons";
import type { McpArgRow, McpKeyValueRow } from "../../utils/mcpConfig";

interface ArgTableProps {
  rows: McpArgRow[];
  /** 删除按钮的 aria-label（文案由调用方给，本组件不碰 i18n） */
  deleteLabel: string;
  addLabel: string;
  onPatch: (index: number, value: string) => void;
  onAdd: () => void;
  onRemove: (index: number) => void;
}

/** 参数表：序号 + 值 + 删除；一行一个参数，含空格的路径原样保留。 */
export function McpArgTable({
  rows,
  deleteLabel,
  addLabel,
  onPatch,
  onAdd,
  onRemove,
}: ArgTableProps) {
  return (
    <div className="mcp-table">
      {rows.map((row, i) => (
        <div className="mcp-table-row" key={i}>
          <span className="mcp-table-idx">{i + 1}</span>
          <Input
            size="small"
            value={row.value}
            onChange={(ev) => onPatch(i, ev.target.value)}
          />
          <Button
            size="small"
            type="text"
            danger
            icon={<DeleteOutlined />}
            aria-label={deleteLabel}
            onClick={() => onRemove(i)}
          />
        </div>
      ))}
      <Button size="small" type="text" onClick={onAdd}>
        ＋ {addLabel}
      </Button>
    </div>
  );
}

interface KvTableProps {
  rows: McpKeyValueRow[];
  keyPlaceholder: string;
  valuePlaceholder: string;
  deleteLabel: string;
  addLabel: string;
  onPatch: (index: number, patch: Partial<McpKeyValueRow>) => void;
  onAdd: () => void;
  onRemove: (index: number) => void;
}

/**
 * 键值表：键 + 值 + 删除；值不 trim、可含 `=`，键为空的行在保存时丢弃。
 *
 * **值列是单行 `TextArea` 而不是 `Input`**：HTML 的 `<input>` 存不住换行
 * （浏览器/DOM 会把 `
` 剥掉），于是含换行的环境变量（如内联 PEM 证书）
 * 在界面上会被显示成拼接后的样子、且一编辑就丢换行。`textarea` 没有这个限制。
 * 用 `rows={1}` 而非 `autoSize`：后者会在 DOM 里另放一个测量用 textarea（测试定位易踩）。
 */
export function McpKvTable({
  rows,
  keyPlaceholder,
  valuePlaceholder,
  deleteLabel,
  addLabel,
  onPatch,
  onAdd,
  onRemove,
}: KvTableProps) {
  return (
    <div className="mcp-table">
      {rows.map((row, i) => (
        <div className="mcp-table-row" key={i}>
          <Input
            size="small"
            className="mcp-table-key"
            value={row.key}
            placeholder={keyPlaceholder}
            onChange={(ev) => onPatch(i, { key: ev.target.value })}
          />
          <TextArea
            rows={1}
            size="small"
            value={row.value}
            placeholder={valuePlaceholder}
            onChange={(ev) => onPatch(i, { value: ev.target.value })}
          />
          <Button
            size="small"
            type="text"
            danger
            icon={<DeleteOutlined />}
            aria-label={deleteLabel}
            onClick={() => onRemove(i)}
          />
        </div>
      ))}
      <Button size="small" type="text" onClick={onAdd}>
        ＋ {addLabel}
      </Button>
    </div>
  );
}
