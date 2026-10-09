<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from "vue";
import type { CiSummary } from "../../../api/queries";
import { previewAction, type WorkflowActionPreview } from "../../../api/workflows";
import CiPicker from "../../../components/CiPicker.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";

/**
 * Who a stored notification action would tell now, for one CI or in general (SHAA-2725 §11.1): the API
 * resolves the recipient sources and judges each user's right to view the CI's type; the UI only shows
 * its answer. Nothing is sent. A CI the admin cannot view is a 404, so the preview reveals nothing the
 * admin could not already see.
 */
const props = defineProps<{
  workflowId: string;
  classId: string;
  /** The stored actions (by key) that tell people: webhooks go to an endpoint, not to users. */
  actions: { key: string; name: string }[];
  /** Unsaved changes: the preview resolves the stored actions. */
  dirty: boolean;
  /** The action to preview first, e.g. the one being edited. */
  selectedKey?: string;
}>();

const key = ref(props.selectedKey ?? props.actions[0]?.key ?? "");
watch(
  () => [props.actions, props.selectedKey] as const,
  ([list, sel]) => {
    if (sel && list.some((a) => a.key === sel)) key.value = sel;
    else if (!list.some((a) => a.key === key.value)) key.value = list[0]?.key ?? "";
  },
);
const ci = ref<{ id: string; name: string } | null>(null);
const result = ref<WorkflowActionPreview | null>(null);
const error = ref<unknown>(null);
const loading = ref(false);
/** The summary line for screen readers, in a live region that is always in the DOM: a region inserted
 * together with its text is not announced, so the first run and the run after an error would be silent. */
const announcement = ref("");
let controller: AbortController | undefined;
onBeforeUnmount(() => controller?.abort());

function pickCi(c: CiSummary | null) {
  ci.value = c ? { id: c.id, name: c.label } : null;
}

async function run() {
  if (!key.value) return;
  controller?.abort();
  const c = new AbortController();
  controller = c;
  loading.value = true;
  error.value = null;
  announcement.value = "";
  try {
    const res = await previewAction(props.workflowId, key.value, ci.value?.id, c.signal);
    if (!c.signal.aborted) {
      result.value = res;
      announcement.value = summary(res);
    }
  } catch (e) {
    if (!c.signal.aborted) {
      error.value = e;
      result.value = null;
    }
  } finally {
    if (controller === c) loading.value = false;
  }
}

function summary(r: WorkflowActionPreview): string {
  const parts = [t("wfActions.preview.included", { n: r.included })];
  if (!r.ciId) parts.push(t("wfPreview.general"));
  if (r.excludesActor) parts.push(t("wfActions.preview.excludesActor"));
  return parts.join(" · ");
}

const reasonTone: Record<string, string> = { included: "ok", no_view: "danger", inactive: "off", truncated: "warn" };
</script>

<template>
  <section class="panel" aria-labelledby="wf-action-preview-title" data-testid="wf-action-preview">
    <div class="panel-header"><h2 id="wf-action-preview-title">{{ t("wfActions.preview.title") }}</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfActions.preview.intro") }}</p>
      <div v-if="dirty" class="alert alert-warn" role="note">{{ t("wfActions.preview.unsaved") }}</div>
      <p v-if="actions.length === 0" class="muted no-margin">{{ t("wfActions.preview.none") }}</p>
      <form v-else class="wf-preview-form" @submit.prevent="run">
        <div class="field">
          <label for="wf-action-preview-action">{{ t("wfActions.preview.action") }}</label>
          <select id="wf-action-preview-action" v-model="key">
            <option v-for="a in actions" :key="a.key" :value="a.key">{{ a.name }} ({{ a.key }})</option>
          </select>
        </div>
        <div class="field">
          <label for="wf-action-preview-ci">{{ t("wfPreview.ci") }}</label>
          <CiPicker
            id="wf-action-preview-ci"
            :class-id="classId"
            :selected="ci"
            :placeholder="t('wfPreview.ciPlaceholder')"
            described-by="wf-action-preview-ci-hint"
            @select="pickCi"
          />
          <span id="wf-action-preview-ci-hint" class="hint">{{ t("wfActions.preview.ciHint") }}</span>
        </div>
        <div class="inline-actions wf-preview-actions">
          <button type="submit" class="btn btn-primary btn-sm" :disabled="!key || loading" data-testid="wf-action-preview-run">
            {{ loading ? t("wfPreview.running") : t("wfPreview.run") }}
          </button>
        </div>
      </form>

      <p class="sr-only" role="status" aria-live="polite" data-testid="wf-action-preview-status">{{ announcement }}</p>
      <ErrorAlert v-if="error" :error="error" :title="t('wfPreview.failed')" />
      <div v-else-if="result" class="stack" data-testid="wf-action-preview-result">
        <p class="no-margin">
          <strong>{{ t("wfActions.preview.included", { n: result.included }) }}</strong>
          <template v-if="!result.ciId"> · <span class="muted">{{ t("wfPreview.general") }}</span></template>
          <template v-if="result.excludesActor"> · <span class="muted">{{ t("wfActions.preview.excludesActor") }}</span></template>
        </p>
        <p v-if="result.users.length === 0" class="muted no-margin">{{ t("wfPreview.nobody") }}</p>
        <div v-else class="table-wrap">
          <table class="data" data-testid="wf-action-preview-users">
            <caption class="sr-only">{{ t("wfActions.preview.caption") }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("wfPreview.user") }}</th>
                <th scope="col">{{ t("wfPreview.outcome") }}</th>
                <th scope="col">{{ t("wfPreview.via") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="u in result.users" :key="u.id">
                <td>
                  <bdi>{{ u.displayName }}</bdi> <span class="mono muted">{{ u.username }}</span>
                </td>
                <td><span :class="['badge', reasonTone[u.reason] ?? '']">{{ t(`wfActions.preview.reason.${u.reason}`) }}</span></td>
                <td class="wrap">{{ u.sources.join(", ") }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-if="result.truncated" class="muted no-margin">{{ t("wfPreview.truncated", { n: result.users.length }) }}</p>
      </div>
    </div>
  </section>
</template>
