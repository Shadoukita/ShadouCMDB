<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { moveItem } from "../lib/reorder";
import { t } from "../i18n";
import Icon from "./Icon.vue";

/**
 * The operator's column chooser for the inventory: the shown columns in order
 * (tick to hide, ↑/↓ to move), then the fields and the class's attributes that
 * are not shown (tick to add at the end). Label is always shown. The choice is
 * the list's `columns` URL parameter; "Reset" goes back to the list view's or
 * the default columns. Non-modal: Esc, Done or a click or Tab outside close it.
 */
const props = defineProps<{
  /** The columns shown, in order. */
  columns: string[];
  fields: { key: string; label: string }[];
  attributes: { key: string; label: string }[];
  /** The one class the list is of: attributes are offered only then (like attribute sorts). */
  className: string | undefined;
  /** Whether the URL chooses the columns (Reset is offered). */
  customized: boolean;
}>();
const emit = defineEmits<{ toggle: [field: string]; reorder: [columns: string[]]; reset: [] }>();

const open = ref(false);
const root = ref<HTMLElement>();
const button = ref<HTMLButtonElement>();
const panel = ref<HTMLElement>();
const labels = computed(() => new Map([...props.fields, ...props.attributes].map((o) => [o.key, o.label])));
const labelOf = (k: string) => labels.value.get(k) ?? k;
const moreFields = computed(() => props.fields.filter((f) => !props.columns.includes(f.key)));
const moreAttributes = computed(() => props.attributes.filter((a) => !props.columns.includes(a.key)));
const id = (field: string) => `col-${field.replace(/\./g, "-")}`;

async function show() {
  open.value = true;
  await nextTick();
  panel.value?.querySelector<HTMLElement>("input:not(:disabled)")?.focus();
}
function close(returnFocus: boolean) {
  open.value = false;
  if (returnFocus) button.value?.focus();
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && open.value) {
    e.stopPropagation();
    close(true);
  }
}
/** Tabbing out of the popover closes it; focus goes where the operator tabbed to. */
function onFocusOut(e: FocusEvent) {
  const to = e.relatedTarget as Node | null;
  if (open.value && to && !root.value?.contains(to)) close(false);
}
function onDocClick(e: MouseEvent) {
  if (open.value && !root.value?.contains(e.target as Node)) close(false);
}
onMounted(() => document.addEventListener("click", onDocClick));
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));

/**
 * Focus follows the column: once the URL has changed and the list has re-rendered,
 * the control the operator used is found again by id (moved to the other list, moved
 * up or down, or disabled at the end of the list).
 */
let refocusTo: string[] | null = null;
watch(
  () => props.columns,
  () => {
    const selectors = refocusTo;
    refocusTo = null;
    for (const s of selectors ?? []) {
      const el = panel.value?.querySelector<HTMLButtonElement | HTMLInputElement>(s);
      if (el && !el.disabled) return el.focus();
    }
  },
  { flush: "post" },
);
function refocus(selectors: string[]) {
  refocusTo = selectors;
}
function toggle(field: string) {
  emit("toggle", field);
  refocus([`#${id(field)}`]);
}
function move(i: number, to: number, dir: "up" | "down") {
  const field = props.columns[i];
  emit("reorder", moveItem(props.columns, i, to));
  const other = dir === "up" ? "down" : "up";
  refocus([`#${id(field)}-${dir}`, `#${id(field)}-${other}`]);
}
function reset() {
  emit("reset");
  refocus(["input:not(:disabled)"]);
}
</script>

<template>
  <div ref="root" class="columns-menu" @keydown="onKeydown" @focusout="onFocusOut">
    <button
      ref="button"
      type="button"
      class="btn"
      aria-haspopup="dialog"
      aria-controls="columns-popover"
      :aria-expanded="open"
      @click="open ? close(false) : show()"
    >
      <Icon name="columns-3" />{{ t("columns.button") }}<span v-if="customized" class="badge">{{ t("columns.custom") }}</span><Icon name="chevron-down" />
    </button>
    <div v-if="open" id="columns-popover" ref="panel" class="popover columns-popover" role="dialog" aria-labelledby="columns-popover-title">
      <h2 id="columns-popover-title" class="popover-title">{{ t("columns.button") }}</h2>
      <ol class="columns-shown" :aria-label="t('columns.shown')">
        <li v-for="(c, i) in columns" :key="c">
          <label class="checkbox-row" :for="id(c)">
            <input :id="id(c)" type="checkbox" checked :disabled="c === 'label'" @change="toggle(c)" />
            {{ labelOf(c) }}
            <span v-if="c === 'label'" class="muted">{{ t("columns.alwaysShown") }}</span>
          </label>
          <span class="row-actions">
            <button :id="`${id(c)}-up`" type="button" class="btn btn-sm btn-icon" :disabled="i === 0" :aria-label="t('columns.moveUp', { name: labelOf(c) })" @click="move(i, i - 1, 'up')"><Icon name="arrow-up" /></button>
            <button
              :id="`${id(c)}-down`"
              type="button"
              class="btn btn-sm btn-icon"
              :disabled="i === columns.length - 1"
              :aria-label="t('columns.moveDown', { name: labelOf(c) })"
              @click="move(i, i + 1, 'down')"
            ><Icon name="arrow-down" /></button>
          </span>
        </li>
      </ol>
      <fieldset v-if="moreFields.length > 0" class="columns-more">
        <legend>{{ t("columns.moreFields") }}</legend>
        <label v-for="f in moreFields" :key="f.key" class="checkbox-row" :for="id(f.key)">
          <input :id="id(f.key)" type="checkbox" @change="toggle(f.key)" /> {{ f.label }}
        </label>
      </fieldset>
      <fieldset v-if="className && moreAttributes.length > 0" class="columns-more">
        <legend>{{ t("columns.attributesOf", { name: className }) }}</legend>
        <label v-for="a in moreAttributes" :key="a.key" class="checkbox-row" :for="id(a.key)">
          <input :id="id(a.key)" type="checkbox" @change="toggle(a.key)" /> {{ a.label }}
        </label>
      </fieldset>
      <p v-else-if="!className" class="muted">{{ t("columns.oneClassHint") }}</p>
      <div class="popover-actions">
        <button type="button" class="btn btn-sm" :disabled="!customized" @click="reset">{{ t("columns.reset") }}</button>
        <button type="button" class="btn btn-sm btn-primary" @click="close(true)">{{ t("columns.done") }}</button>
      </div>
    </div>
  </div>
</template>
