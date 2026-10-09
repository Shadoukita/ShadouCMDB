<script setup lang="ts">
import { computed, onBeforeUnmount, ref, useId } from "vue";
import { ApiError } from "../api/client";
import { searchPrincipals, type Principal } from "../api/services";
import { t } from "../i18n";

/**
 * Picks a user or user group (GET /principals), as an ARIA 1.2 combobox: the input owns a listbox of
 * results through `aria-controls`, the highlighted option is `aria-activedescendant`, and the number of
 * results goes to a polite live region. The lookup starts at 2 characters and waits 250 ms after the
 * last keystroke. Up/Down move, Enter picks, Esc closes (or clears a closed box).
 */
const props = defineProps<{
  label: string;
  /** Ids already chosen: listed, but not selectable. */
  exclude?: string[];
  disabled?: boolean;
  /** Visible hint under the field, e.g. why it is disabled. */
  hint?: string;
  /** Only users, or only groups; both when left out. */
  kind?: Principal["kind"];
}>();
const emit = defineEmits<{ select: [principal: Principal]; forbidden: [] }>();
defineExpose({ focus: () => input.value?.focus() });

const MIN_CHARS = 2;
const DEBOUNCE_MS = 250;
const uid = useId();
const inputId = `principal-${uid}`;
const listId = `principal-list-${uid}`;
const hintId = `principal-hint-${uid}`;
const input = ref<HTMLInputElement>();
const text = ref("");
const results = ref<Principal[]>([]);
const open = ref(false);
const active = ref(-1);
const loading = ref(false);
const error = ref<string | null>(null);
/** What the live region says: the result count, "no match" or the minimum length. */
const status = ref("");
let timer: ReturnType<typeof setTimeout> | undefined;
let controller: AbortController | undefined;

const excluded = (p: Principal) => !!props.exclude?.includes(p.id);
const optionId = (i: number) => `${listId}-${i}`;
const activeId = computed(() => (open.value && active.value >= 0 ? optionId(active.value) : undefined));

function close() {
  open.value = false;
  active.value = -1;
}

function onInput() {
  clearTimeout(timer);
  controller?.abort();
  error.value = null;
  const q = text.value.trim();
  if (q.length < MIN_CHARS) {
    results.value = [];
    loading.value = false;
    close();
    status.value = q.length > 0 ? t("services.owners.minChars") : "";
    return;
  }
  loading.value = true;
  timer = setTimeout(() => void lookup(q), DEBOUNCE_MS);
}

async function lookup(q: string) {
  const c = new AbortController();
  controller = c;
  try {
    const found = (await searchPrincipals(q, c.signal)).filter((p) => !props.kind || p.kind === props.kind);
    if (c.signal.aborted) return;
    results.value = found;
    open.value = true;
    active.value = found.findIndex((p) => !excluded(p));
    status.value = found.length === 0 ? t("services.owners.noResults") : t("services.owners.results", { n: found.length });
  } catch (e) {
    if (c.signal.aborted) return;
    results.value = [];
    close();
    if (e instanceof ApiError && e.status === 403) emit("forbidden");
    error.value = e instanceof ApiError ? e.message : String(e);
    status.value = t("services.owners.searchFailed");
  } finally {
    if (controller === c) loading.value = false;
  }
}

function pick(i: number) {
  const p = results.value[i];
  if (!p || excluded(p)) return;
  emit("select", p);
  text.value = "";
  results.value = [];
  status.value = "";
  close();
  input.value?.focus();
}

function move(step: number) {
  const n = results.value.length;
  if (n === 0) return;
  if (!open.value) open.value = true;
  let i = active.value;
  for (let k = 0; k < n; k++) {
    i = (i + step + n) % n;
    if (!excluded(results.value[i])) break;
  }
  active.value = i;
  document.getElementById(optionId(i))?.scrollIntoView({ block: "nearest" });
}

function onKey(e: KeyboardEvent) {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    move(1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    move(-1);
  } else if (e.key === "Enter") {
    // Enter never submits the surrounding form from here: it picks the highlighted option or does nothing.
    e.preventDefault();
    if (open.value && active.value >= 0) pick(active.value);
  } else if (e.key === "Escape") {
    if (open.value) {
      e.preventDefault();
      close();
    } else if (text.value) {
      e.preventDefault();
      text.value = "";
      onInput();
    }
  }
}

onBeforeUnmount(() => {
  clearTimeout(timer);
  controller?.abort();
});
</script>

<template>
  <div class="field combobox">
    <label :for="inputId">{{ label }}</label>
    <input
      :id="inputId"
      ref="input"
      v-model="text"
      type="text"
      role="combobox"
      autocomplete="off"
      spellcheck="false"
      aria-autocomplete="list"
      :aria-expanded="open"
      :aria-controls="listId"
      :aria-activedescendant="activeId"
      :aria-describedby="hintId"
      :placeholder="t('services.owners.search')"
      :disabled="disabled"
      @input="onInput"
      @keydown="onKey"
      @blur="close"
    />
    <ul v-show="open" :id="listId" class="listbox" role="listbox" :aria-label="label">
      <li
        v-for="(p, i) in results"
        :id="optionId(i)"
        :key="`${p.kind}-${p.id}`"
        role="option"
        :aria-selected="i === active"
        :aria-disabled="excluded(p) || undefined"
        :class="{ active: i === active, excluded: excluded(p) }"
        @mousedown.prevent="pick(i)"
        @mousemove="!excluded(p) && (active = i)"
      >
        <bdi class="option-name">{{ p.displayName }}</bdi> <span v-if="p.username" class="muted mono">{{ p.username }}</span> <span class="badge">{{ p.kind === "group" ? t("services.owners.group") : t("services.owners.user") }}</span> <span v-if="!p.active" class="badge warn">{{ t("services.owners.disabled") }}</span> <span v-if="excluded(p)" class="muted">{{ t("services.owners.alreadySelected") }}</span>
      </li>
      <li v-if="results.length === 0" class="listbox-empty" role="presentation">{{ t("services.owners.noResults") }}</li>
    </ul>
    <span :id="hintId" class="hint">
      <template v-if="hint">{{ hint }}</template>
      <template v-else-if="error">{{ t("services.owners.searchFailed") }} {{ error }}</template>
      <template v-else-if="loading">{{ t("services.owners.searching") }}</template>
      <template v-else>{{ t("services.owners.minChars") }}</template>
    </span>
    <span class="sr-only" role="status" aria-live="polite">{{ status }}</span>
  </div>
</template>
