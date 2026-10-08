<script setup lang="ts">
import { computed, useSlots } from "vue";
import type { UiWidget } from "../../api/uiSettings";
import { t } from "../../i18n";
import { widgetLabel } from "../../lib/uiSettings";
import { useSessionStore } from "../../stores/session";
import CiTableWidget from "./CiTableWidget.vue";
import CountWidget from "./CountWidget.vue";
import RecentActivity from "./RecentActivity.vue";

/**
 * The dashboard's widgets from Customization › Dashboard, in order; size sets the width (a third, half, full).
 * The `lead` slot goes first in the same grid (the changes chart). "Recently changed" is the recent activity
 * from the audit log for a caller with audit.view, otherwise the most recently updated CIs. The `aside` slot
 * (the "Needs attention" panel) sits beside the first "Recently changed", which then takes two thirds of the
 * row, as in the mockup; without one it closes the grid.
 */
const props = defineProps<{ widgets: UiWidget[] }>();
const session = useSessionStore();
const slots = useSlots();
/** The widget the aside sits beside. */
const anchor = computed(() => (slots.aside ? props.widgets.find((w) => w.type === "recent_changes")?.id : undefined));
</script>

<template>
  <div class="widgets">
    <slot name="lead" />
    <template v-for="w in widgets" :key="w.id">
      <div :class="['widget', w.id === anchor ? 'widget-beside-aside' : `widget-${w.size ?? 'medium'}`]" :data-widget="w.id">
        <CountWidget v-if="w.type.startsWith('count_by_')" :widget="w" :title="w.title || widgetLabel(w.type)" />
        <RecentActivity
          v-else-if="w.type === 'recent_changes' && session.can('audit.view')"
          :title="w.title || t('dashboard.widget.recentActivity')"
          :limit="w.limit ?? 10"
        />
        <CiTableWidget v-else :widget="w" :title="w.title || widgetLabel(w.type)" />
      </div>
      <div v-if="w.id === anchor" class="widget widget-aside"><slot name="aside" /></div>
    </template>
    <div v-if="$slots.aside && !anchor" class="widget widget-medium"><slot name="aside" /></div>
  </div>
</template>
