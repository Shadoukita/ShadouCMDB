<script setup lang="ts">
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
 * from the audit log for a caller with audit.view, otherwise the most recently updated CIs.
 */
defineProps<{ widgets: UiWidget[] }>();
const session = useSessionStore();
</script>

<template>
  <div class="widgets">
    <slot name="lead" />
    <div v-for="w in widgets" :key="w.id" :class="['widget', `widget-${w.size ?? 'medium'}`]" :data-widget="w.id">
      <CountWidget v-if="w.type.startsWith('count_by_')" :widget="w" :title="w.title || widgetLabel(w.type)" />
      <RecentActivity
        v-else-if="w.type === 'recent_changes' && session.can('audit.view')"
        :title="w.title || t('dashboard.widget.recentActivity')"
        :limit="w.limit ?? 10"
      />
      <CiTableWidget v-else :widget="w" :title="w.title || widgetLabel(w.type)" />
    </div>
  </div>
</template>
