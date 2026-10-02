<script setup lang="ts">
import { computed } from "vue";
import { ApiError } from "../api/client";
import { t } from "../i18n";

/** Human-readable explanation for any thrown error, with the API's details when present. */
const props = defineProps<{ error: unknown; title?: string; onRetry?: () => void }>();

const apiError = computed(() => (props.error instanceof ApiError ? props.error : null));
// Retrying a refused request cannot succeed until someone changes the user's profiles.
const retry = computed(() => (apiError.value?.code === "FORBIDDEN" ? undefined : props.onRetry));
const heading = computed(() => props.title ?? headingFor(apiError.value));
/** Details that only repeat the message (a guard naming one field) add nothing. */
const details = computed(() => (apiError.value?.details ?? []).filter((d) => d.message !== apiError.value?.message));
const message = computed(() => {
  const e = props.error;
  return e instanceof Error ? e.message : String(e);
});

function headingFor(e: ApiError | null): string {
  if (!e) return t("error.generic");
  switch (e.code) {
    case "NETWORK_ERROR":
      return t("error.network");
    case "DATABASE_UNAVAILABLE":
      return t("error.databaseUnavailable");
    case "IDENTITY_PROVIDER_UNAVAILABLE":
      return t("error.directoryUnavailable");
    case "SCHEMA_NOT_MIGRATED":
      return t("error.schemaNotMigrated");
    case "NOT_FOUND":
      return t("error.notFound");
    case "VALIDATION_ERROR":
      return t("error.validation");
    case "VERSION_CONFLICT":
      return t("error.versionConflict");
    case "SCHEMA_CHANGE_REFUSED":
      return t("error.schemaChangeRefused");
    case "INVALID_NAME":
      return t("error.invalidName");
    case "CONFLICT":
    case "IN_USE":
      return t("error.conflict");
    case "FORBIDDEN":
    case "UNAUTHORIZED":
      return t("error.forbidden");
    default:
      return t("error.requestFailed", { status: e.status || e.code });
  }
}
</script>

<template>
  <div class="alert alert-error" role="alert">
    <strong>{{ heading }}</strong>
    <div>{{ message }}</div>
    <ul v-if="details.length > 0">
      <li v-for="(d, i) in details" :key="i">
        <code v-if="d.field && d.field !== '(root)'">{{ d.field }}</code> {{ d.message }}
      </li>
    </ul>
    <div v-if="apiError?.requestId || retry" class="meta">
      <template v-if="apiError?.requestId">{{ t("error.requestId") }} <code>{{ apiError.requestId }}</code>&#32;</template>
      <button v-if="retry" type="button" class="btn btn-sm" @click="retry()">{{ t("common.retry") }}</button>
    </div>
  </div>
</template>
