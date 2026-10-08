<script setup lang="ts">
import { computed } from "vue";
import { formatNumber, t } from "../i18n";
import { pageItems } from "../lib/pagination";
import Icon from "./Icon.vue";

const SIZES = [25, 50, 100, 200];

/**
 * The range, the page size and the page controls under a list. `numbered` (the inventory, design document
 * §0 step 12c) shows ‹ 1 2 3 … n › instead of First / Prev / Page x of y / Next / Last.
 */
const props = defineProps<{ total: number; limit: number; offset: number; numbered?: boolean }>();
const emit = defineEmits<{ change: [next: { limit: number; offset: number }] }>();

/** A class's list view may set another page size (10-200); offer it too. */
const sizes = computed(() => (SIZES.includes(props.limit) ? SIZES : [...SIZES, props.limit].sort((a, b) => a - b)));
const from = computed(() => (props.total === 0 ? 0 : props.offset + 1));
const to = computed(() => Math.min(props.offset + props.limit, props.total));
const page = computed(() => Math.floor(props.offset / props.limit) + 1);
const pages = computed(() => Math.max(1, Math.ceil(props.total / props.limit)));
const items = computed(() => pageItems(page.value, pages.value));
const go = (offset: number, limit = props.limit) => emit("change", { limit, offset });
</script>

<template>
  <div :class="['pagination', { numbered }]">
    <span aria-live="polite">{{ t("pagination.range", { from: formatNumber(from), to: formatNumber(to), total: formatNumber(total) }) }}</span>
    <div class="actions">
      <label>
        {{ t("pagination.rows") }}
        <select :value="limit" @change="go(0, Number(($event.target as HTMLSelectElement).value))">
          <option v-for="s in sizes" :key="s" :value="s">{{ s }}</option>
        </select>
      </label>
      <nav v-if="numbered" class="page-numbers" :aria-label="t('pagination.pages')">
        <button type="button" class="btn btn-sm btn-icon" :disabled="offset === 0" :aria-label="t('pagination.prevPage')" @click="go(Math.max(0, offset - limit))">
          <Icon name="chevron-left" />
        </button>
        <template v-for="(p, i) in items" :key="p === 'gap' ? `gap-${i}` : p">
          <span v-if="p === 'gap'" class="page-gap" aria-hidden="true">…</span>
          <button
            v-else
            type="button"
            :class="['btn', 'btn-sm', 'page-number', { current: p === page }]"
            :aria-label="t('pagination.pageN', { page: formatNumber(p) })"
            :aria-current="p === page ? 'page' : undefined"
            @click="p !== page && go((p - 1) * limit)"
          >
            {{ formatNumber(p) }}
          </button>
        </template>
        <button type="button" class="btn btn-sm btn-icon" :disabled="offset + limit >= total" :aria-label="t('pagination.nextPage')" @click="go(offset + limit)">
          <Icon name="chevron-right" />
        </button>
      </nav>
      <template v-else>
        <button type="button" class="btn btn-sm" :disabled="offset === 0" @click="go(0)">{{ t("pagination.first") }}</button>
        <button type="button" class="btn btn-sm" :disabled="offset === 0" @click="go(Math.max(0, offset - limit))">{{ t("pagination.prev") }}</button>
        <span>{{ t("pagination.page", { page: formatNumber(page), pages: formatNumber(pages) }) }}</span>
        <button type="button" class="btn btn-sm" :disabled="offset + limit >= total" @click="go(offset + limit)">{{ t("pagination.next") }}</button>
        <button type="button" class="btn btn-sm" :disabled="offset + limit >= total" @click="go((pages - 1) * limit)">{{ t("pagination.last") }}</button>
      </template>
    </div>
  </div>
</template>
