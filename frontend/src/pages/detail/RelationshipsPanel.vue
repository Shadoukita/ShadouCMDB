<script setup lang="ts">
import { computed, ref } from "vue";
import { useCiClasses, useDeleteRelationship, useRelationships, type Ci, type Relationship } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { describeEdge } from "../../lib/relationships";
import type { TrailStep } from "../../lib/trail";
import { useSessionStore } from "../../stores/session";
import AddRelationshipForm from "./AddRelationshipForm.vue";

const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[] }>();
const rels = useRelationships(() => props.ci.id);
const removing = ref<Relationship | null>(null);
const del = useDeleteRelationship();
const session = useSessionStore();
const classes = useCiClasses();
// A relationship belongs to its source CI: adding or removing one needs the edit right on the source's class.
const classIdByKey = computed(() => new Map((classes.data.value ?? []).map((c) => [c.key, c.id])));
const mayRemove = (r: Relationship) => session.canOnClass(classIdByKey.value.get(r.source.classKey), "edit");
const mayAdd = computed(() => session.canOnAnyClass("edit"));
const rows = computed(() =>
  [...(rels.data.value?.data ?? [])]
    .map((r) => ({ r, d: describeEdge(r, props.ci.id) }))
    .sort((a, b) => a.d.label.localeCompare(b.d.label) || a.d.other.name.localeCompare(b.d.other.name)),
);

function cancelRemove() {
  del.reset();
  removing.value = null;
}

function confirmRemove() {
  if (removing.value) del.mutate(removing.value.id, { onSuccess: () => (removing.value = null) });
}
</script>

<template>
  <section class="panel" aria-labelledby="rel-title">
    <div class="panel-header">
      <h2 id="rel-title">Relationships</h2>
      <span v-if="rels.data.value" class="muted">{{ rels.data.value.page.total }} direct</span>
    </div>
    <div class="panel-body flush">
      <LoadingState v-if="rels.isLoading.value" label="Loading relationships…" />
      <div v-if="rels.isError.value" class="panel-body">
        <ErrorAlert :error="rels.error.value" :on-retry="() => rels.refetch()" />
      </div>
      <EmptyState v-if="rels.data.value && rows.length === 0" title="No relationships yet">
        {{
          ci.deletedAt
            ? "Deleted CIs keep no live relationships."
            : mayAdd
              ? "Relate this CI to the things it runs on, depends on, or is located in using the form below."
              : "This CI is not related to anything yet."
        }}
      </EmptyState>
      <div v-if="rows.length > 0" class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">This CI…</th>
              <th scope="col">Related CI</th>
              <th scope="col">Class</th>
              <th scope="col">Direction</th>
              <th scope="col">Notes</th>
              <th v-if="!ci.deletedAt" scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="{ r, d } in rows" :key="r.id">
              <td>{{ d.label }}</td>
              <td>
                <CiLink :id="d.other.id" :from="self" :trail="trail">{{ d.other.name }}</CiLink>
                <span v-if="d.other.deleted" class="badge danger"> deleted</span>
              </td>
              <td>{{ d.other.className }}</td>
              <td class="muted">{{ r.type.isDirectional ? (d.outgoing ? "outgoing →" : "← incoming") : "↔" }}</td>
              <td :title="r.notes ?? undefined">{{ r.notes ?? "" }}</td>
              <td v-if="!ci.deletedAt" class="num">
                <button
                  v-if="mayRemove(r)"
                  type="button"
                  class="btn-link danger"
                  :aria-label="`Remove relationship: ${ci.name} ${d.label} ${d.other.name}`"
                  @click="removing = r"
                >
                  Remove
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <AddRelationshipForm v-if="!ci.deletedAt && mayAdd" :ci="ci" />
    </div>
    <ConfirmDialog
      :open="!!removing"
      title="Remove relationship?"
      confirm-label="Remove relationship"
      :busy="del.isPending.value"
      @cancel="cancelRemove"
      @confirm="confirmRemove"
    >
      <ErrorAlert v-if="del.isError.value" :error="del.error.value" title="Remove failed" />
      <p v-if="removing">
        <strong>{{ removing.source.name }}</strong> <em>{{ removing.type.forwardLabel }}</em>
        <strong>{{ removing.target.name }}</strong> will be removed. Neither CI is deleted.
      </p>
    </ConfirmDialog>
  </section>
</template>
