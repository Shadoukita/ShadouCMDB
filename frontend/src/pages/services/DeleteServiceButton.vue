<script setup lang="ts">
import { computed, ref } from "vue";
import { useQueryClient } from "@tanstack/vue-query";
import { useRouter } from "vue-router";
import { useDeleteCi } from "../../api/queries";
import { serviceKeys, useServicesOfCi, type Service } from "../../api/services";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { canConfirmDelete, directParentCount, parentCheckState } from "../../lib/serviceDelete";
import ServiceError from "./ServiceError.vue";

/**
 * Deleting a business service is a CI delete (spec §1.1): the service and its membership edges go, the
 * member CIs stay. The confirmation names the memberships the caller can see, says when there may be
 * others (a restricted profile; static, so it reveals nothing) and which services it is nested in.
 */
const props = defineProps<{ service: Service }>();
const router = useRouter();
const qc = useQueryClient();
const open = ref(false);
const del = useDeleteCi();
const parents = useServicesOfCi(() => props.service.id, open);
const parentCount = computed(() => directParentCount(parents.data.value?.data));
/** Confirm waits for the "included in N services" check: the operator sees that sentence first (GH#477). */
const check = computed(() => parentCheckState({ isError: parents.isError.value, isSuccess: parents.isSuccess.value }));

function cancel() {
  del.reset();
  open.value = false;
}

function confirm() {
  del.mutate(props.service.id, {
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: serviceKeys.all });
      void router.replace("/services");
    },
  });
}
</script>

<template>
  <button type="button" class="btn btn-danger" @click="open = true">{{ t("common.delete") }}</button>
  <ConfirmDialog
    :open="open"
    :title="t('services.delete.title', { name: service.name })"
    :confirm-label="t('services.delete.confirm')"
    :cancel-label="t('common.cancel')"
    :busy-label="t('common.deleting')"
    :busy="del.isPending.value"
    :confirm-disabled="!canConfirmDelete(check)"
    @cancel="cancel"
    @confirm="confirm"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('services.delete.failed')" />
    <p>
      {{ t("services.delete.body", { n: service.memberCount }) }}
      <template v-if="service.visibility === 'restricted'">{{ t("services.delete.restricted") }}</template>
    </p>
    <ServiceError v-if="check === 'failed'" :error="parents.error.value" :title="t('services.delete.checkFailed')" :on-retry="() => parents.refetch()" />
    <LoadingState v-else-if="check === 'pending'" :label="t('services.delete.checking')" />
    <p v-else-if="parentCount > 0">{{ t("services.delete.nested", { n: parentCount }) }}</p>
  </ConfirmDialog>
</template>
