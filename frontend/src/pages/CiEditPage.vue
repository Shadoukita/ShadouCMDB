<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { ApiError } from "../api/client";
import { useCi, useCiClasses, useClassAttributes } from "../api/queries";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import EditLayoutButton from "../components/layoutEdit/EditLayoutButton.vue";
import { useDocumentTitle } from "../lib/composables";
import { useLayoutEditor } from "../lib/layoutEditor";
import { useSessionStore } from "../stores/session";
import CiForm from "./form/CiForm.vue";

const route = useRoute();
const ci = useCi(() => String(route.params.id));
useDocumentTitle(() => (ci.data.value ? `Edit ${ci.data.value.label}` : "Edit CI"));
const c = computed(() => ci.data.value);
const session = useSessionStore();
// Edit layout (the layout-editor route, in its own window): the form's layout edited, with this CI's values.
const classes = useCiClasses();
const attrs = useClassAttributes(() => c.value?.classId);
const editor = useLayoutEditor({
  classKey: () => classes.data.value?.find((k) => k.id === c.value?.classId)?.key,
  attrs: () => attrs.data.value?.filter((d) => d.isActive),
  ciId: () => String(route.params.id),
});
const forbidden = computed(() => ci.error.value instanceof ApiError && ci.error.value.code === "FORBIDDEN");
</script>

<template>
  <LoadingState v-if="ci.isLoading.value" />
  <EmptyState v-else-if="forbidden" title="Permission denied">
    None of your permission profiles allows viewing this configuration item's class, so it cannot be edited.
    <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
  </EmptyState>
  <ErrorAlert v-else-if="ci.isError.value" :error="ci.error.value" :on-retry="() => ci.refetch()" />
  <EmptyState v-else-if="c && c.deletedAt" title="This configuration item is deleted">
    Deleted CIs cannot be edited.
    <template #actions><RouterLink :to="`/cis/${c.id}`">Back to the record</RouterLink></template>
  </EmptyState>
  <EmptyState v-else-if="c && !session.canOnClass(c.classId, 'edit')" title="Permission denied">
    None of your permission profiles allows editing {{ c.class.name }} configuration items.
    <template #actions><RouterLink :to="`/cis/${c.id}`">Back to the record</RouterLink></template>
  </EmptyState>
  <template v-else-if="c">
    <Breadcrumbs
      :items="[
        { label: 'Inventory', to: '/cis' },
        { label: c.class.name, to: `/cis?classId=${c.classId}` },
        { label: c.label, to: `/cis/${c.id}` },
        { label: 'Edit' },
      ]"
    />
    <div class="page-header">
      <div class="title">
        <h1>Edit {{ c.label }}</h1>
        <span class="muted">{{ c.class.name }} · <span class="mono">{{ c.ident }}</span> · version {{ c.version }}</span>
      </div>
      <div v-if="editor.allowed && !editor.active" class="actions">
        <EditLayoutButton :editor="editor" />
      </div>
    </div>
    <CiForm :key="`${c.id}-${c.version}`" mode="edit" :class-id="c.classId" :class-name="c.class.name" :ci="c" :editor="editor" />
  </template>
</template>
