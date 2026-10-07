<script setup lang="ts">
import { ref, watch } from "vue";
import { useTestIdentityProvider, type IdentityProvider } from "../../../api/identityProviders";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import { t } from "../../../i18n";

/**
 * Checks the saved settings without changing anything. OIDC: discovery and signing keys. LDAP:
 * TLS and the service bind, and with a username the entry, its groups and the profiles they map
 * to — what that user would get at sign-in.
 */
const props = defineProps<{ provider: IdentityProvider; dirty: boolean }>();
const test = useTestIdentityProvider();
const username = ref("");

watch(
  () => props.provider.id,
  () => {
    test.reset();
    username.value = "";
  },
);

function run() {
  test.mutate({ id: props.provider.id, username: props.provider.kind === "ldap" ? username.value.trim() : undefined });
}
</script>

<template>
  <section class="panel" aria-labelledby="test-title">
    <div class="panel-header"><h2 id="test-title">{{ t("idp.test.title") }}</h2></div>
    <form class="panel-body stack" novalidate @submit.prevent="run">
      <p class="muted no-margin">{{ provider.kind === "oidc" ? t("idp.test.oidcBody") : t("idp.test.ldapBody") }}</p>
      <p v-if="dirty" class="alert alert-warn no-margin" role="status">{{ t("idp.test.saveFirst") }}</p>
      <div v-if="provider.kind === 'ldap'" class="field">
        <label for="test-username">{{ t("idp.test.lookUp") }}</label>
        <input id="test-username" v-model="username" type="text" autocomplete="off" spellcheck="false" :placeholder="t('idp.test.lookUpPlaceholder')" />
      </div>
      <div>
        <button type="submit" class="btn" :disabled="test.isPending.value">{{ test.isPending.value ? t("idp.test.running") : t("idp.test.run") }}</button>
      </div>

      <ErrorAlert v-if="test.isError.value" :error="test.error.value" :title="t('idp.test.couldNotRun')" />
      <div v-if="test.data.value" :class="['alert', test.data.value.ok ? 'alert-success' : 'alert-error']" role="status" data-testid="test-result">
        <strong class="test-verdict"><Icon :name="test.data.value.ok ? 'circle-check' : 'circle-x'" />{{ test.data.value.ok ? t("idp.test.passed") : t("idp.test.failed") }}</strong>
        <div>{{ test.data.value.message }}</div>
        <ul v-if="test.data.value.details.length > 0">
          <li v-for="(d, i) in test.data.value.details" :key="i">{{ d }}</li>
        </ul>
      </div>
      <dl v-if="test.data.value?.user" class="props" data-testid="test-user">
        <dt>{{ t("idp.test.entry") }}</dt>
        <dd class="mono">{{ test.data.value.user.dn }}</dd>
        <dt>{{ t("idp.test.username") }}</dt>
        <dd>{{ test.data.value.user.username ?? "—" }}</dd>
        <dt>{{ t("idp.test.displayName") }}</dt>
        <dd>{{ test.data.value.user.displayName ?? "—" }}</dd>
        <dt>{{ t("idp.test.email") }}</dt>
        <dd>{{ test.data.value.user.email ?? "—" }}</dd>
        <dt>{{ t("idp.test.groups") }}</dt>
        <dd>
          <span v-if="test.data.value.user.groups.length === 0" class="muted">{{ t("idp.test.none") }}</span>
          <ul v-else class="plain-list">
            <li v-for="g in test.data.value.user.groups" :key="g" class="mono">{{ g }}</li>
          </ul>
        </dd>
        <dt>{{ t("idp.test.wouldGet") }}</dt>
        <dd>
          <span v-if="test.data.value.user.profiles.length === 0" class="badge danger">{{ t("idp.test.noProfile") }}</span>
          <template v-else>{{ test.data.value.user.profiles.join(", ") }}</template>
        </dd>
      </dl>
    </form>
  </section>
</template>

<style scoped>
.plain-list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.plain-list li {
  word-break: break-all;
}
.test-verdict {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
}
</style>
