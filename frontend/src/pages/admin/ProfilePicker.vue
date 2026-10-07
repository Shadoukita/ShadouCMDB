<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles } from "../../api/admin";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";

/** Checkbox list of permission profiles; a user's rights are the union of the profiles they hold. */
const model = defineModel<string[]>({ required: true });
defineProps<{ error?: string }>();
const profiles = useAllProfiles();
const session = useSessionStore();
const rows = computed(() => profiles.data.value?.data ?? []);
const truncated = computed(() => (profiles.data.value ? profiles.data.value.page.total > rows.value.length : false));

function toggle(id: string, on: boolean) {
  model.value = on ? [...model.value, id] : model.value.filter((p) => p !== id);
}
</script>

<template>
  <fieldset class="group" aria-describedby="profiles-hint">
    <legend>{{ t("admin.section.profiles") }}</legend>
    <p id="profiles-hint" class="muted profile-picker-hint">
      {{ t("admin.picker.hint") }}
      <RouterLink v-if="session.can('profiles.manage')" to="/admin/profiles">{{ t("admin.picker.manage") }}</RouterLink>
    </p>
    <LoadingState v-if="profiles.isLoading.value" :label="t('admin.profiles.loading')" />
    <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
    <ul v-else class="check-list">
      <li v-for="p in rows" :key="p.id">
        <label>
          <input type="checkbox" :checked="model.includes(p.id)" @change="toggle(p.id, ($event.target as HTMLInputElement).checked)" />
          <span>
            {{ p.name }} <span v-if="p.isBuiltin" class="badge">{{ t("admin.profiles.builtin") }}</span>
            <span v-if="p.description" class="hint">{{ p.description }}</span>
          </span>
        </label>
      </li>
    </ul>
    <p v-if="truncated" class="muted">{{ t("admin.picker.truncated", { n: rows.length }) }}</p>
    <span v-if="error" class="field error" role="alert">{{ error }}</span>
  </fieldset>
</template>
