import { useQueryClient, type QueryClient } from "@tanstack/vue-query";
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, toValue, watch, type MaybeRefOrGetter } from "vue";
import { useRoute, useRouter, type LocationQueryRaw, type Router } from "vue-router";
import { ApiError } from "../api/client";
import { fetchCurrentStoredSettings, uiKeys, useSaveUiSettings, useUiSettings, type UiClassLayout, type UiSettingsDocument } from "../api/uiSettings";
import { useSessionStore } from "../stores/session";
import { isFreeTab, layerOf, LAYER_MOVES, measureGrid, moveLayer, settleFrames, toFree, toGrid, type LayerMove } from "./freeLayout";
import { findSection, materialize, type LayoutTab } from "./layoutDesign";
import { normalizeDocument, type AttributeLike } from "./uiSettings";

/**
 * "Edit layout" on the real CI pages (detail, form): the class layout of
 * Customization › Detail and form layout, edited in place. It works on the same
 * settings document the Customization editor saves (the stored copy of the
 * current version), with the same rules (lib/layoutDesign), and saves a new
 * version through PUT /ui-settings, so the history, the audit trail and the
 * optimistic lock are the ones Customization has.
 *
 * The editor has its own route (the page's path + `/layout-editor`, see
 * router.ts) and opens in a separate browser window, one per class, so the page
 * the user came from stays as it is. Saving tells the other windows of the app
 * (BroadcastChannel `layout-updated`), which reload the settings. Only holders
 * of customization.manage get it (the router sends others to the page itself);
 * the API checks that again on save. A class without a layout of its own is shown as its
 * built-in layout made explicit (lib/layoutDesign materialize); it becomes part
 * of the draft at the first change. Every change goes through `apply`, which
 * keeps the undo history. A tab is on the 12-column grid or free (lib/freeLayout):
 * `setPlacement` switches the tab in view, and on a free tab the selected window
 * moves up and down the stack with `layer`.
 */

/** The editor's route: the CI page's path plus this suffix. */
export const EDITOR_SUFFIX = "/layout-editor";
export const LEAVE_QUESTION = "Discard your unsaved layout changes?";

/** Width presets for checking the layout on smaller screens. */
export const WIDTH_PRESETS = [
  { label: "Desktop", width: null },
  { label: "Tablet", width: 768 },
  { label: "Phone", width: 390 },
] as const;

const CHANNEL = "layout-updated";

export interface SectionError {
  /** The API's path, e.g. `settings.layouts.0.tabs.1.sections.0.kind`. */
  path: string;
  message: string;
}
const SECTION_PATH = /(?:^|\.)layouts\.(\d+)\.tabs\.(\d+)\.sections\.(\d+)(?:\.|$)/;

/**
 * The API's refusals of a settings document that point into a section of the
 * layout of `classKey` (`settings.layouts.N.tabs.N.sections.N…`), by the key of
 * that section in `sent`, the document as it was saved: the editors show them
 * next to the section even after it moved.
 */
export function sectionErrors(error: unknown, sent: UiSettingsDocument | null | undefined, classKey: string | undefined): Record<string, SectionError[]> {
  const out: Record<string, SectionError[]> = {};
  if (!(error instanceof ApiError) || !sent) return out;
  for (const d of error.details) {
    const m = d.field ? SECTION_PATH.exec(d.field) : null;
    if (!m) continue;
    const layout = sent.layouts[Number(m[1])];
    const section = layout?.classKey === classKey ? layout.tabs?.[Number(m[2])]?.sections?.[Number(m[3])] : undefined;
    if (section) (out[section.key] ??= []).push({ path: d.field!, message: d.message });
  }
  return out;
}
/** The browser window the layout editor of a class opens in. */
export const editorWindowName = (classKey: string) => `layout-editor-${classKey.replace(/[^A-Za-z0-9_-]/g, "_")}`;
/** The page an editor route edits (and back): `/cis/1/layout-editor` ↔ `/cis/1`. */
export const pageOfEditor = (path: string) => (path.endsWith(EDITOR_SUFFIX) ? path.slice(0, -EDITOR_SUFFIX.length) || "/" : path);
const editorOfPage = (path: string) => `${path.replace(/\/$/, "")}${EDITOR_SUFFIX}`;

/** Why the editor opened in this tab rather than its own window (shown in its bar). */
export const OPENED_HERE_QUERY = "opened";

/**
 * Opens the layout editor of a class for a CI page in its own window. A second
 * call for the same class focuses the window already open rather than loading it
 * again (which would drop its unsaved changes). When a popup blocker refuses the
 * window, the editor opens in this tab instead, and says so.
 */
export function openLayoutEditor(router: Router, page: { path: string; query?: LocationQueryRaw }, classKey: string): "window" | "tab" {
  const target = { path: editorOfPage(page.path), query: page.query };
  const name = editorWindowName(classKey);
  // An empty URL returns the named window as it is when it is already open, and a blank one otherwise.
  const w = window.open("", name, "popup,width=1400,height=900");
  if (!w) {
    void router.push({ ...target, query: { ...target.query, [OPENED_HERE_QUERY]: "tab" } });
    return "tab";
  }
  let blank = true;
  try {
    blank = w.location.href === "about:blank";
  } catch {
    blank = false; // another origin's page under that name: leave it alone
  }
  if (blank) w.location.href = router.resolve(target).href;
  w.focus();
  return "window";
}

/**
 * Keeps this window's layouts current when a layout editor window saves: the
 * settings are loaded again. Without BroadcastChannel it returns false, and the
 * caller asks for a reload after saving instead.
 */
export function listenForLayoutUpdates(qc: QueryClient): boolean {
  if (typeof BroadcastChannel === "undefined") return false;
  const channel = new BroadcastChannel(CHANNEL);
  channel.onmessage = () => void qc.invalidateQueries({ queryKey: uiKeys.settings });
  return true;
}

export function useLayoutEditor(opts: { classKey: MaybeRefOrGetter<string | undefined>; attrs: MaybeRefOrGetter<readonly AttributeLike[] | undefined> }) {
  const route = useRoute();
  const router = useRouter();
  const session = useSessionStore();
  const qc = useQueryClient();
  const saveMutation = useSaveUiSettings();
  const allowed = computed(() => session.can("customization.manage"));
  const settings = useUiSettings(allowed);

  const active = computed(() => allowed.value && route.meta.layoutEditor === true);
  const doc = ref<UiSettingsDocument | null>(null);
  const baseline = ref("");
  const loadedVersion = ref<number | null>(null);
  const loading = ref(false);
  const loadError = ref<unknown>(null);
  const saveError = ref<unknown>(null);
  /** The document of the last save that failed, for placing the API's errors. */
  const refused = ref<UiSettingsDocument | null>(null);
  const saved = ref<string | null>(null);
  const past = ref<string[]>([]);
  const future = ref<string[]>([]);
  /** The built-in layout made explicit while the class has none in the draft. */
  const scratch = ref<UiClassLayout | null>(null);
  const previewWidth = ref<number | null>(null);
  /** The tab in view (its key; the first tab when empty or gone), the window selected on a free tab, and whether windows snap. */
  const tabKey = ref("");
  const selected = ref<string | null>(null);
  const snap = ref(true);
  /** What the last change did, for screen readers (the canvas's live region). */
  const announcement = ref("");

  const classKey = computed(() => toValue(opts.classKey));
  const attrs = computed(() => toValue(opts.attrs));
  const own = computed(() => (doc.value && classKey.value ? doc.value.layouts.find((l) => l.classKey === classKey.value) : undefined));
  /** The layout being edited: the class's own one in the draft, else its built-in one. */
  const layout = computed<UiClassLayout | undefined>(() => own.value ?? scratch.value ?? undefined);
  /** Whether the class uses the built-in layout in the draft (nothing of its own). */
  const builtIn = computed(() => !!doc.value && !own.value);
  const tab = computed<LayoutTab | undefined>(() => layout.value?.tabs?.find((t) => t.key === tabKey.value) ?? layout.value?.tabs?.[0]);
  watch(tabKey, () => (selected.value = null));

  function say(text: string) {
    announcement.value = "";
    void nextTick(() => (announcement.value = text));
  }

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
    selected.value = null;
  }
  watch(active, (on) => (on ? void load() : unload()), { immediate: true });

  /** The drag gesture the last change belongs to: its further changes share one undo step. */
  let gesture: string | null = null;
  /**
   * Makes one change to the layout (undoable). The class gets a layout of its
   * own if it had none. Changes with the same `group` (the steps of one drag,
   * until `endGesture`) are undone together.
   */
  function apply(change: (l: UiClassLayout) => void, group?: string): void {
    const d = doc.value;
    const l = layout.value;
    if (!d || !l) return;
    const before = JSON.stringify(d);
    if (!own.value) {
      d.layouts.push(l);
      scratch.value = null;
    }
    const target = own.value ?? l;
    change(target);
    settleFrames(target.tabs);
    if (JSON.stringify(d) === before) return;
    if (!group || group !== gesture) past.value.push(before);
    gesture = group ?? null;
    future.value = [];
    saved.value = null;
  }
  /** The drag is over: the next change is a new undo step. */
  function endGesture() {
    gesture = null;
  }
  /** Replaces the whole draft (undo, redo, discard, reset), keeping the step undoable where asked. */
  function restore(text: string, record: "past" | "future" | null) {
    if (!doc.value) return;
    gesture = null;
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
    gesture = null;
    d.layouts = d.layouts.filter((l) => l.classKey !== classKey.value);
    refreshScratch();
    saved.value = null;
  }

  /**
   * Puts the tab in view on the grid or free. To free, each section becomes a
   * window where it is on screen now (measured on the canvas); back to the grid,
   * the windows are ordered by position as the API does it.
   */
  function setPlacement(placement: "grid" | "free") {
    const t = tab.value;
    if (!t || (placement === "free") === isFreeTab(t)) return;
    const measured = placement === "free" ? measureGrid(document.querySelector("[data-le-area]")) : undefined;
    apply((l) => {
      const own = l.tabs?.find((x) => x.key === t.key);
      if (own && placement === "free") toFree(own, measured);
      else if (own) toGrid(own);
    });
    selected.value = null;
    say(
      placement === "free"
        ? `Tab ${t.label} is free: drag each section by its title bar, resize it from its edges, and overlap them.`
        : `Tab ${t.label} is on the grid again: sections in reading order, widths from their windows.`,
    );
  }
  /** Moves the selected window (or `key`) up or down its tab's stack. */
  function layer(move: LayerMove, key = selected.value) {
    const t = tab.value;
    if (!t || !key || !isFreeTab(t)) return;
    let moved = false;
    apply((l) => {
      const own = l.tabs?.find((x) => x.key === t.key);
      const s = findSection(l, key)?.section;
      if (own && s) moved = moveLayer(own, s, move);
    });
    const now = tab.value && layout.value && findSection(layout.value, key);
    if (!now) return;
    const at = layerOf(now.tab, now.section);
    const name = LAYER_MOVES.find((m) => m.move === move)!.label;
    say(moved ? `${name}: ${now.section.label} is layer ${at.index} of ${at.count}.` : `${now.section.label} is already layer ${at.index} of ${at.count}.`);
  }

  async function save(comment: string): Promise<boolean> {
    if (!doc.value || loadedVersion.value === null || !dirty.value) return false;
    saveError.value = null;
    try {
      const result = await saveMutation.mutateAsync({ version: loadedVersion.value, settings: doc.value, comment: comment.trim() || null });
      baseline.value = JSON.stringify(doc.value);
      loadedVersion.value = result.version;
      saved.value = `Saved as version ${result.version}.`;
      if (typeof BroadcastChannel !== "undefined") {
        const channel = new BroadcastChannel(CHANNEL);
        channel.postMessage({ classKey: classKey.value, version: result.version });
        channel.close();
      }
      return true;
    } catch (e) {
      saveError.value = e;
      refused.value = JSON.parse(JSON.stringify(doc.value)) as UiSettingsDocument;
      return false;
    }
  }

  /** This page opened the editor in another window. */
  const opened = ref(false);
  const broadcast = typeof BroadcastChannel !== "undefined";
  /** Opens the editor for this page in its own window (or this tab, when popups are blocked). */
  const enter = () => {
    if (!classKey.value) return;
    const how = openLayoutEditor(router, { path: route.path, query: route.query }, classKey.value);
    if (how === "window") opened.value = true;
  };
  /** The editor opened in a window of its own, which Done closes. */
  const popup = !!window.opener && window.name.startsWith("layout-editor-");
  const openedHere = computed(() => route.query[OPENED_HERE_QUERY] === "tab");

  let leaving = false;
  /** Leaves the editor, asking first when there are unsaved changes: closes its window, or back to the page. */
  const exit = async () => {
    if (dirty.value && !window.confirm(LEAVE_QUESTION)) return;
    leaving = true;
    if (popup) {
      window.close();
      if (window.closed) return;
    }
    const query = { ...route.query };
    delete query[OPENED_HERE_QUERY];
    await router.push({ path: pageOfEditor(route.path), query });
    leaving = false;
  };

  // Unsaved changes: ask before leaving the editor, and let the browser ask before a reload or closing the window.
  const removeGuard = router.beforeEach((to, from) => {
    if (leaving || !active.value || !dirty.value || to.path === from.path) return true;
    return window.confirm(LEAVE_QUESTION);
  });
  function onBeforeUnload(e: BeforeUnloadEvent) {
    if (leaving || !active.value || !dirty.value) return;
    e.preventDefault();
    e.returnValue = "";
  }
  onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
  onBeforeUnmount(() => {
    removeGuard();
    window.removeEventListener("beforeunload", onBeforeUnload);
  });

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
    /** The last refused save's errors by section key (lib sectionErrors). */
    sectionErrors: computed(() => sectionErrors(saveError.value, refused.value, classKey.value)),
    saved,
    conflict,
    stale,
    currentVersion: computed(() => settings.data.value?.version),
    loadedVersion,
    canUndo: computed(() => past.value.length > 0),
    canRedo: computed(() => future.value.length > 0),
    previewWidth,
    tabKey,
    tab,
    selected,
    snap,
    announcement,
    say,
    setPlacement,
    layer,
    apply,
    endGesture,
    undo,
    redo,
    discard,
    resetToBuiltIn,
    save,
    reload: load,
    enter,
    exit,
    openedHere,
    /** Ask for a reload here after the other window saves: this browser cannot tell this window. */
    reloadHint: computed(() => opened.value && !broadcast),
  });
}

export type LayoutEditor = ReturnType<typeof useLayoutEditor>;
