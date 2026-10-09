<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from "vue";
import { useUserList, type User } from "../../api/admin";
import { t } from "../../i18n";
import { useDebounced } from "../../lib/composables";

/**
 * Type-ahead picker for a user account (ARIA 1.2 combobox). Searches the API server-side and
 * announces the result count in a polite live region; never loads the user list.
 */
const props = defineProps<{ id: string; label: string; placeholder?: string; disabled?: boolean }>();
const emit = defineEmits<{ select: [user: User] }>();

const listId = computed(() => `${props.id}-list`);
const text = ref("");
const open = ref(false);
const active = ref(0);
const q = useDebounced(() => text.value.trim(), 200);
const { data, isFetching, isError } = useUserList(
  () => ({ q: q.value || undefined, sort: "username", limit: 15 }),
  () => !!q.value,
);
const items = computed(() => (q.value ? (data.value?.data ?? []) : []));
const more = computed(() => (q.value && data.value ? data.value.page.total - items.value.length : 0));
const announcement = computed(() =>
  open.value && q.value && data.value && !isFetching.value ? t("groups.members.results", { n: data.value.page.total }) : "",
);

function choose(u: User) {
  emit("select", u);
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
  } else if (e.key === "Enter") {
    // Never submit a surrounding form from the search box.
    e.preventDefault();
    if (open.value && items.value[active.value]) choose(items.value[active.value]);
  } else if (e.key === "Escape") {
    open.value = false;
  }
}

// Close shortly after blur so a click on an option still lands (same as CiPicker).
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
  <div class="field">
    <label :for="id">{{ label }}</label>
    <div class="combo">
      <input
        :id="id"
        type="search"
        role="combobox"
        :aria-expanded="open && !!q"
        :aria-controls="listId"
        aria-autocomplete="list"
        :aria-activedescendant="open && items[active] ? `${listId}-${active}` : undefined"
        autocomplete="off"
        :placeholder="placeholder"
        :disabled="disabled"
        :value="text"
        class="user-picker-input"
        @input="onInput"
        @focus="onFocus"
        @blur="onBlur"
        @keydown="onKeydown"
      />
      <ul v-if="open && q" :id="listId" class="combo-list" role="listbox" :aria-label="label">
        <li v-if="isError" class="note">{{ t("common.searchFailed") }}</li>
        <li v-else-if="items.length === 0" class="note">{{ isFetching ? t("common.searching") : t("groups.members.noUsers") }}</li>
        <li
          v-for="(u, i) in items"
          :id="`${listId}-${i}`"
          :key="u.id"
          role="option"
          :aria-selected="i === active"
          @mousedown.prevent="choose(u)"
          @mouseenter="active = i"
        >
          <span>{{ u.username }}</span>
          <span class="muted">{{ u.displayName }}</span>
          <span v-if="!u.isActive" class="muted">{{ t("common.disabled") }}</span>
        </li>
        <li v-if="more > 0" class="note">{{ t("common.moreResults", { n: more }) }}</li>
      </ul>
    </div>
    <span class="sr-only" aria-live="polite">{{ announcement }}</span>
  </div>
</template>
