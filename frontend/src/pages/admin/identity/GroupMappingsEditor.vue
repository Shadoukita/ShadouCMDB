<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles } from "../../../api/admin";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";

/** One row: users in `group` get `profileId`. `key` only keeps Vue's rows stable while editing. */
export interface MappingRow {
  key: number;
  group: string;
  profileId: string;
}

/**
 * The provider's groups and the permission profiles they grant. Saving replaces all mappings;
 * at every sign-in an account gets exactly the profiles its groups map to.
 */
const rows = defineModel<MappingRow[]>({ required: true });
const props = defineProps<{ kind: "oidc" | "ldap"; errors: Record<string, string> }>();
const profiles = useAllProfiles();
const options = computed(() => profiles.data.value?.data ?? []);
let next = Date.now();

const groupHint = computed(() =>
  props.kind === "ldap"
    ? "The group's full DN, as the directory lists it in memberOf, e.g. CN=CMDB Operators,OU=Groups,DC=example,DC=com"
    : "The value the provider sends in the groups claim: a group name, or an object id (Microsoft Entra ID).",
);

function add() {
  rows.value = [...rows.value, { key: next++, group: "", profileId: "" }];
}

function remove(i: number) {
  rows.value = rows.value.filter((_, j) => j !== i);
}

function set(i: number, patch: Partial<MappingRow>) {
  rows.value = rows.value.map((r, j) => (j === i ? { ...r, ...patch } : r));
}
</script>

<template>
  <section class="panel" aria-labelledby="mappings-title">
    <div class="panel-header">
      <h2 id="mappings-title">Group mappings</h2>
      <button type="button" class="btn btn-sm" @click="add">+ Add mapping</button>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">
        At every sign-in an account gets exactly the permission profiles its groups map to, and loses the others. A
        user in no mapped group cannot sign in. Groups are compared without regard to case.
        <RouterLink to="/admin/profiles">Permission profiles</RouterLink>
      </p>
      <p class="hint no-margin">{{ groupHint }}</p>
      <LoadingState v-if="profiles.isLoading.value" label="Loading profiles…" />
      <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
      <div v-if="errors.groupMappings" class="field"><span class="error" role="alert">{{ errors.groupMappings }}</span></div>
      <p v-if="rows.length === 0" class="alert alert-warn no-margin" role="status">
        No mappings yet: nobody can sign in through this provider until at least one group maps to a profile.
      </p>
      <div v-else class="table-wrap">
        <table class="data mappings">
          <thead>
            <tr>
              <th scope="col">Group</th>
              <th scope="col">Permission profile</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(r, i) in rows" :key="r.key">
              <td>
                <input
                  :id="`mapping-${i}-group`"
                  :value="r.group"
                  class="mono"
                  type="text"
                  spellcheck="false"
                  autocomplete="off"
                  :aria-label="`Group of mapping ${i + 1}`"
                  :aria-invalid="!!errors[`groupMappings.${i}.group`]"
                  @input="set(i, { group: ($event.target as HTMLInputElement).value })"
                />
                <span v-if="errors[`groupMappings.${i}.group`]" class="error">{{ errors[`groupMappings.${i}.group`] }}</span>
              </td>
              <td>
                <select
                  :id="`mapping-${i}-profile`"
                  :value="r.profileId"
                  :aria-label="`Permission profile of mapping ${i + 1}`"
                  :aria-invalid="!!errors[`groupMappings.${i}.profileId`]"
                  @change="set(i, { profileId: ($event.target as HTMLSelectElement).value })"
                >
                  <option value="" disabled>Choose a profile…</option>
                  <option v-for="p in options" :key="p.id" :value="p.id">{{ p.name }}</option>
                </select>
                <span v-if="errors[`groupMappings.${i}.profileId`]" class="error">{{ errors[`groupMappings.${i}.profileId`] }}</span>
              </td>
              <td class="num">
                <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="`Remove mapping ${i + 1}`" @click="remove(i)">Remove</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</template>

<style scoped>
.mappings input,
.mappings select {
  width: 100%;
}
.mappings td:first-child {
  width: 60%;
}
.mappings .error {
  display: block;
  font-size: var(--fs-sm);
  color: var(--c-danger-text);
}
</style>
