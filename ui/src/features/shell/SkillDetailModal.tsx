// 技能详情弹层（右栏信息页技能行专用）：元信息 + SKILL.md 正文（markdown 渲染，限高滚动）。
// skill 为 null 时不渲染；「使用」经 ws:composer-insert 把 /<name> 追加进当前会话输入框并关闭弹层。
import { useEffect, useState } from "react";
import { Button, Modal } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { SkillMeta } from "../../ipc/types";
import { renderMarkdown } from "../../utils/markdown";

export default function SkillDetailModal({
  skill,
  sessionId,
  onClose,
}: {
  skill: SkillMeta | null;
  sessionId: string | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [body, setBody] = useState("");
  const [loading, setLoading] = useState(false);
  const name = skill?.name ?? null;
  useEffect(() => {
    if (!name) return;
    let stale = false; // 乱序守卫：关闭/切换后迟到的正文不回填
    setLoading(true);
    setBody("");
    void ipc
      .getSkill(sessionId, name)
      .then((full) => {
        if (!stale) setBody(full?.body ?? "");
      })
      .catch(() => {
        if (!stale) setBody("");
      })
      .finally(() => {
        if (!stale) setLoading(false);
      });
    return () => {
      stale = true;
    };
  }, [name, sessionId]);
  // 「使用」：把 /<name> 追加进输入框（ws:composer-insert，保留既有草稿）并关闭弹层
  function useSkill() {
    if (!skill) return;
    window.dispatchEvent(new CustomEvent("ws:composer-insert", { detail: { text: `/${skill.name} ` } }));
    onClose();
  }
  return (
    <Modal
      open={!!skill}
      title={skill ? `/${skill.name}` : ""}
      width={680}
      onCancel={onClose}
      footer={
        [
          <Button key="use" type="primary" onClick={useSkill}>
            {t("skills.detailUse")}
          </Button>,
          <Button key="close" onClick={onClose}>
            {t("skills.detailClose")}
          </Button>,
        ]
      }
    >
      {skill && (
        <div className="skill-detail">
          <p className="dim">{skill.description}</p>
          {!!skill.whenToUse && (
            <p className="dim">
              {t("skills.detailWhen")}：{skill.whenToUse}
            </p>
          )}
          <p className="skill-origin">
            {t("skills.detailOrigin")}：<code>{skill.origin}</code>
          </p>
          {loading ? (
            <p className="dim">{t("skills.detailLoading")}</p>
          ) : (
            <div
              className="skill-body md"
              dangerouslySetInnerHTML={{ __html: renderMarkdown(body) }}
            />
          )}
        </div>
      )}
    </Modal>
  );
}
