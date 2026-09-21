// [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：项目目录外文件的放行确认。
//
// 三个选项而不是两个：「取消」与「仅此一次」是两件事（前者不读这个文件，后者读但下次还问）。
// antd 的确认弹框只有确定/取消两个按钮，所以这里自己搭一个三按钮弹框。
import { Button, Modal } from "antd";
import { useTranslation } from "react-i18next";

/** 用户的决定：仅此一次 / 始终允许这个目录 / 不放行。 */
export type ExternalDirDecision = "once" | "always" | "cancel";

export default function ExternalDirPrompt({
  dir,
  onDecide,
}: {
  /** 要放行的目录（绝对路径） */
  dir: string | null;
  onDecide: (choice: ExternalDirDecision) => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal
      open={!!dir}
      title={t("composer.allowDirTitle")}
      onCancel={() => onDecide("cancel")}
      footer={[
        <Button key="cancel" onClick={() => onDecide("cancel")}>
          {t("composer.allowDirCancel")}
        </Button>,
        <Button key="once" onClick={() => onDecide("once")}>
          {t("composer.allowDirOnce")}
        </Button>,
        <Button key="always" type="primary" onClick={() => onDecide("always")}>
          {t("composer.allowDirAlways")}
        </Button>,
      ]}
    >
      <p>{t("composer.allowDirDesc", { path: dir ?? "" })}</p>
      <p className="dim" style={{ fontSize: 12, marginBottom: 0 }}>
        {t("composer.allowDirAlwaysNote")}
      </p>
    </Modal>
  );
}
