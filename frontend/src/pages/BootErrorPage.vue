<script setup lang="ts">
import { computed } from "vue";
import BrandMark from "../components/BrandMark.vue";
import { t, type MessageKey } from "../i18n";
import { useDocumentTitle } from "../lib/composables";
import { asApiError, canRetry, errorHeading, errorMessage } from "../lib/errors";
import { useBrandingStore } from "../stores/branding";

// The API did not answer the first request of the page load, so nothing else can render: the sign-in card
// names the failure, quotes the API's message, offers Retry (a reload) and says who can fix it (step 10-8).
const props = defineProps<{ error: unknown }>();
const branding = useBrandingStore();
useDocumentTitle(() => t("boot.documentTitle"));

const apiError = computed(() => asApiError(props.error));
const heading = computed(() => errorHeading(apiError.value));
const message = computed(() => errorMessage(props.error));
const details = computed(() => (apiError.value?.details ?? []).filter((d) => d.message !== apiError.value?.message));
const hint = computed<MessageKey>(() => {
  switch (apiError.value?.code) {
    case "NETWORK_ERROR":
      return "boot.hint.network";
    case "DATABASE_UNAVAILABLE":
    case "SCHEMA_NOT_MIGRATED":
      return "boot.hint.server";
    default:
      return "boot.hint.generic";
  }
});

function retry() {
  window.location.reload();
}
</script>

<template>
  <main class="bare">
    <section class="bare-card" aria-labelledby="boot-title" data-testid="boot-error">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="boot-title">{{ heading }}</h1>
      <p class="lead">{{ t("boot.lead", { app: branding.effective.appName }) }}</p>
      <div class="alert alert-error" role="alert">
        <div>{{ message }}</div>
        <ul v-if="details.length > 0">
          <li v-for="(d, i) in details" :key="i">
            <code v-if="d.field && d.field !== '(root)'">{{ d.field }}</code> {{ d.message }}
          </li>
        </ul>
      </div>
      <button v-if="canRetry(apiError)" type="button" class="btn btn-primary block" @click="retry">{{ t("common.retry") }}</button>
      <div class="bare-foot">
        <p class="hint">{{ t(hint) }}</p>
        <p v-if="apiError?.requestId" class="hint" data-testid="boot-request-id">
          {{ t("error.requestId") }} <code class="mono">{{ apiError.requestId }}</code>
        </p>
      </div>
    </section>
  </main>
</template>
