<script setup lang="ts">
import { computed, watchEffect } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useCiClasses, type CiClass } from "../../../api/queries";
import { flattenTree } from "../../../lib/tree";

/**
 * Picks the class a per-class section edits. The choice lives in the URL
 * (?class=<key>), so it survives a reload and switching sections.
 */
const props = defineProps<{ customized: string[]; noun: string }>();
const route = useRoute();
const router = useRouter();
const classes = useCiClasses();
const tree = computed(() => flattenTree(classes.data.value ?? []));
const selectedKey = computed(() => (typeof route.query.class === "string" ? route.query.class : ""));
const selected = defineModel<CiClass | undefined>("selected");
const known = computed(() => new Set((classes.data.value ?? []).map((c) => c.key)));
const unknown = computed(() => props.customized.filter((k) => !known.value.has(k)));

function pick(key: string) {
  router.replace({ query: { ...route.query, class: key || undefined } });
}
// Keep the parent's selected class in step with the URL.
const sync = computed(() => classes.data.value?.find((c) => c.key === selectedKey.value));
watchEffect(() => (selected.value = sync.value));
</script>

<template>
  <div class="inline-control class-picker">
    <label for="cust-class">Class</label>
    <select id="cust-class" :value="selectedKey" style="max-width: 360px" @change="pick(($event.target as HTMLSelectElement).value)">
      <option value="">Choose a class…</option>
      <option v-for="n in tree" :key="n.item.id" :value="n.item.key">
        {{ "  ".repeat(n.depth) }}{{ n.item.name }}{{ customized.includes(n.item.key) ? ` — own ${noun}` : "" }}{{ n.item.isActive ? "" : " (archived)" }}
      </option>
    </select>
    <span class="muted">{{ customized.length }} class{{ customized.length === 1 ? " has its" : "es have their" }} own {{ noun }}</span>
  </div>
  <p v-if="unknown.length > 0" class="hint">
    Also stored for classes that do not exist here (kept in case they come back, e.g. from an import):
    <code v-for="k in unknown" :key="k" style="margin-right: 6px">{{ k }}</code>
  </p>
</template>
