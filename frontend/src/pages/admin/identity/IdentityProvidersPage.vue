<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useIdentityProviders, type IdentityProvider } from "../../../api/identityProviders";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import RowMenu from "../../../components/RowMenu.vue";
import SkeletonRows from "../../../components/SkeletonRows.vue";
import { formatNumber, t } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { onRowKeydown } from "../../../lib/rowKeyboard";
import KeyboardHints from "../../../components/KeyboardHints.vue";
import { providerKindLabel } from "./providerText";

/**
 * Administration › Identity providers, an explorer list (design §2.7, audit A4): OpenID Connect
 * providers and LDAP / AD directories, in the order the sign-in page and the directory look-up use
 * them. A handful per installation, so the API lists them all at once and there is nothing to page.
 */
useDocumentTitle(t("admin.section.identityProviders"));
const list = useIdentityProviders();
const rows = computed(() => list.data.value ?? []);

const endpoint = (p: IdentityProvider) => p.oidc?.issuerUrl ?? p.ldap?.url ?? "";
/** What still keeps it from working, as far as the saved settings tell. */
function warning(p: IdentityProvider): string | null {
  if (p.groupMappings.length === 0) return t("idp.list.noMappings");
  if (p.oidc && !p.oidc.redirectUri) return t("idp.list.noPublicUrl");
  return null;
}
const rowMenu = (p: IdentityProvider) => [{ label: t("inventory.row.open"), to: `/admin/identity-providers/${p.id}` }];
const NEW_OIDC = { path: "/admin/identity-providers/new", query: { kind: "oidc" } };
const NEW_LDAP = { path: "/admin/identity-providers/new", query: { kind: "ldap" } };
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('identity-providers')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.identityProviders") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(rows.length) }) }}</span>
        <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <RouterLink class="btn" :to="NEW_LDAP"><Icon name="plus" />{{ t("idp.newLdap") }}</RouterLink>
        <RouterLink class="btn btn-primary" :to="NEW_OIDC"><Icon name="plus" />{{ t("idp.newOidc") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("idp.list.intro") }}</p>
    <div v-if="rows.length > 0" class="toolbar">
      <p class="toolbar-hint">{{ t("idp.list.orderHint") }}</p>
    </div>
  </div>

  <section class="panel explorer" :aria-label="t('admin.section.identityProviders')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isLoading.value" :label="t('idp.list.loading')" />
    <EmptyState v-else-if="list.data.value && rows.length === 0" icon="key-round" :title="t('idp.list.empty.title')">
      {{ t("idp.list.empty.body") }}
      <template #actions>
        <RouterLink class="btn btn-primary" :to="NEW_OIDC"><Icon name="plus" />{{ t("idp.newOidc") }}</RouterLink>
        <RouterLink class="btn" :to="NEW_LDAP"><Icon name="plus" />{{ t("idp.newLdap") }}</RouterLink>
      </template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll">
        <table class="data list-table" aria-describedby="idp-keys">
          <thead>
            <tr>
              <th scope="col" class="num">{{ t("idp.col.order") }}</th>
              <th scope="col">{{ t("idp.col.name") }}</th>
              <th scope="col">{{ t("idp.col.type") }}</th>
              <th scope="col">{{ t("admin.col.status") }}</th>
              <th scope="col">{{ t("idp.col.server") }}</th>
              <th scope="col" class="num">{{ t("idp.col.mappings") }}</th>
              <th scope="col" class="num">{{ t("idp.col.accounts") }}</th>
              <th scope="col">{{ t("common.updated") }}</th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody @keydown="onRowKeydown($event)">
            <tr v-for="p in rows" :key="p.id" :data-id="p.id" :class="{ disabled: !p.isEnabled }">
              <td class="num">{{ p.sortOrder }}</td>
              <td class="wrap">
                <RouterLink class="list-name" :to="`/admin/identity-providers/${p.id}`" dir="auto">{{ p.name }}</RouterLink>
                <span v-if="warning(p)" class="badge warn spaced">{{ warning(p) }}</span>
                <span v-if="p.oidc?.mfaAssurance === 'trustProvider'" class="badge warn spaced" :title="t('idp.mfaNotVerifiedTitle')" data-testid="mfa-not-verified">
                  {{ t("idp.mfaNotVerified") }}
                </span>
              </td>
              <td>{{ providerKindLabel(p.kind) }}</td>
              <td>
                <span :class="['badge', p.isEnabled ? 'ok' : 'off']"
                  ><span class="status-dot" aria-hidden="true" />{{ p.isEnabled ? t("idp.enabled") : t("common.disabled") }}</span
                >
              </td>
              <td class="mono" :title="endpoint(p)">{{ endpoint(p) }}</td>
              <td class="num" :title="p.groupMappings.map((m) => `${m.group} → ${m.profileName}`).join('\n')">{{ formatNumber(p.groupMappings.length) }}</td>
              <td class="num">{{ formatNumber(p.userCount) }}</td>
              <td><time :datetime="p.updatedAt" :title="formatDateTime(p.updatedAt)">{{ formatRelative(p.updatedAt) }}</time></td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: p.name })" :items="rowMenu(p)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <KeyboardHints id="idp-keys" />
    </template>
  </section>
</template>
