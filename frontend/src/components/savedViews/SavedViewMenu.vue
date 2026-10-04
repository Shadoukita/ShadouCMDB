<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import {
  useCopySavedView,
  useCreateSavedView,
  useDeleteSavedView,
  useSavedViews,
  useSetSavedViewDefault,
  useUpdateSavedView,
  type SavedView,
} from "../../api/savedViews";
import { plural } from "../../lib/format";
import { param, type QueryContext } from "../../lib/inventoryQuery";
import { definitionFromUrl, droppedSummary, groupViews, homeName, sameState, urlState, viewState, viewUrlQuery, type DefinitionCatalogue } from "../../lib/savedViews";
import type { useInventoryQueryState } from "../../lib/useInventoryQueryState";
import type { useSavedViewSelection } from "../../lib/useSavedViewSelection";
import { useSessionStore } from "../../stores/session";
import ConfirmDialog from "../ConfirmDialog.vue";
import ErrorAlert from "../ErrorAlert.vue";
import ManageViewsDialog from "./ManageViewsDialog.vue";
import SavedViewDialog, { type SavedViewDialogMode, type SavedViewDialogValues } from "./SavedViewDialog.vue";
import Icon from "../Icon.vue";

/**
 * The View menu of the inventory and the search page (saved-views spec §1.2):
 * the user's views, the shared views, and the actions on the view shown. Next to
 * it, a Modified marker with Save and Revert while the list differs from the view.
 * A WAI-ARIA menu button: Enter, Space or ↓ opens it on the first item, ↑ on the
 * last; the arrows, Home, End and a typed letter move; Esc closes it and returns
 * focus to the button; Tab closes it.
 */
const props = defineProps<{
  context: QueryContext;
  state: ReturnType<typeof useInventoryQueryState>;
  selection: ReturnType<typeof useSavedViewSelection>;
  classes: readonly { id: string; key: string; name: string }[] | undefined;
  /** Class, lookup list and value keys, to save the URL's ids as keys; null while they load. */
  catalogue: DefinitionCatalogue | null;
  /** The number of CIs listed, once the list for the current URL has loaded (for the announcement). */
  total: number | undefined;
}>();

const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const path = computed(() => (props.context === "inventory" ? "/cis" : "/search"));
const canShare = computed(() => session.can("views.share"));

const viewsQuery = useSavedViews(() => props.context);
const views = computed(() => viewsQuery.data.value?.data ?? []);
const current = computed(() => props.selection.current.value);
const loadError = computed(() => (viewsQuery.isError.value ? viewsQuery.error.value : null));
const requestId = computed(() => (loadError.value instanceof ApiError ? loadError.value.requestId : undefined));

/** The list differs from the view it came from (§1.1): compared after the baseline fills in sort and page size. */
const modified = computed(() => {
  const v = current.value;
  if (!v) return false;
  const saved = viewState(v, props.context);
  if (!saved) return true;
  return !sameState(urlState(route.query, props.context, { sort: props.state.sort.value, limit: props.state.limit.value }), saved);
});

// ---------- Menu ----------

const open = ref(false);
const filter = ref("");
const button = ref<HTMLButtonElement>();
const popup = ref<HTMLElement>();
const menuEl = ref<HTMLElement>();
const filterInput = ref<HTMLInputElement>();
const uid = useId();
const menuId = `view-menu-${uid}`;
const showFilter = computed(() => views.value.length > 10);
const groups = computed(() => groupViews(views.value, filter.value));
const searchNeedsTerm = computed(() => props.context === "search" && !param(route.query, "q").trim());

const buttonLabel = computed(() => {
  if (viewsQuery.isPending.value && props.selection.viewId.value) return "Views…";
  return current.value ? current.value.name : "Unsaved view";
});

/** The default of the list shown: its class's, or the unscoped inventory's. */
const listHome = computed<string | null | undefined>(() => {
  const id = props.state.classId.value;
  if (!id) return null;
  if (id.includes(",")) return undefined; // several classes: no default slot
  return props.classes?.find((c) => c.id === id)?.key;
});
const listDefault = computed(() =>
  listHome.value === undefined ? undefined : views.value.find((v) => v.isDefault && (v.home ?? null) === listHome.value),
);

interface Action {
  key: string;
  label: string;
  run: () => void;
  disabled?: boolean;
  hint?: string;
  danger?: boolean;
}
const actions = computed<Action[]>(() => {
  const v = current.value;
  const out: Action[] = [];
  if (v?.canEdit) out.push({ key: "save", label: "Save view", run: () => void saveCurrent() });
  out.push({
    key: "saveAs",
    label: "Save as new view…",
    run: () => openDialog("create"),
    disabled: searchNeedsTerm.value || !props.catalogue,
    hint: searchNeedsTerm.value ? "Type a search term first: a search view saves the term." : undefined,
  });
  if (v?.canEdit) out.push({ key: "rename", label: "Rename…", run: () => openDialog("rename", v) });
  if (props.context === "inventory") {
    if (v && v.isDefault) out.push({ key: "clearDefault", label: "Clear my default", run: () => void setDefault(v, false) });
    else if (v && v.resolved.state !== "unavailable")
      out.push({ key: "setDefault", label: `Set as my default for ${homeName(v, props.classes)}`, run: () => void setDefault(v, true) });
    else if (!v && listDefault.value)
      out.push({ key: "clearDefault", label: "Clear my default", run: () => void setDefault(listDefault.value!, false) });
  }
  if (v?.visibility === "shared") out.push({ key: "copy", label: "Copy to my views", run: () => openDialog("copy", v) });
  if (v?.visibility === "personal" && canShare.value) out.push({ key: "share", label: "Share a copy…", run: () => openDialog("share", v) });
  if (v?.canEdit) out.push({ key: "delete", label: "Delete…", run: () => openDelete(v), danger: true });
  out.push({ key: "copyLink", label: "Copy link", run: () => void copyLink() });
  out.push({ key: "manage", label: "Manage views…", run: () => (manageOpen.value = true) });
  return out;
});

const items = () => [...(menuEl.value?.querySelectorAll<HTMLElement>("[role^=menuitem]") ?? [])];

async function show(at: "first" | "last" = "first") {
  open.value = true;
  filter.value = "";
  document.addEventListener("pointerdown", onOutside, true);
  await nextTick();
  if (at === "first" && showFilter.value) return filterInput.value?.focus();
  const all = items();
  (at === "first" ? all[0] : all[all.length - 1])?.focus();
}
function hide(refocus: boolean) {
  if (!open.value) return;
  open.value = false;
  document.removeEventListener("pointerdown", onOutside, true);
  if (refocus) button.value?.focus();
}
function onOutside(e: Event) {
  const t = e.target as Node;
  if (!popup.value?.contains(t) && !button.value?.contains(t)) hide(false);
}
onBeforeUnmount(() => hide(false));

function onButtonKey(e: KeyboardEvent) {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    void show(e.key === "ArrowDown" ? "first" : "last");
  }
}
function onFilterKey(e: KeyboardEvent) {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    items()[0]?.focus();
  } else if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    hide(true);
  }
}
function onMenuKey(e: KeyboardEvent) {
  const all = items();
  const at = all.indexOf(document.activeElement as HTMLElement);
  const to = { ArrowDown: at + 1, ArrowUp: at - 1 + all.length, Home: 0, End: all.length - 1 }[e.key];
  if (to !== undefined) {
    e.preventDefault();
    if (e.key === "ArrowUp" && at === 0 && showFilter.value) return filterInput.value?.focus();
    all[to % all.length]?.focus();
  } else if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    hide(true);
  } else if (e.key === "Tab") {
    hide(false);
  } else if (e.key.length === 1 && /\S/.test(e.key) && !e.ctrlKey && !e.metaKey && !e.altKey) {
    // Type-ahead: the next item whose name starts with the letter (not its text: the checked view starts with ✓).
    const k = e.key.toLocaleLowerCase();
    const n = all.length;
    for (let i = 1; i <= n; i++) {
      const el = all[(at + i) % n];
      if ((el.dataset.label ?? el.textContent ?? "").trim().toLocaleLowerCase().startsWith(k)) {
        e.preventDefault();
        return el.focus();
      }
    }
  }
}

/** Retry keeps the menu open, so the operator sees the views come in; focus moves to the first item. */
async function retry() {
  await viewsQuery.refetch();
  await nextTick();
  items()[0]?.focus();
}

function pick(v: SavedView) {
  if (v.resolved.state === "unavailable") return;
  hide(true);
  const to = viewUrlQuery(v, props.context);
  if (!to) return;
  props.selection.applied.value = v.name;
  void router.push({ path: path.value, query: to });
}
function runAction(a: Action) {
  if (a.disabled) return;
  // Focus goes back to the menu button first, so a dialog returns it there when it closes.
  hide(true);
  a.run();
}

// ---------- Announcements (§1.6) ----------

const announcement = ref("");
const say = (text: string) => {
  announcement.value = "";
  void nextTick(() => (announcement.value = text));
};
watch(
  () => [props.selection.applied.value, props.total, props.selection.pending.value] as const,
  ([name, total, pending]) => {
    if (!name || pending || total === undefined) return;
    props.selection.applied.value = null;
    say(props.context === "inventory" ? `View ${name} applied, ${plural(total, "configuration item")}` : `View ${name} applied`);
  },
);

// ---------- Save, Save as, Rename, Copy, Share ----------

const create = useCreateSavedView();
const update = useUpdateSavedView();
const remove = useDeleteSavedView();
const copy = useCopySavedView();
const setDefaultMutation = useSetSavedViewDefault();

/** An error outside the dialogs (Save view, set default, copy link), shown under the toolbar. */
const actionError = ref<unknown>(null);

function definitionNow() {
  if (!props.catalogue) return { ok: false as const, message: "The data model is still loading. Try again in a moment." };
  return definitionFromUrl(route.query, props.context, props.catalogue, { sort: props.state.sort.value, limit: props.state.limit.value });
}
/** Show the saved view: its full state in the URL, so a reload shows what was saved. */
function showView(v: SavedView) {
  const to = viewUrlQuery(v, props.context);
  if (to) void router.replace({ path: path.value, query: to });
}

const dialog = ref<{ mode: SavedViewDialogMode; view?: SavedView } | null>(null);
const dialogError = ref<unknown>(null);
const dialogBusy = computed(() => create.isPending.value || update.isPending.value || copy.isPending.value);
const dialogInitial = computed(() => {
  const d = dialog.value;
  if (!d?.view) return { name: "" };
  if (d.mode === "rename") return { name: d.view.name, description: d.view.description };
  return { name: d.view.name };
});
function openDialog(mode: SavedViewDialogMode, view?: SavedView) {
  dialogError.value = null;
  dialog.value = { mode, view };
}
function closeDialog() {
  dialog.value = null;
  dialogError.value = null;
}

async function submitDialog(values: SavedViewDialogValues) {
  const d = dialog.value;
  if (!d) return;
  dialogError.value = null;
  try {
    if (d.mode === "create") {
      const def = definitionNow();
      if (!def.ok) return void (dialogError.value = new Error(def.message));
      const v = await create.mutateAsync({
        context: props.context,
        name: values.name,
        description: values.description,
        visibility: values.shared ? "shared" : "personal",
        definition: def.definition,
      });
      closeDialog();
      showView(v);
      say(`View ${v.name} saved`);
    } else if (d.mode === "rename" && d.view) {
      const v = await update.mutateAsync({ id: d.view.id, version: d.view.version, name: values.name, description: values.description });
      closeDialog();
      say(`View renamed to ${v.name}`);
    } else if (d.view) {
      const v = await copy.mutateAsync({ id: d.view.id, name: values.name, visibility: d.mode === "share" ? "shared" : "personal" });
      closeDialog();
      if (d.mode === "copy") showView(v);
      say(d.mode === "share" ? `Shared copy ${v.name} created` : `View copied to My views as ${v.name}`);
    }
  } catch (e) {
    if (e instanceof ApiError && e.code === "VERSION_CONFLICT" && d.view) {
      closeDialog();
      conflict.value = { view: d.view, message: e.message };
    } else {
      dialogError.value = e;
    }
  }
}

/** Save view: overwrite the view shown with what the list shows now. */
async function saveCurrent() {
  const v = current.value;
  if (!v) return;
  actionError.value = null;
  const def = definitionNow();
  if (!def.ok) return void (actionError.value = new Error(def.message));
  try {
    const saved = await update.mutateAsync({ id: v.id, version: v.version, definition: def.definition });
    showView(saved);
    say(`View ${saved.name} saved`);
  } catch (e) {
    if (e instanceof ApiError && e.code === "VERSION_CONFLICT") conflict.value = { view: v, message: e.message };
    else actionError.value = e;
  }
}

function revert() {
  const v = current.value;
  const to = v && viewUrlQuery(v, props.context);
  if (to) void router.push({ path: path.value, query: to });
}

async function setDefault(v: SavedView, on: boolean) {
  actionError.value = null;
  try {
    await setDefaultMutation.mutateAsync({ classKey: v.home ?? null, viewId: on ? v.id : null });
    say(on ? `${v.name} is now your default for ${homeName(v, props.classes)}` : `Default for ${homeName(v, props.classes)} cleared`);
  } catch (e) {
    actionError.value = e;
  }
}

async function copyLink() {
  actionError.value = null;
  try {
    await navigator.clipboard.writeText(window.location.href);
    say("Link copied");
  } catch {
    actionError.value = new Error("The browser did not allow copying. Copy the address from the address bar instead.");
  }
}

// ---------- Version conflict (§1.5) ----------

const conflict = ref<{ view: SavedView; message: string } | null>(null);
async function loadLatest() {
  const v = conflict.value?.view;
  conflict.value = null;
  const fresh = await viewsQuery.refetch();
  const latest = fresh.data?.data.find((x) => x.id === v?.id);
  if (latest) showView(latest);
}
function conflictSaveAs() {
  conflict.value = null;
  openDialog("create");
}

// ---------- Delete ----------

const deleting = ref<SavedView | null>(null);
const deleteError = ref<unknown>(null);
function openDelete(v: SavedView) {
  deleteError.value = null;
  deleting.value = v;
}
async function confirmDelete() {
  const v = deleting.value;
  if (!v) return;
  deleteError.value = null;
  try {
    await remove.mutateAsync({ id: v.id, version: v.version });
    deleting.value = null;
    say(`View ${v.name} deleted`);
    if (v.id === props.selection.viewId.value) {
      // The view is gone: its list as it opens without it (the class's list view, then the defaults).
      const classId = props.classes?.find((c) => c.key === v.home)?.id;
      props.selection.skipNextDefault();
      void router.push({ path: path.value, query: props.context === "inventory" && classId ? { classId } : {} });
    }
  } catch (e) {
    if (e instanceof ApiError && e.code === "VERSION_CONFLICT") {
      deleting.value = null;
      conflict.value = { view: v, message: e.message };
    } else {
      deleteError.value = e;
    }
  }
}

// ---------- Manage ----------

const manageOpen = ref(false);
function manageRename(v: SavedView) {
  openDialog("rename", v);
}
function openManageFromLimit() {
  closeDialog();
  manageOpen.value = true;
}

// ---------- Banners (§1.5) ----------

const notice = computed(() => props.selection.notice.value);
/** What resolution dropped from the view shown, while the list still shows it as it came. */
const dropped = computed(() => {
  const v = current.value;
  if (!v || modified.value || v.resolved.issues.length === 0) return null;
  return droppedSummary(v);
});
</script>

<template>
  <div class="view-menu">
    <span :id="`${menuId}-label`" class="label">View</span>
    <div class="view-menu-row">
      <button
        ref="button"
        type="button"
        class="btn view-menu-button"
        aria-haspopup="menu"
        :aria-expanded="open"
        :aria-controls="open ? menuId : undefined"
        :aria-labelledby="`${menuId}-label ${menuId}-button-text`"
        @click="open ? hide(false) : show()"
        @keydown="onButtonKey"
      >
        <span :id="`${menuId}-button-text`" class="view-menu-name">{{ buttonLabel }}</span>
        <span v-if="current?.isDefault" class="badge">Default</span>
        <Icon name="chevron-down" />
      </button>
      <template v-if="modified">
        <span class="view-modified"><Icon name="pencil" :size="14" /> Modified</span>
        <button v-if="current?.canEdit" type="button" class="btn btn-sm" :disabled="update.isPending.value" @click="saveCurrent">
          {{ update.isPending.value ? "Saving…" : "Save" }}
        </button>
        <button type="button" class="btn btn-sm" @click="revert">Revert</button>
      </template>
    </div>

    <div v-if="open" ref="popup" class="popover view-menu-popup">
      <div v-if="showFilter" class="field">
        <label :for="`${menuId}-filter`" class="sr-only">Filter views</label>
        <input :id="`${menuId}-filter`" ref="filterInput" v-model="filter" type="search" placeholder="Filter views…" @keydown="onFilterKey" />
      </div>
      <p v-if="viewsQuery.isPending.value" class="view-menu-status" role="status">
        <span class="spinner" aria-hidden="true" /> Loading saved views…
      </p>
      <div v-else-if="loadError" :id="`${menuId}-error`" class="view-menu-status alert alert-error">
        Saved views could not be loaded.
        <span v-if="requestId" class="meta">Request ID: <span class="mono">{{ requestId }}</span></span>
      </div>
      <p v-else-if="views.length === 0" class="view-menu-status muted">
        No saved views yet. Set filters, sort and columns, then choose <strong>Save as new view</strong>.
      </p>
      <p v-else-if="filter && groups.personal.length + groups.shared.length === 0" class="view-menu-status muted">No view matches “{{ filter }}”.</p>

      <ul :id="menuId" ref="menuEl" role="menu" aria-label="Views" :aria-describedby="loadError ? `${menuId}-error` : undefined" @keydown="onMenuKey">
        <li v-if="loadError" role="none">
          <button type="button" role="menuitem" tabindex="-1" @click="retry">Retry</button>
        </li>
        <template v-for="g in (['personal', 'shared'] as const)" :key="g">
          <li v-if="groups[g].length > 0" role="none">
            <span :id="`${menuId}-${g}`" class="view-menu-heading">{{ g === "personal" ? "My views" : "Shared views" }}</span>
            <ul role="group" :aria-labelledby="`${menuId}-${g}`">
              <li v-for="v in groups[g]" :key="v.id" role="none">
                <button
                  type="button"
                  role="menuitemradio"
                  tabindex="-1"
                  :aria-checked="v.id === current?.id"
                  :data-label="v.name"
                  :aria-disabled="v.resolved.state === 'unavailable' ? 'true' : undefined"
                  :aria-describedby="v.resolved.state === 'unavailable' ? `${menuId}-unavailable` : undefined"
                  :title="v.resolved.state === 'unavailable' ? 'This view refers to a filter that no longer exists.' : (v.description ?? undefined)"
                  @click="pick(v)"
                >
                  <span class="check"><Icon v-if="v.id === current?.id" name="check" /></span>
                  <span class="view-menu-name">{{ v.name }}</span>
                  <span v-if="v.isDefault" class="badge">Default</span>
                  <span v-if="v.resolved.state === 'unavailable'" class="muted">(unavailable)</span>
                </button>
              </li>
            </ul>
          </li>
        </template>
        <li role="separator" class="divider" />
        <li v-for="a in actions" :key="a.key" role="none">
          <button
            type="button"
            role="menuitem"
            tabindex="-1"
            :class="{ danger: a.danger }"
            :aria-disabled="a.disabled ? 'true' : undefined"
            :title="a.hint"
            @click="runAction(a)"
          >
            {{ a.label }}
          </button>
        </li>
      </ul>
      <span :id="`${menuId}-unavailable`" class="sr-only">This view refers to a filter that no longer exists.</span>
    </div>

    <span class="sr-only" role="status" aria-live="polite">{{ announcement }}</span>
  </div>

  <div v-if="notice" class="view-menu-banner alert alert-warn" role="alert">
    <template v-if="notice.kind === 'notAvailable'">The saved view in this link is not available to you. Showing the default list.</template>
    <template v-else>
      The view “{{ notice.name }}” refers to a filter that no longer exists, so it was not applied. Showing the default list.
    </template>
    <button type="button" class="btn btn-sm" @click="selection.dismissNotice()">Dismiss</button>
  </div>
  <div v-if="dropped && dropped.count > 0" class="view-menu-banner alert alert-warn" role="status">
    <template v-if="dropped.messages.length > 0">
      Parts of this view no longer exist and were left out:
      <ul>
        <li v-for="m in dropped.messages" :key="m">{{ m }}</li>
      </ul>
    </template>
    <template v-else>
      {{ dropped.count === 1 ? "1 part of this view is" : `${dropped.count} parts of this view are` }} not available to you and
      {{ dropped.count === 1 ? "was" : "were" }} left out.
    </template>
    <p v-if="dropped.notes.length > 0" class="meta">{{ dropped.notes.join(" ") }}</p>
    <div v-if="current?.canEdit">
      <button type="button" class="btn btn-sm" :disabled="update.isPending.value" @click="saveCurrent">Save to fix</button>
      <span class="muted"> Saves the view as the list shows it now.</span>
    </div>
  </div>
  <div v-else-if="dropped && dropped.notes.length > 0" class="view-menu-banner alert" role="status">{{ dropped.notes.join(" ") }}</div>
  <div v-if="actionError" class="view-menu-banner">
    <ErrorAlert :error="actionError" title="The view action failed" />
  </div>

  <!-- In <body>, not the toolbar: the toolbar's control widths would apply to the dialog fields (GH#483). -->
  <Teleport to="body">
    <SavedViewDialog
      :open="!!dialog"
      :mode="dialog?.mode ?? 'create'"
      :initial="dialogInitial"
      :can-share="canShare"
      :busy="dialogBusy"
      :error="dialogError"
      @submit="submitDialog"
      @cancel="closeDialog"
      @manage="openManageFromLimit"
    />

    <ConfirmDialog
      :open="!!deleting"
      :title="`Delete view “${deleting?.name ?? ''}”?`"
      confirm-label="Delete view"
      busy-label="Deleting…"
      :busy="remove.isPending.value"
      @confirm="confirmDelete"
      @cancel="deleting = null"
    >
      <template v-if="deleting">
        <ErrorAlert v-if="deleteError" :error="deleteError" title="The view was not deleted" />
        <p v-if="deleting.visibility === 'shared'">
          It is shared with everyone<template v-if="deleting.defaultCount !== undefined">
            and is the default for <strong>{{ deleting.defaultCount.toLocaleString() }}</strong>
            {{ deleting.defaultCount === 1 ? "user" : "users" }}</template
          >. They will return to the standard list. Configuration items are not affected.
        </p>
        <p v-else-if="deleting.isDefault">
          This cannot be undone. It is your default for <strong>{{ homeName(deleting, classes) }}</strong>. The
          {{ homeName(deleting, classes) }} list will open with the standard columns and filters.
        </p>
        <p v-else>This cannot be undone. Configuration items are not affected.</p>
      </template>
    </ConfirmDialog>

    <ConfirmDialog
      :open="!!conflict"
      title="This view was changed elsewhere"
      confirm-label="Load latest"
      cancel-label="Cancel"
      tone="primary"
      @confirm="loadLatest"
      @cancel="conflict = null"
    >
      <p>{{ conflict?.message }}</p>
      <p>Load the latest version, or keep what the list shows now as a new view.</p>
      <p><button type="button" class="btn" @click="conflictSaveAs">Save as new view…</button></p>
    </ConfirmDialog>
  </Teleport>

  <ManageViewsDialog
    :open="manageOpen"
    :classes="classes"
    :can-share="canShare"
    @close="manageOpen = false"
    @rename="manageRename"
    @delete="openDelete"
    @set-default="(v, on) => setDefault(v, on)"
  />
</template>
