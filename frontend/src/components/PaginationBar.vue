<script setup lang="ts">
import { computed } from "vue";
import { formatNumber, t } from "../i18n";

const SIZES = [25, 50, 100, 200];

const props = defineProps<{ total: number; limit: number; offset: number }>();
const emit = defineEmits<{ change: [next: { limit: number; offset: number }] }>();

/** A class's list view may set another page size (10-200); offer it too. */
const sizes = computed(() => (SIZES.includes(props.limit) ? SIZES : [...SIZES, props.limit].sort((a, b) => a - b)));
const from = computed(() => (props.total === 0 ? 0 : props.offset + 1));
const to = computed(() => Math.min(props.offset + props.limit, props.total));
const page = computed(() => Math.floor(props.offset / props.limit) + 1);
const pages = computed(() => Math.max(1, Math.ceil(props.total / props.limit)));
const go = (offset: number, limit = props.limit) => emit("change", { limit, offset });
</script>

<template>
  <div class="pagination">
    <span aria-live="polite">{{ t("pagination.range", { from: formatNumber(from), to: formatNumber(to), total: formatNumber(total) }) }}</span>
    <div class="actions">
      <label>
        {{ t("pagination.rows") }}
        <select :value="limit" @change="go(0, Number(($event.target as HTMLSelectElement).value))">
          <option v-for="s in sizes" :key="s" :value="s">{{ s }}</option>
        </select>
      </label>
      <button type="button" class="btn btn-sm" :disabled="offset === 0" @click="go(0)">{{ t("pagination.first") }}</button>
      <button type="button" class="btn btn-sm" :disabled="offset === 0" @click="go(Math.max(0, offset - limit))">{{ t("pagination.prev") }}</button>
      <span>{{ t("pagination.page", { page: formatNumber(page), pages: formatNumber(pages) }) }}</span>
      <button type="button" class="btn btn-sm" :disabled="offset + limit >= total" @click="go(offset + limit)">{{ t("pagination.next") }}</button>
      <button type="button" class="btn btn-sm" :disabled="offset + limit >= total" @click="go((pages - 1) * limit)">{{ t("pagination.last") }}</button>
    </div>
  </div>
</template>
