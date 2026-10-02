import { useQueryClient, type QueryClient } from "@tanstack/vue-query";
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, toValue, watch, type MaybeRefOrGetter } from "vue";
import { useRoute, useRouter, type LocationQueryRaw, type Router } from "vue-router";
import { ApiError } from "../api/client";
import {
  fetchCiLayout,
  fetchCurrentStoredSettings,
  layoutApi,
  uiKeys,
  useLayoutTemplateUsage,
  useSaveUiSettings,
  useUiSettings,
  type CiLayout,
  type CiLayoutUpdate,
  type UiClassLayout,
  type UiSettingsDocument,
} from "../api/uiSettings";
import { t } from "../i18n";
import { useSessionStore } from "../stores/session";
import { layerOf, LAYER_MOVES, moveLayer, settleFrames, type LayerMove } from "./freeLayout";
import { findSection, materialize, type LayoutTab } from "./layoutDesign";
import { addTemplate, asClassLayout, classTemplateKey, compactLayouts, layoutContent, sectionErrors, setClassTemplate, type SentLayout } from "./layoutTemplates";

export { sectionErrors, type SectionError, type SentLayout } from "./layoutTemplates";
import { normalizeDocument, normalizeLayout, type AttributeLike } from "./uiSettings";

/**
 * "Edit layout" on the real CI pages (detail, form): a layout edited in place,
 * with the same rules as everywhere (lib/layoutDesign). What is edited (the
 * target) is a layout template (SHAA-1472) or the CI's own layout:
 *
 * - on a CI with a layout of its own, that layout ("This CI only");
 * - else the template the CI shows (one chosen for it, or its class's default);
 * - on the create form, the class's default template;
 * - with `?template=<key>` (Customization › Layouts), that template.
 *
 * Saving goes to the target or elsewhere: to a template (a new settings version
 * through PUT /ui-settings, so history, audit trail and optimistic lock are the
 * ones Customization has), as a new template (optionally the class's default),
 * or for this CI only (PUT /configuration-items/{id}/layout). A CI can also be
 * reset to its class's default or given another template.
 *
 * The editor has its own route (the page's path + `/layout-editor`, see
 * router.ts) and opens in a separate browser window, one per class (or per
 * template), so the page the user came from stays as it is. Saving tells the
 * other windows of the app (BroadcastChannel `layout-updated`), which reload
 * the settings and CI layouts. Only holders of customization.manage get it (the
 * router sends others to the page itself); the API checks that again on save.
 * A layout without tabs is shown as the built-in layout made explicit (lib/layoutDesign
 * materialize); it becomes the draft at the first change. Every change goes
 * through `apply`, which keeps the undo history. Every section is a window of
 * its tab (lib/freeLayout); the selected window moves up and down the stack
 * with `layer`. A tab stored on the earlier 12-column grid loads as windows
 * where its sections were.
 */

/** The editor's route: the CI page's path plus this suffix. */
export const EDITOR_SUFFIX = "/layout-editor";
/** Query parameter of the editor's route: the template to edit, whatever the CI shows. */
export const TEMPLATE_QUERY = "template";
export const LEAVE_QUESTION = "Discard your unsaved layout changes?";
/**
 * Layouts shape the web UI only: the API returns hidden fields and accepts writes to read-only ones
 * (GH#192). Shown wherever a layout hides a field or makes it read-only, so nobody mistakes it for access control.
 */
export const PRESENTATION_ONLY =
  "Hidden and read-only fields change what the web UI shows, not who can read or change the data: the API still returns and accepts them. To restrict access, use permission profiles.";

const CHANNEL = "layout-updated";

/** What the editor edits: a layout template, or the CI's own layout. */
export type EditTarget = { kind: "template"; key: string } | { kind: "ci" };

/** The browser window the layout editor of a class (or of a template) opens in. */
export const editorWindowName = (classKey: string, template?: string) =>
  `layout-editor-${(template ? `template-${template}` : classKey).replace(/[^A-Za-z0-9_-]/g, "_")}`;
/** The page an editor route edits (and back): `/cis/1/layout-editor` ↔ `/cis/1`. */
export const pageOfEditor = (path: string) => (path.endsWith(EDITOR_SUFFIX) ? path.slice(0, -EDITOR_SUFFIX.length) || "/" : path);
const editorOfPage = (path: string) => `${path.replace(/\/$/, "")}${EDITOR_SUFFIX}`;

/** Why the editor opened in this tab rather than its own window (shown in its bar). */
export const OPENED_HERE_QUERY = "opened";

/**
 * Opens the layout editor of a class for a CI page in its own window; with
 * `template`, on that template. A second call for the same class (template)
 * focuses the window already open rather than loading it again (which would
 * drop its unsaved changes). When a popup blocker refuses the window, the
 * editor opens in this tab instead, and says so.
 */
export function openLayoutEditor(router: Router, page: { path: string; query?: LocationQueryRaw }, classKey: string, template?: string): "window" | "tab" {
  const target = { path: editorOfPage(page.path), query: { ...page.query, ...(template ? { [TEMPLATE_QUERY]: template } : {}) } };
  const name = editorWindowName(classKey, template);
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

/** What a layout save changes in the other windows: the settings, the CIs' layouts and the template usage. */
function invalidateLayouts(qc: QueryClient) {
  void qc.invalidateQueries({ queryKey: uiKeys.settings });
  void qc.invalidateQueries({ queryKey: ["ui-settings", "ci-layout"] });
  void qc.invalidateQueries({ queryKey: uiKeys.templateUsage });
}

/**
 * Keeps this window's layouts current when a layout editor window saves: the
 * settings and CI layouts are loaded again. Without BroadcastChannel it returns
 * false, and the caller asks for a reload after saving instead.
 */
export function listenForLayoutUpdates(qc: QueryClient): boolean {
  if (typeof BroadcastChannel === "undefined") return false;
  const channel = new BroadcastChannel(CHANNEL);
  channel.onmessage = () => invalidateLayouts(qc);
  return true;
}

/** Whether a layout has anything of its own (else the page shows the built-in arrangement). */
const hasContent = (l: UiClassLayout | null | undefined) => !!l && ((l.tabs?.length ?? 0) > 0 || (l.hiddenFields?.length ?? 0) > 0 || (l.readOnlyFields?.length ?? 0) > 0);
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

export function useLayoutEditor(opts: {
  classKey: MaybeRefOrGetter<string | undefined>;
  attrs: MaybeRefOrGetter<readonly AttributeLike[] | undefined>;
  /** The CI the page shows (detail, edit form); none on the create form. */
  ciId?: MaybeRefOrGetter<string | undefined>;
}) {
  const route = useRoute();
  const router = useRouter();
  const session = useSessionStore();
  const qc = useQueryClient();
  const saveMutation = useSaveUiSettings();
  const allowed = computed(() => session.can("customization.manage"));
  const settings = useUiSettings(allowed);

  const active = computed(() => allowed.value && route.meta.layoutEditor === true);
  const usage = useLayoutTemplateUsage(active);
  /** The stored settings document of `loadedVersion`, as last loaded or saved here. */
  const doc = ref<UiSettingsDocument | null>(null);
  /** The layout the CI shows and where it comes from, as last loaded or saved here. */
  const ci = ref<CiLayout | null>(null);
  const target = ref<EditTarget | null>(null);
  /** The layout being edited (no tabs: the built-in arrangement), and as last loaded or saved. */
  const draft = ref<UiClassLayout | null>(null);
  const baseline = ref("");
  const loadedVersion = ref<number | null>(null);
  const loading = ref(false);
  const busy = ref(false);
  const loadError = ref<unknown>(null);
  const saveError = ref<unknown>(null);
  /** The layout of the last save that failed, for placing the API's errors. */
  const refused = ref<SentLayout | null>(null);
  const saved = ref<string | null>(null);
  const past = ref<string[]>([]);
  const future = ref<string[]>([]);
  /** The built-in layout made explicit while the draft has nothing of its own. */
  const scratch = ref<UiClassLayout | null>(null);
  /** The tab in view (its key; the first tab when empty or gone), the window selected on it, and whether windows snap. */
  const tabKey = ref("");
  const selected = ref<string | null>(null);
  const snap = ref(true);
  /** What the last change did, for screen readers (the canvas's live region). */
  const announcement = ref("");
  /** The drag gesture the last change belongs to: its further changes share one undo step. */
  let gesture: string | null = null;

  const classKey = computed(() => toValue(opts.classKey));
  const ciId = computed(() => toValue(opts.ciId));
  const attrs = computed(() => toValue(opts.attrs));
  /** The layout being edited: the draft, else (nothing of its own) the built-in one. */
  const layout = computed<UiClassLayout | undefined>(() => (hasContent(draft.value) ? draft.value! : (scratch.value ?? undefined)));
  /** Whether the draft has nothing of its own: the built-in arrangement applies. */
  const builtIn = computed(() => !!draft.value && !hasContent(draft.value));
  const tab = computed<LayoutTab | undefined>(() => layout.value?.tabs?.find((t) => t.key === tabKey.value) ?? layout.value?.tabs?.[0]);
  watch(tabKey, () => (selected.value = null));

  /** The template being edited (with its index in the settings), if the target is one. */
  const template = computed(() => {
    const tg = target.value;
    if (tg?.kind !== "template" || !doc.value) return undefined;
    const index = doc.value.layoutTemplates.findIndex((t) => t.key === tg.key);
    return index < 0 ? undefined : { index, ...doc.value.layoutTemplates[index] };
  });
  /** Who uses the template being edited: classes by name, and the number of CIs (null: some you may not view; undefined: not known yet). */
  const users = computed(() => {
    const key = template.value?.key;
    const u = key ? usage.data.value?.templates.find((x) => x.key === key) : undefined;
    const classNames = new Map((usage.data.value?.classes ?? []).map((c) => [c.classKey, c.className]));
    return { classes: (u?.classKeys ?? []).map((k) => classNames.get(k) ?? k), ciCount: u ? u.overrideCount : undefined };
  });
  /** The CI has a layout of its own: another template, or one for this CI only. */
  const ciHasOwn = computed(() => !!ci.value && ci.value.source !== "class_default");
  const templates = computed(() => doc.value?.layoutTemplates ?? []);

  function say(text: string) {
    announcement.value = "";
    void nextTick(() => (announcement.value = text));
  }

  function refreshScratch() {
    scratch.value = draft.value && classKey.value && attrs.value && !hasContent(draft.value) ? materialize(classKey.value, attrs.value) : null;
  }
  watch([classKey, attrs, () => hasContent(draft.value)], refreshScratch);

  const dirty = computed(() => !!draft.value && JSON.stringify(draft.value) !== baseline.value);
  const conflict = computed(() => saveError.value instanceof ApiError && saveError.value.code === "VERSION_CONFLICT");
  /** Someone saved a newer settings version since the draft of a template was loaded. */
  const stale = computed(
    () => target.value?.kind === "template" && loadedVersion.value !== null && settings.data.value !== undefined && settings.data.value.version !== loadedVersion.value,
  );

  /** Starts editing `to` (fresh from what was loaded): no undo history, nothing unsaved. */
  function edit(to: EditTarget) {
    const k = classKey.value;
    if (!doc.value || !k) return;
    target.value = to;
    const content = to.kind === "ci" ? ci.value?.layout : doc.value.layoutTemplates.find((t) => t.key === to.key)?.layout;
    draft.value = normalizeLayout(asClassLayout(k, content));
    baseline.value = JSON.stringify(draft.value);
    past.value = [];
    future.value = [];
    gesture = null;
    selected.value = null;
    refreshScratch();
  }

  /** What the editor starts on: the template asked for in the URL, else what the CI shows, else the class's default. */
  function initialTarget(): EditTarget | null {
    const d = doc.value;
    const k = classKey.value;
    if (!d || !k) return null;
    const asked = route.query[TEMPLATE_QUERY];
    if (typeof asked === "string" && d.layoutTemplates.some((t) => t.key === asked)) return { kind: "template", key: asked };
    if (ci.value?.source === "custom") return { kind: "ci" };
    return { kind: "template", key: ci.value?.templateKey ?? classTemplateKey(d, k) };
  }
  /** Picks the target once both the settings and the class are known (the class can load after the settings). */
  function start() {
    if (target.value || loading.value) return;
    const to = initialTarget();
    if (to) edit(to);
  }
  watch(classKey, start);

  async function load() {
    loading.value = true;
    loadError.value = null;
    saveError.value = null;
    saved.value = null;
    target.value = null;
    try {
      const stored = await fetchCurrentStoredSettings(qc);
      const id = ciId.value;
      ci.value = id ? await fetchCiLayout(qc, id) : null;
      doc.value = normalizeDocument(stored.settings);
      loadedVersion.value = stored.version;
    } catch (e) {
      loadError.value = e;
    } finally {
      loading.value = false;
    }
    if (!loadError.value) start();
  }
  function unload() {
    doc.value = null;
    ci.value = null;
    target.value = null;
    draft.value = null;
    scratch.value = null;
    loadedVersion.value = null;
    past.value = [];
    future.value = [];
    saveError.value = null;
    saved.value = null;
    selected.value = null;
  }
  watch(active, (on) => (on ? void load() : unload()), { immediate: true });

  /**
   * Makes one change to the layout (undoable). A draft without anything of its
   * own starts from the built-in layout made explicit. Changes with the same
   * `group` (the steps of one drag, until `endGesture`) are undone together.
   */
  function apply(change: (l: UiClassLayout) => void, group?: string): void {
    const d = draft.value;
    const shown = layout.value;
    if (!d || !shown) return;
    const before = JSON.stringify(d);
    const own = hasContent(d);
    const working = own ? d : clone(shown);
    change(working);
    settleFrames(working.tabs);
    if (own ? JSON.stringify(d) === before : JSON.stringify(working) === JSON.stringify(shown)) return;
    if (!own) {
      draft.value = working;
      scratch.value = null;
    }
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
    if (!draft.value) return;
    gesture = null;
    const now = JSON.stringify(draft.value);
    if (record === "past") past.value.push(now);
    if (record === "future") future.value.push(now);
    draft.value = JSON.parse(text) as UiClassLayout;
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
  /** Empties the draft: the built-in layout applies once saved. Undo brings it back. */
  function resetToBuiltIn() {
    const d = draft.value;
    if (!d || !hasContent(d)) return;
    restore(JSON.stringify({ classKey: d.classKey, tabs: [], hiddenFields: [], readOnlyFields: [] }), "past");
    future.value = [];
  }

  /** Moves the selected window (or `key`) up or down its tab's stack. */
  function layer(move: LayerMove, key = selected.value) {
    const t = tab.value;
    if (!t || !key) return;
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

  function announceSave() {
    invalidateLayouts(qc);
    if (typeof BroadcastChannel !== "undefined") {
      const channel = new BroadcastChannel(CHANNEL);
      channel.postMessage({ classKey: classKey.value, ciId: ciId.value });
      channel.close();
    }
  }
  /** The CI's layout again after a save (what it shows may have changed); a failure keeps the one known. */
  async function refreshCi() {
    const id = ciId.value;
    if (!id) return;
    try {
      ci.value = await fetchCiLayout(qc, id);
    } catch {
      // The save itself went through; the badge catches up with the next load.
    }
  }
  /** What is being saved, as a layout of its own (no tabs: the built-in arrangement). */
  const content = () => layoutContent(draft.value ?? { tabs: [] });

  /** Saves a changed settings document (the templates); the draft is then the saved state of `next`. */
  async function saveSettings(next: UiSettingsDocument, comment: string, sent: SentLayout, to: EditTarget, message: (version: number) => string): Promise<boolean> {
    if (!draft.value || loadedVersion.value === null) return false;
    saveError.value = null;
    busy.value = true;
    try {
      const result = await saveMutation.mutateAsync({ version: loadedVersion.value, settings: compactLayouts(next), comment: comment.trim() || null });
      doc.value = next;
      loadedVersion.value = result.version;
      target.value = to;
      baseline.value = JSON.stringify(draft.value);
      saved.value = message(result.version);
      announceSave();
      await refreshCi();
      return true;
    } catch (e) {
      saveError.value = e;
      refused.value = clone(sent);
      return false;
    } finally {
      busy.value = false;
    }
  }

  /** Saves the draft to the template being edited: every class and CI that uses it changes. */
  async function saveToTemplate(comment: string): Promise<boolean> {
    const tp = template.value;
    if (!doc.value || !tp || !draft.value) return false;
    const next = clone(doc.value);
    next.layoutTemplates[tp.index].layout = content();
    return saveSettings(next, comment, { prefix: `settings.layoutTemplates.${tp.index}.layout`, layout: draft.value }, { kind: "template", key: tp.key }, (v) =>
      t("layoutEditor.savedTemplate", { name: tp.name, version: v }),
    );
  }

  /** Saves the draft as a new template named `name`, optionally the default of the class; the editor then edits it. */
  async function saveAsTemplate(input: { name: string; description?: string; makeDefault: boolean; comment: string }): Promise<boolean> {
    const k = classKey.value;
    if (!doc.value || !draft.value || !k) return false;
    const next = clone(doc.value);
    const created = addTemplate(next, input.name, content(), input.description);
    if (input.makeDefault) setClassTemplate(next, k, created.key);
    const index = next.layoutTemplates.length - 1;
    return saveSettings(next, input.comment, { prefix: `settings.layoutTemplates.${index}.layout`, layout: draft.value }, { kind: "template", key: created.key }, (v) =>
      t(input.makeDefault ? "layoutEditor.savedNewDefault" : "layoutEditor.savedNew", { name: created.name, version: v }),
    );
  }

  /** Changes the CI's own layout; `then` is what to edit afterwards. */
  async function putCi(body: CiLayoutUpdate, sent: SentLayout | null, message: string, then: (c: CiLayout) => void): Promise<boolean> {
    const id = ciId.value;
    if (!id) return false;
    saveError.value = null;
    busy.value = true;
    try {
      const version = ci.value?.version ?? undefined;
      ci.value = await layoutApi.setCiLayout(id, version !== undefined ? { ...body, version } : body);
      then(ci.value);
      saved.value = message;
      announceSave();
      return true;
    } catch (e) {
      saveError.value = e;
      refused.value = sent && clone(sent);
      return false;
    } finally {
      busy.value = false;
    }
  }

  /** Saves the draft for this CI only (its own layout). */
  async function saveForCi(): Promise<boolean> {
    if (!draft.value) return false;
    const sent = { prefix: "layout", layout: draft.value };
    return putCi({ layout: content() }, sent, t("layoutEditor.savedCi"), () => {
      target.value = { kind: "ci" };
      baseline.value = JSON.stringify(draft.value);
    });
  }

  /** Shows template `key` on this CI instead of the class's default, and edits it (unsaved changes are dropped). */
  async function chooseTemplate(key: string): Promise<boolean> {
    const name = templates.value.find((x) => x.key === key)?.name ?? key;
    return putCi({ templateKey: key }, null, t("layoutEditor.usesTemplate", { name }), () => edit({ kind: "template", key }));
  }

  /** Back to the class's default template for this CI, and edits it (unsaved changes are dropped). */
  async function resetCi(): Promise<boolean> {
    const id = ciId.value;
    if (!id) return false;
    saveError.value = null;
    busy.value = true;
    try {
      await layoutApi.resetCiLayout(id);
      ci.value = await fetchCiLayout(qc, id);
      edit({ kind: "template", key: ci.value.templateKey ?? ci.value.classTemplateKey });
      saved.value = t("layoutEditor.resetDone", { name: ci.value.templateName ?? "" });
      announceSave();
      return true;
    } catch (e) {
      saveError.value = e;
      refused.value = null;
      return false;
    } finally {
      busy.value = false;
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
    delete query[TEMPLATE_QUERY];
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
    saving: computed(() => busy.value || saveMutation.isPending.value),
    saveError,
    /** The last refused save's errors by section key (lib sectionErrors). */
    sectionErrors: computed(() => sectionErrors(saveError.value, refused.value)),
    saved,
    conflict,
    stale,
    currentVersion: computed(() => settings.data.value?.version),
    loadedVersion,
    canUndo: computed(() => past.value.length > 0),
    canRedo: computed(() => future.value.length > 0),
    /** What is edited, the template if it is one, who uses it, and the templates there are. */
    target,
    template,
    users,
    templates,
    /** The CI the editor runs on (none on the create form), and the layout it shows. */
    onCi: computed(() => !!ciId.value),
    ci,
    ciHasOwn,
    tabKey,
    tab,
    selected,
    snap,
    announcement,
    say,
    layer,
    apply,
    endGesture,
    undo,
    redo,
    discard,
    resetToBuiltIn,
    saveToTemplate,
    saveAsTemplate,
    saveForCi,
    chooseTemplate,
    resetCi,
    reload: load,
    enter,
    exit,
    openedHere,
    /** Ask for a reload here after the other window saves: this browser cannot tell this window. */
    reloadHint: computed(() => opened.value && !broadcast),
  });
}

export type LayoutEditor = ReturnType<typeof useLayoutEditor>;
