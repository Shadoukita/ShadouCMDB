<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { ApiError } from "../../api/client";
import {
  normaliseCode,
  useConfirmTotp,
  useDisableTotp,
  useMfaStatus,
  useRegenerateRecoveryCodes,
  useStartTotp,
  type TotpEnrolment,
} from "../../api/mfa";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import QrCode from "../../components/QrCode.vue";
import { t } from "../../i18n";
import { vAutofocus } from "../../lib/directives";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";
import RecoveryCodes from "./RecoveryCodes.vue";

/**
 * The signed-in user's own two-factor authentication: set up an authenticator
 * app (password → scan → confirm → recovery codes), replace the recovery codes,
 * or turn it off. The secret and the codes live only in this component's state.
 */
const props = defineProps<{ forced?: boolean }>();
const emit = defineEmits<{ enrolled: [] }>();
const session = useSessionStore();
const status = useMfaStatus();
const start = useStartTotp();
const confirm = useConfirmTotp();
const disable = useDisableTotp();
const regenerate = useRegenerateRecoveryCodes();

type Mode = "idle" | "scan" | "disable" | "regenerate";
const mode = ref<Mode>("idle");
const password = ref("");
const code = ref("");
const enrolment = ref<TotpEnrolment | null>(null);
const codes = ref<string[] | null>(null);
/** Set after enrolment or new codes, until the user has saved the codes. */
const codesReason = ref<"enrolled" | "regenerated">("enrolled");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
const notice = ref<string | null>(null);

const enabled = computed(() => !!status.data.value?.totpEnabled);
const required = computed(() => !!status.data.value?.required);
const remaining = computed(() => status.data.value?.recoveryCodesRemaining ?? 0);
const busy = computed(() => start.isPending.value || confirm.isPending.value || disable.isPending.value || regenerate.isPending.value);
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
/** Errors the fields below do not show: a lockout, a conflict, a network failure. */
const generalError = computed(() => {
  const e = error.value;
  if (!e) return null;
  if (e instanceof ApiError && e.code === "VALIDATION_ERROR" && e.details.every((d) => ["currentPassword", "code"].includes(d.field))) return null;
  return e;
});
/** The account's identity provider: a directory account confirms with its directory password, an OIDC account has none here. */
const provider = computed(() => session.user?.identityProvider ?? null);
const directory = computed(() => provider.value?.kind === "ldap");
const passwordLabel = computed(() => (directory.value ? t("account.mfa.directoryPassword") : t("account.mfa.currentPassword")));
const passwordHint = computed(() => (directory.value ? t("account.mfa.directoryPasswordHint", { provider: provider.value!.name }) : undefined));
/** The secret in groups of four, as authenticator apps display and accept it. */
const groupedSecret = computed(() => enrolment.value?.secret.match(/.{1,4}/g)?.join(" ") ?? "");

function clearForm() {
  password.value = "";
  code.value = "";
  error.value = null;
  local.value = {};
}

function open(next: Mode) {
  clearForm();
  notice.value = null;
  mode.value = next;
}

function cancel() {
  clearForm();
  // An unconfirmed secret changes nothing at sign-in; the next set-up replaces it.
  enrolment.value = null;
  mode.value = "idle";
}

function need(fields: { password?: boolean; code?: boolean }) {
  const errs: Record<string, string> = {};
  if (fields.password && !password.value) errs.currentPassword = t("common.required");
  if (fields.code && !normaliseCode(code.value)) errs.code = t("common.required");
  local.value = errs;
  const first = Object.keys(errs)[0];
  if (first) document.getElementById(`mfa-${first}`)?.focus();
  return !first;
}

async function focus(id: string) {
  await nextTick();
  document.getElementById(id)?.focus();
}

async function begin() {
  error.value = null;
  if (!need({ password: true })) return;
  try {
    enrolment.value = await start.mutateAsync(password.value);
    start.reset(); // the answer holds the secret: keep one copy only, here
    clearForm();
    mode.value = "scan";
    await focus("mfa-code");
  } catch (e) {
    error.value = e;
    password.value = "";
    if (e instanceof ApiError && e.status === 409) {
      void status.refetch();
      // Set up in another browser meanwhile: the session's copy tells the set-up screen to ask for a sign-in with a code.
      if (props.forced) void session.refresh().catch(() => undefined);
    }
  }
}

async function finish() {
  error.value = null;
  if (!need({ code: true })) return;
  try {
    const result = await confirm.mutateAsync(normaliseCode(code.value));
    confirm.reset();
    codes.value = result.codes;
    codesReason.value = "enrolled";
    enrolment.value = null;
    clearForm();
    mode.value = "idle";
  } catch (e) {
    error.value = e;
    code.value = "";
  }
}

async function turnOff() {
  error.value = null;
  if (!need({ password: true, code: true })) return;
  try {
    await disable.mutateAsync({ currentPassword: password.value, code: normaliseCode(code.value) });
    clearForm();
    mode.value = "idle";
    notice.value = required.value ? t("account.mfa.offRequired") : t("account.mfa.offDone");
  } catch (e) {
    error.value = e;
    code.value = "";
  }
}

async function renewCodes() {
  error.value = null;
  if (!need({ password: true, code: true })) return;
  try {
    const result = await regenerate.mutateAsync({ currentPassword: password.value, code: normaliseCode(code.value) });
    regenerate.reset();
    codes.value = result.codes;
    codesReason.value = "regenerated";
    clearForm();
    mode.value = "idle";
  } catch (e) {
    error.value = e;
    code.value = "";
  }
}

function codesSaved() {
  const reason = codesReason.value;
  codes.value = null;
  notice.value = reason === "enrolled" ? t("account.mfa.onDone") : t("account.mfa.codesRenewed");
  if (reason === "enrolled") emit("enrolled");
}
</script>

<template>
  <section class="panel" aria-labelledby="mfa-title">
    <div class="panel-header">
      <h2 id="mfa-title">{{ t("account.mfa.title") }}</h2>
      <span v-if="status.data.value" class="badges">
        <span v-if="enabled" class="badge ok">{{ t("account.mfa.on") }}</span>
        <span v-else class="badge off">{{ t("account.mfa.off") }}</span>
        <span v-if="required" class="badge warn" :title="t('account.mfa.requiredTitle')">{{ t("account.mfa.required") }}</span>
      </span>
    </div>
    <LoadingState v-if="status.isLoading.value" :label="t('account.mfa.loading')" />
    <div v-else-if="status.isError.value" class="panel-body">
      <ErrorAlert :error="status.error.value" :on-retry="() => status.refetch()" />
    </div>
    <div v-else class="panel-body stack">
      <RecoveryCodes v-if="codes" :codes="codes" :username="session.user?.username ?? 'user'" @done="codesSaved" />

      <template v-else>
        <div v-if="notice" class="alert" role="status">{{ notice }}</div>
        <ErrorAlert v-if="generalError" :error="generalError" :title="t('account.mfa.failed')" />

        <!-- Off: set up an authenticator app -->
        <p v-if="!enabled && provider && !directory" class="muted flush">
          {{ t("account.mfa.viaProvider", { provider: provider.name }) }}
        </p>
        <template v-else-if="!enabled && mode !== 'scan'">
          <div v-if="forced" class="alert alert-warn" role="note">
            {{ t("account.mfa.forced") }}
          </div>
          <p class="muted flush">
            {{ t("account.mfa.intro") }}
          </p>
          <form class="stack" novalidate @submit.prevent="begin">
            <div class="form-grid">
              <FormField id="mfa-currentPassword" :label="passwordLabel" required :error="fieldErrors.currentPassword" :hint="passwordHint ?? t('account.mfa.confirmsItsYou')">
                <template #default="{ id, invalid, describedBy }">
                  <input :id="id" v-model="password" v-autofocus="forced" type="password" autocomplete="current-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
            <div><button type="submit" class="btn btn-primary" :disabled="busy">{{ start.isPending.value ? t("account.mfa.starting") : t("account.mfa.start") }}</button></div>
          </form>
        </template>

        <!-- Scan the QR code and confirm with a code -->
        <form v-else-if="!enabled && enrolment" class="stack" novalidate @submit.prevent="finish">
          <ol class="steps flush">
            <li>{{ t("account.mfa.step.add") }}</li>
            <li>{{ t("account.mfa.step.scan", { digits: enrolment.digits, period: enrolment.period }) }}</li>
            <li>{{ t("account.mfa.step.enter") }}</li>
          </ol>
          <div class="enrol">
            <QrCode :value="enrolment.otpauthUri" :label="t('account.mfa.qr')" />
            <div class="stack">
              <div class="field">
                <label for="mfa-secret">{{ t("account.mfa.setupKey") }}</label>
                <input id="mfa-secret" class="mono" type="text" readonly spellcheck="false" autocomplete="off" :value="groupedSecret" @focus="($event.target as HTMLInputElement).select()" />
                <span class="hint">{{ t("account.mfa.setupKeyHint") }}</span>
              </div>
              <FormField id="mfa-code" :label="t('account.mfa.codeFromApp')" required :error="fieldErrors.code">
                <template #default="{ id, invalid, describedBy }">
                  <input
                    :id="id"
                    v-model="code"
                    class="mono"
                    type="text"
                    inputmode="numeric"
                    autocomplete="one-time-code"
                    maxlength="10"
                    :aria-invalid="invalid"
                    :aria-describedby="describedBy"
                  />
                </template>
              </FormField>
              <div class="actions">
                <button type="submit" class="btn btn-primary" :disabled="busy">{{ confirm.isPending.value ? t("account.mfa.confirming") : t("account.mfa.confirm") }}</button>
                <button type="button" class="btn" :disabled="busy" @click="cancel">{{ t("common.cancel") }}</button>
              </div>
            </div>
          </div>
        </form>

        <!-- On -->
        <template v-else-if="enabled">
          <dl class="props">
            <dt>{{ t("account.mfa.app") }}</dt>
            <dd>{{ t("account.mfa.appSetUp") }}</dd>
            <dt>{{ t("account.mfa.recoveryCodes") }}</dt>
            <dd>
              <span :class="{ 'error-text': remaining <= 3 }">{{ t("account.mfa.unused", { n: remaining }) }}</span>
              <span v-if="remaining <= 3" class="muted">{{ t("account.mfa.runningOut") }}</span>
            </dd>
          </dl>
          <div v-if="mode === 'idle'" class="actions">
            <button type="button" class="btn" @click="open('regenerate')">{{ t("account.mfa.newCodes") }}</button>
            <button type="button" class="btn btn-danger" @click="open('disable')">{{ t("account.mfa.turnOff") }}</button>
          </div>
          <form v-else class="stack mfa-confirm" novalidate @submit.prevent="mode === 'disable' ? turnOff() : renewCodes()">
            <h3 class="flush">{{ mode === "disable" ? t("account.mfa.disableTitle") : t("account.mfa.regenerateTitle") }}</h3>
            <p class="flush">
              <template v-if="mode === 'disable'">
                {{ t("account.mfa.disableBody") }}
                <strong v-if="required">{{ t("account.mfa.disableRequired") }}</strong>
              </template>
              <template v-else>{{ t("account.mfa.regenerateBody", { n: remaining }) }}</template>
            </p>
            <div class="form-grid">
              <FormField id="mfa-currentPassword" :label="passwordLabel" required :error="fieldErrors.currentPassword" :hint="passwordHint">
                <template #default="{ id, invalid, describedBy }">
                  <input :id="id" v-model="password" v-autofocus type="password" autocomplete="current-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="mfa-code" :label="t('account.mfa.code')" required :error="fieldErrors.code" :hint="t('account.mfa.codeHint')">
                <template #default="{ id, invalid, describedBy }">
                  <input :id="id" v-model="code" class="mono" type="text" autocomplete="one-time-code" spellcheck="false" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
            <div class="actions">
              <button type="submit" :class="['btn', mode === 'disable' ? 'btn-danger' : 'btn-primary']" :disabled="busy">
                {{ busy ? t("account.mfa.working") : mode === "disable" ? t("account.mfa.disableTitle") : t("account.mfa.regenerateTitle") }}
              </button>
              <button type="button" class="btn" :disabled="busy" @click="cancel">{{ t("common.cancel") }}</button>
            </div>
          </form>
        </template>
      </template>
    </div>
  </section>
</template>

<style scoped>
.badges {
  display: inline-flex;
  gap: var(--sp-2);
}
.flush {
  margin: 0;
}
.steps {
  padding-left: 1.4em;
}
.enrol {
  display: flex;
  flex-wrap: wrap;
  gap: var(--sp-5);
  align-items: flex-start;
}
.enrol > .stack {
  flex: 1 1 260px;
  max-width: 420px;
}
.mfa-confirm {
  padding: var(--sp-3);
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
}
.mfa-confirm h3 {
  font-size: var(--fs-md);
}
.error-text {
  color: var(--c-danger-text);
  font-weight: 600;
}
</style>
