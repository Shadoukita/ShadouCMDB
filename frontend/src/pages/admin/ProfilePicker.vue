<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles } from "../../api/admin";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
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
    <legend>Permission profiles</legend>
    <p id="profiles-hint" class="muted" style="margin: 0 0 var(--sp-3)">
      The user may do what any of their profiles allows. A user without a profile can sign in but sees no CI.
      <RouterLink v-if="session.can('profiles.manage')" to="/admin/profiles">Manage profiles</RouterLink>
    </p>
    <LoadingState v-if="profiles.isLoading.value" label="Loading profiles…" />
    <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
    <ul v-else class="check-list">
      <li v-for="p in rows" :key="p.id">
        <label>
          <input type="checkbox" :checked="model.includes(p.id)" @change="toggle(p.id, ($event.target as HTMLInputElement).checked)" />
          <span>
            {{ p.name }} <span v-if="p.isBuiltin" class="badge">Built-in</span>
            <span v-if="p.description" class="hint">{{ p.description }}</span>
          </span>
        </label>
      </li>
    </ul>
    <p v-if="truncated" class="muted">Only the first {{ rows.length }} profiles are listed.</p>
    <span v-if="error" class="field error" role="alert" style="color: var(--c-danger-text)">{{ error }}</span>
  </fieldset>
</template>
