<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../../api/client";
import { useCreateWebhookAllowedHost, useDeleteWebhookAllowedHost, useWebhookAllowedHosts, type WebhookAllowedHost } from "../../../api/webhooks";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import FormDialog from "../../../components/FormDialog.vue";
import Icon from "../../../components/Icon.vue";
import RowMenu from "../../../components/RowMenu.vue";
import SkeletonRows from "../../../components/SkeletonRows.vue";
import { t } from "../../../i18n";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { useFlashStore } from "../../../stores/flash";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";

/**
 * The administrator's allowlist (SHAA-2725 §5.1): the hosts webhook endpoints may reach. An empty list allows
 * none. Removing an entry suspends the endpoints no remaining entry allows; the answer names them.
 */
const hosts = useWebhookAllowedHosts();
const rows = computed(() => hosts.data.value?.data ?? []);
const flash = useFlashStore();

// ---------- Add ----------
const create = useCreateWebhookAllowedHost();
const adding = ref(false);
const pattern = ref("");
const port = ref("");
const allowHttp = ref(false);
const comment = ref("");
const local = ref<Record<string, string>>({});
const apiError = computed(() => (create.error.value instanceof ApiError ? create.error.value : null));
const errorFor = (f: string) => local.value[f] ?? apiError.value?.fieldErrors()[f];
const unplaced = computed(() => apiError.value?.details.filter((d) => !["hostPattern", "port", "allowHttp", "comment"].includes(d.field)) ?? []);

function openAdd() {
  create.reset();
  pattern.value = "";
  port.value = "";
  allowHttp.value = false;
  comment.value = "";
  local.value = {};
  adding.value = true;
}

function submitAdd() {
  const errs: Record<string, string> = {};
  const host = pattern.value.trim();
  if (!host) errs.hostPattern = t("common.required");
  else if (host === "*" || /^\*[^.]/.test(host) || host.includes("://") || host.includes("/")) errs.hostPattern = t("webhooks.hosts.patternFormat");
  let p: number | null = null;
  if (port.value.trim()) {
    p = Number(port.value);
    if (!Number.isInteger(p) || p < 1 || p > 65535) errs.port = t("webhooks.field.range", { min: 1, max: 65535 });
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) return;
  create.mutate(
    { hostPattern: host, port: p, allowHttp: allowHttp.value, comment: comment.value.trim() || null },
    {
      onSuccess: (h) => {
        flash.show(t("webhooks.hosts.added", { host: h.hostPattern }));
        adding.value = false;
      },
    },
  );
}

// ---------- Remove ----------
const remove = useDeleteWebhookAllowedHost();
const removing = ref<WebhookAllowedHost | null>(null);

function askRemove(h: WebhookAllowedHost) {
  remove.reset();
  removing.value = h;
}

function confirmRemove() {
  const h = removing.value;
  if (!h) return;
  remove.mutate(h.id, {
    onSuccess: (res) => {
      const n = res.suspendedEndpoints.length;
      flash.show(
        n === 0
          ? t("webhooks.hosts.removed", { host: h.hostPattern })
          : t("webhooks.hosts.removedSuspended", { host: h.hostPattern, n, keys: res.suspendedEndpoints.join(", ") }),
      );
      removing.value = null;
    },
  });
}

const hostLabel = (h: WebhookAllowedHost) => (h.port ? `${h.hostPattern}:${h.port}` : h.hostPattern);
</script>

<template>
  <section class="panel" aria-labelledby="wh-hosts-title" data-testid="webhook-allowlist">
    <div class="panel-header">
      <h2 id="wh-hosts-title">{{ t("webhooks.hosts.title") }}</h2>
      <button type="button" class="btn btn-sm" @click="openAdd"><Icon name="plus" />{{ t("webhooks.hosts.add") }}</button>
    </div>
    <div class="panel-body">
      <p class="muted no-margin">{{ t("webhooks.hosts.intro") }}</p>
    </div>
    <div v-if="hosts.isError.value" class="panel-body">
      <ErrorAlert :error="hosts.error.value" :on-retry="() => hosts.refetch()" />
    </div>
    <SkeletonRows v-else-if="hosts.isPending.value" :label="t('common.loading')" />
    <EmptyState v-else-if="rows.length === 0" icon="lock" :title="t('webhooks.hosts.empty.title')">
      {{ t("webhooks.hosts.empty.body") }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="openAdd"><Icon name="plus" />{{ t("webhooks.hosts.add") }}</button>
      </template>
    </EmptyState>
    <div v-else class="table-wrap table-scroll" role="region" tabindex="0" :aria-label="t('webhooks.hosts.title')">
      <table class="data list-table">
        <thead>
          <tr>
            <th scope="col">{{ t("webhooks.hosts.col.host") }}</th>
            <th scope="col">{{ t("webhooks.hosts.col.http") }}</th>
            <th scope="col">{{ t("webhooks.hosts.col.comment") }}</th>
            <th scope="col">{{ t("common.created") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="h in rows" :key="h.id">
            <td><code>{{ hostLabel(h) }}</code></td>
            <td>
              <span v-if="h.allowHttp" class="badge warn">{{ t("webhooks.unencrypted") }}</span>
              <span v-else class="muted">{{ t("webhooks.hosts.httpsOnly") }}</span>
            </td>
            <td dir="auto">{{ h.comment }}</td>
            <td>
              <time :datetime="h.createdAt" :title="t('admin.tokens.createdAtBy', { at: formatDateTime(h.createdAt), by: h.createdByName })">{{ formatRelative(h.createdAt) }}</time>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: hostLabel(h) })" :items="[{ label: t('webhooks.hosts.removeConfirm'), action: () => askRemove(h), danger: true }]" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <FormDialog :open="adding" :title="t('webhooks.hosts.addTitle')" :submit-label="t('webhooks.hosts.addSubmit')" :busy="create.isPending.value" @cancel="adding = false" @submit="submitAdd">
    <FormErrorBanner v-if="create.error.value" :error="create.error.value" :unplaced="unplaced" />
    <div class="form-grid">
      <FormField id="wh-host-pattern" v-slot="p" :label="t('webhooks.hosts.col.host')" required wide :error="errorFor('hostPattern')" :hint="t('webhooks.hosts.patternHint')">
        <input :id="p.id" v-model="pattern" class="mono" type="text" maxlength="253" autocomplete="off" spellcheck="false" placeholder="itsm.example.com" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <FormField id="wh-host-port" v-slot="p" :label="t('webhooks.hosts.port')" :error="errorFor('port')" :hint="t('webhooks.hosts.portHint')">
        <input :id="p.id" v-model="port" type="number" inputmode="numeric" min="1" max="65535" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <FormField id="wh-host-comment" v-slot="p" :label="t('webhooks.hosts.col.comment')" :error="errorFor('comment')">
        <input :id="p.id" v-model="comment" type="text" maxlength="500" autocomplete="off" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <div class="field wide">
        <label class="checkbox-row">
          <input v-model="allowHttp" type="checkbox" aria-describedby="wh-host-http-hint" />
          {{ t("webhooks.hosts.allowHttp") }}
        </label>
        <span id="wh-host-http-hint" class="hint">{{ errorFor("allowHttp") ?? t("webhooks.hosts.allowHttpHint") }}</span>
      </div>
    </div>
  </FormDialog>

  <ConfirmDialog
    :open="!!removing"
    :title="t('webhooks.hosts.removeTitle', { host: removing ? hostLabel(removing) : '' })"
    :confirm-label="t('webhooks.hosts.removeConfirm')"
    :busy="remove.isPending.value"
    @cancel="removing = null"
    @confirm="confirmRemove"
  >
    <ErrorAlert v-if="remove.isError.value" :error="remove.error.value" :title="t('webhooks.hosts.removeFailed')" />
    <p>{{ t("webhooks.hosts.removeBody") }}</p>
  </ConfirmDialog>
</template>
