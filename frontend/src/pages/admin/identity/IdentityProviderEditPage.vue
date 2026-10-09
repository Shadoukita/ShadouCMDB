<script setup lang="ts">
import { t } from "../../../i18n";
import { adminCrumbs } from "../sections";
import { computed, nextTick, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import {
  useCreateIdentityProvider,
  useIdentityProvider,
  useUpdateIdentityProvider,
  type IdentityProvider,
  type IdentityProviderCreateBody,
  type IdentityProviderUpdateBody,
  type MfaAssurance,
  type ProviderKind,
} from "../../../api/identityProviders";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import SaveBar from "../../../components/SaveBar.vue";
import { useDocumentTitle, useUnsavedGuard } from "../../../lib/composables";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { useFlashStore } from "../../../stores/flash";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import GroupMappingsEditor, { type MappingRow } from "./GroupMappingsEditor.vue";
import ProviderAccessPanel from "./ProviderAccessPanel.vue";
import ProviderTestPanel from "./ProviderTestPanel.vue";
import { providerKindLabel, reentryHint } from "./providerText";
import SecretInput from "./SecretInput.vue";
import { secretMissing, secretReentryField, secretRequiredFields, syncSecret, type SecretField } from "./secretReentry";

/**
 * Create or edit an identity provider: an OpenID Connect provider (a "Sign in with …" button) or an
 * LDAP / Active Directory directory (its users sign in with the password form). Secrets are
 * write-only; group mappings decide which permission profiles its users get. Title row, `⋯` menu and
 * save bar as on the other admin edit pages (design §2.7, audit A3).
 */
const route = useRoute();
const router = useRouter();
const flash = useFlashStore();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const provider = useIdentityProvider(id);
const create = useCreateIdentityProvider();
const update = useUpdateIdentityProvider();
const pending = computed(() => create.isPending.value || update.isPending.value);
useDocumentTitle(() => (isNew.value ? t("idp.edit.new") : provider.data.value?.name));

interface Form {
  kind: ProviderKind;
  name: string;
  isEnabled: boolean;
  sortOrder: string;
  caCertificate: string;
  issuerUrl: string;
  clientId: string;
  clientSecret: string | null | undefined;
  scopes: string;
  usernameClaim: string;
  groupsClaim: string;
  mfaAssurance: MfaAssurance;
  /** requiredAcr as typed: values separated by spaces or line breaks */
  requiredAcr: string;
  url: string;
  bindDn: string;
  bindPassword: string | null | undefined;
  userBaseDn: string;
  userFilter: string;
  usernameAttribute: string;
  displayNameAttribute: string;
  emailAttribute: string;
  groupAttribute: string;
  mappings: MappingRow[];
}

let rowKey = 0;
/** A new provider starts with the server's defaults filled in, so the administrator sees what applies. */
const blank = (kind: ProviderKind = "oidc"): Form => ({
  kind,
  name: "",
  isEnabled: true,
  sortOrder: "0",
  caCertificate: "",
  issuerUrl: "",
  clientId: "",
  clientSecret: undefined,
  scopes: "profile email",
  usernameClaim: "preferred_username",
  groupsClaim: "groups",
  mfaAssurance: "verify",
  requiredAcr: "",
  url: "",
  bindDn: "",
  bindPassword: undefined,
  userBaseDn: "",
  userFilter: "(&(objectClass=user)(sAMAccountName={username}))",
  usernameAttribute: "sAMAccountName",
  displayNameAttribute: "displayName",
  emailAttribute: "mail",
  groupAttribute: "memberOf",
  mappings: [],
});
const fromProvider = (p: IdentityProvider): Form => ({
  ...blank(p.kind),
  name: p.name,
  isEnabled: p.isEnabled,
  sortOrder: String(p.sortOrder),
  caCertificate: p.caCertificate ?? "",
  ...(p.oidc && {
    issuerUrl: p.oidc.issuerUrl,
    clientId: p.oidc.clientId,
    scopes: p.oidc.scopes,
    usernameClaim: p.oidc.usernameClaim,
    groupsClaim: p.oidc.groupsClaim,
    mfaAssurance: p.oidc.mfaAssurance,
    requiredAcr: p.oidc.requiredAcr.join(" "),
  }),
  ...(p.ldap && {
    url: p.ldap.url,
    bindDn: p.ldap.bindDn ?? "",
    userBaseDn: p.ldap.userBaseDn,
    userFilter: p.ldap.userFilter,
    usernameAttribute: p.ldap.usernameAttribute,
    displayNameAttribute: p.ldap.displayNameAttribute,
    emailAttribute: p.ldap.emailAttribute,
    groupAttribute: p.ldap.groupAttribute,
  }),
  mappings: p.groupMappings.map((m) => ({ key: rowKey++, group: m.group, profileId: m.profileId })),
});

const initialKind = (): ProviderKind => (route.query.kind === "ldap" ? "ldap" : "oidc");
const form = ref<Form>(blank(initialKind()));
/** The form as last loaded or saved, to tell whether there are unsaved changes (the connection test uses saved settings). */
const baseline = ref(JSON.stringify(form.value));
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
const copyState = ref<"" | "copied" | "failed">("");
/** Secrets the API asked for again (422 secret_required), whatever the form thinks; until the next load or save. */
const apiRequired = ref<SecretField[]>([]);

function seed(f: Form) {
  form.value = f;
  baseline.value = JSON.stringify(f);
  apiRequired.value = [];
}
watch(
  () => provider.data.value,
  (p) => p && seed(fromProvider(p)),
  { immediate: true },
);
watch(id, () => {
  if (!id.value) seed(blank(initialKind()));
  error.value = null;
  local.value = {};
});
/** The fields that differ from the form as loaded or saved (group mappings count as one). */
const changes = computed(() => {
  const now = form.value as unknown as Record<string, unknown>;
  const before = JSON.parse(baseline.value) as Record<string, unknown>;
  return Object.keys(now).filter((k) => JSON.stringify(now[k]) !== JSON.stringify(before[k])).length;
});
const dirty = computed(() => changes.value > 0);
const guard = useUnsavedGuard(() => dirty.value, () => t("admin.unsaved.leave"));

/** A changed server address sends the stored secret nowhere: it has to be entered again (GH#238). */
const reentry = computed(() => secretReentryField(provider.data.value, form.value));
const secretRequired = (field: SecretField) => reentry.value === field || apiRequired.value.includes(field);
const SECRET_KEYS = { "oidc.clientSecret": "clientSecret", "ldap.bindPassword": "bindPassword" } as const;
watch(
  () => `${secretRequired("oidc.clientSecret")} ${secretRequired("ldap.bindPassword")}`,
  () => {
    for (const [field, key] of Object.entries(SECRET_KEYS) as [SecretField, (typeof SECRET_KEYS)[SecretField]][]) {
      form.value[key] = syncSecret(form.value[key], secretRequired(field));
    }
  },
);

const p = computed(() => provider.data.value);
/** The issuer or directory URL, shown in mono on the head band's meta line. */
const endpoint = computed(() => p.value?.oidc?.issuerUrl ?? p.value?.ldap?.url ?? "");
const isOidc = computed(() => form.value.kind === "oidc");
/** StartTLS follows the URL's scheme (the API refuses the other combinations): shown, never asked. */
const transport = computed(() => {
  const u = form.value.url.trim().toLowerCase();
  if (u.startsWith("ldaps://")) return t("idp.ldap.transportLdaps");
  if (u.startsWith("ldap://")) return t("idp.ldap.transportStartTls");
  return t("idp.ldap.transportHint");
});

const FIELDS = [
  "name",
  "isEnabled",
  "sortOrder",
  "caCertificate",
  "oidc.issuerUrl",
  "oidc.clientId",
  "oidc.clientSecret",
  "oidc.scopes",
  "oidc.usernameClaim",
  "oidc.groupsClaim",
  "oidc.mfaAssurance",
  "oidc.requiredAcr",
  "ldap.url",
  "ldap.startTls",
  "ldap.bindDn",
  "ldap.bindPassword",
  "ldap.userBaseDn",
  "ldap.userFilter",
  "ldap.usernameAttribute",
  "ldap.displayNameAttribute",
  "ldap.emailAttribute",
  "ldap.groupAttribute",
];
const fieldErrors = computed<Record<string, string>>(() => {
  const api = error.value instanceof ApiError ? error.value.fieldErrors() : {};
  // StartTLS is derived from the URL, so its complaints belong next to the URL.
  if (api["ldap.startTls"]) api["ldap.url"] = api["ldap.url"] ? `${api["ldap.url"]}; ${api["ldap.startTls"]}` : api["ldap.startTls"];
  // One input holds the whole list: a complaint about one value ("oidc.requiredAcr.2") belongs next to it.
  for (const [k, v] of Object.entries(api)) {
    const m = /^oidc\.requiredAcr[.[](\d+)/.exec(k);
    if (m) api["oidc.requiredAcr"] = [api["oidc.requiredAcr"], t("idp.oidc.acrValueError", { n: Number(m[1]) + 1, message: v })].filter(Boolean).join("; ");
  }
  return { ...api, ...local.value };
});
const unplaced = computed(() =>
  error.value instanceof ApiError
    ? error.value.details.filter((d) => !FIELDS.includes(d.field) && !/^(groupMappings|oidc\.requiredAcr)\b/.test(d.field))
    : [],
);

function validate(f: Form): Record<string, string> {
  const errs: Record<string, string> = {};
  const need = (key: string, v: string) => !v.trim() && (errs[key] = t("common.required"));
  need("name", f.name);
  if (!/^-?\d+$/.test(f.sortOrder.trim())) errs.sortOrder = t("idp.error.wholeNumber");
  if (f.kind === "oidc") {
    need("oidc.issuerUrl", f.issuerUrl);
    need("oidc.clientId", f.clientId);
    need("oidc.usernameClaim", f.usernameClaim);
    need("oidc.groupsClaim", f.groupsClaim);
    if (secretRequired("oidc.clientSecret") && secretMissing(f.clientSecret)) errs["oidc.clientSecret"] = t("common.required");
    if (f.mfaAssurance === "verify") {
      const acr = acrValues(f.requiredAcr);
      const bad = acr.find((v) => !ACR_VALUE.test(v));
      if (acr.length > MAX_REQUIRED_ACR) errs["oidc.requiredAcr"] = t("idp.error.acrTooMany", { n: MAX_REQUIRED_ACR });
      else if (bad !== undefined) errs["oidc.requiredAcr"] = t("idp.error.acrValue", { value: bad.length > 40 ? `${bad.slice(0, 40)}…` : bad });
    }
  } else {
    need("ldap.url", f.url);
    need("ldap.userBaseDn", f.userBaseDn);
    need("ldap.userFilter", f.userFilter);
    if (f.userFilter.trim() && !f.userFilter.includes("{username}")) errs["ldap.userFilter"] = t("idp.error.userFilter");
    need("ldap.usernameAttribute", f.usernameAttribute);
    need("ldap.displayNameAttribute", f.displayNameAttribute);
    need("ldap.emailAttribute", f.emailAttribute);
    need("ldap.groupAttribute", f.groupAttribute);
    const hasPassword = typeof f.bindPassword === "string" ? f.bindPassword !== "" : f.bindPassword === undefined && !!p.value?.ldap?.bindPasswordSet;
    if (secretRequired("ldap.bindPassword") && secretMissing(f.bindPassword)) errs["ldap.bindPassword"] = t("common.required");
    else if (f.bindDn.trim() && !hasPassword) errs["ldap.bindPassword"] = t("idp.error.bindDnNeedsPassword");
    if (!f.bindDn.trim() && typeof f.bindPassword === "string" && f.bindPassword) errs["ldap.bindPassword"] = t("idp.error.passwordNeedsBindDn");
  }
  f.mappings.forEach((m, i) => {
    if (!m.group.trim()) errs[`groupMappings.${i}.group`] = t("common.required");
    if (!m.profileId) errs[`groupMappings.${i}.profileId`] = t("idp.mappings.chooseError");
  });
  return errs;
}

const MAX_REQUIRED_ACR = 10;
/** Printable ASCII without spaces, as the API accepts. */
const ACR_VALUE = /^[!-~]{1,200}$/;
/** The typed list, each value once, in order. */
const acrValues = (text: string) => [...new Set(text.split(/\s+/).filter(Boolean))];

/** Secrets: undefined keeps the stored one, "" (an untouched replace box) too; null removes. */
const secret = (v: string | null | undefined) => (v === "" ? undefined : v);

function body(f: Form): IdentityProviderUpdateBody {
  const b: IdentityProviderUpdateBody = {
    name: f.name.trim(),
    isEnabled: f.isEnabled,
    sortOrder: Number(f.sortOrder.trim()),
    caCertificate: f.caCertificate.trim() || null,
    groupMappings: f.mappings.map((m) => ({ group: m.group.trim(), profileId: m.profileId })),
  };
  if (f.kind === "oidc") {
    b.oidc = {
      issuerUrl: f.issuerUrl.trim(),
      clientId: f.clientId.trim(),
      scopes: f.scopes.trim(),
      usernameClaim: f.usernameClaim.trim(),
      groupsClaim: f.groupsClaim.trim(),
      mfaAssurance: f.mfaAssurance,
    };
    // Under Trust the list is left out: the API then empties it.
    if (f.mfaAssurance === "verify") b.oidc.requiredAcr = acrValues(f.requiredAcr);
    const s = secret(f.clientSecret);
    if (s !== undefined) b.oidc.clientSecret = s;
  } else {
    b.ldap = {
      url: f.url.trim(),
      bindDn: f.bindDn.trim() || null,
      userBaseDn: f.userBaseDn.trim(),
      userFilter: f.userFilter.trim(),
      usernameAttribute: f.usernameAttribute.trim(),
      displayNameAttribute: f.displayNameAttribute.trim(),
      emailAttribute: f.emailAttribute.trim(),
      groupAttribute: f.groupAttribute.trim(),
    };
    const s = secret(f.bindPassword);
    if (s !== undefined) b.ldap.bindPassword = s;
    // No bind DN: an anonymous search, so no password either.
    if (!b.ldap.bindDn && p.value?.ldap?.bindPasswordSet) b.ldap.bindPassword = null;
  }
  return b;
}

async function submit() {
  error.value = null;
  const f = form.value;
  local.value = validate(f);
  const first = Object.keys(local.value)[0];
  if (first) {
    const el = document.getElementById(fieldId(first));
    el?.focus();
    return;
  }
  try {
    if (isNew.value) {
      const b = body(f);
      const created = await create.mutateAsync({ ...b, kind: f.kind, name: b.name! } as IdentityProviderCreateBody);
      if (created) {
        guard.allow();
        flash.show(t("idp.edit.created", { name: created.name }));
        await router.push(`/admin/identity-providers/${created.id}`);
      }
      return;
    }
    const next = await update.mutateAsync({ id: id.value!, body: body(f) });
    if (next) seed(fromProvider(next));
    flash.show(t("idp.edit.saved", { name: next?.name ?? form.value.name }));
  } catch (e) {
    error.value = e;
    // The API wants the secret again (the stored address may have changed meanwhile): open its input.
    const asked = secretRequiredFields(e);
    if (asked.length) {
      apiRequired.value = [...new Set([...apiRequired.value, ...asked])];
      await nextTick();
      document.getElementById(fieldId(asked[0]))?.focus();
    }
  }
}

/** Form element ids by field path ("oidc.issuerUrl" → "idp-oidc-issuerUrl", "groupMappings.2.group" → "mapping-2-group"). */
function fieldId(path: string): string {
  const m = /^groupMappings\.(\d+)\.(group|profileId)$/.exec(path);
  if (m) return `mapping-${m[1]}-${m[2] === "group" ? "group" : "profile"}`;
  return `idp-${path.replace(".", "-")}`;
}

async function copyRedirect() {
  const uri = p.value?.oidc?.redirectUri;
  if (!uri) return;
  try {
    await navigator.clipboard.writeText(uri);
    copyState.value = "copied";
  } catch {
    const el = document.getElementById("idp-redirect") as HTMLInputElement | null;
    el?.select();
    copyState.value = document.execCommand("copy") ? "copied" : "failed";
  }
}

/** Back to the stored settings. */
function discard() {
  seed(isNew.value || !provider.data.value ? blank(initialKind()) : fromProvider(provider.data.value));
  error.value = null;
  local.value = {};
}

// Delete sits in the `⋯` menu; its dialog is the availability panel's.
const access = ref<InstanceType<typeof ProviderAccessPanel>>();
const moreActions = computed<RowMenuItem[]>(() => [{ label: t("idp.access.delete"), danger: true, action: () => access.value?.openDelete() }]);

const crumbs = computed(() => adminCrumbs("identity-providers", { label: isNew.value ? t("admin.crumb.new") : (p.value?.name ?? "…") }));
const notFound = computed(() => {
  const e = provider.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs v-if="!isNew && (provider.isLoading.value || provider.isError.value)" :items="crumbs" />
  <LoadingState v-if="!isNew && provider.isLoading.value" :label="t('idp.edit.loading')" />
  <template v-else-if="!isNew && provider.isError.value">
    <EmptyState v-if="notFound" :title="t('idp.edit.notFound.title')">
      {{ t("idp.edit.notFound.body", { id: id ?? "" }) }}
      <template #actions><RouterLink class="btn" to="/admin/identity-providers">{{ t("idp.edit.back") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="provider.error.value" :on-retry="() => provider.refetch()" />
  </template>
  <template v-else>
    <div class="record-head record-head-plain">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="key-round" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ isNew ? t("idp.edit.new") : p?.name }}</h1>
            </div>
            <p v-if="p && !isNew" class="record-meta" data-testid="record-meta">
              <span :class="['badge', p.isEnabled ? 'ok' : 'off']"
                ><span class="status-dot" aria-hidden="true" />{{ p.isEnabled ? t("idp.enabled") : t("common.disabled") }}</span
              >
              <span class="badge">{{ providerKindLabel(p.kind) }}</span>
              <span v-if="p.oidc?.mfaAssurance === 'trustProvider'" class="badge warn" :title="t('idp.mfaNotVerifiedTitle')">{{ t("idp.mfaNotVerified") }}</span>
              <span class="badge">{{ t("idp.edit.accounts", { n: p.userCount }) }}</span>
              <span class="record-meta-line">
                <span v-if="endpoint" class="mono" dir="ltr">{{ endpoint }}</span>
                <span v-if="endpoint" class="sep" aria-hidden="true">·</span>
                <time :datetime="p.updatedAt" :title="formatDateTime(p.updatedAt)">{{ t("idp.edit.updated", { when: formatRelative(p.updatedAt) }) }}</time>
              </span>
            </p>
          </div>
        </div>
        <div v-if="p && !isNew" class="actions">
          <RowMenu :label="t('record.actions.more')" :items="moreActions" large />
        </div>
      </div>
    </div>

    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div class="grid-2">
      <form id="idp-form" class="stack" :aria-label="t('idp.edit.formLabel')" novalidate @submit.prevent="submit">
        <section class="panel" aria-labelledby="idp-general-title">
          <div class="panel-header"><h2 id="idp-general-title">{{ t("idp.edit.general") }}</h2></div>
          <div class="panel-body stack">
            <fieldset v-if="isNew" class="group">
              <legend>{{ t("idp.col.type") }}</legend>
              <div class="radio-choice">
                <label class="checkbox-row">
                  <input v-model="form.kind" type="radio" name="idp-kind" value="oidc" />
                  <span><strong>{{ t("idp.kind.oidc") }}</strong> — {{ t("idp.edit.kindOidc") }}</span>
                </label>
                <label class="checkbox-row">
                  <input v-model="form.kind" type="radio" name="idp-kind" value="ldap" />
                  <span><strong>{{ t("idp.kind.ldap") }}</strong> — {{ t("idp.edit.kindLdap") }}</span>
                </label>
              </div>
            </fieldset>
            <div class="form-grid">
              <FormField id="idp-name" :label="t('idp.col.name')" required :error="fieldErrors.name" :hint="isOidc ? t('idp.edit.nameHintOidc', { name: form.name.trim() || t('idp.col.name') }) : t('idp.edit.nameHintLdap')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.name" v-autofocus="isNew" type="text" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-sortOrder" :label="t('idp.col.order')" :error="fieldErrors.sortOrder" :hint="isOidc ? t('idp.edit.orderHintOidc') : t('idp.edit.orderHintLdap')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.sortOrder" type="number" step="1" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <div v-if="isNew" class="field">
                <span class="label">{{ t("admin.col.status") }}</span>
                <label class="checkbox-row"><input v-model="form.isEnabled" type="checkbox" /> {{ t("idp.edit.enabledCheckbox") }}</label>
              </div>
            </div>
          </div>
        </section>

        <section v-if="isOidc" class="panel" aria-labelledby="idp-oidc-title">
          <div class="panel-header"><h2 id="idp-oidc-title">{{ t("idp.kind.oidc") }}</h2></div>
          <div class="panel-body stack">
            <div class="field">
              <label for="idp-redirect">{{ t("idp.oidc.redirect") }}</label>
              <span v-if="isNew" class="hint">{{ t("idp.oidc.redirectAfterCreate") }}</span>
              <template v-else-if="p?.oidc?.redirectUri">
                <div class="copy-row">
                  <input id="idp-redirect" class="mono" type="text" readonly :value="p.oidc.redirectUri" aria-describedby="idp-redirect-hint" @focus="($event.target as HTMLInputElement).select()" />
                  <button type="button" class="btn" @click="copyRedirect"><Icon name="copy" />{{ t("idp.oidc.copy") }}</button>
                </div>
                <span id="idp-redirect-hint" class="hint">{{ t("idp.oidc.redirectHint") }}</span>
                <span role="status" :class="copyState === 'failed' ? 'error' : 'hint'">
                  {{ copyState === "copied" ? t("idp.oidc.copied") : copyState === "failed" ? t("idp.oidc.copyFailed") : "" }}
                </span>
              </template>
              <div v-else class="alert alert-warn" role="status" data-testid="public-url-missing">
                <strong>{{ t("idp.oidc.noRedirectTitle") }}</strong>
                <div>{{ t("idp.oidc.noRedirectBody") }}</div>
              </div>
            </div>
            <div class="form-grid">
              <FormField id="idp-oidc-issuerUrl" :label="t('idp.oidc.issuer')" required wide :error="fieldErrors['oidc.issuerUrl']" :hint="t('idp.oidc.issuerHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.issuerUrl" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-oidc-clientId" :label="t('idp.oidc.clientId')" required :error="fieldErrors['oidc.clientId']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.clientId" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <SecretInput
                id="idp-oidc-clientSecret"
                v-model="form.clientSecret"
                :label="t('idp.oidc.clientSecret')"
                :is-set="!!p?.oidc?.clientSecretSet"
                removable
                :required="secretRequired('oidc.clientSecret')"
                :error="fieldErrors['oidc.clientSecret']"
                :hint="secretRequired('oidc.clientSecret') ? reentryHint('oidc.clientSecret') : t('idp.oidc.clientSecretHint')"
              />
              <FormField id="idp-oidc-scopes" :label="t('idp.oidc.scopes')" :error="fieldErrors['oidc.scopes']" :hint="t('idp.oidc.scopesHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.scopes" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-oidc-usernameClaim" :label="t('idp.oidc.usernameClaim')" required :error="fieldErrors['oidc.usernameClaim']" :hint="t('idp.oidc.usernameClaimHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.usernameClaim" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-oidc-groupsClaim" :label="t('idp.oidc.groupsClaim')" required :error="fieldErrors['oidc.groupsClaim']" :hint="t('idp.oidc.groupsClaimHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.groupsClaim" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
            <fieldset class="group mfa" aria-describedby="idp-mfa-hint">
              <legend>{{ t("idp.mfa.title") }}</legend>
              <span id="idp-mfa-hint" class="hint">{{ t("idp.mfa.hint") }}</span>
              <div class="radio-choice">
                <label class="checkbox-row">
                  <input v-model="form.mfaAssurance" type="radio" name="idp-mfa" value="verify" />
                  <span><strong>{{ t("idp.mfa.verify") }}</strong>: {{ t("idp.mfa.verifyBody") }}</span>
                </label>
                <label class="checkbox-row">
                  <input v-model="form.mfaAssurance" type="radio" name="idp-mfa" value="trustProvider" />
                  <span><strong>{{ t("idp.mfa.trust") }}</strong>: {{ t("idp.mfa.trustBody") }}</span>
                </label>
              </div>
              <FormField
                v-if="form.mfaAssurance === 'verify'"
                id="idp-oidc-requiredAcr"
                :label="t('idp.mfa.acr')"
                wide
                :error="fieldErrors['oidc.requiredAcr']"
                :hint="t('idp.mfa.acrHint')"
              >
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.requiredAcr" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <div v-else class="alert alert-warn" role="status" data-testid="mfa-trust-warning">
                <strong>{{ t("idp.mfa.notVerifiedTitle") }}</strong>
                <div>{{ t("idp.mfa.notVerifiedBody") }}</div>
              </div>
            </fieldset>
          </div>
        </section>

        <section v-else class="panel" aria-labelledby="idp-ldap-title">
          <div class="panel-header"><h2 id="idp-ldap-title">{{ t("idp.kind.ldap") }}</h2></div>
          <div class="panel-body stack">
            <div class="form-grid">
              <FormField id="idp-ldap-url" :label="t('idp.ldap.url')" required wide :error="fieldErrors['ldap.url']" :hint="transport">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.url" class="mono" type="text" spellcheck="false" autocomplete="off" placeholder="ldaps://dc1.example.com" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-bindDn" :label="t('idp.ldap.bindDn')" wide :error="fieldErrors['ldap.bindDn']" :hint="t('idp.ldap.bindDnHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.bindDn" class="mono" type="text" spellcheck="false" autocomplete="off" placeholder="CN=svc-cmdb,OU=Service Accounts,DC=example,DC=com" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <SecretInput
                id="idp-ldap-bindPassword"
                v-model="form.bindPassword"
                :label="t('idp.ldap.bindPassword')"
                :is-set="!!p?.ldap?.bindPasswordSet"
                :required="secretRequired('ldap.bindPassword')"
                :error="fieldErrors['ldap.bindPassword']"
                :hint="secretRequired('ldap.bindPassword') ? reentryHint('ldap.bindPassword') : t('idp.ldap.bindPasswordHint')"
              />
              <FormField id="idp-ldap-userBaseDn" :label="t('idp.ldap.userBaseDn')" required wide :error="fieldErrors['ldap.userBaseDn']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.userBaseDn" class="mono" type="text" spellcheck="false" autocomplete="off" placeholder="OU=Staff,DC=example,DC=com" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-userFilter" :label="t('idp.ldap.userFilter')" required wide :error="fieldErrors['ldap.userFilter']" :hint="t('idp.ldap.userFilterHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.userFilter" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-usernameAttribute" :label="t('idp.ldap.usernameAttribute')" required :error="fieldErrors['ldap.usernameAttribute']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.usernameAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-displayNameAttribute" :label="t('idp.ldap.displayNameAttribute')" required :error="fieldErrors['ldap.displayNameAttribute']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.displayNameAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-emailAttribute" :label="t('idp.ldap.emailAttribute')" required :error="fieldErrors['ldap.emailAttribute']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.emailAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-groupAttribute" :label="t('idp.ldap.groupAttribute')" required :error="fieldErrors['ldap.groupAttribute']" :hint="t('idp.ldap.groupAttributeHint')">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.groupAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
          </div>
        </section>

        <section class="panel" aria-labelledby="idp-tls-title">
          <div class="panel-header"><h2 id="idp-tls-title">{{ t("idp.tls.title") }}</h2></div>
          <div class="panel-body stack">
            <FormField id="idp-caCertificate" :label="t('idp.tls.ca')" :error="fieldErrors.caCertificate" :hint="t('idp.tls.caHint')">
              <template #default="{ id: fid, invalid, describedBy }">
                <textarea :id="fid" v-model="form.caCertificate" class="mono" rows="4" spellcheck="false" placeholder="-----BEGIN CERTIFICATE-----" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
          </div>
        </section>

        <GroupMappingsEditor v-model="form.mappings" :kind="form.kind" :errors="fieldErrors" />
      </form>

      <div class="stack">
        <template v-if="p && !isNew">
          <ProviderTestPanel :provider="p" :dirty="dirty" />
          <ProviderAccessPanel ref="access" :provider="p" />
        </template>
        <section class="panel" aria-labelledby="idp-about-title">
          <div class="panel-header"><h2 id="idp-about-title">{{ t("idp.about.title") }}</h2></div>
          <div class="panel-body stack">
            <p class="no-margin">{{ t("idp.about.firstSignIn") }}</p>
            <p class="no-margin">{{ t("idp.about.noTakeover") }}</p>
            <p class="muted no-margin">{{ t("idp.about.local") }}</p>
            <p class="muted no-margin">{{ t("idp.about.mfa") }}</p>
          </div>
        </section>
        <section v-if="p && !isNew" class="panel" aria-labelledby="idp-facts-title">
          <div class="panel-header"><h2 id="idp-facts-title">{{ t("idp.edit.record") }}</h2></div>
          <div class="panel-body">
            <dl class="props">
              <dt>{{ t("common.created") }}</dt>
              <dd>{{ formatDateTime(p.createdAt) }}</dd>
              <dt>{{ t("common.updated") }}</dt>
              <dd>{{ formatDateTime(p.updatedAt) }}</dd>
            </dl>
          </div>
        </section>
      </div>
    </div>

    <SaveBar :label="t('record.save.region')" :dirty="!isNew && dirty" :changes="isNew ? 0 : changes">
      <RouterLink class="btn" to="/admin/identity-providers">{{ t("common.cancel") }}</RouterLink>
      <button v-if="!isNew && dirty" type="button" class="btn" :disabled="pending" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="submit" form="idp-form" class="btn btn-primary" :disabled="pending">
        {{ pending ? t("common.saving") : isNew ? t("idp.edit.create") : t("common.saveChanges") }}
      </button>
    </SaveBar>
  </template>
</template>

<style scoped>
.radio-choice {
  display: flex;
  flex-direction: column;
  gap: var(--space-0_5);
}
.radio-choice .checkbox-row {
  height: auto;
  align-items: flex-start;
}
.radio-choice input {
  margin-top: 3px;
}
.copy-row {
  display: flex;
  gap: var(--space-1);
}
.copy-row input {
  flex: 1;
}
.mfa {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  border-top: 1px solid var(--c-border);
  padding-top: var(--space-3);
  margin-top: var(--space-3);
}
.mfa legend {
  float: left;
  padding-bottom: 0;
}
.mfa .hint {
  font-size: var(--fs-xs);
  color: var(--c-text-secondary);
}
</style>
