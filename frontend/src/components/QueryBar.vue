<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { useRoute } from "vue-router";
import { t, type MessageKey } from "../i18n";
import {
  accept as acceptSuggestion,
  barPatch,
  parseBar,
  sameAsUrl,
  segmentBar,
  serializeBar,
  suggest,
  type BarCatalogue,
  type BarError,
  type BarLabels,
  type Suggestion,
} from "../lib/queryBar";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import Icon from "./Icon.vue";

/**
 * The inventory's query bar (design document §2.7): the search term and the filters as one line of
 * `key:value` tokens, read from and written to the URL like every other control (lib/queryBar).
 * Typing applies after a pause, Enter at once; a text with an error is not applied, and the error
 * says why under the bar. Keys and values are offered from the catalogue as the operator types.
 *
 * The text is coloured by an overlay laid over the input (keys, separators, values, the negation
 * sign, and the tokens an error is reported on): the input's own glyphs are transparent, so the
 * caret, selection and native editing stay the input's, and the overlay only has to follow its
 * horizontal scroll. Both use the same font metrics with ligatures and kerning off (explorer.css),
 * which e2e/query-bar.spec.ts checks glyph run by glyph run.
 */
const props = defineProps<{ state: ReturnType<typeof useInventoryQueryState>; catalogue: BarCatalogue }>();

const route = useRoute();
const input = ref<HTMLInputElement>();
const overlay = ref<HTMLElement>();
const labels = computed<BarLabels>(() => ({
  class: t("filters.class"),
  criticality: t("filters.criticality"),
  validity: t("filters.validity"),
  "validity.active": t("filters.validity.active"),
  "validity.all": t("filters.validity.all"),
  "validity.inactive": t("filters.validity.inactive"),
  deleted: t("filters.deleted"),
  "deleted.hide": t("filters.deleted.hide"),
  "deleted.include": t("filters.deleted.include"),
  "deleted.only": t("filters.deleted.only"),
  ip: t("filters.ipWithin"),
  layout: t("filters.layout"),
  "layout.own": t("filters.layout.own"),
  "layout.default": t("filters.layout.classDefault"),
  template: t("filters.layoutTemplate"),
}));

const fromUrl = computed(() => serializeBar(route.query, props.catalogue));
const text = ref(fromUrl.value.text);
/** Typed since the text last matched the URL: the URL does not overwrite it while the bar has focus. */
const dirty = ref(false);
const focused = ref(false);

watch(
  () => fromUrl.value.text,
  (next) => {
    if (dirty.value && focused.value) return;
    const p = parseBar(text.value, props.catalogue);
    if (p.errors.length === 0 && !p.pending && sameAsUrl(route.query, barPatch(p, fromUrl.value.unrepresented))) return;
    text.value = next;
    dirty.value = false;
  },
);

/** The text last checked (after the pause or on Enter), and what was wrong with it. */
const checked = ref<string | null>(null);
const errors = computed<BarError[]>(() => (checked.value !== null && checked.value === text.value ? parseBar(checked.value, props.catalogue).errors : []));

function apply() {
  clearTimeout(timer);
  checked.value = text.value;
  const p = parseBar(text.value, props.catalogue);
  if (p.errors.length > 0 || p.pending) return;
  dirty.value = false;
  const patch = barPatch(p, fromUrl.value.unrepresented);
  if (!sameAsUrl(route.query, patch)) void props.state.update(patch);
}
let timer: ReturnType<typeof setTimeout> | undefined;
onBeforeUnmount(() => clearTimeout(timer));

// ---------- Syntax colouring ----------
const segments = computed(() => segmentBar(text.value, new Set(errors.value.map((e) => e.at))));
/** The overlay follows the input's horizontal scroll (a long query scrolls under the caret). */
function syncScroll() {
  if (input.value && overlay.value) overlay.value.scrollLeft = input.value.scrollLeft;
}
// After Vue has written a new text into both, and once more after the browser has scrolled the input to the caret.
watch(text, async () => {
  await nextTick();
  syncScroll();
  requestAnimationFrame(syncScroll);
});

// ---------- Suggestions (a combobox: focus stays in the input) ----------
const caret = ref(0);
const open = ref(false);
const active = ref(-1);
const suggestions = computed(() => (open.value ? suggest(text.value, caret.value, props.catalogue, labels.value) : null));
const expanded = computed(() => !!suggestions.value);
watch(suggestions, () => (active.value = -1));

function readCaret() {
  caret.value = input.value?.selectionStart ?? text.value.length;
  syncScroll();
}

function onInput(e: Event) {
  text.value = (e.target as HTMLInputElement).value;
  dirty.value = true;
  open.value = true;
  readCaret();
  clearTimeout(timer);
  timer = setTimeout(apply, 300);
}

async function take(item: Suggestion) {
  const s = suggestions.value;
  if (!s) return;
  const next = acceptSuggestion(text.value, s, item);
  text.value = next.text;
  dirty.value = true;
  caret.value = next.caret;
  await nextTick();
  input.value?.setSelectionRange(next.caret, next.caret);
  syncScroll();
  clearTimeout(timer);
  timer = setTimeout(apply, 300);
}

function onKeydown(e: KeyboardEvent) {
  const items = suggestions.value?.items ?? [];
  if (e.key === "ArrowDown" && items.length) {
    e.preventDefault();
    active.value = (active.value + 1) % items.length;
  } else if (e.key === "ArrowUp" && items.length) {
    e.preventDefault();
    active.value = active.value <= 0 ? items.length - 1 : active.value - 1;
  } else if (e.key === "Enter") {
    e.preventDefault();
    if (active.value >= 0 && items[active.value]) void take(items[active.value]!);
    else {
      open.value = false;
      apply();
    }
  } else if (e.key === "Escape" && expanded.value) {
    // Close the list only; a second Escape clears the field as any search box does.
    e.preventDefault();
    open.value = false;
  }
}

// Close shortly after blur so a click on an option still lands.
let closeTimer: ReturnType<typeof setTimeout> | undefined;
function onBlur() {
  focused.value = false;
  requestAnimationFrame(syncScroll);
  closeTimer = setTimeout(() => (open.value = false), 150);
}
function onFocus() {
  clearTimeout(closeTimer);
  focused.value = true;
  open.value = true;
  readCaret();
}
onBeforeUnmount(() => clearTimeout(closeTimer));

const errorText = (e: BarError) =>
  t(`queryBar.error.${e.code}` as MessageKey, { token: e.token, key: e.key, value: e.value ?? "", allowed: (e.allowed ?? []).join(", ") });
const describedBy = computed(() => ["f-q-help", errors.value.length ? "f-q-errors" : ""].filter(Boolean).join(" "));
</script>

<template>
  <div class="field search query-bar">
    <label for="f-q">{{ t("inventory.search") }}</label>
    <div class="combo input-icon query-input">
      <Icon name="search" />
      <input
        id="f-q"
        ref="input"
        type="search"
        role="combobox"
        aria-autocomplete="list"
        :aria-expanded="expanded"
        aria-controls="f-q-list"
        :aria-activedescendant="active >= 0 ? `f-q-opt-${active}` : undefined"
        :aria-describedby="describedBy"
        :aria-invalid="errors.length > 0 || undefined"
        autocomplete="off"
        spellcheck="false"
        :placeholder="t('queryBar.placeholder')"
        :value="text"
        @input="onInput"
        @keydown="onKeydown"
        @keyup="readCaret"
        @click="readCaret"
        @focus="onFocus"
        @blur="onBlur"
        @scroll="syncScroll"
        @select="syncScroll"
      />
      <!-- The coloured copy of the text (see above). Hidden from assistive technology: the input is the text. -->
      <div ref="overlay" class="query-overlay" aria-hidden="true"><span v-for="(s, i) in segments" :key="i" :class="[`qs-${s.kind}`, { 'qs-error': s.error }]">{{ s.text }}</span></div>
      <!-- Hover only highlights (CSS): if it moved `active`, a list opening under a resting pointer
           would choose what Enter takes instead of applying the text. -->
      <ul v-show="expanded" id="f-q-list" class="combo-list query-suggestions" role="listbox" :aria-label="t('queryBar.suggestions')">
        <li
          v-for="(s, i) in suggestions?.items ?? []"
          :id="`f-q-opt-${i}`"
          :key="s.insert"
          role="option"
          :aria-selected="i === active"
          @mousedown.prevent="take(s)"
        >
          <span class="mono">{{ s.label }}</span>
          <span class="query-suggestion-detail" dir="auto">{{ s.detail }}</span>
        </li>
      </ul>
    </div>
    <span id="f-q-help" class="sr-only">{{ t("queryBar.help") }}</span>
    <!-- Always in the page, so a screen reader hears the errors when they appear. -->
    <div aria-live="polite">
      <ul v-if="errors.length" id="f-q-errors" class="query-errors">
        <li v-for="(e, i) in errors" :key="i" class="error">{{ errorText(e) }}</li>
        <li class="hint">{{ t("queryBar.notApplied") }}</li>
      </ul>
    </div>
  </div>
</template>
