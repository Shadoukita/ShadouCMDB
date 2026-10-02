<script setup lang="ts">
import { useQueryClient } from "@tanstack/vue-query";
import { computed, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError, api, unwrap } from "../../api/client";
import { useAllLookupListValues, useLookupLists } from "../../api/datamodel";
import {
  savedViewKeys,
  useCopySavedView,
  useCreateSavedView,
  useDeleteSavedView,
  useSetDefaultView,
  useUpdateSavedView,
  type SavedView,
  type SavedViewContext,
} from "../../api/savedViews";
import { plural } from "../../lib/format";
import {
  definitionFromUrl,
  degradedLines,
  deleteConsequence,
  homeLabel,
  saveFailure,
  viewActions,
  viewQuery,
  type SaveFailure,
  type StateDefaults,
} from "../../lib/savedViews";
import type { SavedViewState } from "../../lib/useSavedViewState";
import { useSessionStore } from "../../stores/session";
import ConfirmDialog from "../ConfirmDialog.vue";
import ErrorAlert from "../ErrorAlert.vue";
import ManageViewsDialog from "./ManageViewsDialog.vue";
import ViewConflictDialog from "./ViewConflictDialog.vue";
import ViewMenu, { type ViewAction } from "./ViewMenu.vue";
import ViewNameDialog from "./ViewNameDialog.vue";

/**
 * Saved views on a list page (/cis, /search): the View menu, the Modified marker
 * with Save and Revert, the banners about the view in the URL, every dialog, and a
 * polite live region ("View X applied, 1,234 configuration items", "View saved").
 * Errors are shown where they happen (in the dialog, under the field, or in an
 * alert here), never only as a toast. The columns control sits in the slot.
 */
const props = defineProps<{
  context: SavedViewContext;
  sv: SavedViewState;
  /** The list's effective sort and page size (a URL without them stands for them). */
  defaults: StateDefaults;
  classes: readonly { id: string; key: string; name: string }[];
  /** The list's result: the total once loaded, for the announcement. */
  total: number | undefined;
  fetching: boolean;
}>();

const route = useRoute();
const router = useRouter();
const qc = useQueryClient();
const session = useSessionStore();
const canShare = computed(() => session.can("views.share"));
const lists = useLookupLists();
const values = useAllLookupListValues();

const current = computed(() => props.sv.current.value);
const modified = computed(() => props.sv.modified(props.defaults));
const homeName = computed(() => (props.context === "inventory" ? homeLabel(props.sv.home.value, props.classes) : null));
const actions = computed(() => viewActions(current.value, { canShare: canShare.value, context: props.context, home: props.sv.home.value }));
const label = computed(() => {
  if (current.value) return current.value.name;
  if (props.sv.viewId.value && props.sv.viewsLoading.value) return "Views…";
  return "Unsaved view";
});
const degraded = computed(() => (current.value && current.value.resolved.state === "degraded" ? degradedLines(current.value) : []));

// ---------- Announcements ----------
const announcement = ref("");
const announceApplied = ref<string | null>(null);
function announce(text: string) {
  // Cleared first, so the same text twice is announced twice.
  announcement.value = "";
  requestAnimationFrame(() => (announcement.value = text));
}
watch(
  () => [announceApplied.value, props.total, props.fetching, props.sv.holding.value] as const,
  ([name, total, fetching, holding]) => {
    if (!name || fetching || holding || total === undefined) return;
    announceApplied.value = null;
    const what = props.context === "search" ? plural(total, "match", "matches") : plural(total, "configuration item");
    announce(`View ${name} applied, ${what}`);
  },
);

// ---------- Errors outside a dialog ----------
const barError = ref<unknown>(null);
const barErrorTitle = ref("");
function fail(title: string, e: unknown) {
  barErrorTitle.value = title;
  barError.value = e;
}

// ---------- Definition of the list shown ----------
function definition(): { ok: true; def: ReturnType<typeof definitionFromUrl>["definition"] } | { ok: false; message: string } {
  const needsLookups = !!(route.query.lookupValueId || route.query.criticalityValueId);
  if (needsLookups && (!lists.data.value || !values.data.value)) return { ok: false, message: "The lookup values are still loading. Try again in a moment." };
  const r = definitionFromUrl(route.query, props.context, { classes: props.classes, lists: lists.data.value ?? [], values: values.data.value ?? [] });
  if (r.unknown.length > 0)
    return { ok: false, message: "A class or lookup value in the filters no longer exists. Remove that filter, then save the view." };
  if (props.context === "search" && !r.definition.filters?.q) return { ok: false, message: "Type a search term first: a search view saves the term and its filters." };
  return { ok: true, def: r.definition };
}

// ---------- Mutations ----------
const create = useCreateSavedView();
const update = useUpdateSavedView();
const remove = useDeleteSavedView();
const copy = useCopySavedView();
const setDefault = useSetDefaultView();

type NameMode = "saveAs" | "rename" | "copyToMine" | "shareCopy";
const nameDialog = ref<{ mode: NameMode; view?: SavedView } | null>(null);
const nameFailure = ref<SaveFailure | null>(null);
const nameBusy = computed(() => create.isPending.value || update.isPending.value || copy.isPending.value);
const nameTexts = computed(() => {
  const d = nameDialog.value;
  const v = d?.view;
  switch (d?.mode) {
    case "rename":
      return { title: `Rename view “${v?.name}”`, submit: "Rename", name: v?.name ?? "", description: v?.description, withDescription: true };
    case "copyToMine":
      return { title: `Copy “${v?.name}” to my views`, submit: "Copy", name: v?.name ?? "", intro: "Creates a personal view with the same filters, sort and columns." };
    case "shareCopy":
      return {
        title: `Share a copy of “${v?.name}”`,
        submit: "Share copy",
        name: v?.name ?? "",
        intro: "Creates a shared view that every user who may view its classes can open. Your personal view stays as it is.",
      };
    default:
      return {
        title: "Save as new view",
        submit: "Save view",
        name: "",
        description: null,
        withDescription: true,
        intro:
          props.context === "search"
            ? "Saves the search term and its filters."
            : "Saves the classes, filters, sort, columns and page size of this list.",
      };
  }
});

function openName(mode: NameMode, view?: SavedView) {
  nameFailure.value = null;
  nameDialog.value = { mode, view };
}
function onApiFailure(e: unknown, target: "name" | "bar", title: string) {
  if (e instanceof ApiError) {
    const f = saveFailure(e);
    if (f.kind === "conflict" && target === "bar") return openConflict();
    if (target === "name") return void (nameFailure.value = f);
  }
  fail(title, e);
}

async function submitName(v: { name: string; description: string | null; share: boolean }) {
  const d = nameDialog.value;
  if (!d) return;
  nameFailure.value = null;
  try {
    if (d.mode === "saveAs") {
      const def = definition();
      if (!def.ok) return void (nameFailure.value = { kind: "fields", definition: [], other: def.message });
      const created = await create.mutateAsync({
        context: props.context,
        name: v.name,
        description: v.description,
        visibility: v.share && canShare.value ? "shared" : "personal",
        definition: def.def,
      });
      nameDialog.value = null;
      conflict.value = null;
      await router.replace({ path: route.path, query: viewQuery(created) ?? { ...route.query, view: created.id } });
      announce(`View ${created.name} saved`);
    } else if (d.mode === "rename" && d.view) {
      const updated = await update.mutateAsync({ id: d.view.id, body: { version: d.view.version, name: v.name, description: v.description } });
      nameDialog.value = null;
      announce(`View renamed to ${updated.name}`);
    } else if (d.view) {
      const created = await copy.mutateAsync({ id: d.view.id, name: v.name, visibility: d.mode === "shareCopy" ? "shared" : "personal" });
      nameDialog.value = null;
      if (d.mode === "copyToMine") await props.sv.apply(created);
      announce(d.mode === "shareCopy" ? `Shared a copy as ${created.name}` : `Copied to my views as ${created.name}`);
    }
  } catch (e) {
    onApiFailure(e, "name", "The view was not saved");
  }
}

/** Save view: the list shown becomes the current view's definition. */
async function save() {
  const v = current.value;
  if (!v) return;
  barError.value = null;
  const def = definition();
  if (!def.ok) return fail("The view was not saved", new Error(def.message));
  try {
    const updated = await update.mutateAsync({ id: v.id, body: { version: v.version, definition: def.def } });
    const q = viewQuery(updated);
    if (q) await router.replace({ path: route.path, query: q });
    announce(`View ${updated.name} saved`);
  } catch (e) {
    onApiFailure(e, "bar", "The view was not saved");
  }
}

// ---------- Version conflict ----------
const conflict = ref<{ latest?: SavedView } | null>(null);
async function openConflict() {
  const v = current.value;
  if (!v) return;
  conflict.value = {};
  try {
    const latest = await qc.fetchQuery({
      queryKey: savedViewKeys.view(v.id),
      staleTime: 0,
      queryFn: ({ signal }) => unwrap(api.GET("/api/v1/saved-views/{id}", { params: { path: { id: v.id } }, signal })),
    });
    if (conflict.value) conflict.value = { latest };
  } catch {
    // The dialog still offers Save as and Cancel; Load latest then reloads the list.
  }
  void qc.invalidateQueries({ queryKey: savedViewKeys.all });
}
async function loadLatest() {
  const latest = conflict.value?.latest;
  conflict.value = null;
  if (latest) await props.sv.apply(latest);
}
function conflictSaveAs() {
  conflict.value = null;
  openName("saveAs");
}

// ---------- Delete ----------
const deleting = ref<SavedView | null>(null);
const deleteError = ref<unknown>(null);
const deleteText = computed(() => (deleting.value ? deleteConsequence(deleting.value, homeLabel(deleting.value.home, props.classes)) : ""));
function askDelete(v: SavedView) {
  deleteError.value = null;
  deleting.value = v;
}
async function confirmDelete() {
  const v = deleting.value;
  if (!v) return;
  try {
    await remove.mutateAsync({ id: v.id, version: v.version });
    deleting.value = null;
    if (v.id === props.sv.viewId.value) await props.sv.detach();
    announce(`View ${v.name} deleted`);
  } catch (e) {
    deleteError.value = e;
  }
}

// ---------- Default ----------
async function changeDefault(v: SavedView, on: boolean) {
  barError.value = null;
  try {
    await setDefault.mutateAsync({ classKey: v.home ?? null, viewId: on ? v.id : null });
    const slot = homeLabel(v.home, props.classes);
    announce(on ? `${v.name} is now your default for ${slot}` : `Cleared your default for ${slot}`);
  } catch (e) {
    fail(on ? "The default was not set" : "The default was not cleared", e);
  }
}

// ---------- Menu ----------
const manageOpen = ref(false);
async function copyLink() {
  try {
    await navigator.clipboard.writeText(window.location.href);
    announce("Link copied: it holds every filter, the sort and the columns");
  } catch (e) {
    fail("The link could not be copied", new Error(`Copy this address instead: ${window.location.href}`));
  }
}
function onAction(a: ViewAction) {
  const v = current.value;
  barError.value = null;
  switch (a) {
    case "save":
      return void save();
    case "saveAs":
      return openName("saveAs");
    case "rename":
      return v && openName("rename", v);
    case "copyToMine":
      return v && openName("copyToMine", v);
    case "shareCopy":
      return v && openName("shareCopy", v);
    case "setDefault":
      return v && void changeDefault(v, true);
    case "clearDefault":
      return v && void changeDefault(v, false);
    case "delete":
      return v && askDelete(v);
    case "copyLink":
      return void copyLink();
    case "manage":
      manageOpen.value = true;
  }
}
function select(v: SavedView) {
  barError.value = null;
  announceApplied.value = v.name;
  void props.sv.apply(v);
}
function revert() {
  const v = current.value;
  if (v) select(v);
}
function manageFromLimit() {
  nameDialog.value = null;
  manageOpen.value = true;
}
</script>

<template>
  <div class="view-bar">
    <div class="view-bar-controls">
      <ViewMenu
        :views="sv.views.value"
        :loading="sv.viewsLoading.value"
        :error="sv.listError.value"
        :current-id="current?.id"
        :label="label"
        :actions="actions"
        :home-name="homeName"
        @select="select"
        @action="onAction"
        @retry="sv.refetch()"
      />
      <slot />
      <template v-if="modified">
        <span class="view-modified"><span aria-hidden="true">●</span> Modified</span>
        <button v-if="actions.save" type="button" class="btn btn-sm btn-primary" :disabled="update.isPending.value" @click="save">
          {{ update.isPending.value ? "Saving…" : "Save" }}
        </button>
        <button type="button" class="btn btn-sm" @click="revert">Revert</button>
      </template>
    </div>

    <div v-if="sv.notice.value" class="alert alert-warn view-notice" role="status">
      <template v-if="sv.notice.value.kind === 'notAvailable'">The saved view in this link is not available to you. Showing the default list.</template>
      <template v-else>The saved view “{{ sv.notice.value.name }}” refers to a filter that no longer exists, so it was not applied. Showing the default list.</template>
      <button type="button" class="btn btn-sm spaced" @click="sv.dismissNotice()">Dismiss</button>
    </div>
    <div v-if="degraded.length > 0" class="alert alert-warn view-notice" role="status">
      <strong>Parts of the view “{{ current?.name }}” no longer exist and were left out.</strong>
      <ul>
        <li v-for="(line, i) in degraded" :key="i">{{ line }}</li>
      </ul>
      <button v-if="current?.canEdit" type="button" class="btn btn-sm" :disabled="update.isPending.value" @click="save">Save to fix</button>
    </div>
    <div v-if="barError" class="view-notice">
      <ErrorAlert :error="barError" :title="barErrorTitle" />
      <button type="button" class="btn btn-sm" @click="barError = null">Dismiss</button>
    </div>
    <p class="sr-only" role="status" aria-live="polite">{{ announcement }}</p>

    <ViewNameDialog
      :open="!!nameDialog"
      :title="nameTexts.title"
      :submit-label="nameTexts.submit"
      :intro="nameTexts.intro"
      :name="nameTexts.name"
      :description="nameTexts.description"
      :with-description="nameTexts.withDescription"
      :with-share="nameDialog?.mode === 'saveAs' && canShare"
      :busy="nameBusy"
      :failure="nameFailure"
      @submit="submitName"
      @cancel="nameDialog = null"
      @manage="manageFromLimit"
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
      <p class="dialog-intro">{{ deleteText }}</p>
      <ErrorAlert v-if="deleteError" :error="deleteError" title="The view was not deleted" />
    </ConfirmDialog>
    <ViewConflictDialog
      :open="!!conflict"
      :name="current?.name ?? ''"
      :updated-by="conflict?.latest?.updatedBy.name"
      :updated-at="conflict?.latest?.updatedAt"
      @latest="loadLatest"
      @save-as="conflictSaveAs"
      @cancel="conflict = null"
    />
    <ManageViewsDialog
      :open="manageOpen"
      :can-share="canShare"
      :classes="classes"
      @close="manageOpen = false"
      @rename="(v) => openName('rename', v)"
      @delete="askDelete"
      @set-default="(v) => changeDefault(v, true)"
      @clear-default="(v) => changeDefault(v, false)"
    />
  </div>
</template>
