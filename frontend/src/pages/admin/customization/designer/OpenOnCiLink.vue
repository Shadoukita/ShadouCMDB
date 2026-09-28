<script setup lang="ts">
import { computed } from "vue";
import { useRouter } from "vue-router";
import { useCiList, type CiClass } from "../../../../api/queries";
import { openLayoutEditor } from "../../../../lib/layoutEditor";

/**
 * "Open on a CI": the class's layout editor in its own window, on a real page:
 * its first CI by label, or an empty form of the class when it has none yet.
 */
const props = defineProps<{ cls: CiClass }>();
const first = useCiList(() => ({ classId: props.cls.id, limit: 1, sort: "label" }));
const ci = computed(() => first.data.value?.data[0]);
const router = useRouter();
const open = () =>
  openLayoutEditor(router, ci.value ? { path: `/cis/${ci.value.id}` } : { path: "/cis/new", query: { classId: props.cls.id } }, props.cls.key);
</script>

<template>
  <span class="inline-control">
    <button v-if="!first.isLoading.value" type="button" class="btn" title="Opens the layout editor in a new window" @click="open">Open on a CI</button>
    <span class="muted">
      <template v-if="first.isLoading.value">Looking for a {{ cls.name }} CI…</template>
      <template v-else-if="ci">Edit the saved layout on {{ ci.label }}, in a new window</template>
      <template v-else>No {{ cls.name }} CI yet: edit the saved layout on an empty form, in a new window</template>
    </span>
  </span>
</template>
