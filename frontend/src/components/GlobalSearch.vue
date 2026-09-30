<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useSearch } from "../api/queries";
import { useDebounced } from "../lib/composables";

/**
 * Header search. Type-ahead shows the top ranked hits from GET /search;
 * Enter opens the full result page (/search?q=…). Press "/" anywhere to focus.
 */
const router = useRouter();
const route = useRoute();
const input = ref<HTMLInputElement>();
const text = ref("");
const open = ref(false);
const active = ref(-1);
const q = useDebounced(() => text.value.trim(), 200);
const { data, isFetching, isError } = useSearch(q, 8);
const hits = computed(() => (q.value ? (data.value?.data ?? []) : []));

function onGlobalKey(e: KeyboardEvent) {
  const t = e.target as HTMLElement;
  if (e.key === "/" && !["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName) && !t.isContentEditable) {
    e.preventDefault();
    input.value?.focus();
  }
}
onMounted(() => window.addEventListener("keydown", onGlobalKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onGlobalKey));

watch(() => route.fullPath, () => (open.value = false));

function go(path: string) {
  open.value = false;
  text.value = "";
  input.value?.blur();
  router.push(path);
}

function onSubmit() {
  const hit = active.value >= 0 ? hits.value[active.value] : undefined;
  if (hit) go(`/cis/${hit.item.id}`);
  else if (text.value.trim()) go(`/search?q=${encodeURIComponent(text.value.trim())}`);
}

function onInput(e: Event) {
  clearTimeout(closeTimer);
  text.value = (e.target as HTMLInputElement).value;
  open.value = true;
  active.value = -1;
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    active.value = Math.min(active.value + 1, hits.value.length - 1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    active.value = Math.max(active.value - 1, -1);
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
  <form class="global-search combo" role="search" @submit.prevent="onSubmit">
    <label for="global-search" class="sr-only">Search configuration items</label>
    <input
      id="global-search"
      ref="input"
      type="search"
      role="combobox"
      :aria-expanded="open && !!q"
      aria-controls="global-search-list"
      :aria-activedescendant="active >= 0 ? `gs-${active}` : undefined"
      autocomplete="off"
      placeholder="Search CIs by label, ident, IP, attribute…  ( / )"
      :value="text"
      @input="onInput"
      @focus="onFocus"
      @blur="onBlur"
      @keydown="onKeydown"
    />
    <ul v-if="open && q" id="global-search-list" class="combo-list" role="listbox" style="width: 100%">
      <li v-if="isError" class="note">Search failed — press Enter to see the error</li>
      <li v-else-if="hits.length === 0" class="note">{{ isFetching ? "Searching…" : `No CI matches “${q}”` }}</li>
      <li
        v-for="(h, i) in hits"
        :id="`gs-${i}`"
        :key="h.item.id"
        role="option"
        :aria-selected="i === active"
        @mousedown.prevent="go(`/cis/${h.item.id}`)"
        @mouseenter="active = i"
      >
        <strong dir="auto">{{ h.item.label }}</strong>
        <span class="muted" dir="auto">{{ h.item.class.name }}</span>
        <span v-if="h.matches[0]" class="muted">
          <bdi>{{ h.matches[0].label }}</bdi>: <span class="mono" dir="auto">{{ h.matches[0].value }}</span>
        </span>
      </li>
      <li
        v-if="data && data.page.total > hits.length"
        class="note"
        style="cursor: pointer"
        @mousedown.prevent="go(`/search?q=${encodeURIComponent(q)}`)"
      >
        See all {{ data.page.total.toLocaleString() }} results ↵
      </li>
    </ul>
  </form>
</template>
