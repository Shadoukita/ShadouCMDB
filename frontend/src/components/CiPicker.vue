<script setup lang="ts">
import { computed, onBeforeUnmount, ref, useId } from "vue";
import { useCiList, type CiSummary } from "../api/queries";
import { useDebounced } from "../lib/composables";

/**
 * Type-ahead picker for a configuration item. Queries the API server-side
 * (optionally restricted to a class and its subclasses); never loads the inventory.
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
  { id: undefined, classId: undefined, excludeId: undefined, placeholder: "Search by name, hostname, IP…", describedBy: undefined },
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
  limit: 15,
  sort: "name",
}));
const items = computed(() => (data.value?.data ?? []).filter((c) => c.id !== props.excludeId));

function choose(ci: CiSummary) {
  emit("select", ci);
  text.value = "";
  open.value = false;
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
  <div v-if="selected" class="checkbox-row">
    <strong>{{ selected.name }}</strong>
    <button type="button" class="btn btn-sm" :aria-label="`Clear ${selected.name}`" @click="emit('select', null)">Change</button>
  </div>
  <div v-else class="combo">
    <input
      :id="inputId"
      type="search"
      role="combobox"
      :aria-expanded="open"
      :aria-controls="listId"
      aria-autocomplete="list"
      :aria-activedescendant="open && items[active] ? `${listId}-${active}` : undefined"
      :aria-invalid="invalid || undefined"
      :aria-describedby="describedBy"
      autocomplete="off"
      :placeholder="placeholder"
      :value="text"
      style="width: 280px"
      @input="onInput"
      @focus="onFocus"
      @blur="onBlur"
      @keydown="onKeydown"
    />
    <ul v-if="open" :id="listId" class="combo-list" role="listbox">
      <li v-if="isError" class="note">Search failed</li>
      <li v-else-if="items.length === 0" class="note">{{ isFetching ? "Searching…" : "No matching CIs" }}</li>
      <li
        v-for="(ci, i) in items"
        :id="`${listId}-${i}`"
        :key="ci.id"
        role="option"
        :aria-selected="i === active"
        @mousedown.prevent="choose(ci)"
        @mouseenter="active = i"
      >
        <span>{{ ci.name }}</span>
        <span class="muted">{{ ci.class.name }}</span>
        <span v-if="ci.hostname" class="muted mono">{{ ci.hostname }}</span>
      </li>
      <li v-if="data && data.page.total > items.length" class="note">
        {{ data.page.total - items.length }} more — keep typing to narrow down
      </li>
    </ul>
  </div>
</template>
