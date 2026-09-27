<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useDeleteIdentityProvider, useUpdateIdentityProvider, type IdentityProvider } from "../../../api/identityProviders";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { plural } from "../../../lib/format";

/**
 * Disable / enable and delete. Both end the sessions of the provider's accounts. A provider that
 * still has accounts cannot be deleted (409 IN_USE): the dialog offers to disable it instead.
 */
const props = defineProps<{ provider: IdentityProvider }>();
const router = useRouter();
const update = useUpdateIdentityProvider();
const del = useDeleteIdentityProvider();
const confirming = ref<"toggle" | "delete" | null>(null);
const done = ref<string | null>(null);

const accounts = computed(() => plural(props.provider.userCount, "account"));
const inUse = computed(() => del.error.value instanceof ApiError && del.error.value.code === "IN_USE");

function openToggle() {
  update.reset();
  done.value = null;
  confirming.value = "toggle";
}

function confirmToggle() {
  const enable = !props.provider.isEnabled;
  update.mutate(
    { id: props.provider.id, body: { isEnabled: enable } },
    {
      onSuccess: () => {
        confirming.value = null;
        const name = props.provider.name;
        done.value = enable ? `${name} is enabled.` : `${name} is disabled. Its accounts were signed out.`;
      },
    },
  );
}

function openDelete() {
  del.reset();
  update.reset();
  done.value = null;
  confirming.value = "delete";
}

function confirmDelete() {
  del.mutate(props.provider.id, { onSuccess: () => router.replace("/admin/identity-providers") });
}

/** From the IN_USE answer: disable instead of deleting. */
function disableInstead() {
  update.mutate(
    { id: props.provider.id, body: { isEnabled: false } },
    {
      onSuccess: () => {
        confirming.value = null;
        done.value = `${props.provider.name} is disabled. Its accounts were signed out.`;
      },
    },
  );
}
</script>

<template>
  <section class="panel" aria-labelledby="provider-access-title">
    <div class="panel-header"><h2 id="provider-access-title">Availability</h2></div>
    <div class="panel-body stack">
      <div v-if="done" class="alert" role="status">{{ done }}</div>
      <p class="muted" style="margin: 0">
        {{ accounts }} {{ provider.userCount === 1 ? "signs" : "sign" }} in through this provider.
        <template v-if="provider.isEnabled">Disabling it stops those sign-ins and ends their sessions; local accounts keep working.</template>
        <template v-else>It is disabled: nobody signs in through it.</template>
      </p>
      <div class="actions">
        <button v-if="provider.isEnabled" type="button" class="btn" @click="openToggle">Disable provider</button>
        <button v-else type="button" class="btn" @click="openToggle">Enable provider</button>
        <button type="button" class="btn btn-danger" @click="openDelete">Delete provider</button>
      </div>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming === 'toggle'"
    :title="provider.isEnabled ? `Disable ${provider.name}?` : `Enable ${provider.name}?`"
    :confirm-label="provider.isEnabled ? 'Disable provider' : 'Enable provider'"
    :busy="update.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmToggle"
  >
    <ErrorAlert v-if="update.isError.value" :error="update.error.value" :title="provider.isEnabled ? 'Not disabled' : 'Not enabled'" />
    <p v-if="provider.isEnabled">
      Nobody can sign in through <strong>{{ provider.name }}</strong> until it is enabled again, and the sessions of its
      {{ accounts }} end now. The accounts, their history and the settings are kept.
    </p>
    <p v-else>
      Users can sign in through <strong>{{ provider.name }}</strong> again, with the profiles their groups map to.
    </p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="confirming === 'delete'"
    :title="`Delete identity provider ${provider.name}?`"
    confirm-label="Delete provider"
    :busy="del.isPending.value || update.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmDelete"
  >
    <div v-if="inUse" class="alert alert-warn" role="alert">
      <strong>{{ provider.name }} still has accounts.</strong>
      <div>
        {{ del.error.value instanceof Error ? del.error.value.message : "" }} Disable it instead: nobody can sign in
        through it, and its accounts keep their names on past changes.
      </div>
      <div class="meta">
        <button v-if="provider.isEnabled" type="button" class="btn btn-sm" :disabled="update.isPending.value" @click="disableInstead">
          Disable instead
        </button>
        <span v-else>It is already disabled.</span>
      </div>
    </div>
    <ErrorAlert v-else-if="del.isError.value" :error="del.error.value" title="Delete failed" />
    <ErrorAlert v-if="update.isError.value" :error="update.error.value" title="Not disabled" />
    <p>
      Removes <strong>{{ provider.name }}</strong> ({{ provider.kind === "oidc" ? "OpenID Connect" : "LDAP / Active Directory" }})
      with its settings, stored secrets and {{ plural(provider.groupMappings.length, "group mapping") }}. This cannot be
      undone.
    </p>
    <p v-if="provider.userCount > 0">
      {{ accounts }} {{ provider.userCount === 1 ? "signs" : "sign" }} in through it, so the server will refuse to delete
      it. Disable it instead.
    </p>
  </ConfirmDialog>
</template>
