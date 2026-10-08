<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ApiError } from "../../api/client";
import { NOTES_PAGE, useCiNotes, useCreateCiNote, useDeleteCiNote, useUpdateCiNote, type CiNote } from "../../api/ciNotes";
import type { Ci } from "../../api/queries";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { t } from "../../i18n";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";

/**
 * The CI's notes (gap G13, design §0.6): a stream of plain-text notes, newest first and paged on the server, with
 * an add box for users with edit on the class. Edit and Delete show on the notes the API says the caller may
 * change (`canEdit`, `canDelete`: the author inside the edit window, an administrator deleting at any time); the
 * server checks again, and its refusal (403) or a version conflict (409) is shown on the note. The text is shown
 * as typed, line breaks kept, never as Markdown or HTML.
 */
const props = defineProps<{ ci: Ci }>();
const session = useSessionStore();
const flash = useFlashStore();

const MAX = 10_000;
const paging = ref({ limit: NOTES_PAGE, offset: 0 });
watch(
  () => props.ci.id,
  () => {
    paging.value = { limit: NOTES_PAGE, offset: 0 };
    draft.value = "";
    addError.value = null;
    editing.value = null;
    noteError.value = null;
  },
);
const notes = useCiNotes(() => props.ci.id, paging);
const items = computed(() => notes.data.value?.data ?? []);
const total = computed(() => notes.data.value?.page.total ?? 0);
/** Adding needs edit on the class; a deleted CI takes no new notes. */
const canAdd = computed(() => !props.ci.deletedAt && session.canOnClass(props.ci.classId, "edit"));

const fieldError = (e: unknown) => (e instanceof ApiError && e.code === "VALIDATION_ERROR" ? e.fieldErrors().body : undefined);

// ---------- Add ----------
const draft = ref("");
const addError = ref<unknown>(null);
const create = useCreateCiNote(() => props.ci.id);
const addFieldError = computed(() => fieldError(addError.value));
async function onAdd() {
  addError.value = null;
  if (!draft.value.trim()) {
    addError.value = new ApiError(400, "VALIDATION_ERROR", t("notes.add.blank"), [{ field: "body", message: t("notes.add.blank") }]);
    document.getElementById("note-add")?.focus();
    return;
  }
  try {
    await create.mutateAsync(draft.value);
    draft.value = "";
    // The new note is the newest: show the first page, where it is.
    paging.value = { limit: paging.value.limit, offset: 0 };
    flash.show(t("notes.added"));
  } catch (e) {
    addError.value = e;
  }
}

// ---------- Edit ----------
/** The note being edited: the version it was loaded at, the text typed, and whether a save found it changed. */
const editing = ref<{ noteId: string; version: number; text: string; conflict: boolean } | null>(null);
/** The last refusal on a note (edit or delete), shown on that note. */
const noteError = ref<{ noteId: string; error: unknown } | null>(null);
const update = useUpdateCiNote(() => props.ci.id);
const editFieldError = computed(() => (editing.value && noteError.value?.noteId === editing.value.noteId ? fieldError(noteError.value.error) : undefined));
const errorOn = (n: CiNote) => (noteError.value?.noteId === n.id && !(editing.value?.noteId === n.id && editFieldError.value) ? noteError.value.error : null);

async function startEdit(n: CiNote) {
  editing.value = { noteId: n.id, version: n.version, text: n.body, conflict: false };
  noteError.value = null;
  await nextTick();
  document.getElementById(`note-edit-${n.id}`)?.focus();
}
function cancelEdit() {
  const id = editing.value?.noteId;
  editing.value = null;
  noteError.value = null;
  if (id) void nextTick(() => document.getElementById(`note-edit-btn-${id}`)?.focus());
}
async function onSave(n: CiNote) {
  const e = editing.value;
  if (!e) return;
  noteError.value = null;
  if (!e.text.trim()) {
    noteError.value = { noteId: n.id, error: new ApiError(400, "VALIDATION_ERROR", t("notes.add.blank"), [{ field: "body", message: t("notes.add.blank") }]) };
    return;
  }
  // After a conflict the operator has seen the saved text: saving again replaces that version.
  const version = e.conflict ? n.version : e.version;
  try {
    await update.mutateAsync({ noteId: n.id, version, body: e.text });
    editing.value = null;
    flash.show(t("notes.saved"));
  } catch (err) {
    noteError.value = { noteId: n.id, error: err };
    if (err instanceof ApiError && err.code === "VERSION_CONFLICT") e.conflict = true;
    // The window closed or the right was withdrawn: the editor can no longer save.
    if (err instanceof ApiError && err.code === "FORBIDDEN") editing.value = null;
  }
}

// ---------- Delete ----------
const deleting = ref<CiNote | null>(null);
const remove = useDeleteCiNote(() => props.ci.id);
async function onDelete() {
  const n = deleting.value;
  if (!n) return;
  noteError.value = null;
  try {
    await remove.mutateAsync({ noteId: n.id, version: n.version });
    if (editing.value?.noteId === n.id) editing.value = null;
    // The last note of a later page: go back a page.
    if (items.value.length === 1 && paging.value.offset > 0) paging.value = { limit: paging.value.limit, offset: Math.max(0, paging.value.offset - paging.value.limit) };
    flash.show(t("notes.deleted"));
  } catch (err) {
    noteError.value = { noteId: n.id, error: err };
  } finally {
    deleting.value = null;
  }
}
const excerpt = (body: string) => (body.length > 160 ? `${body.slice(0, 160)}…` : body);
const edited = (n: CiNote) => !!n.editedAt;
</script>

<template>
  <section class="panel notes-panel" aria-labelledby="notes-title">
    <div class="panel-header">
      <h2 id="notes-title">{{ t("notes.title") }} <span v-if="notes.data.value" class="count mono">{{ total.toLocaleString() }}</span></h2>
      <span class="muted">{{ t("notes.order") }}</span>
    </div>

    <form v-if="canAdd" class="note-add panel-body" novalidate @submit.prevent="onAdd">
      <div class="field">
        <label for="note-add">{{ t("notes.add.label") }}</label>
        <textarea
          id="note-add"
          v-model="draft"
          rows="3"
          :maxlength="MAX"
          dir="auto"
          :aria-invalid="!!addFieldError || undefined"
          :aria-describedby="addFieldError ? 'note-add-err note-add-hint' : 'note-add-hint'"
          @keydown.ctrl.enter.prevent="onAdd"
          @keydown.meta.enter.prevent="onAdd"
        />
        <span v-if="addFieldError" id="note-add-err" class="error">{{ addFieldError }}</span>
        <span id="note-add-hint" class="hint">{{ t("notes.add.hint") }}</span>
      </div>
      <ErrorAlert v-if="addError && !addFieldError" :error="addError" />
      <div class="note-add-actions">
        <button type="submit" class="btn btn-primary" :disabled="create.isPending.value">{{ create.isPending.value ? t("common.saving") : t("notes.add.submit") }}</button>
      </div>
    </form>

    <LoadingState v-if="notes.isLoading.value" :label="t('notes.loading')" />
    <ErrorAlert v-else-if="notes.isError.value && !notes.data.value" :error="notes.error.value" :on-retry="() => notes.refetch()" />
    <EmptyState v-else-if="total === 0" :title="t('notes.empty.title')" icon="file-text">
      {{ canAdd ? t("notes.empty.body") : ci.deletedAt ? t("notes.empty.deleted") : t("notes.empty.readOnly") }}
    </EmptyState>
    <template v-else>
      <ErrorAlert v-if="notes.isError.value" :error="notes.error.value" :on-retry="() => notes.refetch()" />
      <ol class="note-stream" :aria-label="t('notes.title')" :aria-busy="notes.isFetching.value || undefined" data-testid="note-stream">
        <li v-for="n in items" :key="n.id" class="note" data-testid="note">
          <p :id="`note-head-${n.id}`" class="note-head">
            <span class="note-author">{{ n.author.name }}<span v-if="!n.author.id" class="muted"> {{ t("notes.authorDeleted") }}</span></span>
            <time class="note-time" :datetime="n.createdAt" :title="formatDateTime(n.createdAt)">{{ formatRelative(n.createdAt) }}</time>
            <span v-if="edited(n)" class="note-edited muted" :title="t('notes.editedAt', { when: formatDateTime(n.editedAt) })">{{ t("notes.edited") }}</span>
          </p>
          <form v-if="editing?.noteId === n.id" class="note-edit" novalidate @submit.prevent="onSave(n)">
            <template v-if="editing.conflict">
              <p class="hint">{{ t("notes.conflict.current") }}</p>
              <p class="note-body note-current" dir="auto">{{ n.body }}</p>
            </template>
            <div class="field">
              <label :for="`note-edit-${n.id}`" class="sr-only">{{ t("notes.edit.label") }}</label>
              <textarea
                :id="`note-edit-${n.id}`"
                v-model="editing.text"
                rows="4"
                :maxlength="MAX"
                dir="auto"
                :aria-invalid="!!editFieldError || undefined"
                :aria-describedby="editFieldError ? `note-edit-${n.id}-err` : undefined"
                @keydown.esc.prevent="cancelEdit"
              />
              <span v-if="editFieldError" :id="`note-edit-${n.id}-err`" class="error">{{ editFieldError }}</span>
            </div>
            <ErrorAlert v-if="errorOn(n)" :error="errorOn(n)" />
            <div class="note-actions">
              <button type="submit" class="btn btn-primary btn-sm" :disabled="update.isPending.value">
                {{ update.isPending.value ? t("common.saving") : editing.conflict ? t("notes.conflict.replace") : t("notes.edit.save") }}
              </button>
              <button type="button" class="btn btn-sm" :disabled="update.isPending.value" @click="cancelEdit">{{ t("common.cancel") }}</button>
            </div>
          </form>
          <template v-else>
            <p class="note-body" dir="auto">{{ n.body }}</p>
            <ErrorAlert v-if="errorOn(n)" :error="errorOn(n)" />
            <div v-if="n.canEdit || n.canDelete" class="note-actions">
              <button
                v-if="n.canEdit"
                :id="`note-edit-btn-${n.id}`"
                type="button"
                class="btn btn-link btn-sm"
                :aria-describedby="`note-head-${n.id}`"
                @click="startEdit(n)"
              >
                {{ t("common.edit") }}
              </button>
              <button v-if="n.canDelete" type="button" class="btn btn-link btn-sm danger" :aria-describedby="`note-head-${n.id}`" @click="deleting = n">
                {{ t("common.delete") }}
              </button>
              <span v-if="n.canEdit && n.editableUntil" class="muted note-window" :title="formatDateTime(n.editableUntil)">{{
                t("notes.editableUntil", { when: formatDateTime(n.editableUntil) })
              }}</span>
            </div>
          </template>
        </li>
      </ol>
      <PaginationBar v-if="total > paging.limit" :total="total" :limit="paging.limit" :offset="paging.offset" @change="(p) => (paging = p)" />
    </template>

    <ConfirmDialog
      :open="!!deleting"
      :title="t('notes.delete.title')"
      :confirm-label="t('notes.delete.confirm')"
      :busy="remove.isPending.value"
      @cancel="deleting = null"
      @confirm="onDelete"
    >
      <p>{{ t("notes.delete.body", { author: deleting?.author.name ?? "", when: deleting ? formatDateTime(deleting.createdAt) : "" }) }}</p>
      <blockquote v-if="deleting" class="note-body note-quote" dir="auto">{{ excerpt(deleting.body) }}</blockquote>
      <p class="hint">{{ t("notes.delete.audit") }}</p>
    </ConfirmDialog>
  </section>
</template>
