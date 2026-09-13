// [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：运行队列面板——运行中提交的任务入队；条目支持「立即运行（打断当前并执行）/ 编辑 / 删除」+ 拖拽排序；
// 出错/取消后队列暂停但保留，「继续」恢复执行。
// 拖拽排序（缺陷修复：此前把手纯装饰）：原生 HTML5 DnD——仅按住把手才可拖动，
// 悬停目标高亮，落点把被拖条目移动到目标位置（store.reorderQueue）。
import { useRef, useState } from "react";
import { Button, Tooltip } from "antd";
import {
  ArrowUpOutlined,
  CaretRightOutlined,
  DeleteOutlined,
  EditOutlined,
  HolderOutlined,
  PaperClipOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useSessions } from "../../stores/sessions";
import { useRun } from "../../stores/run";

/** 运行队列面板：展示当前会话排队任务（文本 + 附件数），支持立即运行/编辑/删除与把手拖拽排序；
 *  无运行中任务时显示「已暂停 + 继续」行。 */
export default function QueuePanel() {
  const { t } = useTranslation();
  const activeKey = useSessions((s) => s.activeKey);
  const queue = useRun((s) => (activeKey ? s.tabs[activeKey]?.queue : undefined)) ?? [];
  const running = useRun((s) => (activeKey ? s.tabs[activeKey]?.running : false)) ?? false;
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);
  // 仅在按住把手时才允许拖动（恒可拖会干扰文本选择与按钮点击）
  const gripArmed = useRef(false);
  if (!activeKey || queue.length === 0) return null;

  return (
    <div className="queue-panel">
      {queue.map((q) => (
        <div
          className={`queue-item${draggingId === q.id ? " dragging" : ""}${overId === q.id && draggingId && draggingId !== q.id ? " drop-target" : ""}`}
          key={q.id}
          draggable={draggingId === q.id}
          onDragStart={(e) => {
            if (!gripArmed.current) {
              e.preventDefault();
              return;
            }
            setDraggingId(q.id);
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData("text/plain", q.id); // Firefox 需要非空数据才会启动拖拽
          }}
          onDragEnd={() => {
            gripArmed.current = false;
            setDraggingId(null);
            setOverId(null);
          }}
          onDragOver={(e) => {
            if (!draggingId || draggingId === q.id) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            setOverId(q.id);
          }}
          onDragLeave={() => {
            if (overId === q.id) setOverId(null);
          }}
          onDrop={(e) => {
            e.preventDefault();
            if (draggingId && draggingId !== q.id) {
              useRun.getState().reorderQueue(activeKey, draggingId, q.id);
            }
            gripArmed.current = false;
            setDraggingId(null);
            setOverId(null);
          }}
        >
          <span
            className="queue-grip"
            title={t("queue.dragToSort")}
            onMouseDown={() => {
              gripArmed.current = true;
            }}
            onMouseUp={() => {
              gripArmed.current = false;
            }}
            style={{ cursor: "grab" }}
          >
            <HolderOutlined />
          </span>
          <span className="queue-text" title={q.text}>
            {q.text}
          </span>
          {q.images?.length ? (
            <Tooltip title={t("queue.images", { n: q.images.length })}>
              <span className="queue-attach">
                <PaperClipOutlined /> {q.images.length}
              </span>
            </Tooltip>
          ) : null}
          <span className="queue-ops">
            <Tooltip title={t("queue.runNowTip")}>
              <Button size="small" icon={<ArrowUpOutlined />} onClick={() => void useRun.getState().runNow(activeKey, q.id)}>
                {t("queue.runNow")}
              </Button>
            </Tooltip>
            <Tooltip title={t("queue.edit")}>
              <Button size="small" type="text" icon={<EditOutlined />} onClick={() => useRun.getState().editQueueItem(activeKey, q.id)} />
            </Tooltip>
            <Tooltip title={t("queue.delete")}>
              <Button size="small" type="text" icon={<DeleteOutlined />} onClick={() => useRun.getState().removeQueueItem(activeKey, q.id)} />
            </Tooltip>
          </span>
        </div>
      ))}
      {!running && (
        <div className="queue-paused">
          <span className="queue-paused-hint">{t("queue.paused")}</span>
          <Button size="small" type="primary" icon={<CaretRightOutlined />} onClick={() => void useRun.getState().runQueueNext(activeKey)}>
            {t("queue.resume")}
          </Button>
        </div>
      )}
    </div>
  );
}
