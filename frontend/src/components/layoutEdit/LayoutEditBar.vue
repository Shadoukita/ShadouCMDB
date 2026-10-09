<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { t } from "../../i18n";
import { LAYER_ICONS, layerOf, LAYER_MOVES } from "../../lib/freeLayout";
import { findSection } from "../../lib/layoutDesign";
import type { LayoutEditor } from "../../lib/layoutEditor";
import { templateNameProblem, TEMPLATE_DESCRIPTION_MAX, TEMPLATE_NAME_MAX } from "../../lib/layoutTemplates";
import ConfirmDialog from "../ConfirmDialog.vue";
import ErrorAlert from "../ErrorAlert.vue";
import FormDialog from "../FormDialog.vue";
import RowMenu, { type RowMenuItem } from "../RowMenu.vue";
import Icon from "../Icon.vue";

/**
 * The bar over a CI page in the layout editor: what is being edited (a layout
 * template, with who uses it, or this CI's own layout), undo and redo, back to
 * the built-in layout, the CI's layout (another template, back to the class's
 * default), and save: to the template (confirmed with who it changes), as a new
 * template (optionally the class's default) or for this CI only, then Discard and
 * Done (closes the editor's window). Ctrl+Z / Ctrl+Shift+Z (or Ctrl+Y) undo and
 * redo outside text fields. For the tab in view: whether windows snap, and the
 * layers of the selected window.
 */
const props = defineProps<{ editor: LayoutEditor; className: string }>();
/** The window selected on the tab in view, with its place in the stack. */
const picked = computed(() => {
  const l = props.editor.layout;
  const tb = props.editor.tab;
  const at = l && tb && props.editor.selected ? findSection(l, props.editor.selected) : undefined;
  return at && at.tab.key === tb!.key ? { section: at.section, ...layerOf(tb!, at.section) } : null;
});
const onTemplate = computed(() => props.editor.target?.kind === "template");
const templateName = computed(() => props.editor.template?.name ?? "");
const ciCount = computed(() => props.editor.users.ciCount);

/** "Template: <name> (used by N classes, M CIs)" or "This CI only". */
const badge = computed(() => {
  if (!onTemplate.value) return t("layoutEditor.badgeCi");
  const classes = props.editor.users.classes.length;
  if (ciCount.value === undefined) return t("layoutEditor.badgeTemplateLoading", { name: templateName.value });
  return ciCount.value === null
    ? t("layoutEditor.badgeTemplateHidden", { name: templateName.value, classes })
    : t("layoutEditor.badgeTemplate", { name: templateName.value, classes, cis: ciCount.value });
});
/** The editor previews another layout than the CI shows (a template opened from Customization). */
const elsewhere = computed(() => {
  const ci = props.editor.ci;
  const tg = props.editor.target;
  if (!ci || !tg) return null;
  if (tg.kind === "template" && (ci.source === "custom" || ci.templateKey !== tg.key)) {
    return ci.source === "custom" ? t("layoutEditor.ciShowsOwn") : t("layoutEditor.ciShowsTemplate", { name: ci.templateName ?? "" });
  }
  return null;
});

// ---- Save to the template (confirmed with who it changes) ----
const confirmSave = ref(false);
const note = ref("");
const shownClasses = computed(() => props.editor.users.classes.slice(0, 8));
async function onSaveTemplate() {
  confirmSave.value = false;
  if (await props.editor.saveToTemplate(note.value)) note.value = "";
}
function onPrimary() {
  if (onTemplate.value) confirmSave.value = true;
  else void props.editor.saveForCi();
}

// ---- Save as a new template ----
const saveAs = ref(false);
const newName = ref("");
const newDescription = ref("");
const makeDefault = ref(false);
const newNote = ref("");
const nameTouched = ref(false);
const nameProblem = computed(() => templateNameProblem(newName.value, props.editor.templates));
const nameError = computed(() => (nameTouched.value && nameProblem.value ? t(`layoutTemplates.name.${nameProblem.value}`, { max: TEMPLATE_NAME_MAX }) : ""));
function openSaveAs() {
  newName.value = "";
  newDescription.value = "";
  makeDefault.value = false;
  newNote.value = "";
  nameTouched.value = false;
  saveAs.value = true;
}
async function onSaveAs() {
  nameTouched.value = true;
  if (nameProblem.value) {
    document.getElementById("le-new-name")?.focus();
    return;
  }
  saveAs.value = false;
  await props.editor.saveAsTemplate({ name: newName.value, description: newDescription.value, makeDefault: makeDefault.value, comment: newNote.value });
}

const moreSave = computed<RowMenuItem[]>(() => [
  { label: t("layoutEditor.saveAsNew"), action: openSaveAs },
  ...(props.editor.onCi && onTemplate.value ? [{ label: t("layoutEditor.saveForCi"), action: () => void props.editor.saveForCi() }] : []),
]);

// ---- This CI: another template, or back to the class's default ----
const choosing = ref(false);
const chosen = ref("");
function openChoose() {
  chosen.value = props.editor.ci?.templateKey ?? props.editor.templates[0]?.key ?? "";
  choosing.value = true;
}
async function onChoose() {
  if (!chosen.value) return;
  choosing.value = false;
  await props.editor.chooseTemplate(chosen.value);
}
const confirmResetCi = ref(false);
const classDefaultName = computed(() => props.editor.templates.find((x) => x.key === props.editor.ci?.classTemplateKey)?.name ?? "");
async function onResetCi() {
  confirmResetCi.value = false;
  await props.editor.resetCi();
}

const confirmReset = ref(false);
function onReset() {
  confirmReset.value = false;
  props.editor.resetToBuiltIn();
}

function onKey(e: KeyboardEvent) {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
  const tg = e.target as HTMLElement | null;
  if (tg && (tg.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(tg.tagName))) return;
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
  <div class="le-bar" role="region" :aria-label="t('layoutEditor.region')">
    <div class="le-bar-row">
      <span class="le-bar-title">
        <span>
          <strong>{{ t("layoutEditor.title", { class: className }) }}</strong>
          <span class="badge le-target" data-testid="le-target">{{ badge }}</span>
        </span>
        <span class="muted">
          {{ onTemplate ? t("layoutEditor.appliesTemplate") : t("layoutEditor.appliesCi", { class: className }) }}<template v-if="editor.builtIn"> · {{ t("layoutEditor.builtIn") }}</template>
        </span>
        <span v-if="elsewhere" class="muted" data-testid="le-elsewhere">{{ elsewhere }}</span>
      </span>
      <span class="le-bar-group" role="group" :aria-label="t('layoutEditor.history')">
        <button type="button" class="btn btn-sm" :disabled="!editor.canUndo" :title="t('layoutEditor.undoTitle')" @click="editor.undo()"><Icon name="arrow-left" />{{ t("layoutEditor.undo") }}</button>
        <button type="button" class="btn btn-sm" :disabled="!editor.canRedo" :title="t('layoutEditor.redoTitle')" @click="editor.redo()"><Icon name="arrow-right" />{{ t("layoutEditor.redo") }}</button>
      </span>
      <button type="button" class="btn btn-sm" :disabled="editor.builtIn" @click="confirmReset = true">{{ t("layoutEditor.resetBuiltIn") }}</button>
      <span v-if="editor.onCi && editor.ci" class="le-bar-group" role="group" :aria-label="t('layoutEditor.ciGroup')">
        <button type="button" class="btn btn-sm" :disabled="editor.saving" @click="openChoose">{{ t("layoutEditor.useTemplate") }}</button>
        <button v-if="editor.ciHasOwn" type="button" class="btn btn-sm" :disabled="editor.saving" @click="confirmResetCi = true">{{ t("layoutEditor.resetCi") }}</button>
      </span>
    </div>
    <div class="le-bar-row">
      <template v-if="editor.tab">
        <button type="button" class="btn btn-sm le-snap-toggle" :aria-pressed="editor.snap" :title="t('layoutEditor.snapTitle')" @click="editor.snap = !editor.snap">
          <Icon :name="editor.snap ? 'check' : 'square'" />{{ t("layoutEditor.snap") }}
        </button>
        <span class="le-bar-group" role="group" :aria-label="t('layoutEditor.layers')">
          <span class="muted le-layer" data-testid="le-layer" :title="picked ? picked.section.label : undefined">{{ picked ? t("layoutEditor.layerOf", { index: picked.index, count: picked.count }) : t("layoutEditor.noWindow") }}</span>
          <button
            v-for="m in LAYER_MOVES"
            :key="m.move"
            type="button"
            class="btn btn-sm"
            :aria-label="t(`layoutEditor.layer.${m.move}`)"
            :title="`${t(`layoutEditor.layer.${m.move}`)} (${m.keys})`"
            :disabled="!picked || (m.move === 'front' || m.move === 'forward' ? picked.index >= picked.count : picked.index <= 1)"
            @click="editor.layer(m.move)"
          >
            <Icon :name="LAYER_ICONS[m.move]" />
          </button>
        </span>
      </template>
      <span class="le-bar-save">
        <span v-if="editor.dirty" class="save-bar-status"><Icon name="circle-alert" /><strong>{{ t("record.save.unsaved") }}</strong></span>
        <button type="button" class="btn btn-primary" :disabled="!editor.dirty || editor.saving" data-testid="le-save" @click="onPrimary">
          {{ editor.saving ? t("layoutEditor.saving") : onTemplate ? t("layoutEditor.saveToTemplate", { name: templateName }) : t("layoutEditor.saveForCi") }}
        </button>
        <RowMenu :label="t('layoutEditor.moreSave')" :items="moreSave" />
        <button type="button" class="btn" :disabled="!editor.dirty || editor.saving" @click="editor.discard()">{{ t("record.save.discard") }}</button>
        <button type="button" class="btn" @click="editor.exit()">{{ t("layoutEditor.done") }}</button>
      </span>
    </div>
    <div v-if="editor.openedHere" class="alert" role="note">{{ t("layoutEditor.openedHere") }}</div>
    <div v-if="editor.saved && !editor.dirty" class="alert alert-success" role="status">{{ editor.saved }}</div>
    <div v-if="editor.conflict || (editor.stale && editor.dirty)" class="alert alert-warn" role="alert">
      <strong>{{ t("layoutEditor.conflictTitle") }}</strong>
      <div>
        <template v-if="onTemplate">{{ t("layoutEditor.conflictSettings", { current: editor.currentVersion ?? "", loaded: editor.loadedVersion ?? "" }) }}</template>
        <template v-else>{{ t("layoutEditor.conflictCi") }}</template>
        <button type="button" class="btn btn-sm" @click="editor.reload()">{{ t("layoutEditor.loadLatest") }}</button> {{ t("layoutEditor.discardsChanges") }}
      </div>
    </div>
    <ErrorAlert v-else-if="editor.saveError" :error="editor.saveError" :title="t('layoutEditor.notSaved')" />
  </div>

  <ConfirmDialog :open="confirmReset" :title="t('layoutEditor.resetBuiltInTitle')" :confirm-label="t('layoutEditor.resetBuiltIn')" @confirm="onReset" @cancel="confirmReset = false">
    {{ onTemplate ? t("layoutEditor.resetBuiltInTemplate", { name: templateName }) : t("layoutEditor.resetBuiltInCi") }}
  </ConfirmDialog>

  <FormDialog :open="confirmSave" :title="t('layoutEditor.saveTemplateTitle', { name: templateName })" :submit-label="t('layoutEditor.saveTemplateConfirm')" @submit="onSaveTemplate" @cancel="confirmSave = false">
    <div class="stack">
    <p data-testid="le-impact">
      <template v-if="ciCount === null">{{ t("layoutEditor.impactHidden", { classes: editor.users.classes.length }) }}</template>
      <template v-else>{{ t("layoutEditor.impact", { classes: editor.users.classes.length, cis: ciCount ?? 0 }) }}</template>
    </p>
    <ul v-if="shownClasses.length > 0" class="le-impact-list">
      <li v-for="c in shownClasses" :key="c">{{ c }}</li>
      <li v-if="editor.users.classes.length > shownClasses.length" class="muted">{{ t("layoutEditor.impactMore", { n: editor.users.classes.length - shownClasses.length }) }}</li>
    </ul>
    <div class="field">
      <label for="le-note">{{ t("layoutEditor.note") }}</label>
      <input id="le-note" v-model="note" type="text" maxlength="500" :placeholder="t('layoutEditor.notePlaceholder')" />
    </div>
    </div>
  </FormDialog>

  <FormDialog :open="saveAs" :title="t('layoutEditor.saveAsTitle')" :submit-label="t('layoutEditor.saveAsConfirm')" @submit="onSaveAs" @cancel="saveAs = false">
    <div class="stack">
    <div class="field">
      <label for="le-new-name">{{ t("layoutTemplates.name") }}<span class="req" aria-hidden="true">*</span></label>
      <input
        id="le-new-name"
        v-model="newName"
        type="text"
        required
        :maxlength="TEMPLATE_NAME_MAX"
        :aria-invalid="!!nameError"
        :aria-describedby="nameError ? 'le-new-name-error' : undefined"
        autocomplete="off"
        @blur="nameTouched = true"
      />
      <span v-if="nameError" id="le-new-name-error" class="error">{{ nameError }}</span>
    </div>
    <div class="field">
      <label for="le-new-description">{{ t("layoutTemplates.description") }}</label>
      <input id="le-new-description" v-model="newDescription" type="text" :maxlength="TEMPLATE_DESCRIPTION_MAX" autocomplete="off" />
    </div>
    <label class="checkbox-row">
      <input v-model="makeDefault" type="checkbox" />
      {{ t("layoutEditor.makeDefault", { class: className }) }}
    </label>
    <p class="hint">{{ makeDefault ? t("layoutEditor.makeDefaultHint", { class: className }) : t("layoutEditor.notDefaultHint") }}</p>
    <div class="field">
      <label for="le-new-note">{{ t("layoutEditor.note") }}</label>
      <input id="le-new-note" v-model="newNote" type="text" maxlength="500" :placeholder="t('layoutEditor.notePlaceholder')" />
    </div>
    </div>
  </FormDialog>

  <FormDialog :open="choosing" :title="t('layoutEditor.useTemplateTitle')" :submit-label="t('layoutEditor.useTemplateConfirm')" @submit="onChoose" @cancel="choosing = false">
    <div class="stack">
    <div class="field">
      <label for="le-choose">{{ t("layoutEditor.useTemplateLabel") }}</label>
      <select id="le-choose" v-model="chosen">
        <option v-for="tp in editor.templates" :key="tp.key" :value="tp.key">
          {{ tp.name }}{{ tp.key === editor.ci?.classTemplateKey ? ` (${t("layoutEditor.classDefault")})` : "" }}
        </option>
      </select>
    </div>
    <p class="hint">{{ t("layoutEditor.useTemplateHint") }}<template v-if="editor.dirty"> {{ t("layoutEditor.dropsChanges") }}</template></p>
    </div>
  </FormDialog>

  <ConfirmDialog :open="confirmResetCi" :title="t('layoutEditor.resetCiTitle')" :confirm-label="t('layoutEditor.resetCi')" @confirm="onResetCi" @cancel="confirmResetCi = false">
    {{ editor.ci?.source === "custom" ? t("layoutEditor.resetCiBodyCustom", { class: className, name: classDefaultName }) : t("layoutEditor.resetCiBodyTemplate", { class: className, name: classDefaultName, current: editor.ci?.templateName ?? "" }) }}
    <template v-if="editor.dirty"> {{ t("layoutEditor.dropsChanges") }}</template>
  </ConfirmDialog>
</template>

<style scoped>
.le-bar {
  position: sticky;
  top: calc(-1 * var(--space-3));
  z-index: 6;
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  padding: var(--space-1) var(--space-3);
  margin-bottom: var(--space-3);
  border: 1px solid var(--c-border);
  border-top: 3px solid var(--c-primary);
  border-radius: var(--radius-lg);
  background: var(--c-surface);
  box-shadow: var(--shadow-md);
}
.le-bar-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-1) var(--space-2);
}
.le-bar-title {
  display: flex;
  flex-direction: column;
  margin-right: auto;
}
.le-target {
  margin-left: var(--space-1);
}
.le-bar-group,
.le-bar-save {
  display: inline-flex;
  align-items: center;
  gap: 2px;
}
.le-bar-save {
  gap: var(--space-1);
  margin-left: auto;
}
/* Snap is a toggle: pressed reads as selected (A10: the old rule never matched it). */
.le-snap-toggle[aria-pressed="true"] {
  border-color: var(--c-primary);
  background: var(--c-row-selected);
  color: var(--c-primary);
}
.le-layer {
  padding: 0 var(--space-2);
  font-variant-numeric: tabular-nums;
}
.le-bar .alert {
  margin: 0;
}
.le-impact-list {
  margin: 0 0 var(--space-2);
  padding-left: var(--space-3);
}
</style>
