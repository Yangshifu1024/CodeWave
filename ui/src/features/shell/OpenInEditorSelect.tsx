// 「在编辑器中打开」下拉（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//
// 选项来自后端 `list_editors`（候选表顺序：VS Code → Cursor → Windsurf → Zed → Sublime Text →
// Notepad++ → JetBrains 组），**默认 = 第一个检测到的**，用户改选后记 localStorage 并为新默认；
// 选定即用该编辑器打开当前目录（项目会话 = 主目录，临时会话 = 工作区）。
// 一个编辑器都没检测到 → 整个控件不渲染（保持面板干净，文件管理器按钮仍在）。
import { useEffect, useState } from "react";
import { App as AntApp, Select } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { EditorInfo } from "../../ipc/types";
import { readPreferredEditor, writePreferredEditor } from "../../utils/rightbarPrefs";

interface Props {
  /** 要打开的目录（空串 = 无目标，此时只切换默认项、不派发打开） */
  dir: string;
}

export default function OpenInEditorSelect({ dir }: Props) {
  const { t } = useTranslation();
  const { message } = AntApp.useApp();
  const [editors, setEditors] = useState<EditorInfo[]>([]);
  const [picked, setPicked] = useState<string | null>(() => readPreferredEditor());

  useEffect(() => {
    void ipc
      .listEditors()
      .then((list) => setEditors(Array.isArray(list) ? list : []))
      .catch(() => setEditors([]));
  }, []);

  if (editors.length === 0) return null;

  // 记忆项已不可用（卸载/改名）时回落到第一个检测到的
  const current = editors.find((e) => e.id === picked) ?? editors[0];

  const open = (editorId: string) => {
    setPicked(editorId);
    writePreferredEditor(editorId);
    if (!dir) return;
    void ipc
      .openInEditor(editorId, dir)
      .catch((e) =>
        message.error(
          `${t("rightbar.openInEditorFailed")}\n${String(e).replace(/^Error[:\s]*/i, "")}`,
        ),
      );
  };

  return (
    <Select
      className="rb-editor-select"
      size="small"
      value={current.id}
      onChange={open}
      title={t("rightbar.openInEditor")}
      aria-label={t("rightbar.openInEditor")}
      options={editors.map((e) => ({ value: e.id, label: e.name }))}
    />
  );
}
