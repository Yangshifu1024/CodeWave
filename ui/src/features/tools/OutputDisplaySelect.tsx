import { Segmented } from "antd";
import { useTranslation } from "react-i18next";
import { useUi } from "../../stores/ui";
import type { OutputDisplay } from "../../utils/outputDisplay";

export default function OutputDisplaySelect({ showLabel = false }: { showLabel?: boolean }) {
  const { t } = useTranslation();
  const display = useUi((s) => s.outputDisplay);
  return <span className="output-display-select">
    {showLabel && <span className="dim">{t("tools.outputDisplay")}</span>}
    <Segmented size="small" aria-label={t("tools.outputDisplay")} value={display}
      onChange={(value) => useUi.getState().setOutputDisplay(value as OutputDisplay)}
      options={[{ label: t("tools.displayCompact"), value: "compact" }, { label: t("tools.displayDetailed"), value: "detailed" }]} />
  </span>;
}
