<script setup lang="ts">
import { ref, watch } from "vue";
import { useTestIdentityProvider, type IdentityProvider } from "../../../api/identityProviders";
import ErrorAlert from "../../../components/ErrorAlert.vue";

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
    <div class="panel-header"><h2 id="test-title">Test connection</h2></div>
    <form class="panel-body stack" novalidate @submit.prevent="run">
      <p class="muted no-margin">
        <template v-if="provider.kind === 'oidc'">Fetches the provider's discovery document and signing keys.</template>
        <template v-else>
          Connects with TLS and binds as the service account. Enter a username to look the user up and see their groups
          and the profiles they would get (no password needed).
        </template>
      </p>
      <p v-if="dirty" class="alert alert-warn no-margin" role="status">The test uses the saved settings. Save your changes first.</p>
      <div v-if="provider.kind === 'ldap'" class="field">
        <label for="test-username">Look up a user (optional)</label>
        <input id="test-username" v-model="username" type="text" autocomplete="off" spellcheck="false" placeholder="e.g. jdoe" />
      </div>
      <div><button type="submit" class="btn" :disabled="test.isPending.value">{{ test.isPending.value ? "Testing…" : "Run test" }}</button></div>

      <ErrorAlert v-if="test.isError.value" :error="test.error.value" title="The test could not run" />
      <div v-if="test.data.value" :class="['alert', test.data.value.ok ? '' : 'alert-error']" role="status" data-testid="test-result">
        <strong>{{ test.data.value.ok ? "Test passed" : "Test failed" }}</strong>
        <div>{{ test.data.value.message }}</div>
        <ul v-if="test.data.value.details.length > 0">
          <li v-for="(d, i) in test.data.value.details" :key="i">{{ d }}</li>
        </ul>
      </div>
      <dl v-if="test.data.value?.user" class="props" data-testid="test-user">
        <dt>Entry</dt>
        <dd class="mono">{{ test.data.value.user.dn }}</dd>
        <dt>Username</dt>
        <dd>{{ test.data.value.user.username ?? "—" }}</dd>
        <dt>Display name</dt>
        <dd>{{ test.data.value.user.displayName ?? "—" }}</dd>
        <dt>E-mail</dt>
        <dd>{{ test.data.value.user.email ?? "—" }}</dd>
        <dt>Groups</dt>
        <dd>
          <span v-if="test.data.value.user.groups.length === 0" class="muted">None</span>
          <ul v-else class="plain-list">
            <li v-for="g in test.data.value.user.groups" :key="g" class="mono">{{ g }}</li>
          </ul>
        </dd>
        <dt>Would get</dt>
        <dd>
          <span v-if="test.data.value.user.profiles.length === 0" class="badge danger">No profile — sign-in refused</span>
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
</style>
