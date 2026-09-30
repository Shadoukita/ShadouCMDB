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
const passwordLabel = computed(() => (directory.value ? "Directory password" : "Current password"));
const passwordHint = computed(() => (directory.value ? `The password you sign in with, checked against ${provider.value!.name}` : undefined));
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
  if (fields.password && !password.value) errs.currentPassword = "Required";
  if (fields.code && !normaliseCode(code.value)) errs.code = "Required";
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
    notice.value = required.value
      ? "Two-factor authentication is off. A permission profile you hold requires it: set it up again to continue working."
      : "Two-factor authentication is off. Sign-in asks for your password only.";
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
  notice.value = reason === "enrolled" ? "Two-factor authentication is on. From now on sign-in asks for a code after your password." : "New recovery codes saved. The old ones no longer work.";
  if (reason === "enrolled") emit("enrolled");
}
</script>

<template>
  <section class="panel" aria-labelledby="mfa-title">
    <div class="panel-header">
      <h2 id="mfa-title">Two-factor authentication</h2>
      <span v-if="status.data.value" class="badges">
        <span v-if="enabled" class="badge ok">On</span>
        <span v-else class="badge off">Off</span>
        <span v-if="required" class="badge warn" title="A permission profile you hold requires two-factor authentication">Required</span>
      </span>
    </div>
    <LoadingState v-if="status.isLoading.value" label="Loading two-factor status…" />
    <div v-else-if="status.isError.value" class="panel-body">
      <ErrorAlert :error="status.error.value" :on-retry="() => status.refetch()" />
    </div>
    <div v-else class="panel-body stack">
      <RecoveryCodes v-if="codes" :codes="codes" :username="session.user?.username ?? 'user'" @done="codesSaved" />

      <template v-else>
        <div v-if="notice" class="alert" role="status">{{ notice }}</div>
        <ErrorAlert v-if="generalError" :error="generalError" title="Two-factor authentication not changed" />

        <!-- Off: set up an authenticator app -->
        <p v-if="!enabled && provider && !directory" class="muted flush">
          You sign in through {{ provider.name }}, which asks for your second factor. There is nothing to set up here.
        </p>
        <template v-else-if="!enabled && mode !== 'scan'">
          <div v-if="forced" class="alert alert-warn" role="note">
            A permission profile you hold requires two-factor authentication. Set up an authenticator app to continue.
          </div>
          <p class="muted flush">
            Protect your account with a second step at sign-in: a 6-digit code from an authenticator app on your phone
            (Microsoft Authenticator, Google Authenticator, 1Password, Aegis or any other TOTP app).
          </p>
          <form class="stack" novalidate @submit.prevent="begin">
            <div class="form-grid">
              <FormField id="mfa-currentPassword" :label="passwordLabel" required :error="fieldErrors.currentPassword" :hint="passwordHint ?? 'Confirms it is you'">
                <template #default="{ id, invalid, describedBy }">
                  <input :id="id" v-model="password" v-autofocus="forced" type="password" autocomplete="current-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
            <div><button type="submit" class="btn btn-primary" :disabled="busy">{{ start.isPending.value ? "Starting…" : "Set up authenticator app" }}</button></div>
          </form>
        </template>

        <!-- Scan the QR code and confirm with a code -->
        <form v-else-if="!enabled && enrolment" class="stack" novalidate @submit.prevent="finish">
          <ol class="steps flush">
            <li>Open your authenticator app and add an account.</li>
            <li>Scan this QR code, or enter the key by hand (time-based, {{ enrolment.digits }} digits, every {{ enrolment.period }} seconds).</li>
            <li>Enter the code the app shows to finish.</li>
          </ol>
          <div class="enrol">
            <QrCode :value="enrolment.otpauthUri" label="QR code to add ShadouCMDB to your authenticator app" />
            <div class="stack">
              <div class="field">
                <label for="mfa-secret">Setup key</label>
                <input id="mfa-secret" class="mono" type="text" readonly spellcheck="false" autocomplete="off" :value="groupedSecret" @focus="($event.target as HTMLInputElement).select()" />
                <span class="hint">Spaces do not matter.</span>
              </div>
              <FormField id="mfa-code" label="Code from the app" required :error="fieldErrors.code">
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
                <button type="submit" class="btn btn-primary" :disabled="busy">{{ confirm.isPending.value ? "Verifying…" : "Verify and turn on" }}</button>
                <button type="button" class="btn" :disabled="busy" @click="cancel">Cancel</button>
              </div>
            </div>
          </div>
        </form>

        <!-- On -->
        <template v-else-if="enabled">
          <dl class="props">
            <dt>Authenticator app</dt>
            <dd>Set up — sign-in asks for a code after your password.</dd>
            <dt>Recovery codes</dt>
            <dd>
              <span :class="{ 'error-text': remaining <= 3 }">{{ remaining }} unused</span>
              <span v-if="remaining <= 3" class="muted"> — create new ones before you run out.</span>
            </dd>
          </dl>
          <div v-if="mode === 'idle'" class="actions">
            <button type="button" class="btn" @click="open('regenerate')">New recovery codes</button>
            <button type="button" class="btn btn-danger" @click="open('disable')">Turn off</button>
          </div>
          <form v-else class="stack mfa-confirm" novalidate @submit.prevent="mode === 'disable' ? turnOff() : renewCodes()">
            <h3 class="flush">{{ mode === "disable" ? "Turn off two-factor authentication" : "Create new recovery codes" }}</h3>
            <p class="flush">
              <template v-if="mode === 'disable'">
                Removes your authenticator and your recovery codes; sign-in then asks for your password only.
                <strong v-if="required">A permission profile you hold requires two-factor authentication: you will have to set it up again before you can continue working.</strong>
              </template>
              <template v-else>You get 10 new codes; the {{ remaining }} you have now stop working.</template>
            </p>
            <div class="form-grid">
              <FormField id="mfa-currentPassword" :label="passwordLabel" required :error="fieldErrors.currentPassword" :hint="passwordHint">
                <template #default="{ id, invalid, describedBy }">
                  <input :id="id" v-model="password" v-autofocus type="password" autocomplete="current-password" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="mfa-code" label="Authentication code" required :error="fieldErrors.code" hint="From your app, or a recovery code">
                <template #default="{ id, invalid, describedBy }">
                  <input :id="id" v-model="code" class="mono" type="text" autocomplete="one-time-code" spellcheck="false" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
            <div class="actions">
              <button type="submit" :class="['btn', mode === 'disable' ? 'btn-danger' : 'btn-primary']" :disabled="busy">
                {{ busy ? "Working…" : mode === "disable" ? "Turn off two-factor authentication" : "Create new recovery codes" }}
              </button>
              <button type="button" class="btn" :disabled="busy" @click="cancel">Cancel</button>
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
  color: var(--c-danger);
  font-weight: 600;
}
</style>
