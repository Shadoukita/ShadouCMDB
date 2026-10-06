<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId } from "vue";
import { useCiList, type CiSummary } from "../api/queries";
import { t } from "../i18n";
import { useDebounced } from "../lib/composables";
import Icon from "./Icon.vue";

/**
 * Type-ahead picker for a configuration item. Queries the API server-side
 * (optionally restricted to a class and its subclasses); never loads the inventory.
 * The `selected` slot renders the chosen CI's name (a CI page makes it a link).
 * A chosen CI reads as a filled input: the name in a control-sized box with a clear button, which
 * brings the search back and focuses it (audit R10).
 */
const props = withDefaults(
  defineProps<{
    id?: string;
    classId?: string | null;
    excludeId?: string;
    selected: { id: string; name: string } | null;
    placeholder?: string;
    invalid?: boolean;
    describedBy?: string;
  }>(),
  { id: undefined, classId: undefined, excludeId: undefined, placeholder: undefined, describedBy: undefined },
);
const emit = defineEmits<{ select: [ci: CiSummary | null] }>();

const autoId = useId();
const inputId = computed(() => props.id ?? autoId);
const listId = computed(() => `${inputId.value}-list`);
const text = ref("");
const open = ref(false);
const active = ref(0);
const q = useDebounced(() => text.value.trim(), 200);
const { data, isFetching, isError } = useCiList(() => ({
  q: q.value || undefined,
  classId: props.classId || undefined,
  // Planned and retired CIs can be referenced too.
  active: "all",
  limit: 15,
  sort: "label",
}));
const items = computed(() => (data.value?.data ?? []).filter((c) => c.id !== props.excludeId));

function choose(ci: CiSummary) {
  emit("select", ci);
  text.value = "";
  open.value = false;
}

const input = ref<HTMLInputElement | null>(null);
function clear() {
  emit("select", null);
  // The parent swaps the value for the search box: focus it there, not on the button that went away.
  void nextTick(() => input.value?.focus());
}

function onInput(e: Event) {
  clearTimeout(closeTimer);
  text.value = (e.target as HTMLInputElement).value;
  open.value = true;
  active.value = 0;
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    open.value = true;
    active.value = Math.min(active.value + 1, items.value.length - 1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    active.value = Math.max(active.value - 1, 0);
  } else if (e.key === "Enter" && open.value && items.value[active.value]) {
    e.preventDefault();
    choose(items.value[active.value]);
  } else if (e.key === "Escape") {
    open.value = false;
  }
}

// Close shortly after blur so a click on an option still lands. A later focus or
// keystroke cancels the pending close, or a stale timer would shut the next list.
let closeTimer: ReturnType<typeof setTimeout> | undefined;
function onBlur() {
  closeTimer = setTimeout(() => (open.value = false), 150);
}
function onFocus() {
  clearTimeout(closeTimer);
  open.value = true;
}
onBeforeUnmount(() => clearTimeout(closeTimer));
</script>

<template>
  <div v-if="selected" :class="['picker-value', { invalid }]">
    <span class="picker-name" dir="auto"><slot name="selected" :selected="selected">{{ selected.name }}</slot></span>
    <button type="button" class="btn btn-sm btn-ghost btn-icon" :aria-label="t('ciPicker.clear', { name: selected.name })" :title="t('ciPicker.clear', { name: selected.name })" :aria-describedby="describedBy" @click="clear">
      <Icon name="x" :size="14" />
    </button>
  </div>
  <div v-else class="combo">
    <input
      :id="inputId"
      ref="input"
      type="search"
      role="combobox"
      :aria-expanded="open"
      :aria-controls="listId"
      aria-autocomplete="list"
      :aria-activedescendant="open && items[active] ? `${listId}-${active}` : undefined"
      :aria-invalid="invalid || undefined"
      :aria-describedby="describedBy"
      autocomplete="off"
      :placeholder="placeholder ?? t('ciPicker.placeholder')"
      :value="text"
      @input="onInput"
      @focus="onFocus"
      @blur="onBlur"
      @keydown="onKeydown"
    />
    <ul v-if="open" :id="listId" class="combo-list" role="listbox">
      <li v-if="isError" class="note">{{ t("common.searchFailed") }}</li>
      <li v-else-if="items.length === 0" class="note">{{ isFetching ? t("common.searching") : t("ciPicker.noMatch") }}</li>
      <li
        v-for="(ci, i) in items"
        :id="`${listId}-${i}`"
        :key="ci.id"
        role="option"
        :aria-selected="i === active"
        @mousedown.prevent="choose(ci)"
        @mouseenter="active = i"
      >
        <span>{{ ci.label }}</span>
        <span class="muted">{{ ci.class.name }}</span>
        <span class="muted mono">{{ ci.ident }}</span>
        <span v-if="!ci.active" class="muted">{{ t("ciPicker.inactive") }}</span>
      </li>
      <li v-if="data && data.page.total > items.length" class="note">
        {{ t("common.moreResults", { n: data.page.total - items.length }) }}
      </li>
    </ul>
  </div>
</template>
