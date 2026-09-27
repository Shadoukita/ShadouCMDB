<script setup lang="ts">
import { computed } from "vue";
import { ApiError } from "../api/client";

/** Human-readable explanation for any thrown error, with the API's details when present. */
const props = defineProps<{ error: unknown; title?: string; onRetry?: () => void }>();

const apiError = computed(() => (props.error instanceof ApiError ? props.error : null));
const heading = computed(() => props.title ?? headingFor(apiError.value));
/** Details that only repeat the message (a guard naming one field) add nothing. */
const details = computed(() => (apiError.value?.details ?? []).filter((d) => d.message !== apiError.value?.message));
const message = computed(() => {
  const e = props.error;
  return e instanceof Error ? e.message : String(e);
});

function headingFor(e: ApiError | null): string {
  if (!e) return "Something went wrong";
  switch (e.code) {
    case "NETWORK_ERROR":
      return "API unreachable";
    case "DATABASE_UNAVAILABLE":
      return "The CMDB database is unavailable";
    case "NOT_FOUND":
      return "Not found";
    case "VALIDATION_ERROR":
      return "The API rejected the request";
    case "VERSION_CONFLICT":
      return "Someone else changed this record";
    case "SCHEMA_CHANGE_REFUSED":
      return "The database change was refused";
    case "INVALID_NAME":
      return "The technical name cannot be used";
    case "CONFLICT":
    case "IN_USE":
      return "Conflict";
    case "FORBIDDEN":
    case "UNAUTHORIZED":
      return "Permission denied";
    default:
      return `Request failed (${e.status || e.code})`;
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
    <div v-if="apiError?.requestId || onRetry" class="meta">
      <template v-if="apiError?.requestId">Request id <code>{{ apiError.requestId }}</code>&#32;</template>
      <button v-if="onRetry" type="button" class="btn btn-sm" @click="onRetry()">Retry</button>
    </div>
  </div>
</template>
