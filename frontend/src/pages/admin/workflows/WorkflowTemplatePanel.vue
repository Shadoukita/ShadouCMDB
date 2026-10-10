<script setup lang="ts">
import { computed } from "vue";
import { useAllProfiles } from "../../../api/admin";
import { ApiError } from "../../../api/client";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { t } from "../../../i18n";
import { categoryLabel, type Draft } from "../../../lib/workflowDraft";
import type { StateValueOption } from "../../../lib/workflowTemplates";

/**
 * What a template creates, beside the new-workflow form: its states (with the state field value each is
 * kept in step with) and transitions, and the permission profiles that may run the transitions. Without
 * a granted profile only administrators could run them, which the lint reports; the Grants tab changes
 * them later. Listing profiles needs `profiles.manage` or `users.manage`; without it the grants are left
 * to the Grants tab.
 */
const props = defineProps<{ draft: Draft; stateValues: StateValueOption[] | null }>();
const profileIds = defineModel<string[]>("profiles", { required: true });

const profiles = useAllProfiles();
const cannotList = computed(() => profiles.error.value instanceof ApiError && profiles.error.value.status === 403);
const stateName = (key: string) => props.draft.states.find((s) => s.key === key)?.name ?? key;
const valueName = (key: string) => props.stateValues?.find((v) => v.key === key)?.name ?? key;

function toggle(id: string, on: boolean) {
  profileIds.value = on ? [...profileIds.value, id] : profileIds.value.filter((p) => p !== id);
}
</script>

<template>
  <section class="panel" aria-labelledby="wf-template-title" data-testid="wf-template-panel">
    <div class="panel-header"><h2 id="wf-template-title">{{ t("wfTemplate.preview.title") }}</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfTemplate.preview.intro") }}</p>
      <div class="table-wrap">
        <table class="data">
          <caption class="sr-only">{{ t("wfTemplate.preview.states") }}</caption>
          <thead>
            <tr>
              <th scope="col">{{ t("wfTemplate.preview.state") }}</th>
              <th scope="col">{{ t("wfTemplate.preview.category") }}</th>
              <th scope="col">{{ t("wfTemplate.preview.value") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in draft.states" :key="s.key">
              <td>
                {{ s.name }}
                <span v-if="s.key === draft.initialState" class="badge info">{{ t("wfTemplate.preview.initial") }}</span>
                <span v-if="s.terminal" class="badge">{{ t("wfTemplate.preview.final") }}</span>
              </td>
              <td>{{ categoryLabel(s.category) }}</td>
              <td v-if="!stateValues" class="muted">{{ t("wfAdmin.none") }}</td>
              <td v-else-if="s.stateValue">{{ valueName(s.stateValue) }} <span class="muted mono">({{ s.stateValue }})</span></td>
              <td v-else class="warn-text">{{ t("wfTemplate.preview.noMatch") }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="hint no-margin">{{ stateValues ? t("wfTemplate.preview.valueHint") : t("wfTemplate.preview.noStateField") }}</p>
      <div>
        <h3 class="subhead">{{ t("wfTemplate.preview.transitions") }}</h3>
        <ul class="plain-list">
          <li v-for="x in draft.transitions" :key="x.key">
            <strong>{{ x.name }}</strong>: {{ t("wfTemplate.preview.fromTo", { from: stateName(x.from), to: stateName(x.to) }) }}
          </li>
        </ul>
      </div>
      <fieldset class="group" aria-describedby="wf-template-grants-hint">
        <legend>{{ t("wfTemplate.grants.legend") }}</legend>
        <p id="wf-template-grants-hint" class="hint no-margin">{{ t("wfTemplate.grants.hint") }}</p>
        <p v-if="cannotList" class="muted no-margin">{{ t("wfTemplate.grants.cannotList") }}</p>
        <LoadingState v-else-if="profiles.isLoading.value" :label="t('admin.profiles.loading')" />
        <ErrorAlert v-else-if="profiles.isError.value" :error="profiles.error.value" :on-retry="() => profiles.refetch()" />
        <ul v-else class="check-list">
          <li v-for="p in profiles.data.value?.data ?? []" :key="p.id">
            <label>
              <input type="checkbox" :checked="profileIds.includes(p.id)" @change="toggle(p.id, ($event.target as HTMLInputElement).checked)" />
              <span dir="auto">{{ p.name }}</span>
            </label>
          </li>
        </ul>
      </fieldset>
    </div>
  </section>
</template>

<style scoped>
.warn-text {
  color: var(--c-warning-text);
}
.subhead {
  margin: 0 0 var(--space-1);
  font-size: var(--fs-sm);
}
.plain-list {
  margin: 0;
  padding-left: var(--space-5);
  font-size: var(--fs-sm);
}
</style>
