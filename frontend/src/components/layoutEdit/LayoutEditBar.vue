<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { WIDTH_PRESETS, type LayoutEditor } from "../../lib/layoutEditor";
import ConfirmDialog from "../ConfirmDialog.vue";
import ErrorAlert from "../ErrorAlert.vue";

/**
 * The bar over a CI page in layout edit mode: which class's layout is being
 * edited (it applies to every CI of that class), undo and redo, preview widths,
 * back to the built-in layout, and save (a new settings version with an
 * optional note) or discard. Ctrl+Z / Ctrl+Shift+Z (or Ctrl+Y) undo and redo
 * outside text fields.
 */
const props = defineProps<{ editor: LayoutEditor; className: string; classKey: string }>();
const comment = ref("");
const confirmReset = ref(false);

async function onSave() {
  if (await props.editor.save(comment.value)) comment.value = "";
}
function onReset() {
  confirmReset.value = false;
  props.editor.resetToBuiltIn();
}

function onKey(e: KeyboardEvent) {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
  const t = e.target as HTMLElement | null;
  if (t && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName))) return;
  const key = e.key.toLowerCase();
  if (key === "z" && !e.shiftKey) {
    e.preventDefault();
    props.editor.undo();
  } else if ((key === "z" && e.shiftKey) || key === "y") {
    e.preventDefault();
    props.editor.redo();
  }
}
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="le-bar" role="region" aria-label="Layout editing">
    <div class="le-bar-row">
      <span class="le-bar-title">
        <strong>Editing the {{ className }} layout</strong>
        <span class="muted">Changes apply to every {{ className }} configuration item<template v-if="editor.builtIn"> · built-in layout</template></span>
      </span>
      <span class="le-bar-group" role="group" aria-label="History">
        <button type="button" class="btn btn-sm" :disabled="!editor.canUndo" title="Undo (Ctrl+Z)" @click="editor.undo()">Undo</button>
        <button type="button" class="btn btn-sm" :disabled="!editor.canRedo" title="Redo (Ctrl+Shift+Z)" @click="editor.redo()">Redo</button>
      </span>
      <span class="le-bar-group" role="group" aria-label="Preview width">
        <button v-for="p in WIDTH_PRESETS" :key="p.label" type="button" class="btn btn-sm" :aria-pressed="editor.previewWidth === p.width" @click="editor.previewWidth = p.width">
          {{ p.label }}
        </button>
      </span>
      <button type="button" class="btn btn-sm" :disabled="editor.builtIn" @click="confirmReset = true">Reset to built-in layout</button>
      <RouterLink class="btn btn-sm" :to="{ path: '/admin/customization/layouts', query: { class: classKey } }">Open in the designer</RouterLink>
    </div>
    <div class="le-bar-row">
      <span v-if="editor.dirty" class="badge warn">Unsaved changes</span>
      <span v-else class="muted">No unsaved changes</span>
      <label class="sr-only" for="le-comment">Note for this version</label>
      <input id="le-comment" v-model="comment" type="text" maxlength="500" placeholder="Note for this version (optional)" :disabled="!editor.dirty" />
      <button type="button" class="btn btn-primary" :disabled="!editor.dirty || editor.saving" @click="onSave">{{ editor.saving ? "Saving…" : "Save layout" }}</button>
      <button type="button" class="btn" :disabled="!editor.dirty || editor.saving" @click="editor.discard()">Discard</button>
      <button type="button" class="btn" @click="editor.exit()">Done</button>
    </div>
    <div v-if="editor.saved && !editor.dirty" class="alert" role="status">{{ editor.saved }} Every {{ className }} configuration item now shows it.</div>
    <div v-if="editor.conflict || (editor.stale && editor.dirty)" class="alert alert-warn" role="alert">
      <strong>Someone else saved the settings while you were editing.</strong>
      <div>
        Version {{ editor.currentVersion }} is now current; your changes are based on version {{ editor.loadedVersion }} and were not saved.
        <button type="button" class="btn btn-sm" @click="editor.reload()">Load the latest version</button> (discards your changes)
      </div>
    </div>
    <ErrorAlert v-else-if="editor.saveError" :error="editor.saveError" title="Layout not saved" />
  </div>

  <ConfirmDialog :open="confirmReset" :title="`Reset the ${className} layout?`" confirm-label="Reset to built-in layout" @confirm="onReset" @cancel="confirmReset = false">
    The class's own tabs, sections, widths, hidden and read-only fields are dropped from the draft, and every {{ className }} CI
    shows the built-in layout once you save. Undo brings them back until then.
  </ConfirmDialog>
</template>

<style scoped>
.le-bar {
  position: sticky;
  top: calc(-1 * var(--sp-4));
  z-index: 6;
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
  padding: var(--sp-2) var(--sp-4);
  margin-bottom: var(--sp-4);
  border: 2px solid var(--c-primary);
  border-radius: var(--radius);
  background: var(--c-surface-alt);
  box-shadow: 0 2px 8px rgb(0 0 0 / 10%);
}
.le-bar-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--sp-2) var(--sp-3);
}
.le-bar-title {
  display: flex;
  flex-direction: column;
  margin-right: auto;
}
.le-bar-group {
  display: inline-flex;
  gap: 2px;
}
.le-bar-group [aria-pressed="true"] {
  border-color: var(--c-primary);
  color: var(--c-primary);
}
.le-bar input[type="text"] {
  flex: 1;
  max-width: 420px;
}
.le-bar .alert {
  margin: 0;
}
</style>
