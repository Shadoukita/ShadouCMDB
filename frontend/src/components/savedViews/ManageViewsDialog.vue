<script setup lang="ts">
import { computed, onMounted, ref, useId, watch } from "vue";
import { useSavedViews, type SavedView } from "../../api/savedViews";
import { formatDateTime } from "../../lib/format";
import { t } from "../../i18n";
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
const contextLabel = (v: SavedView) => t(v.context === "inventory" ? "views.manage.context.inventory" : "views.manage.context.search");
const listLabel = (v: SavedView) => (v.context === "search" ? "—" : homeName(v, props.classes));
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm form-dialog wide manage-views" :aria-labelledby="titleId" @cancel="onCancel">
      <h2 :id="titleId">{{ t("views.manage.title") }}</h2>
      <div v-if="open" class="body stack">
        <LoadingState v-if="list.isPending.value" :label="t('views.loading')" />
        <ErrorAlert v-else-if="list.isError.value" :error="list.error.value" :title="t('views.loadFailed.title')" :on-retry="() => list.refetch()" />
        <template v-else>
          <section v-for="g in [{ key: 'personal', title: t('views.mine'), rows: personal }, ...(canShare ? [{ key: 'shared', title: t('views.shared'), rows: shared }] : [])]" :key="g.key">
            <h3 class="manage-views-heading">
              {{ g.title }}
              <span v-if="limits" class="muted">
                {{ t("views.manage.usage", { used: (g.key === "personal" ? limits.personal : limits.shared).used, max: (g.key === "personal" ? limits.personal : limits.shared).max }) }}
              </span>
            </h3>
            <p v-if="g.rows.length === 0" class="muted">
              {{ g.key === "personal" ? t("views.manage.noneMine") : t("views.manage.noneShared") }}
            </p>
            <div v-else class="table-wrap">
              <table class="data">
                <caption class="sr-only">{{ g.title }}</caption>
                <thead>
                  <tr>
                    <th scope="col">{{ t("views.manage.col.name") }}</th>
                    <th scope="col">{{ t("views.manage.col.context") }}</th>
                    <th scope="col">{{ t("views.manage.col.list") }}</th>
                    <th scope="col">{{ t("views.manage.col.default") }}</th>
                    <th scope="col">{{ t("views.manage.col.updated") }}</th>
                    <th v-if="g.key === 'shared'" scope="col">{{ t("views.manage.col.defaultFor") }}</th>
                    <th scope="col" class="row-actions"><span class="sr-only">{{ t("views.manage.col.actions") }}</span></th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="v in g.rows" :key="v.id">
                    <td>
                      {{ v.name }}
                      <span v-if="v.resolved.state === 'unavailable'" class="badge" :title="t('views.unavailable.hint')">{{ t("views.manage.unavailable") }}</span>
                      <span v-else-if="v.resolved.state === 'degraded'" class="badge">{{ t("views.manage.degraded") }}</span>
                    </td>
                    <td>{{ contextLabel(v) }}</td>
                    <td>{{ listLabel(v) }}</td>
                    <td>{{ v.isDefault ? t("common.yes") : "" }}</td>
                    <td>{{ formatDateTime(v.updatedAt) }} <span class="muted">{{ t("views.manage.by", { name: v.updatedBy.name }) }}</span></td>
                    <td v-if="g.key === 'shared'">{{ v.defaultCount === undefined ? "" : t("views.manage.users", { n: v.defaultCount }) }}</td>
                    <td class="row-actions">
                      <button v-if="v.canEdit" type="button" class="btn btn-sm" :aria-label="t('views.manage.rename.label', { name: v.name })" @click="emit('rename', v)">{{ t("views.manage.rename") }}</button>
                      <button
                        v-if="v.context === 'inventory' && v.isDefault"
                        type="button"
                        class="btn btn-sm"
                        :aria-label="t('views.manage.clearDefault.label', { name: v.name })"
                        @click="emit('setDefault', v, false)"
                      >
                        {{ t("views.manage.clearDefault") }}
                      </button>
                      <button
                        v-else-if="v.context === 'inventory' && v.resolved.state !== 'unavailable'"
                        type="button"
                        class="btn btn-sm"
                        :aria-label="t('views.manage.setDefault.label', { name: v.name, list: homeName(v, classes) })"
                        @click="emit('setDefault', v, true)"
                      >
                        {{ t("views.manage.setDefault") }}
                      </button>
                      <button v-if="v.canEdit" type="button" class="btn btn-sm btn-danger" :aria-label="t('views.manage.delete.label', { name: v.name })" @click="emit('delete', v)">{{ t("views.manage.delete") }}</button>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </section>
        </template>
      </div>
      <div class="footer">
        <button type="button" class="btn btn-primary" @click="emit('close')">{{ t("views.manage.close") }}</button>
      </div>
    </dialog>
  </Teleport>
</template>
