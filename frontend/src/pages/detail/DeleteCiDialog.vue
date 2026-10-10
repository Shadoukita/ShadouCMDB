<script setup lang="ts">
import { computed } from "vue";
import { useRouter } from "vue-router";
import { useIsPersonClass, useSignInAccount } from "../../api/admin";
import { useDeleteCi, useRelationships, type Ci } from "../../api/queries";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { currentLocale, t, tAround } from "../../i18n";
import { describeEdge } from "../../lib/relationships";

/** The delete confirmation: it lists every relationship that will break. Opened from the record's actions menu. */
const props = defineProps<{ ci: Ci }>();
const open = defineModel<boolean>("open", { required: true });
const router = useRouter();
const rels = useRelationships(() => props.ci.id);
const del = useDeleteCi();
const edges = computed(() => (rels.data.value?.data ?? []).map((r) => ({ r, d: describeEdge(r, props.ci.id) })));
const total = computed(() => rels.data.value?.page.total ?? 0);
// A Person linked to a sign-in account cannot be deleted (409 person_linked): say so before the operator confirms.
const isPerson = useIsPersonClass(() => props.ci.classId);
const signInAccount = useSignInAccount(() => props.ci.id, () => open.value && isPerson.value);
const linkedTo = computed(() => signInAccount.data.value?.account ?? null);
// English writes the class in lower case mid-sentence ("Delete server …"); German keeps the noun as it is.
const className = computed(() => (currentLocale() === "en" ? props.ci.class.name.toLowerCase() : props.ci.class.name));
const bodyParts = computed(() => tAround("record.delete.body", "name"));
const breakParts = computed(() => tAround("record.delete.breakMany", "count"));

function cancel() {
  del.reset();
  open.value = false;
}

function confirm() {
  del.mutate(props.ci.id, { onSuccess: () => router.replace("/cis") });
}
</script>

<template>
  <ConfirmDialog
    :open="open"
    :title="t('record.delete.title', { class: className, name: ci.label })"
    :confirm-label="total > 0 ? t('record.delete.confirmWithRels', { n: total }) : t('record.delete.confirm')"
    :busy="del.isPending.value"
    :confirm-disabled="!!linkedTo || (isPerson && signInAccount.isPending.value)"
    @cancel="cancel"
    @confirm="confirm"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('record.delete.failed')" />
    <div v-if="linkedTo" class="alert alert-warn" role="alert" data-testid="person-linked">
      {{ t("people.delete.linked", { username: linkedTo.username }) }}
    </div>
    <p>
      {{ bodyParts[0] }}<strong dir="auto">{{ ci.label }}</strong> (<bdi>{{ ci.class.name }}</bdi>, <bdi>{{ ci.ident }}</bdi>){{ bodyParts[1] }}
    </p>
    <LoadingState v-if="rels.isLoading.value" :label="t('record.delete.checking')" />
    <ErrorAlert v-if="rels.isError.value" :error="rels.error.value" :title="t('record.delete.checkFailed')" />
    <p v-if="rels.data.value && total === 0">{{ t("record.delete.noRelationships") }}</p>
    <template v-if="rels.data.value && total > 0">
      <p>
        <template v-if="total === 1">{{ t("record.delete.breakOne") }}</template>
        <template v-else>{{ breakParts[0] }}<strong>{{ t("record.delete.relCount", { n: total }) }}</strong>{{ breakParts[1] }}</template>
      </p>
      <ul>
        <li v-for="{ r, d } in edges" :key="r.id">
          <bdi>{{ ci.label }}</bdi> <em dir="auto">{{ d.label }}</em> <strong dir="auto">{{ d.other.name }}</strong> <span class="muted">(<bdi>{{ d.other.className }}</bdi>)</span>
        </li>
      </ul>
      <p v-if="total > edges.length" class="muted">{{ t("record.delete.more", { n: total - edges.length }) }}</p>
    </template>
  </ConfirmDialog>
</template>
