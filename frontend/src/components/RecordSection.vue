<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import Icon from "./Icon.vue";

/**
 * One section of a CI's layout on the record page and the CI form (audit R4): a panel with its heading.
 * Only a section the layout marks collapsed can be opened and closed; its heading is then a disclosure
 * button. Every other section is a plain panel with no toggle, so nothing looks collapsible that is not.
 * `forceOpen` shows the body whatever the toggle says, e.g. while a field in it has an error.
 */
const props = defineProps<{ label: string; collapsed?: boolean; forceOpen?: boolean }>();
const open = ref(!props.collapsed);
// The layout can change under the page (another template chosen): follow it.
watch(
  () => props.collapsed,
  (c) => (open.value = !c),
);
const shown = computed(() => open.value || !!props.forceOpen);
const bodyId = useId();
</script>

<template>
  <section :class="['panel', 'layout-panel', { 'is-collapsed': !shown }]">
    <div class="panel-header">
      <h2 v-if="!collapsed" dir="auto">{{ label }}</h2>
      <h2 v-else>
        <button type="button" class="section-toggle" :aria-expanded="shown" :aria-controls="bodyId" @click="open = !shown">
          <Icon :name="shown ? 'chevron-down' : 'chevron-right'" :size="14" />
          <span dir="auto">{{ label }}</span>
        </button>
      </h2>
    </div>
    <div v-show="shown" :id="bodyId" class="layout-panel-body">
      <slot />
    </div>
  </section>
</template>
