import { useEffect, useState } from "react";
import { Button, Empty, Input, Modal, Popconfirm, Tag } from "antd";
import { App } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { ScheduledTask } from "../../ipc/types";
import { useActiveId } from "../../stores/sessions";
import { useUi } from "../../stores/ui";

const { TextArea } = Input;

/** 任务中心弹窗：对当前会话的定时任务进行创建、查看与删除。 */
export default function TaskCenterPanel() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const sessionId = useActiveId();
  const [tasks, setTasks] = useState<ScheduledTask[]>([]);
  const [name, setName] = useState("");
  const [schedule, setSchedule] = useState("");
  const [instruction, setInstruction] = useState("");
  /** 三字段齐全才能创建：禁用原因就地显示在按钮旁（与设置页同一范式，不藏在 Tooltip 里） */
  const canCreate = !!name && !!schedule && !!instruction;

  async function refresh() {
    if (!sessionId) return;
    setTasks(await ipc.listScheduledTasks(sessionId).catch(() => []));
  }

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  async function create() {
    if (!sessionId) return;
    try {
      await ipc.createScheduledTask(sessionId, name, instruction, schedule);
      message.success(t("tasks.created"));
      setName("");
      setSchedule("");
      setInstruction("");
      await refresh();
    } catch (e) {
      message.error(String(e));
    }
  }

  async function remove(id: string) {
    if (!sessionId) return;
    await ipc.deleteScheduledTask(sessionId, id);
    await refresh();
  }

  return (
    <Modal
      open
      onCancel={() => useUi.setState({ tasksOpen: false })}
      footer={null}
      width={720}
      styles={{ body: { maxHeight: "70vh", overflowY: "auto" } }}
      title={t("tasks.title")}
    >
      <div className="new-task">
        <Input className="grow" size="small" value={name} placeholder={t("tasks.name")} onChange={(e) => setName(e.target.value)} />
        {/* 计划表达式 + 其语法说明：说明常驻在输入框下方（原先只当 placeholder，敲第一个字符就没了） */}
        <div className="task-sched-field">
          <Input
            size="small"
            style={{ width: 220 }}
            value={schedule}
            placeholder="cron:0 9 * * *"
            onChange={(e) => setSchedule(e.target.value)}
          />
          <span className="hint">{t("tasks.scheduleHint")}</span>
        </div>
        <TextArea
          style={{ width: "100%" }}
          rows={2}
          size="small"
          value={instruction}
          placeholder={t("tasks.instruction")}
          onChange={(e) => setInstruction(e.target.value)}
        />
        <Button size="small" type="primary" disabled={!canCreate} onClick={() => void create()}>
          {t("tasks.create")}
        </Button>
        {!canCreate && <span className="hint">{t("tasks.createDisabled")}</span>}
      </div>

      {tasks.length === 0 && <Empty description={t("tasks.empty")} style={{ marginTop: 24 }} />}
      {tasks.map((task) => (
        <div className="task" key={task.id}>
          <div className="row1">
            <b>{task.name}</b>
            <code className="sched">{task.schedule}</code>
            {/* 色彩强度只映射风险等级：成功态用中性默认标签（预设绿违反「无色彩=默认」约定），
                失败态才是红；「待触发」不再硬编码中文 */}
            <Tag color={task.last_status && task.last_status !== "ok" ? "error" : "default"}>
              {task.last_status ?? t("tasks.pending")}
            </Tag>
            <div className="flex" />
            <Popconfirm title={`${t("common.delete")}?`} onConfirm={() => void remove(task.id)}>
              <Button size="small" type="text" danger>{t("common.delete")}</Button>
            </Popconfirm>
          </div>
          <div className="row2 dim">{task.instruction}</div>
          {task.last_summary && <div className="row2">→ {task.last_summary}</div>}
        </div>
      ))}
    </Modal>
  );
}
