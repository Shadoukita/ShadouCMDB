<script setup lang="ts">
import type { UiWidget } from "../../api/uiSettings";
import { widgetLabel } from "../../lib/uiSettings";
import CiTableWidget from "./CiTableWidget.vue";
import CountWidget from "./CountWidget.vue";

/** The dashboard's widgets from Customization › Dashboard, in order; size sets the width (a third, half, full). */
defineProps<{ widgets: UiWidget[] }>();
</script>

<template>
  <div class="widgets">
    <div v-for="w in widgets" :key="w.id" :class="['widget', `widget-${w.size ?? 'medium'}`]" :data-widget="w.id">
      <CountWidget v-if="w.type.startsWith('count_by_')" :widget="w" :title="w.title || widgetLabel(w.type)" />
      <CiTableWidget v-else :widget="w" :title="w.title || widgetLabel(w.type)" />
    </div>
  </div>
</template>
