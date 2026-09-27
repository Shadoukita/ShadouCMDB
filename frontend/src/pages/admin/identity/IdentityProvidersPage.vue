<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { KIND_LABELS, useIdentityProviders, type IdentityProvider } from "../../../api/identityProviders";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { formatRelative } from "../../../lib/format";

/**
 * Administration › Identity providers: OpenID Connect providers and LDAP / AD directories, in the
 * order the sign-in page and the directory look-up use them. A handful per installation, so the API
 * lists them all at once.
 */
useDocumentTitle("Identity providers");
const list = useIdentityProviders();
const rows = computed(() => list.data.value ?? []);

const endpoint = (p: IdentityProvider) => p.oidc?.issuerUrl ?? p.ldap?.url ?? "";
/** What still keeps it from working, as far as the saved settings tell. */
function warning(p: IdentityProvider): string | null {
  if (p.groupMappings.length === 0) return "No group mappings: nobody can sign in";
  if (p.oidc && !p.oidc.redirectUri) return "PUBLIC_URL is not set: no sign-in button";
  return null;
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Identity providers' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Identity providers</h1>
      <span v-if="list.data.value" class="muted">{{ rows.length.toLocaleString() }} total</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <div class="actions">
      <RouterLink class="btn" :to="{ path: '/admin/identity-providers/new', query: { kind: 'ldap' } }">+ New LDAP directory</RouterLink>
      <RouterLink class="btn btn-primary" :to="{ path: '/admin/identity-providers/new', query: { kind: 'oidc' } }">+ New OpenID Connect provider</RouterLink>
    </div>
  </div>
  <p class="muted" style="margin-top: 0">
    Let people sign in with their company account. OpenID Connect providers appear as “Sign in with …” buttons;
    directory users sign in with the username and password form. Group mappings decide their permission profiles.
    Local accounts keep working next to them.
  </p>

  <section class="panel" aria-label="Identity providers">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading identity providers…" />
    <EmptyState v-if="list.data.value && rows.length === 0" title="No identity providers yet">
      Everyone signs in with a local ShadouCMDB account. Add an OpenID Connect provider (Microsoft Entra ID, Okta,
      Keycloak, ADFS, Google Workspace…) or an LDAP / Active Directory directory to use company accounts.
      <template #actions>
        <RouterLink class="btn btn-primary" :to="{ path: '/admin/identity-providers/new', query: { kind: 'oidc' } }">+ New OpenID Connect provider</RouterLink>
        <RouterLink class="btn" :to="{ path: '/admin/identity-providers/new', query: { kind: 'ldap' } }">+ New LDAP directory</RouterLink>
      </template>
    </EmptyState>

    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col" class="num">Order</th>
            <th scope="col">Name</th>
            <th scope="col">Type</th>
            <th scope="col">Status</th>
            <th scope="col">Server</th>
            <th scope="col" class="num">Group mappings</th>
            <th scope="col" class="num">Accounts</th>
            <th scope="col">Updated</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in rows" :key="p.id" :class="{ disabled: !p.isEnabled }">
            <td class="num">{{ p.sortOrder }}</td>
            <td>
              <RouterLink :to="`/admin/identity-providers/${p.id}`">{{ p.name }}</RouterLink>
              <span v-if="warning(p)" class="badge warn" style="margin-left: 6px">{{ warning(p) }}</span>
            </td>
            <td>{{ KIND_LABELS[p.kind] }}</td>
            <td>
              <span v-if="p.isEnabled" class="badge ok">Enabled</span>
              <span v-else class="badge off">Disabled</span>
            </td>
            <td class="mono" :title="endpoint(p)">{{ endpoint(p) }}</td>
            <td class="num" :title="p.groupMappings.map((m) => `${m.group} → ${m.profileName}`).join('\n')">{{ p.groupMappings.length }}</td>
            <td class="num">{{ p.userCount.toLocaleString() }}</td>
            <td :title="p.updatedAt">{{ formatRelative(p.updatedAt) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
