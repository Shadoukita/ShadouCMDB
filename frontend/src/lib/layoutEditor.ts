import { useQueryClient } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, onMounted, reactive, ref, toValue, watch, type MaybeRefOrGetter } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, useRoute, useRouter, type RouteLocationNormalized } from "vue-router";
import { ApiError } from "../api/client";
import { fetchCurrentStoredSettings, useSaveUiSettings, useUiSettings, type UiClassLayout, type UiSettingsDocument } from "../api/uiSettings";
import { useSessionStore } from "../stores/session";
import { materialize } from "./layoutDesign";
import { normalizeDocument, type AttributeLike } from "./uiSettings";

/**
 * "Edit layout" on the real CI pages (detail, form): the class layout of
 * Customization › Detail and form layout, edited in place. It works on the same
 * settings document the Customization editor saves (the stored copy of the
 * current version), with the same rules (lib/layoutDesign), and saves a new
 * version through PUT /ui-settings, so the history, the audit trail and the
 * optimistic lock are the ones Customization has.
 *
 * Edit mode lives in the URL (`?layout=edit`), so the designer can link to it and
 * a reload keeps it. Only holders of customization.manage get it; the API checks
 * that again on save. A class without a layout of its own is shown as its
 * built-in layout made explicit (lib/layoutDesign materialize); it becomes part
 * of the draft at the first change. Every change goes through `apply`, which
 * keeps the undo history.
 */

export const EDIT_LAYOUT_QUERY = "layout";
export const EDIT_LAYOUT_VALUE = "edit";
export const LEAVE_QUESTION = "Discard your unsaved layout changes?";

/** Width presets for checking the layout on smaller screens. */
export const WIDTH_PRESETS = [
  { label: "Desktop", width: null },
  { label: "Tablet", width: 768 },
  { label: "Phone", width: 390 },
] as const;

const isEditRoute = (r: Pick<RouteLocationNormalized, "query">) => r.query[EDIT_LAYOUT_QUERY] === EDIT_LAYOUT_VALUE;

export function useLayoutEditor(opts: { classKey: MaybeRefOrGetter<string | undefined>; attrs: MaybeRefOrGetter<readonly AttributeLike[] | undefined> }) {
  const route = useRoute();
  const router = useRouter();
  const session = useSessionStore();
  const qc = useQueryClient();
  const saveMutation = useSaveUiSettings();
  const allowed = computed(() => session.can("customization.manage"));
  const settings = useUiSettings(allowed);

  const active = computed(() => allowed.value && isEditRoute(route));
  const doc = ref<UiSettingsDocument | null>(null);
  const baseline = ref("");
  const loadedVersion = ref<number | null>(null);
  const loading = ref(false);
  const loadError = ref<unknown>(null);
  const saveError = ref<unknown>(null);
  const saved = ref<string | null>(null);
  const past = ref<string[]>([]);
  const future = ref<string[]>([]);
  /** The built-in layout made explicit while the class has none in the draft. */
  const scratch = ref<UiClassLayout | null>(null);
  const previewWidth = ref<number | null>(null);

  const classKey = computed(() => toValue(opts.classKey));
  const attrs = computed(() => toValue(opts.attrs));
  const own = computed(() => (doc.value && classKey.value ? doc.value.layouts.find((l) => l.classKey === classKey.value) : undefined));
  /** The layout being edited: the class's own one in the draft, else its built-in one. */
  const layout = computed<UiClassLayout | undefined>(() => own.value ?? scratch.value ?? undefined);
  /** Whether the class uses the built-in layout in the draft (nothing of its own). */
  const builtIn = computed(() => !!doc.value && !own.value);

  function refreshScratch() {
    scratch.value = doc.value && classKey.value && attrs.value && !own.value ? materialize(classKey.value, attrs.value) : null;
  }
  watch([classKey, attrs, own], refreshScratch);

  const dirty = computed(() => !!doc.value && JSON.stringify(doc.value) !== baseline.value);
  const conflict = computed(() => saveError.value instanceof ApiError && saveError.value.code === "VERSION_CONFLICT");
  /** Someone saved a newer version since the draft was loaded. */
  const stale = computed(() => loadedVersion.value !== null && settings.data.value !== undefined && settings.data.value.version !== loadedVersion.value);

  async function load() {
    loading.value = true;
    loadError.value = null;
    saveError.value = null;
    saved.value = null;
    try {
      const stored = await fetchCurrentStoredSettings(qc);
      doc.value = normalizeDocument(stored.settings);
      baseline.value = JSON.stringify(doc.value);
      loadedVersion.value = stored.version;
      past.value = [];
      future.value = [];
      refreshScratch();
    } catch (e) {
      loadError.value = e;
    } finally {
      loading.value = false;
    }
  }
  function unload() {
    doc.value = null;
    scratch.value = null;
    loadedVersion.value = null;
    past.value = [];
    future.value = [];
    saveError.value = null;
    saved.value = null;
    previewWidth.value = null;
  }
  watch(active, (on) => (on ? void load() : unload()), { immediate: true });

  /** Makes one change to the layout (undoable). The class gets a layout of its own if it had none. */
  function apply(change: (l: UiClassLayout) => void): void {
    const d = doc.value;
    const l = layout.value;
    if (!d || !l) return;
    const before = JSON.stringify(d);
    if (!own.value) {
      d.layouts.push(l);
      scratch.value = null;
    }
    change(own.value ?? l);
    if (JSON.stringify(d) === before) return;
    past.value.push(before);
    future.value = [];
    saved.value = null;
  }
  /** Replaces the whole draft (undo, redo, discard, reset), keeping the step undoable where asked. */
  function restore(text: string, record: "past" | "future" | null) {
    if (!doc.value) return;
    const now = JSON.stringify(doc.value);
    if (record === "past") past.value.push(now);
    if (record === "future") future.value.push(now);
    doc.value = JSON.parse(text) as UiSettingsDocument;
    refreshScratch();
    saved.value = null;
  }
  function undo() {
    const s = past.value.pop();
    if (s !== undefined) restore(s, "future");
  }
  function redo() {
    const s = future.value.pop();
    if (s !== undefined) restore(s, "past");
  }
  /** Back to the last saved layout; undo brings the changes back. */
  function discard() {
    if (!dirty.value) return;
    restore(baseline.value, "past");
    future.value = [];
  }
  /** Drops the class's own layout from the draft: the built-in one applies once saved. */
  function resetToBuiltIn() {
    const d = doc.value;
    if (!d || !own.value) return;
    past.value.push(JSON.stringify(d));
    future.value = [];
    d.layouts = d.layouts.filter((l) => l.classKey !== classKey.value);
    refreshScratch();
    saved.value = null;
  }

  async function save(comment: string): Promise<boolean> {
    if (!doc.value || loadedVersion.value === null || !dirty.value) return false;
    saveError.value = null;
    try {
      const result = await saveMutation.mutateAsync({ version: loadedVersion.value, settings: doc.value, comment: comment.trim() || null });
      baseline.value = JSON.stringify(doc.value);
      loadedVersion.value = result.version;
      saved.value = `Saved as version ${result.version}.`;
      return true;
    } catch (e) {
      saveError.value = e;
      return false;
    }
  }

  const enter = () => router.push({ query: { ...route.query, [EDIT_LAYOUT_QUERY]: EDIT_LAYOUT_VALUE } });
  /** Leaves edit mode; the route guard asks first when there are unsaved changes. */
  const exit = () => {
    const query = { ...route.query };
    delete query[EDIT_LAYOUT_QUERY];
    return router.push({ query });
  };

  // Unsaved changes: ask before leaving edit mode or the page, and let the browser ask before a reload or closing the tab.
  const ask = () => !(active.value && dirty.value) || window.confirm(LEAVE_QUESTION);
  onBeforeRouteLeave(ask);
  onBeforeRouteUpdate((to, from) => (isEditRoute(to) && to.path === from.path ? true : ask()));
  function onBeforeUnload(e: BeforeUnloadEvent) {
    if (!active.value || !dirty.value) return;
    e.preventDefault();
    e.returnValue = "";
  }
  onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
  onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));

  return reactive({
    allowed,
    active,
    loading,
    loadError,
    layout,
    builtIn,
    dirty,
    saving: saveMutation.isPending,
    saveError,
    saved,
    conflict,
    stale,
    currentVersion: computed(() => settings.data.value?.version),
    loadedVersion,
    canUndo: computed(() => past.value.length > 0),
    canRedo: computed(() => future.value.length > 0),
    previewWidth,
    apply,
    undo,
    redo,
    discard,
    resetToBuiltIn,
    save,
    reload: load,
    enter,
    exit,
  });
}

export type LayoutEditor = ReturnType<typeof useLayoutEditor>;
