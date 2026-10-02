<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../../api/client";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t, tAround } from "../../i18n";

/** Top-of-form summary. Field errors render next to their fields; this lists what could not be placed. */
const props = defineProps<{ error: unknown; unplaced: { field: string; message: string }[]; versionConflictHref?: string }>();
const apiError = computed(() => (props.error instanceof ApiError ? props.error : null));
const reapply = computed(() => tAround("formError.reapply", "link"));
</script>

<template>
  <div v-if="apiError?.code === 'VERSION_CONFLICT'" class="alert alert-warn" role="alert">
    <strong>{{ t("formError.versionConflict") }}</strong>
    <div>
      {{ apiError.message }} {{ t("formError.notSavedSentence") }}
      <template v-if="versionConflictHref">
        {{ reapply[0] }}<RouterLink :to="versionConflictHref">{{ t("formError.openCurrent") }}</RouterLink>{{ reapply[1] }}
      </template>
    </div>
  </div>
  <div v-else-if="apiError?.code === 'VALIDATION_ERROR' || apiError?.code === 'CONFLICT' || apiError?.code === 'INVALID_NAME'" class="alert alert-error" role="alert">
    <strong>{{ t("formError.fixFields") }}</strong>
    <div>{{ apiError.message }}</div>
    <ul v-if="unplaced.length > 0">
      <li v-for="(d, i) in unplaced" :key="i">
        <code v-if="d.field && d.field !== '(root)'">{{ d.field }}</code> {{ d.message }}
      </li>
    </ul>
  </div>
  <ErrorAlert v-else :error="error" :title="t('formError.notSaved')" />
</template>
