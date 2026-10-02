<script setup lang="ts">
import { computed, onMounted, ref, useId, watch } from "vue";
import { useSavedViews, type SavedView } from "../../api/savedViews";
import { formatDateTime } from "../../lib/format";
import { homeName } from "../../lib/savedViews";
import ErrorAlert from "../ErrorAlert.vue";
import LoadingState from "../LoadingState.vue";

/**
 * Manage views (§1.2): the user's views of both contexts and, with `views.share`,
 * the shared views, with rename, delete and set-default per row. The row actions
 * open their dialogs on top of this one (the parent owns them).
 */
const props = defineProps<{
  open: boolean;
  classes: readonly { key: string; name: string }[] | undefined;
  canShare: boolean;
}>();
const emit = defineEmits<{ close: []; rename: [view: SavedView]; delete: [view: SavedView]; setDefault: [view: SavedView, on: boolean] }>();

const dialog = ref<HTMLDialogElement>();
const titleId = `manage-views-${useId()}`;
const list = useSavedViews("all", () => props.open);
const personal = computed(() => list.data.value?.data.filter((v) => v.visibility === "personal") ?? []);
const shared = computed(() => (props.canShare ? (list.data.value?.data.filter((v) => v.visibility === "shared") ?? []) : []));
const limits = computed(() => list.data.value?.limits);

function sync() {
  const d = dialog.value;
  if (!d) return;
  if (props.open && !d.open) d.showModal();
  if (!props.open && d.open) d.close();
}
onMounted(sync);
watch(() => props.open, sync, { flush: "post" });

function onCancel(e: Event) {
  e.preventDefault();
  emit("close");
}
const contextLabel = (v: SavedView) => (v.context === "inventory" ? "Inventory" : "Search");
const listLabel = (v: SavedView) => (v.context === "search" ? "—" : homeName(v, props.classes));
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm form-dialog wide manage-views" :aria-labelledby="titleId" @cancel="onCancel">
      <h2 :id="titleId">Manage views</h2>
      <div v-if="open" class="body stack">
        <LoadingState v-if="list.isPending.value" label="Loading saved views…" />
        <ErrorAlert v-else-if="list.isError.value" :error="list.error.value" title="Saved views could not be loaded" :on-retry="() => list.refetch()" />
        <template v-else>
          <section v-for="g in [{ key: 'personal', title: 'My views', rows: personal }, ...(canShare ? [{ key: 'shared', title: 'Shared views', rows: shared }] : [])]" :key="g.key">
            <h3 class="manage-views-heading">
              {{ g.title }}
              <span v-if="limits" class="muted">
                ({{ (g.key === "personal" ? limits.personal : limits.shared).used }} of {{ (g.key === "personal" ? limits.personal : limits.shared).max }})
              </span>
            </h3>
            <p v-if="g.rows.length === 0" class="muted">
              {{ g.key === "personal" ? "You have no saved views yet. Set filters, sort and columns on a list, then choose Save as new view in its View menu." : "No views are shared yet." }}
            </p>
            <div v-else class="table-wrap">
              <table class="data">
                <caption class="sr-only">{{ g.title }}</caption>
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">Context</th>
                    <th scope="col">List</th>
                    <th scope="col">Default</th>
                    <th scope="col">Updated</th>
                    <th v-if="g.key === 'shared'" scope="col">Default for</th>
                    <th scope="col"><span class="sr-only">Actions</span></th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="v in g.rows" :key="v.id">
                    <td>
                      {{ v.name }}
                      <span v-if="v.resolved.state === 'unavailable'" class="badge" title="This view refers to a filter that no longer exists.">Unavailable</span>
                      <span v-else-if="v.resolved.state === 'degraded'" class="badge">Partly unavailable</span>
                    </td>
                    <td>{{ contextLabel(v) }}</td>
                    <td>{{ listLabel(v) }}</td>
                    <td>{{ v.isDefault ? "Yes" : "" }}</td>
                    <td>{{ formatDateTime(v.updatedAt) }} <span class="muted">by {{ v.updatedBy.name }}</span></td>
                    <td v-if="g.key === 'shared'">{{ v.defaultCount === undefined ? "" : `${v.defaultCount.toLocaleString()} ${v.defaultCount === 1 ? "user" : "users"}` }}</td>
                    <td class="row-actions">
                      <button v-if="v.canEdit" type="button" class="btn btn-sm" :aria-label="`Rename ${v.name}`" @click="emit('rename', v)">Rename</button>
                      <button
                        v-if="v.context === 'inventory' && v.isDefault"
                        type="button"
                        class="btn btn-sm"
                        :aria-label="`Clear ${v.name} as my default`"
                        @click="emit('setDefault', v, false)"
                      >
                        Clear default
                      </button>
                      <button
                        v-else-if="v.context === 'inventory' && v.resolved.state !== 'unavailable'"
                        type="button"
                        class="btn btn-sm"
                        :aria-label="`Set ${v.name} as my default for ${homeName(v, classes)}`"
                        @click="emit('setDefault', v, true)"
                      >
                        Set as default
                      </button>
                      <button v-if="v.canEdit" type="button" class="btn btn-sm btn-danger" :aria-label="`Delete ${v.name}`" @click="emit('delete', v)">Delete</button>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </section>
        </template>
      </div>
      <div class="footer">
        <button type="button" class="btn btn-primary" @click="emit('close')">Close</button>
      </div>
    </dialog>
  </Teleport>
</template>
