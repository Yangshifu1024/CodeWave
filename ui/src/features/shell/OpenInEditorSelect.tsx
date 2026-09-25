// 右栏「在编辑器中打开」按钮（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//
// 按钮形态：通用编辑器图标 (CodeOutlined) + 右侧 caret（CaretDownOutlined）。
// 始终不展示当前选中编辑器名称——按钮文本恒为通用图标，点击才展开 Dropdown 列出候选。
//
// 选项来自后端 `list_editors`（候选顺序：VS Code → Cursor → Windsurf → Zed → Sublime Text →
// Notepad++ → JetBrains 组），**默认 = 第一个检测到的**，用户改选后写 localStorage 并为新默认；
// 选定即用该编辑器打开当前目录（项目会话 = 主目录，临时会话 = 工作区）。
// 一个编辑器都没检测到 → 整个控件不渲染（保持面板干净，文件管理器按钮仍在）。
import { useEffect, useState } from "react";
import { App as AntApp, Button, Dropdown } from "antd";
import { CaretDownOutlined, CodeOutlined } from "@ant-design/icons";
import type { MenuProps } from "antd";
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
  // 仅用于「记忆到 default 后 caret 染 accent」视觉提示；不参与按钮文字（按钮恒显示通用图标）
  const [picked, setPicked] = useState<string | null>(() => readPreferredEditor());

  useEffect(() => {
    void ipc
      .listEditors()
      .then((list) => setEditors(Array.isArray(list) ? list : []))
      .catch(() => setEditors([]));
  }, []);

  if (editors.length === 0) return null;

  const open = (editorId: string) => {
    setPicked(editorId);
    writePreferredEditor(editorId);
    if (!dir) return;
    void ipc
      .openInEditor(editorId, dir)
      .catch((e) => {
        message.error(
          `${t("rightbar.openInEditorFailed")}\n${String(e).replace(/^Error[:\s]*/i, "")}`,
        );
      });
  };

  // Dropdown menu：每项点击 → open(editorId)；antd Dropdown 默认 click 即关
  const menu: MenuProps = {
    items: editors.map((e) => ({
      key: e.id,
      label: e.name,
      onClick: () => open(e.id),
    })),
  };

  return (
    <Dropdown
      menu={menu}
      trigger={["click"]}
      placement="bottomRight"
      // 不显示选中态对勾：避免与"按钮文本恒为通用图标"的语义冲突
    >
      <Button
        className="rb-editor-btn"
        type="text"
        size="small"
        title={t("rightbar.openInEditor")}
        aria-label={t("rightbar.openInEditor")}
        data-active={picked ? "true" : undefined}
      >
        <CodeOutlined className="rb-editor-btn-icon" />
        <CaretDownOutlined className="rb-editor-btn-caret" />
      </Button>
    </Dropdown>
  );
}
