<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles } from "../../../api/admin";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { t } from "../../../i18n";

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

const groupHint = computed(() => (props.kind === "ldap" ? t("idp.mappings.ldapHint") : t("idp.mappings.oidcHint")));

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
      <h2 id="mappings-title">{{ t("idp.mappings.title") }}</h2>
      <button type="button" class="btn btn-sm mappings-add" @click="add"><Icon name="plus" />{{ t("idp.mappings.add") }}</button>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">
        {{ t("idp.mappings.body") }}
        <RouterLink to="/admin/profiles">{{ t("admin.section.profiles") }}</RouterLink>
      </p>
      <p class="hint no-margin">{{ groupHint }}</p>
      <LoadingState v-if="profiles.isLoading.value" :label="t('idp.mappings.loadingProfiles')" />
      <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
      <div v-if="errors.groupMappings" class="field"><span class="error" role="alert">{{ errors.groupMappings }}</span></div>
      <p v-if="rows.length === 0" class="alert alert-warn no-margin" role="status">{{ t("idp.mappings.none") }}</p>
      <div v-else class="table-wrap">
        <table class="data mappings">
          <thead>
            <tr>
              <th scope="col">{{ t("idp.mappings.group") }}</th>
              <th scope="col">{{ t("idp.mappings.profile") }}</th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
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
                  :aria-label="t('idp.mappings.groupOf', { n: i + 1 })"
                  :aria-invalid="!!errors[`groupMappings.${i}.group`]"
                  @input="set(i, { group: ($event.target as HTMLInputElement).value })"
                />
                <span v-if="errors[`groupMappings.${i}.group`]" class="error">{{ errors[`groupMappings.${i}.group`] }}</span>
              </td>
              <td>
                <select
                  :id="`mapping-${i}-profile`"
                  :value="r.profileId"
                  :aria-label="t('idp.mappings.profileOf', { n: i + 1 })"
                  :aria-invalid="!!errors[`groupMappings.${i}.profileId`]"
                  @change="set(i, { profileId: ($event.target as HTMLSelectElement).value })"
                >
                  <option value="" disabled>{{ t("idp.mappings.choose") }}</option>
                  <option v-for="p in options" :key="p.id" :value="p.id">{{ p.name }}</option>
                </select>
                <span v-if="errors[`groupMappings.${i}.profileId`]" class="error">{{ errors[`groupMappings.${i}.profileId`] }}</span>
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm btn-icon btn-quiet-danger" :aria-label="t('idp.mappings.removeOf', { n: i + 1 })" :title="t('idp.mappings.remove')" @click="remove(i)">
                  <Icon name="x" />
                </button>
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
.panel-header .mappings-add {
  margin-left: auto;
}
</style>
