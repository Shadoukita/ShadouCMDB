import { t } from "../i18n/index";

/** A CI's row menu in the inventory and the search results: open it, or analyse its impact (not for a deleted CI). */
export function ciRowMenu(ci: { id: string; deletedAt?: string | null }): { label: string; to: string }[] {
  return [
    { label: t("inventory.row.open"), to: `/cis/${ci.id}` },
    ...(ci.deletedAt ? [] : [{ label: t("inventory.row.impact"), to: `/cis/${ci.id}/impact` }]),
  ];
}
