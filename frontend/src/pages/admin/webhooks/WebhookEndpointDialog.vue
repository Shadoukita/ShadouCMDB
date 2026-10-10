<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ApiError } from "../../../api/client";
import {
  useCreateWebhookEndpoint,
  useUpdateWebhookEndpoint,
  type WebhookEndpoint,
  type WebhookEndpointCreateBody,
  type WebhookEndpointUpdateBody,
} from "../../../api/webhooks";
import { t } from "../../../i18n";
import { suggestKey } from "../../../lib/keys";
import { ENDPOINT_KEY_PATTERN, ENDPOINT_LIMITS, HEADER_NAME_PATTERN } from "../../../lib/outbound";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import WebhookSecretReveal from "./WebhookSecretReveal.vue";

/**
 * New or changed webhook endpoint (SHAA-2725 §5, §11.1). On create the server generates the signing secret
 * and answers it once: the dialog then shows it, and holds that only copy in its state until it closes.
 * The auth header's value is write-only: an edit keeps, replaces or removes it, never shows it.
 */
const props = defineProps<{ open: boolean; endpoint: WebhookEndpoint | null }>();
const emit = defineEmits<{ close: [] }>();
const create = useCreateWebhookEndpoint();
const update = useUpdateWebhookEndpoint();
const editing = computed(() => !!props.endpoint);
const mutation = computed(() => (editing.value ? update : create));

const dialog = ref<HTMLDialogElement>();
const nameInput = ref<HTMLInputElement>();
const name = ref("");
const key = ref("");
const keyTouched = ref(false);
const url = ref("");
const timeoutMs = ref("");
const maxPerMinute = ref("");
const maxInFlight = ref("");
/** `keep` and `remove` only when editing an endpoint that has a header. */
const headerMode = ref<"none" | "keep" | "set" | "remove">("none");
const headerName = ref("Authorization");
const headerValue = ref("");
const local = ref<Record<string, string>>({});
const created = ref<{ endpoint: WebhookEndpoint; secret: string } | null>(null);

watch(
  () => props.open,
  async (open) => {
    const d = dialog.value;
    if (!d) return;
    if (open) {
      reset();
      if (!d.open) d.showModal();
      await nextTick();
      nameInput.value?.focus();
    } else if (d.open) d.close();
  },
  { flush: "post" },
);

const num = (v: number | null | undefined) => (v == null ? "" : String(v));

function reset() {
  create.reset();
  update.reset();
  created.value = null;
  local.value = {};
  const e = props.endpoint;
  name.value = e?.name ?? "";
  key.value = e?.key ?? "";
  keyTouched.value = !!e;
  url.value = e?.url ?? "";
  timeoutMs.value = num(e?.timeoutMs);
  maxPerMinute.value = num(e?.maxPerMinute);
  maxInFlight.value = num(e?.maxInFlight);
  headerMode.value = e?.authHeaderSet ? "keep" : "none";
  headerName.value = e?.authHeaderName ?? "Authorization";
  headerValue.value = "";
}

watch(name, (n) => {
  if (!editing.value && !keyTouched.value) key.value = suggestKey(n).replace(/_/g, "-");
});

function close(e?: Event) {
  e?.preventDefault();
  if (mutation.value.isPending.value) return;
  created.value = null;
  emit("close");
}

const apiError = computed(() => (mutation.value.error.value instanceof ApiError ? mutation.value.error.value : null));
const apiErrors = computed(() => apiError.value?.fieldErrors() ?? {});
const errorFor = (field: string) => local.value[field] ?? apiErrors.value[field];
const PLACED = ["name", "key", "url", "timeoutMs", "maxPerMinute", "maxInFlight", "authHeader.name", "authHeader.value"];
const unplaced = computed(() => apiError.value?.details.filter((d) => !PLACED.includes(d.field)) ?? []);
const disabled = computed(() => apiError.value?.code === "WEBHOOKS_DISABLED");

function intOrNull(field: keyof typeof ENDPOINT_LIMITS, raw: string, errs: Record<string, string>): number | null {
  if (!raw.trim()) return null;
  const n = Number(raw);
  const lim = ENDPOINT_LIMITS[field];
  if (!Number.isInteger(n) || n < lim.min || n > lim.max) {
    errs[field] = t("webhooks.field.range", { min: lim.min, max: lim.max });
    return null;
  }
  return n;
}

function submit() {
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = t("common.required");
  if (!editing.value && !ENDPOINT_KEY_PATTERN.test(key.value)) errs.key = key.value ? t("webhooks.field.keyFormat") : t("common.required");
  if (!url.value.trim()) errs.url = t("common.required");
  else if (!/^https?:\/\//i.test(url.value.trim())) errs.url = t("webhooks.field.urlFormat");
  const limits = {
    timeoutMs: intOrNull("timeoutMs", timeoutMs.value, errs),
    maxPerMinute: intOrNull("maxPerMinute", maxPerMinute.value, errs),
    maxInFlight: intOrNull("maxInFlight", maxInFlight.value, errs),
  };
  if (headerMode.value === "set") {
    if (!HEADER_NAME_PATTERN.test(headerName.value.trim())) errs["authHeader.name"] = t("webhooks.field.headerNameFormat");
    if (!headerValue.value) errs["authHeader.value"] = t("common.required");
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) return;
  const header = headerMode.value === "set" ? { name: headerName.value.trim(), value: headerValue.value } : undefined;
  const e = props.endpoint;
  if (e) {
    // Only what changed: an unchanged URL is not judged again against an allowlist that may have moved on.
    const body: WebhookEndpointUpdateBody = { version: e.version };
    if (name.value.trim() !== e.name) body.name = name.value.trim();
    if (url.value.trim() !== e.url) body.url = url.value.trim();
    if (limits.timeoutMs !== (e.timeoutMs ?? null)) body.timeoutMs = limits.timeoutMs;
    if (limits.maxPerMinute !== (e.maxPerMinute ?? null)) body.maxPerMinute = limits.maxPerMinute;
    if (limits.maxInFlight !== (e.maxInFlight ?? null)) body.maxInFlight = limits.maxInFlight;
    if (header) body.authHeader = header;
    else if (headerMode.value === "remove") body.authHeader = null;
    update.mutate({ id: e.id, body }, { onSuccess: () => emit("close") });
    return;
  }
  const body: WebhookEndpointCreateBody = { key: key.value, name: name.value.trim(), url: url.value.trim(), ...limits, authHeader: header ?? null };
  create.mutate(body, {
    onSuccess: (res) => {
      created.value = { endpoint: res.endpoint, secret: res.secret };
      // Drop the mutation's copy of the answer, so the secret is held in one place only.
      create.reset();
      headerValue.value = "";
    },
  });
}
</script>

<template>
  <dialog ref="dialog" class="confirm form-dialog wide token-dialog" aria-labelledby="webhook-dialog-title" @cancel="close">
    <form v-if="!created" novalidate @submit.prevent="submit">
      <h2 id="webhook-dialog-title">{{ editing ? t("webhooks.edit.title", { name: endpoint?.name ?? "" }) : t("webhooks.new.title") }}</h2>
      <div v-if="open" class="body">
        <div v-if="disabled" class="alert alert-error" role="alert">
          <strong>{{ t("webhooks.disabled.title") }}</strong>
          <div>{{ t("webhooks.disabled.body") }}</div>
        </div>
        <FormErrorBanner v-else-if="mutation.error.value" :error="mutation.error.value" :unplaced="unplaced" />
        <div class="form-grid">
          <FormField id="wh-name" v-slot="p" :label="t('webhooks.field.name')" required :error="errorFor('name')">
            <input :id="p.id" ref="nameInput" v-model="name" type="text" maxlength="200" autocomplete="off" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="wh-key" v-slot="p" :label="t('webhooks.field.key')" :required="!editing" :error="errorFor('key')" :hint="editing ? t('webhooks.field.keyFixed') : t('webhooks.field.keyHint')">
            <input
              :id="p.id"
              v-model="key"
              class="mono"
              type="text"
              maxlength="63"
              autocomplete="off"
              spellcheck="false"
              :readonly="editing"
              :aria-invalid="p.invalid || undefined"
              :aria-describedby="p.describedBy"
              @input="keyTouched = true"
            />
          </FormField>
          <FormField id="wh-url" v-slot="p" :label="t('webhooks.field.url')" required wide :error="errorFor('url')" :hint="t('webhooks.field.urlHint')">
            <input
              :id="p.id"
              v-model="url"
              class="mono"
              type="url"
              maxlength="2048"
              autocomplete="off"
              spellcheck="false"
              placeholder="https://itsm.example.com/hooks/cmdb"
              :aria-invalid="p.invalid || undefined"
              :aria-describedby="p.describedBy"
            />
          </FormField>
          <FormField id="wh-timeout" v-slot="p" :label="t('webhooks.field.timeoutMs')" :error="errorFor('timeoutMs')" :hint="t('webhooks.field.defaultIs', { n: ENDPOINT_LIMITS.timeoutMs.default })">
            <input :id="p.id" v-model="timeoutMs" type="number" inputmode="numeric" :min="ENDPOINT_LIMITS.timeoutMs.min" :max="ENDPOINT_LIMITS.timeoutMs.max" step="500" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="wh-rate" v-slot="p" :label="t('webhooks.field.maxPerMinute')" :error="errorFor('maxPerMinute')" :hint="t('webhooks.field.defaultIs', { n: ENDPOINT_LIMITS.maxPerMinute.default })">
            <input :id="p.id" v-model="maxPerMinute" type="number" inputmode="numeric" :min="ENDPOINT_LIMITS.maxPerMinute.min" :max="ENDPOINT_LIMITS.maxPerMinute.max" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="wh-inflight" v-slot="p" :label="t('webhooks.field.maxInFlight')" :error="errorFor('maxInFlight')" :hint="t('webhooks.field.defaultIs', { n: ENDPOINT_LIMITS.maxInFlight.default })">
            <input :id="p.id" v-model="maxInFlight" type="number" inputmode="numeric" :min="ENDPOINT_LIMITS.maxInFlight.min" :max="ENDPOINT_LIMITS.maxInFlight.max" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="wh-header-mode" v-slot="p" :label="t('webhooks.field.authHeader')" wide :hint="t('webhooks.field.authHeaderHint')">
            <select :id="p.id" v-model="headerMode" :aria-describedby="p.describedBy">
              <option value="none" :disabled="!!endpoint?.authHeaderSet">{{ t("webhooks.header.none") }}</option>
              <option v-if="endpoint?.authHeaderSet" value="keep">{{ t("webhooks.header.keep", { name: endpoint.authHeaderName ?? "" }) }}</option>
              <option value="set">{{ endpoint?.authHeaderSet ? t("webhooks.header.replace") : t("webhooks.header.set") }}</option>
              <option v-if="endpoint?.authHeaderSet" value="remove">{{ t("webhooks.header.remove") }}</option>
            </select>
          </FormField>
          <template v-if="headerMode === 'set'">
            <FormField id="wh-header-name" v-slot="p" :label="t('webhooks.field.headerName')" required :error="errorFor('authHeader.name')">
              <input :id="p.id" v-model="headerName" class="mono" type="text" maxlength="64" autocomplete="off" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
            </FormField>
            <FormField id="wh-header-value" v-slot="p" :label="t('webhooks.field.headerValue')" required :error="errorFor('authHeader.value')" :hint="t('webhooks.field.headerValueHint')">
              <input :id="p.id" v-model="headerValue" class="mono" type="password" autocomplete="new-password" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
            </FormField>
          </template>
        </div>
      </div>
      <div class="footer">
        <button type="button" class="btn" :disabled="mutation.isPending.value" @click="close()">{{ t("common.cancel") }}</button>
        <button type="submit" class="btn btn-primary" :disabled="mutation.isPending.value">
          {{ mutation.isPending.value ? t("common.saving") : editing ? t("webhooks.edit.submit") : t("webhooks.new.submit") }}
        </button>
      </div>
    </form>

    <template v-else>
      <h2 id="webhook-dialog-title">{{ t("webhooks.new.createdTitle", { name: created.endpoint.name }) }}</h2>
      <div class="body stack">
        <WebhookSecretReveal :secret="created.secret" />
        <p class="muted no-margin">{{ t("webhooks.new.next") }}</p>
      </div>
      <div class="footer">
        <button type="button" class="btn btn-primary" @click="close()">{{ t("admin.token.done") }}</button>
      </div>
    </template>
  </dialog>
</template>
