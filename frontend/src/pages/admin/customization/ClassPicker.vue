<script setup lang="ts">
import { computed, watchEffect } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useCiClasses, type CiClass } from "../../../api/queries";
import { t } from "../../../i18n";
import { flattenTree } from "../../../lib/tree";

/**
 * Picks the class a per-class section edits. The choice lives in the URL
 * (?class=<key>), so it survives a reload and switching sections. `ownLabel` marks a customized class
 * in the list ("own list view"), `ownCount` says how many there are; both come translated from the section.
 */
const props = defineProps<{ customized: string[]; ownLabel: string; ownCount: string }>();
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

function optionLabel(c: CiClass): string {
  const name = props.customized.includes(c.key) ? t("customization.classPicker.own", { name: c.name, own: props.ownLabel }) : c.name;
  return c.isActive ? name : t("customization.archivedName", { name });
}
</script>

<template>
  <div class="inline-control class-picker">
    <label for="cust-class">{{ t("customization.classPicker.class") }}</label>
    <select id="cust-class" :value="selectedKey" style="max-width: 360px" @change="pick(($event.target as HTMLSelectElement).value)">
      <option value="">{{ t("customization.classPicker.choose") }}</option>
      <option v-for="n in tree" :key="n.item.id" :value="n.item.key">
        {{ "  ".repeat(n.depth) }}{{ optionLabel(n.item) }}
      </option>
    </select>
    <span class="muted">{{ ownCount }}</span>
  </div>
  <p v-if="unknown.length > 0" class="hint">
    {{ t("customization.classPicker.unknown") }}
    <code v-for="k in unknown" :key="k" style="margin-right: 6px">{{ k }}</code>
  </p>
</template>
