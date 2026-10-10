<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import type { CiSummary } from "../../../api/queries";
import type { Principal } from "../../../api/services";
import { previewApprovers, type WorkflowApproverPreview } from "../../../api/workflows";
import CiPicker from "../../../components/CiPicker.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import PrincipalCombobox from "../../../components/PrincipalCombobox.vue";
import { t } from "../../../i18n";
import type { StepRow } from "../../../lib/workflowApprovals";

/**
 * Who would be asked to approve one step for one CI (SHAA-1869 §10.1). The API resolves the stored
 * assignments; the UI only shows its answer: each user eligible or out with the API's reason, and
 * what each source resolved to. A CI the admin cannot view is a 404 like everywhere else, so the
 * preview reveals nothing the admin could not already see.
 */
const props = defineProps<{
  workflowId: string;
  classId: string;
  rows: StepRow[];
  /** Unsaved assignments: the preview resolves the stored ones. */
  dirty: boolean;
}>();

const stepId = ref(props.rows[0] ? `${props.rows[0].transitionKey}|${props.rows[0].stepKey}` : "");
watch(
  () => props.rows,
  (rows) => {
    if (!rows.some((r) => `${r.transitionKey}|${r.stepKey}` === stepId.value)) stepId.value = rows[0] ? `${rows[0].transitionKey}|${rows[0].stepKey}` : "";
  },
);
const ci = ref<{ id: string; name: string } | null>(null);
const requester = ref<{ id: string; name: string } | null>(null);
/** /principals refused: the requester can only be picked by an admin who may look up users (GH#839). */
const requesterForbidden = ref(false);

const result = ref<WorkflowApproverPreview | null>(null);
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
function pickRequester(p: Principal) {
  requester.value = { id: p.id, name: p.username ? `${p.displayName} (${p.username})` : p.displayName };
}

async function run() {
  const [transition, step] = stepId.value.split("|");
  if (!transition || !step) return;
  controller?.abort();
  const c = new AbortController();
  controller = c;
  loading.value = true;
  error.value = null;
  announcement.value = "";
  try {
    const requestedBy = requesterForbidden.value ? undefined : requester.value?.id;
    const res = await previewApprovers(props.workflowId, { transition, step, ciId: ci.value?.id, requestedBy }, c.signal);
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

const reasonTone: Record<string, string> = { eligible: "ok", escalation_only: "info", excluded: "warn", no_view_right: "danger", inactive: "off" };
const shortfall = computed(() => isShort(result.value));
function isShort(r: WorkflowApproverPreview | null): boolean {
  return !!r && r.requiredApprovals !== null && r.eligibleCount < r.requiredApprovals;
}
function summary(r: WorkflowApproverPreview): string {
  const parts = [t("wfPreview.eligible", { n: r.eligibleCount })];
  if (r.requiredApprovals !== null) parts.push(t("wfPreview.needs", { n: r.requiredApprovals }));
  if (isShort(r)) parts.push(t("wfPreview.short"));
  if (!r.ciId) parts.push(t("wfPreview.general"));
  return parts.join(" · ");
}
</script>

<template>
  <section class="panel" aria-labelledby="wf-preview-title" data-testid="wf-approver-preview">
    <div class="panel-header"><h2 id="wf-preview-title">{{ t("wfPreview.title") }}</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfPreview.intro") }}</p>
      <div v-if="dirty" class="alert alert-warn" role="note">{{ t("wfPreview.unsaved") }}</div>
      <form class="wf-preview-form" @submit.prevent="run">
        <div class="field">
          <label for="wf-preview-step">{{ t("wfPreview.step") }}</label>
          <select id="wf-preview-step" v-model="stepId">
            <option v-for="r in rows" :key="`${r.transitionKey}|${r.stepKey}`" :value="`${r.transitionKey}|${r.stepKey}`">
              {{ r.transitionName }} › {{ r.stepName }}
            </option>
          </select>
        </div>
        <div class="field">
          <label for="wf-preview-ci">{{ t("wfPreview.ci") }}</label>
          <CiPicker id="wf-preview-ci" :class-id="classId" :selected="ci" :placeholder="t('wfPreview.ciPlaceholder')" described-by="wf-preview-ci-hint" @select="pickCi" />
          <span id="wf-preview-ci-hint" class="hint">{{ t("wfPreview.ciHint") }}</span>
        </div>
        <div v-if="requesterForbidden" class="field">
          <span class="label">{{ t("wfPreview.requester") }}</span>
          <span class="hint">{{ t("wfPreview.requesterForbidden") }}</span>
        </div>
        <PrincipalCombobox
          v-else
          :label="t('wfPreview.requester')"
          kind="user"
          :hint="requester ? t('wfApprovers.picked', { name: requester.name }) : t('wfPreview.requesterHint')"
          @select="pickRequester"
          @forbidden="requesterForbidden = true"
        />
        <div class="inline-actions wf-preview-actions">
          <button type="submit" class="btn btn-primary btn-sm" :disabled="!stepId || loading" data-testid="wf-preview-run">
            {{ loading ? t("wfPreview.running") : t("wfPreview.run") }}
          </button>
          <button v-if="requester && !requesterForbidden" type="button" class="btn btn-sm" @click="requester = null">{{ t("wfPreview.clearRequester") }}</button>
        </div>
      </form>

      <p class="sr-only" role="status" aria-live="polite" data-testid="wf-preview-status">{{ announcement }}</p>
      <ErrorAlert v-if="error" :error="error" :title="t('wfPreview.failed')" />
      <div v-else-if="result" class="stack" data-testid="wf-preview-result">
        <p class="no-margin">
          <strong>{{ t("wfPreview.eligible", { n: result.eligibleCount }) }}</strong>
          <template v-if="result.requiredApprovals !== null"> · {{ t("wfPreview.needs", { n: result.requiredApprovals }) }}</template>
          <span v-if="shortfall" class="badge warn spaced">{{ t("wfPreview.short") }}</span>
          <template v-if="!result.ciId"> · <span class="muted">{{ t("wfPreview.general") }}</span></template>
        </p>
        <div class="table-wrap">
          <table class="data">
            <caption class="sr-only">{{ t("wfPreview.sourcesCaption") }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("wfApprovers.role") }}</th>
                <th scope="col">{{ t("wfApprovers.source") }}</th>
                <th scope="col">{{ t("wfPreview.users") }}</th>
                <th scope="col">{{ t("wfPreview.note") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(s, i) in result.sources" :key="i">
                <td>{{ t(`wfApprovers.role.${s.role}`) }}</td>
                <td>{{ s.label }}</td>
                <td class="num">{{ s.userCount }}</td>
                <td class="wrap">
                  <span v-if="s.dropped" class="badge warn">{{ t("wfPreview.dropped") }}</span> {{ s.note ?? "" }}
                  <ul v-if="s.droppedParts.length" class="no-margin">
                    <li v-for="(d, k) in s.droppedParts" :key="k">{{ d.label }}: {{ d.message }}</li>
                  </ul>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-if="result.usersHidden" class="muted no-margin" data-testid="wf-preview-users-hidden">{{ t("wfPreview.usersHidden") }}</p>
        <p v-else-if="result.users.length === 0" class="muted no-margin">{{ t("wfPreview.nobody") }}</p>
        <div v-else class="table-wrap">
          <table class="data" data-testid="wf-preview-users">
            <caption class="sr-only">{{ t("wfPreview.usersCaption") }}</caption>
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
                <td class="wrap">
                  <span :class="['badge', reasonTone[u.reason] ?? '']">{{ t(`wfPreview.reason.${u.reason}`) }}</span>
                  <span v-if="!u.eligible" class="muted"> {{ u.message }}</span>
                </td>
                <td class="wrap">{{ u.via.join(", ") }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-if="result.truncated" class="muted no-margin">{{ t("wfPreview.truncated", { n: result.users.length }) }}</p>
      </div>
    </div>
  </section>
</template>
