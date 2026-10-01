import { computed } from "vue";
import { useImportSettings } from "../api/imports";
import { useSessionStore } from "../stores/session";

/**
 * Whether the signed-in user may use bulk import: the `cis.import` permission (administrators hold it) and the
 * instance switch. Entry points show only when both hold (§1.1); the API enforces both anyway.
 */
export function useImportAccess() {
  const session = useSessionStore();
  const permitted = computed(() => session.can("cis.import"));
  const settings = useImportSettings(permitted);
  const available = computed(() => permitted.value && !!settings.data.value?.enabled);
  return { permitted, settings, available };
}
