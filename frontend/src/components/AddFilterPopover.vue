<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { t } from "../i18n";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import Icon from "./Icon.vue";
import InventoryFilters from "./InventoryFilters.vue";

/**
 * "Add filter" on the inventory (design document §0, step 12c): the class, criticality, validity and
 * deleted filters in a popover, so the toolbar shows the applied filters as chips instead of a row of
 * selects. The selects are the shared InventoryFilters with the same ids, bound to the same URL state.
 * Non-modal like the Columns popover: Esc or a click or Tab outside close it.
 */
defineProps<{ state: ReturnType<typeof useInventoryQueryState>; idPrefix: string }>();

const open = ref(false);
const root = ref<HTMLElement>();
const button = ref<HTMLButtonElement>();
const panel = ref<HTMLElement>();

async function show() {
  open.value = true;
  await nextTick();
  panel.value?.querySelector<HTMLElement>("select:not(:disabled)")?.focus();
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
function onFocusOut(e: FocusEvent) {
  const to = e.relatedTarget as Node | null;
  if (open.value && to && !root.value?.contains(to)) close(false);
}
function onDocClick(e: MouseEvent) {
  if (open.value && !root.value?.contains(e.target as Node)) close(false);
}
onMounted(() => document.addEventListener("click", onDocClick));
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));
defineExpose({ show });
</script>

<template>
  <div ref="root" class="add-filter" @keydown="onKeydown" @focusout="onFocusOut">
    <button
      ref="button"
      type="button"
      class="btn add-filter-button"
      aria-haspopup="dialog"
      :aria-controls="`${idPrefix}-add-filter`"
      :aria-expanded="open"
      @click="open ? close(false) : show()"
    >
      <Icon name="plus" :size="14" />{{ t("filters.add") }}
    </button>
    <div v-if="open" :id="`${idPrefix}-add-filter`" ref="panel" class="popover add-filter-popover" role="dialog" :aria-labelledby="`${idPrefix}-add-filter-title`">
      <h2 :id="`${idPrefix}-add-filter-title`" class="popover-title">{{ t("filters.add.title") }}</h2>
      <div class="add-filter-fields">
        <InventoryFilters :state="state" :id-prefix="idPrefix" />
      </div>
      <div class="popover-actions">
        <button type="button" class="btn btn-sm" @click="close(true)">{{ t("filters.add.done") }}</button>
      </div>
    </div>
  </div>
</template>
