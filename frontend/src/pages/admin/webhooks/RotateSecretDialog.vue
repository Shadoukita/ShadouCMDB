<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useRotateWebhookSecret, type WebhookEndpoint } from "../../../api/webhooks";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";
import { formatDateTime } from "../../../lib/format";
import { GRACE_HOURS } from "../../../lib/outbound";
import FormField from "../../form/FormField.vue";
import WebhookSecretReveal from "./WebhookSecretReveal.vue";

/**
 * Replace an endpoint's signing secret (SHAA-2725 §5.4): choose how long the old one is still sent next to
 * the new one, then the new secret is shown once. An endpoint suspended for `secret_required` becomes
 * paused, to be resumed once the receiver has the new secret.
 */
const props = defineProps<{ endpoint: WebhookEndpoint | null }>();
const emit = defineEmits<{ close: [] }>();
const rotate = useRotateWebhookSecret();
const dialog = ref<HTMLDialogElement>();
const select = ref<HTMLSelectElement>();
const graceHours = ref(24);
const rotated = ref<{ endpoint: WebhookEndpoint; secret: string } | null>(null);

watch(
  () => props.endpoint,
  async (e) => {
    const d = dialog.value;
    if (!d) return;
    if (e) {
      rotate.reset();
      rotated.value = null;
      graceHours.value = 24;
      if (!d.open) d.showModal();
      await nextTick();
      select.value?.focus();
    } else if (d.open) d.close();
  },
  { flush: "post" },
);

function close(e?: Event) {
  e?.preventDefault();
  if (rotate.isPending.value) return;
  rotated.value = null;
  emit("close");
}

const graceLabel = (h: number) => (h === 0 ? t("webhooks.rotate.graceNone") : t("webhooks.rotate.graceHours", { n: h }));
const graceUntil = computed(() => {
  const until = rotated.value?.endpoint.previousSecretUntil;
  return until ? t("webhooks.rotate.graceUntil", { when: formatDateTime(until) }) : null;
});

function submit() {
  const e = props.endpoint;
  if (!e) return;
  rotate.mutate(
    { id: e.id, graceHours: graceHours.value },
    {
      onSuccess: (res) => {
        rotated.value = { endpoint: res.endpoint, secret: res.secret };
        rotate.reset();
      },
    },
  );
}
</script>

<template>
  <dialog ref="dialog" class="confirm form-dialog token-dialog" aria-labelledby="rotate-dialog-title" @cancel="close">
    <form v-if="!rotated" novalidate @submit.prevent="submit">
      <h2 id="rotate-dialog-title">{{ t("webhooks.rotate.title", { name: endpoint?.name ?? "" }) }}</h2>
      <div v-if="endpoint" class="body stack">
        <ErrorAlert v-if="rotate.error.value" :error="rotate.error.value" :title="t('webhooks.rotate.failed')" />
        <p class="no-margin">{{ t("webhooks.rotate.body") }}</p>
        <FormField id="rotate-grace" v-slot="p" :label="t('webhooks.rotate.grace')" :hint="t('webhooks.rotate.graceHint')">
          <select :id="p.id" ref="select" v-model.number="graceHours" :aria-describedby="p.describedBy">
            <option v-for="h in GRACE_HOURS" :key="h" :value="h">{{ graceLabel(h) }}</option>
          </select>
        </FormField>
      </div>
      <div class="footer">
        <button type="button" class="btn" :disabled="rotate.isPending.value" @click="close()">{{ t("common.cancel") }}</button>
        <button type="submit" class="btn btn-primary" :disabled="rotate.isPending.value">
          {{ rotate.isPending.value ? t("webhooks.rotate.submitting") : t("webhooks.rotate.submit") }}
        </button>
      </div>
    </form>
    <template v-else>
      <h2 id="rotate-dialog-title">{{ t("webhooks.rotate.doneTitle", { name: rotated.endpoint.name }) }}</h2>
      <div class="body stack">
        <WebhookSecretReveal :secret="rotated.secret" :grace-until="graceUntil" />
        <p v-if="rotated.endpoint.status === 'paused'" class="muted no-margin">{{ t("webhooks.rotate.resumeNext") }}</p>
      </div>
      <div class="footer">
        <button type="button" class="btn btn-primary" @click="close()">{{ t("admin.token.done") }}</button>
      </div>
    </template>
  </dialog>
</template>
