<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import {
  KIND_LABELS,
  useCreateIdentityProvider,
  useIdentityProvider,
  useUpdateIdentityProvider,
  type IdentityProvider,
  type IdentityProviderCreateBody,
  type IdentityProviderUpdateBody,
  type ProviderKind,
} from "../../../api/identityProviders";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime, plural } from "../../../lib/format";
import { useFlashStore } from "../../../stores/flash";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import GroupMappingsEditor, { type MappingRow } from "./GroupMappingsEditor.vue";
import ProviderAccessPanel from "./ProviderAccessPanel.vue";
import ProviderTestPanel from "./ProviderTestPanel.vue";
import SecretInput from "./SecretInput.vue";

/**
 * Create or edit an identity provider: an OpenID Connect provider (a "Sign in with …" button) or an
 * LDAP / Active Directory directory (its users sign in with the password form). Secrets are
 * write-only; group mappings decide which permission profiles its users get.
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
const flashText = computed(() => (id.value ? flash.forCi(id.value) : undefined));
useDocumentTitle(() => (isNew.value ? "New identity provider" : provider.data.value?.name));

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
const saved = ref<string | null>(null);
const copyState = ref<"" | "copied" | "failed">("");

function seed(f: Form) {
  form.value = f;
  baseline.value = JSON.stringify(f);
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
  saved.value = null;
});
const dirty = computed(() => JSON.stringify(form.value) !== baseline.value);

const p = computed(() => provider.data.value);
const isOidc = computed(() => form.value.kind === "oidc");
/** StartTLS follows the URL's scheme (the API refuses the other combinations): shown, never asked. */
const transport = computed(() => {
  const u = form.value.url.trim().toLowerCase();
  if (u.startsWith("ldaps://")) return "LDAPS: TLS from the first byte.";
  if (u.startsWith("ldap://")) return "StartTLS: the connection is upgraded to TLS before anything is sent. Plain LDAP is never used.";
  return "Use ldaps://host[:port] (TLS), or ldap://host[:port] (upgraded with StartTLS).";
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
  return { ...api, ...local.value };
});
const unplaced = computed(() =>
  error.value instanceof ApiError
    ? error.value.details.filter((d) => !FIELDS.includes(d.field) && !d.field.startsWith("groupMappings"))
    : [],
);

function validate(f: Form): Record<string, string> {
  const errs: Record<string, string> = {};
  const need = (key: string, v: string) => !v.trim() && (errs[key] = "Required");
  need("name", f.name);
  if (!/^-?\d+$/.test(f.sortOrder.trim())) errs.sortOrder = "A whole number";
  if (f.kind === "oidc") {
    need("oidc.issuerUrl", f.issuerUrl);
    need("oidc.clientId", f.clientId);
    need("oidc.usernameClaim", f.usernameClaim);
    need("oidc.groupsClaim", f.groupsClaim);
  } else {
    need("ldap.url", f.url);
    need("ldap.userBaseDn", f.userBaseDn);
    need("ldap.userFilter", f.userFilter);
    if (f.userFilter.trim() && !f.userFilter.includes("{username}")) errs["ldap.userFilter"] = "Must contain {username}";
    need("ldap.usernameAttribute", f.usernameAttribute);
    need("ldap.displayNameAttribute", f.displayNameAttribute);
    need("ldap.emailAttribute", f.emailAttribute);
    need("ldap.groupAttribute", f.groupAttribute);
    const hasPassword = typeof f.bindPassword === "string" ? f.bindPassword !== "" : f.bindPassword === undefined && !!p.value?.ldap?.bindPasswordSet;
    if (f.bindDn.trim() && !hasPassword) errs["ldap.bindPassword"] = "A bind DN needs its password";
    if (!f.bindDn.trim() && typeof f.bindPassword === "string" && f.bindPassword) errs["ldap.bindPassword"] = "A bind password needs a bind DN";
  }
  f.mappings.forEach((m, i) => {
    if (!m.group.trim()) errs[`groupMappings.${i}.group`] = "Required";
    if (!m.profileId) errs[`groupMappings.${i}.profileId`] = "Choose a profile";
  });
  return errs;
}

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
    };
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
  saved.value = null;
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
        flash.show(created.id, `Created ${created.name}. Run the connection test to check the settings.`);
        await router.push(`/admin/identity-providers/${created.id}`);
      }
      return;
    }
    const next = await update.mutateAsync({ id: id.value!, body: body(f) });
    if (next) seed(fromProvider(next));
    saved.value = `Saved ${next?.name ?? "the provider"}. Mapping changes apply at each account's next sign-in.`;
  } catch (e) {
    error.value = e;
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

const crumbs = computed(() => [
  { label: "Administration", to: "/admin" },
  { label: "Identity providers", to: "/admin/identity-providers" },
  { label: isNew.value ? "New" : (p.value?.name ?? "…") },
]);
const notFound = computed(() => {
  const e = provider.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <LoadingState v-if="!isNew && provider.isLoading.value" label="Loading identity provider…" />
  <template v-else-if="!isNew && provider.isError.value">
    <EmptyState v-if="notFound" title="Identity provider not found">
      No identity provider has the id <code>{{ id }}</code>. It may have been deleted.
      <template #actions><RouterLink class="btn" to="/admin/identity-providers">Back to identity providers</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="provider.error.value" :on-retry="() => provider.refetch()" />
  </template>
  <template v-else>
    <div class="page-header">
      <div class="title">
        <h1>{{ isNew ? "New identity provider" : p?.name }}</h1>
        <template v-if="p && !isNew">
          <span class="badge">{{ KIND_LABELS[p.kind] }}</span>
          <span v-if="p.isEnabled" class="badge ok">Enabled</span>
          <span v-else class="badge off">Disabled</span>
          <span class="muted">{{ plural(p.userCount, "account") }}</span>
        </template>
      </div>
    </div>
    <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>

    <div class="grid-2">
      <form class="stack" aria-label="Identity provider settings" novalidate @submit.prevent="submit">
        <section class="panel" aria-labelledby="idp-general-title">
          <div class="panel-header"><h2 id="idp-general-title">General</h2></div>
          <div class="panel-body stack">
            <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
            <div v-if="saved" class="alert" role="status">{{ saved }}</div>
            <fieldset v-if="isNew" class="group">
              <legend>Type</legend>
              <div class="kind-choice">
                <label class="checkbox-row">
                  <input v-model="form.kind" type="radio" name="idp-kind" value="oidc" />
                  <span><strong>OpenID Connect</strong> — Microsoft Entra ID, Okta, Keycloak, ADFS, Google Workspace…: a “Sign in with …” button</span>
                </label>
                <label class="checkbox-row">
                  <input v-model="form.kind" type="radio" name="idp-kind" value="ldap" />
                  <span><strong>LDAP / Active Directory</strong>: directory users sign in with their username and password</span>
                </label>
              </div>
            </fieldset>
            <div class="form-grid">
              <FormField
                id="idp-name"
                label="Name"
                required
                :error="fieldErrors.name"
                :hint="isOidc ? 'On the sign-in button: “Sign in with {name}”. Also in the audit trail.' : 'Shown to administrators and in the audit trail.'"
              >
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.name" v-autofocus="isNew" type="text" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField
                id="idp-sortOrder"
                label="Order"
                :error="fieldErrors.sortOrder"
                :hint="isOidc ? 'Buttons are listed from the lowest number.' : 'Directories are asked from the lowest number; the first that knows the name decides.'"
              >
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.sortOrder" type="number" step="1" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <div v-if="isNew" class="field">
                <span class="label">Status</span>
                <label class="checkbox-row"><input v-model="form.isEnabled" type="checkbox" /> Enabled (users can sign in through it)</label>
              </div>
            </div>
          </div>
        </section>

        <section v-if="isOidc" class="panel" aria-labelledby="idp-oidc-title">
          <div class="panel-header"><h2 id="idp-oidc-title">OpenID Connect</h2></div>
          <div class="panel-body stack">
            <div class="field">
              <label for="idp-redirect">Redirect URI</label>
              <template v-if="isNew">
                <span class="hint">Shown here after you create the provider. Register it at the provider as the application's redirect (reply) URI.</span>
              </template>
              <template v-else-if="p?.oidc?.redirectUri">
                <div class="copy-row">
                  <input id="idp-redirect" class="mono" type="text" readonly :value="p.oidc.redirectUri" aria-describedby="idp-redirect-hint" @focus="($event.target as HTMLInputElement).select()" />
                  <button type="button" class="btn" @click="copyRedirect">Copy</button>
                </div>
                <span id="idp-redirect-hint" class="hint">Register this at the provider as the application's redirect (reply) URI.</span>
                <span role="status" :class="copyState === 'failed' ? 'error' : 'hint'">
                  {{ copyState === "copied" ? "Copied to the clipboard." : copyState === "failed" ? "Could not copy — select it and copy it by hand." : "" }}
                </span>
              </template>
              <div v-else class="alert alert-warn" role="status" data-testid="public-url-missing">
                <strong>No redirect URI yet.</strong>
                <div>
                  Set <code>PUBLIC_URL</code> on the server to the address users open ShadouCMDB at. Until then the
                  redirect URI is unknown and the sign-in page shows no OpenID Connect buttons.
                </div>
              </div>
            </div>
            <div class="form-grid">
              <FormField id="idp-oidc-issuerUrl" label="Issuer URL" required wide :error="fieldErrors['oidc.issuerUrl']" hint="e.g. https://login.microsoftonline.com/{tenant}/v2.0 — must be https">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.issuerUrl" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-oidc-clientId" label="Client ID" required :error="fieldErrors['oidc.clientId']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.clientId" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <SecretInput
                id="idp-oidc-clientSecret"
                v-model="form.clientSecret"
                label="Client secret"
                :is-set="!!p?.oidc?.clientSecretSet"
                removable
                :error="fieldErrors['oidc.clientSecret']"
                hint="Leave empty for a public client (PKCE only). Never shown again after saving."
              />
              <FormField id="idp-oidc-scopes" label="Scopes" :error="fieldErrors['oidc.scopes']" hint="Space-separated; openid is always added">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.scopes" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-oidc-usernameClaim" label="Username claim" required :error="fieldErrors['oidc.usernameClaim']" hint="ID token claim used as the ShadouCMDB username">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.usernameClaim" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-oidc-groupsClaim" label="Groups claim" required :error="fieldErrors['oidc.groupsClaim']" hint="Dots descend into objects, e.g. realm_access.roles (Keycloak)">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.groupsClaim" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
          </div>
        </section>

        <section v-else class="panel" aria-labelledby="idp-ldap-title">
          <div class="panel-header"><h2 id="idp-ldap-title">LDAP / Active Directory</h2></div>
          <div class="panel-body stack">
            <div class="form-grid">
              <FormField id="idp-ldap-url" label="Server URL" required wide :error="fieldErrors['ldap.url']" :hint="transport">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.url" class="mono" type="text" spellcheck="false" autocomplete="off" placeholder="ldaps://dc1.example.com" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-bindDn" label="Service account (bind DN)" wide :error="fieldErrors['ldap.bindDn']" hint="Read-only is enough. Leave empty to search anonymously.">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.bindDn" class="mono" type="text" spellcheck="false" autocomplete="off" placeholder="CN=svc-cmdb,OU=Service Accounts,DC=example,DC=com" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <SecretInput
                id="idp-ldap-bindPassword"
                v-model="form.bindPassword"
                label="Service account password"
                :is-set="!!p?.ldap?.bindPasswordSet"
                :error="fieldErrors['ldap.bindPassword']"
                hint="Never shown again after saving. Clearing the bind DN removes it."
              />
              <FormField id="idp-ldap-userBaseDn" label="User search base" required wide :error="fieldErrors['ldap.userBaseDn']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.userBaseDn" class="mono" type="text" spellcheck="false" autocomplete="off" placeholder="OU=Staff,DC=example,DC=com" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-userFilter" label="User filter" required wide :error="fieldErrors['ldap.userFilter']" hint="{username} is replaced by the escaped sign-in name; must find exactly one entry.">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.userFilter" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-usernameAttribute" label="Username attribute" required :error="fieldErrors['ldap.usernameAttribute']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.usernameAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-displayNameAttribute" label="Display name attribute" required :error="fieldErrors['ldap.displayNameAttribute']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.displayNameAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-emailAttribute" label="E-mail attribute" required :error="fieldErrors['ldap.emailAttribute']">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.emailAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
              <FormField id="idp-ldap-groupAttribute" label="Group attribute" required :error="fieldErrors['ldap.groupAttribute']" hint="Holds the DNs of the user's groups (direct membership)">
                <template #default="{ id: fid, invalid, describedBy }">
                  <input :id="fid" v-model="form.groupAttribute" class="mono" type="text" spellcheck="false" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
                </template>
              </FormField>
            </div>
          </div>
        </section>

        <section class="panel" aria-labelledby="idp-tls-title">
          <div class="panel-header"><h2 id="idp-tls-title">Trusted certificates</h2></div>
          <div class="panel-body stack">
            <FormField
              id="idp-caCertificate"
              label="Extra CA certificates (PEM)"
              :error="fieldErrors.caCertificate"
              hint="Only for a private CA. Certificates are always verified against the public roots and the server's trust store."
            >
              <template #default="{ id: fid, invalid, describedBy }">
                <textarea :id="fid" v-model="form.caCertificate" class="mono" rows="4" spellcheck="false" placeholder="-----BEGIN CERTIFICATE-----" :aria-invalid="invalid" :aria-describedby="describedBy" />
              </template>
            </FormField>
          </div>
        </section>

        <GroupMappingsEditor v-model="form.mappings" :kind="form.kind" :errors="fieldErrors" />

        <div class="panel">
          <div class="form-footer">
            <button type="submit" class="btn btn-primary" :disabled="pending">
              {{ pending ? "Saving…" : isNew ? "Create provider" : "Save changes" }}
            </button>
            <RouterLink class="btn" to="/admin/identity-providers">{{ isNew ? "Cancel" : "Back to identity providers" }}</RouterLink>
            <span v-if="!isNew && dirty" class="muted unsaved">Unsaved changes</span>
          </div>
        </div>
      </form>

      <div class="stack">
        <section class="panel" aria-labelledby="idp-about-title">
          <div class="panel-header"><h2 id="idp-about-title">How accounts work</h2></div>
          <div class="panel-body stack">
            <p class="no-margin">
              A person's first sign-in creates their account. Every sign-in sets the display name, e-mail and permission
              profiles from the provider; changes made by hand last until then.
            </p>
            <p class="no-margin">
              An existing local account is never taken over: if the username is taken, the sign-in is refused.
            </p>
            <p class="muted no-margin">
              Local accounts keep working when a provider is down. Keep a local administrator with two-factor
              authentication for emergencies. Two-factor authentication for provider accounts is the provider's job.
            </p>
          </div>
        </section>
        <template v-if="p && !isNew">
          <ProviderTestPanel :provider="p" :dirty="dirty" />
          <ProviderAccessPanel :provider="p" />
          <section class="panel" aria-labelledby="idp-facts-title">
            <div class="panel-header"><h2 id="idp-facts-title">Record</h2></div>
            <div class="panel-body">
              <dl class="props">
                <dt>Created</dt>
                <dd>{{ formatDateTime(p.createdAt) }}</dd>
                <dt>Updated</dt>
                <dd>{{ formatDateTime(p.updatedAt) }}</dd>
              </dl>
            </div>
          </section>
        </template>
      </div>
    </div>
  </template>
</template>

<style scoped>
.kind-choice {
  display: flex;
  flex-direction: column;
  gap: var(--sp-1);
}
.kind-choice .checkbox-row {
  height: auto;
  align-items: flex-start;
}
.kind-choice input {
  margin-top: 3px;
}
.copy-row {
  display: flex;
  gap: var(--sp-2);
}
.copy-row input {
  flex: 1;
}
.unsaved {
  align-self: center;
}
</style>
