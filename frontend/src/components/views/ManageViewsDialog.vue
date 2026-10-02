<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useSavedViews, type SavedView } from "../../api/savedViews";
import { formatDateTime } from "../../lib/format";
import { groupViews, homeLabel } from "../../lib/savedViews";
import ErrorAlert from "../ErrorAlert.vue";
import LoadingState from "../LoadingState.vue";

/**
 * Manage views (§1.2): the user's views of both the inventory and search, and for
 * users with views.share the shared ones with how many users have each as their
 * default. Each row can be renamed, deleted and (inventory views) set as or cleared
 * from the user's default; the parent opens those dialogs over this one.
 */
const props = defineProps<{ open: boolean; canShare: boolean; classes: readonly { key: string; name: string }[] }>();
const emit = defineEmits<{ close: []; rename: [view: SavedView]; delete: [view: SavedView]; setDefault: [view: SavedView]; clearDefault: [view: SavedView] }>();

const inventory = useSavedViews("inventory", () => props.open);
const search = useSavedViews("search", () => props.open);
const all = computed(() => [...(inventory.data.value?.data ?? []), ...(search.data.value?.data ?? [])]);
const groups = computed(() => groupViews(all.value));
const loading = computed(() => inventory.isPending.value || search.isPending.value);
const error = computed(() => inventory.error.value ?? search.error.value);
const limits = computed(() => inventory.data.value?.limits);
const sections = computed(() => [
  { key: "mine", title: "My views", views: groups.value.mine },
  ...(props.canShare ? [{ key: "shared", title: "Shared views", views: groups.value.shared }] : []),
]);

const dialog = ref<HTMLDialogElement>();
const closeButton = ref<HTMLButtonElement>();
let opener: HTMLElement | null = null;
watch(
  () => props.open,
  async (open) => {
    const d = dialog.value;
    if (!d) return;
    if (open && !d.open) {
      opener = document.activeElement as HTMLElement | null;
      d.showModal();
      await nextTick();
      closeButton.value?.focus();
    } else if (!open && d.open) {
      d.close();
      opener?.focus();
      opener = null;
    }
  },
  { flush: "post" },
);
function cancel(e: Event) {
  e.preventDefault();
  emit("close");
}
const where = (v: SavedView) => (v.context === "inventory" ? "Inventory" : "Search");
const slot = (v: SavedView) => (v.context === "search" ? "—" : homeLabel(v.home, props.classes));
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm view-dialog manage-views" aria-labelledby="manage-views-title" aria-modal="true" @cancel="cancel">
      <h2 id="manage-views-title">Manage views</h2>
      <div class="body stack">
        <p v-if="limits" class="muted dialog-intro">
          You have {{ limits.personal.used }} of {{ limits.personal.max }} personal views<template v-if="canShare">; the instance has {{ limits.shared.used }} of {{ limits.shared.max }} shared views</template>.
        </p>
        <LoadingState v-if="loading" label="Loading views…" />
        <ErrorAlert v-else-if="error" :error="error" title="Saved views could not be loaded" :on-retry="() => (inventory.refetch(), search.refetch())" />
        <template v-else>
          <section v-for="s in sections" :key="s.key" :aria-labelledby="`manage-${s.key}`">
            <h3 :id="`manage-${s.key}`" class="manage-heading">{{ s.title }}</h3>
            <p v-if="s.views.length === 0" class="muted dialog-intro">
              {{ s.key === "mine" ? "You have no saved views. Set filters, sort and columns, then choose Save as new view in the View menu." : "No views are shared." }}
            </p>
            <div v-else class="table-wrap">
              <table class="data">
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">Context</th>
                    <th scope="col">Class</th>
                    <th scope="col">Default</th>
                    <th v-if="s.key === 'shared'" scope="col">Default for</th>
                    <th scope="col">Updated</th>
                    <th scope="col"><span class="sr-only">Actions</span></th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="v in s.views" :key="v.id">
                    <th scope="row" class="name-cell">
                      <span dir="auto">{{ v.name }}</span>
                      <span v-if="v.resolved.state === 'unavailable'" class="badge off spaced" title="This view refers to a filter that no longer exists.">Unavailable</span>
                      <span v-else-if="v.resolved.state === 'degraded'" class="badge warn spaced">Changed</span>
                    </th>
                    <td>{{ where(v) }}</td>
                    <td>{{ slot(v) }}</td>
                    <td>{{ v.isDefault ? "Yes" : "" }}</td>
                    <td v-if="s.key === 'shared'">{{ v.defaultCount === undefined ? "—" : v.defaultCount === 1 ? "1 user" : `${v.defaultCount} users` }}</td>
                    <td>{{ formatDateTime(v.updatedAt) }} <span class="muted">by {{ v.updatedBy.name }}</span></td>
                    <td class="row-actions">
                      <button v-if="v.canEdit" type="button" class="btn btn-sm" :aria-label="`Rename ${v.name}`" @click="emit('rename', v)">Rename</button>
                      <template v-if="v.context === 'inventory'">
                        <button v-if="v.isDefault" type="button" class="btn btn-sm" :aria-label="`Clear ${v.name} as my default`" @click="emit('clearDefault', v)">
                          Clear default
                        </button>
                        <button
                          v-else-if="v.resolved.state !== 'unavailable'"
                          type="button"
                          class="btn btn-sm"
                          :aria-label="`Set ${v.name} as my default for ${slot(v)}`"
                          @click="emit('setDefault', v)"
                        >
                          Set default
                        </button>
                      </template>
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
        <button ref="closeButton" type="button" class="btn" @click="emit('close')">Close</button>
      </div>
    </dialog>
  </Teleport>
</template>
