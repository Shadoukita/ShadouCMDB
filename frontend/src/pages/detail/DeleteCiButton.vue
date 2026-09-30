<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { useDeleteCi, useRelationships, type Ci } from "../../api/queries";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { plural } from "../../lib/format";
import { describeEdge } from "../../lib/relationships";

/** Delete with a confirmation that lists every relationship that will break. */
const props = defineProps<{ ci: Ci }>();
const router = useRouter();
const open = ref(false);
const rels = useRelationships(() => props.ci.id);
const del = useDeleteCi();
const edges = computed(() => (rels.data.value?.data ?? []).map((r) => ({ r, d: describeEdge(r, props.ci.id) })));
const total = computed(() => rels.data.value?.page.total ?? 0);

function cancel() {
  del.reset();
  open.value = false;
}

function confirm() {
  del.mutate(props.ci.id, { onSuccess: () => router.replace("/cis") });
}
</script>

<template>
  <button type="button" class="btn btn-danger" @click="open = true">Delete</button>
  <ConfirmDialog
    :open="open"
    :title="`Delete ${ci.class.name.toLowerCase()} “${ci.label}”?`"
    :confirm-label="total > 0 ? `Delete CI and ${plural(total, 'relationship')}` : 'Delete CI'"
    :busy="del.isPending.value"
    @cancel="cancel"
    @confirm="confirm"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" title="Delete failed" />
    <p>
      <strong dir="auto">{{ ci.label }}</strong> (<bdi>{{ ci.class.name }}</bdi>, <bdi>{{ ci.ident }}</bdi>) will be removed from the
      inventory. The record and its history stay available as a deleted CI.
    </p>
    <LoadingState v-if="rels.isLoading.value" label="Checking relationships…" />
    <ErrorAlert v-if="rels.isError.value" :error="rels.error.value" title="Could not check which relationships would break" />
    <p v-if="rels.data.value && total === 0">It has no relationships, so no other CI is affected.</p>
    <template v-if="rels.data.value && total > 0">
      <p>
        <template v-if="total === 1">This relationship will break:</template>
        <template v-else>These <strong>{{ plural(total, "relationship") }}</strong> will break:</template>
      </p>
      <ul>
        <li v-for="{ r, d } in edges" :key="r.id">
          <bdi>{{ ci.label }}</bdi> <em dir="auto">{{ d.label }}</em> <strong dir="auto">{{ d.other.name }}</strong> <span class="muted">(<bdi>{{ d.other.className }}</bdi>)</span>
        </li>
      </ul>
      <p v-if="total > edges.length" class="muted">…and {{ total - edges.length }} more.</p>
    </template>
  </ConfirmDialog>
</template>
