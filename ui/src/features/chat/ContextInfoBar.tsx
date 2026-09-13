// Composer 工具条内的「压缩上下文」图标按钮（原 ContextInfoBar 瘦身后的产物：
// 模型名 / 上下文 / MCP 摘要移入 Composer 工具条，本组件只保留压缩动作）。
// 加载态（需求批次）：压缩进行中旋转并禁用；同时插入本地「压缩中」提示（与后端事件去重）。
import { App, Button } from "antd";
import { CompressOutlined, LoadingOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { i18n } from "../../i18n";
import { useSessions } from "../../stores/sessions";
import { useActiveRun, useRun } from "../../stores/run";
import { ipc } from "../../ipc/client";

/** 压缩上下文按钮：触发当前会话的 compaction；本地先行插入提示并置 compacting 态，
 *  状态翻转以 run:compacted 事件为准，命令失败路径在组件内兜底复位。 */
export default function CompactButton() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const hasSession = useSessions((s) => !!s.activeKey);
  const active = useActiveRun();
  const compacting = active.compacting;

  async function compact() {
    const sessionId = useSessions.getState().activeKey;
    if (!sessionId) return;
    // 先插入本地提示（后端 run:compacting 会按 compacting 态去重，不会重复）；状态翻转交给事件驱动
    useRun.setState((s) => {
      const t = s.tabs[sessionId];
      if (!t || t.compacting) return;
      t.items.push({ kind: "notice", text: i18n.t("notice.compactingShort") });
      t.compacting = true;
    });
    try {
      await ipc.compactSession(sessionId);
      message.success(t("app.compact") + " ✓");
    } catch (e) {
      message.error(String(e));
      // 兜底后端既未发 compacted 也未发失败的路径：命令直接返回 Err 时在此复位状态
      useRun.setState((s) => {
        const t = s.tabs[sessionId];
        if (t) t.compacting = false;
      });
    }
  }

  return (
    <Button
      size="small"
      type="text"
      icon={compacting ? <LoadingOutlined spin /> : <CompressOutlined />}
      disabled={!hasSession || compacting}
      aria-label={t("app.compact")}
      title={t("app.compact")}
      onClick={() => void compact()}
    />
  );
}
