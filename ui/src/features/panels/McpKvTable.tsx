// MCP 配置里的表格编辑器：参数（单列）与环境变量 / 请求头（键值两列）。
//
// 为什么是表格而不是文本框：文本框只能表达「一行一条」的**行投影**，
// 于是含空格的 Windows 路径会被按空白切坏、值里含换行根本写不出来。
// 表格直接编辑行模型（`McpArgRow` / `McpKeyValueRow`），零编解码、零有损。
//
// 增删改都由调用方落到草稿；本组件只持有敏感值的临时显隐状态。
import { useEffect, useState } from "react";
import { Button, Input, Tooltip } from "antd";

const { TextArea } = Input;
import { DeleteOutlined, EyeInvisibleOutlined, EyeOutlined, PlusOutlined } from "@ant-design/icons";
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
    <div className="mcp-table mcp-arg-table">
      {rows.map((row, i) => (
        <div className="mcp-table-row" key={i}>
          <span className="mcp-table-idx">{i + 1}</span>
          <Input
            value={row.value}
            aria-label={`${addLabel} ${i + 1}`}
            onChange={(ev) => onPatch(i, ev.target.value)}
          />
          <Tooltip title={deleteLabel} trigger={["hover", "focus"]}>
            <Button type="text" danger icon={<DeleteOutlined />} aria-label={deleteLabel} onClick={() => onRemove(i)} />
          </Tooltip>
        </div>
      ))}
      <Button className="mcp-table-add" icon={<PlusOutlined />} onClick={onAdd}>{addLabel}</Button>
    </div>
  );
}

interface KvTableProps {
  rows: McpKeyValueRow[];
  keyPlaceholder: string;
  valuePlaceholder: string;
  deleteLabel: string;
  addLabel: string;
  showSecretLabel?: string;
  hideSecretLabel?: string;
  maskSensitive?: boolean;
  onPatch: (index: number, patch: Partial<McpKeyValueRow>) => void;
  onAdd: () => void;
  onRemove: (index: number) => void;
}

/** Only credential-shaped environment/header names are concealed; all values remain unchanged in the draft. */
export function isSensitiveMcpKey(key: string): boolean {
  const normalized = key.replace(/([a-z0-9])([A-Z])/g, "$1_$2");
  return /(?:^|[_-])(?:api[_-]?key|private[_-]?key|access[_-]?token|key|token|secret|password|credential|authorization|cookie|pat)(?:$|[_-])/i.test(normalized);
}

function McpKvRow({ row, index, keyPlaceholder, valuePlaceholder, deleteLabel, showSecretLabel, hideSecretLabel, maskSensitive, onPatch, onRemove }: {
  row: McpKeyValueRow;
  index: number;
  keyPlaceholder: string;
  valuePlaceholder: string;
  deleteLabel: string;
  showSecretLabel?: string;
  hideSecretLabel?: string;
  maskSensitive?: boolean;
  onPatch: KvTableProps["onPatch"];
  onRemove: KvTableProps["onRemove"];
}) {
  const [revealed, setRevealed] = useState(false);
  const [editingNewValue, setEditingNewValue] = useState(false);
  useEffect(() => { setRevealed(false); setEditingNewValue(false); }, [row.key]);
  const sensitive = !!maskSensitive && isSensitiveMcpKey(row.key);
  const concealed = sensitive && !!row.value && !revealed && !editingNewValue;
  return <div className="mcp-table-row">
    <Input
      className="mcp-table-key"
      value={row.key}
      placeholder={keyPlaceholder}
      aria-label={`${keyPlaceholder} ${index + 1}`}
      onChange={(ev) => onPatch(index, { key: ev.target.value })}
    />
    <div className="mcp-table-value">
      <TextArea
        autoSize={{ minRows: 1, maxRows: 6 }}
        value={concealed ? "••••••••" : row.value}
        readOnly={concealed}
        aria-label={`${valuePlaceholder} ${index + 1}`}
        placeholder={valuePlaceholder}
        onFocus={() => { if (!row.value) setEditingNewValue(true); }}
        onBlur={() => setEditingNewValue(false)}
        onChange={(ev) => { if (!concealed) onPatch(index, { value: ev.target.value }); }}
      />
      {sensitive && !!row.value && <Tooltip title={revealed ? hideSecretLabel : showSecretLabel} trigger={["hover", "focus"]}>
        <Button
          type="text"
          className="mcp-table-reveal"
          icon={revealed ? <EyeInvisibleOutlined /> : <EyeOutlined />}
          aria-label={revealed ? hideSecretLabel : showSecretLabel}
          onClick={() => { setRevealed((prev) => !prev); setEditingNewValue(false); }}
        />
      </Tooltip>}
    </div>
    <Tooltip title={deleteLabel} trigger={["hover", "focus"]}>
      <Button type="text" danger icon={<DeleteOutlined />} aria-label={deleteLabel} onClick={() => onRemove(index)} />
    </Tooltip>
  </div>;
}

/**
 * 键值表：键 + 值 + 删除；值不 trim、可含 `=`，键为空的行在保存时丢弃。
 *
 * **值列是 `TextArea` 而不是 `Input`**：HTML 的 `<input>` 存不住换行
 * （浏览器/DOM 会把 `
` 剥掉），于是含换行的环境变量（如内联 PEM 证书）
 * 在界面上会被显示成拼接后的样子、且一编辑就丢换行。`textarea` 没有这个限制。
 * 高度随显式换行及窄列软换行自动增长（最多 6 行），长内容仍可滚动或手动拉高。
 */
export function McpKvTable({
  rows,
  keyPlaceholder,
  valuePlaceholder,
  deleteLabel,
  addLabel,
  showSecretLabel,
  hideSecretLabel,
  maskSensitive,
  onPatch,
  onAdd,
  onRemove,
}: KvTableProps) {
  // Rows have no persisted identity. After a structural edit, remount every row so
  // a revealed secret can never be inherited by the row that shifts into its index.
  const [structureVersion, setStructureVersion] = useState(0);
  const addRow = () => { setStructureVersion((version) => version + 1); onAdd(); };
  const removeRow = (index: number) => { setStructureVersion((version) => version + 1); onRemove(index); };
  return (
    <div className="mcp-table mcp-kv-table">
      {rows.map((row, i) => <McpKvRow
        key={`${structureVersion}:${i}`}
        row={row}
        index={i}
        keyPlaceholder={keyPlaceholder}
        valuePlaceholder={valuePlaceholder}
        deleteLabel={deleteLabel}
        showSecretLabel={showSecretLabel}
        hideSecretLabel={hideSecretLabel}
        maskSensitive={maskSensitive}
        onPatch={onPatch}
        onRemove={removeRow}
      />)}
      <Button className="mcp-table-add" icon={<PlusOutlined />} onClick={addRow}>{addLabel}</Button>
    </div>
  );
}
