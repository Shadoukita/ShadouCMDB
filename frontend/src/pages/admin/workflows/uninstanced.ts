import type { WorkflowWarning } from "../../../api/workflows";
import { t } from "../../../i18n";

/** The `UNINSTANCED_CIS` warning in one sentence, with what to do about it. */
export function uninstancedText(w: Pick<WorkflowWarning, "count">): string {
  const n = w.count;
  if (n === 0) return t("wfAdmin.uninstanced.none");
  const what = n === null ? t("wfAdmin.uninstanced.unknown") : t("wfAdmin.uninstanced.some", { n });
  return `${what} ${t("wfAdmin.uninstanced.locked")}`;
}
