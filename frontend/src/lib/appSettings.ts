import { defineStore } from "pinia";
import { computed, ref } from "vue";
import { useUiSettings, type UiNavEntry } from "../api/uiSettings";
import { useSessionStore } from "../stores/session";
import { normalizeDocument } from "./uiSettings";

/** Unsaved navigation from the Customization editor, shown in the real sidebar while it is open. */
export const useNavPreviewStore = defineStore("navPreview", () => {
  const entries = ref<UiNavEntry[] | null>(null);
  return { entries };
});

/**
 * The UI settings every signed-in screen applies. If they cannot be loaded the
 * screens fall back to their built-in behaviour (an empty document) rather than failing.
 */
export function useAppSettings() {
  const session = useSessionStore();
  const query = useUiSettings(() => session.status === "signedIn");
  const doc = computed(() => normalizeDocument(query.data.value?.settings));
  return { query, doc };
}
