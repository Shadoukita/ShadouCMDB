<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useDeleteIdentityProvider, useUpdateIdentityProvider, type IdentityProvider } from "../../../api/identityProviders";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";
import { useFlashStore } from "../../../stores/flash";
import { providerKindLabel } from "./providerText";

/**
 * Disable / enable and delete. Both end the sessions of the provider's accounts. A provider that
 * still has accounts cannot be deleted (409 IN_USE): the dialog offers to disable it instead.
 * Delete is opened from the title row's `⋯` menu (`openDelete`), as on the other admin edit pages.
 */
const props = defineProps<{ provider: IdentityProvider }>();
const router = useRouter();
const flash = useFlashStore();
const update = useUpdateIdentityProvider();
const del = useDeleteIdentityProvider();
const confirming = ref<"toggle" | "delete" | null>(null);

const inUse = computed(() => del.error.value instanceof ApiError && del.error.value.code === "IN_USE");

function openToggle() {
  update.reset();
  confirming.value = "toggle";
}

function confirmToggle() {
  const enable = !props.provider.isEnabled;
  const name = props.provider.name;
  update.mutate(
    { id: props.provider.id, body: { isEnabled: enable } },
    {
      onSuccess: () => {
        confirming.value = null;
        flash.show(enable ? t("idp.access.enabledDone", { name }) : t("idp.access.disabledDone", { name }));
      },
    },
  );
}

function openDelete() {
  del.reset();
  update.reset();
  confirming.value = "delete";
}
defineExpose({ openDelete });

function confirmDelete() {
  const name = props.provider.name;
  del.mutate(props.provider.id, {
    onSuccess: () => {
      flash.show(t("idp.access.deletedDone", { name }));
      void router.replace("/admin/identity-providers");
    },
  });
}

/** From the IN_USE answer: disable instead of deleting. */
function disableInstead() {
  const name = props.provider.name;
  update.mutate(
    { id: props.provider.id, body: { isEnabled: false } },
    {
      onSuccess: () => {
        confirming.value = null;
        flash.show(t("idp.access.disabledDone", { name }));
      },
    },
  );
}
</script>

<template>
  <section class="panel" aria-labelledby="provider-access-title">
    <div class="panel-header"><h2 id="provider-access-title">{{ t("idp.access.title") }}</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">
        {{ t("idp.access.accounts", { n: provider.userCount }) }}
        {{ provider.isEnabled ? t("idp.access.enabledNote") : t("idp.access.disabledNote") }}
      </p>
      <div>
        <button type="button" class="btn" @click="openToggle">{{ provider.isEnabled ? t("idp.access.disable") : t("idp.access.enable") }}</button>
      </div>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming === 'toggle'"
    :title="provider.isEnabled ? t('idp.access.disableTitle', { name: provider.name }) : t('idp.access.enableTitle', { name: provider.name })"
    :confirm-label="provider.isEnabled ? t('idp.access.disable') : t('idp.access.enable')"
    :busy="update.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmToggle"
  >
    <ErrorAlert v-if="update.isError.value" :error="update.error.value" :title="provider.isEnabled ? t('idp.access.notDisabled') : t('idp.access.notEnabled')" />
    <p v-if="provider.isEnabled">{{ t("idp.access.disableBody", { name: provider.name, n: provider.userCount }) }}</p>
    <p v-else>{{ t("idp.access.enableBody", { name: provider.name }) }}</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="confirming === 'delete'"
    :title="t('idp.access.deleteTitle', { name: provider.name })"
    :confirm-label="t('idp.access.delete')"
    :busy="del.isPending.value || update.isPending.value"
    @cancel="confirming = null"
    @confirm="confirmDelete"
  >
    <div v-if="inUse" class="alert alert-warn" role="alert">
      <strong>{{ t("idp.access.inUseTitle", { name: provider.name }) }}</strong>
      <div>{{ del.error.value instanceof Error ? del.error.value.message : "" }} {{ t("idp.access.inUseBody") }}</div>
      <div class="meta">
        <button v-if="provider.isEnabled" type="button" class="btn btn-sm" :disabled="update.isPending.value" @click="disableInstead">
          {{ t("idp.access.disableInstead") }}
        </button>
        <span v-else>{{ t("idp.access.alreadyDisabled") }}</span>
      </div>
    </div>
    <ErrorAlert v-else-if="del.isError.value" :error="del.error.value" :title="t('idp.access.deleteFailed')" />
    <ErrorAlert v-if="update.isError.value" :error="update.error.value" :title="t('idp.access.notDisabled')" />
    <p>{{ t("idp.access.deleteBody", { name: provider.name, kind: providerKindLabel(provider.kind), n: provider.groupMappings.length }) }}</p>
    <p v-if="provider.userCount > 0">{{ t("idp.access.deleteRefused", { n: provider.userCount }) }}</p>
  </ConfirmDialog>
</template>
